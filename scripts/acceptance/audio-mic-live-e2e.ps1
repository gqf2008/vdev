# mic-agent Windows live 端到端（两次对照）：
#   A) --mix 0    直通（dry）：证明 capture → frames → ring → 注入 vdev 扬声器 → vdev 麦克风采回 这条链是通的
#   B) --adaptive 降噪（wet）：同一信号经 RNNoise，电平应明显下降 —— 证明降噪真在工作
# 信号源：往 Realtek 扬声器播 1kHz 正弦，Stereo Mix 采到它（无需物理麦克风）。
#         扬声器被静音时源电平会低到约 −30 dBFS peak（Stereo Mix 量到的就是它），
#         此时 A/B 的绝对电平反映的是"源"，不是链路衰减；要量链路本身的刻度，
#         把源换成 vdev 环回（已知 −6.02 dBFS peak）。
#
# 2026-09-15 复测到两条真 bug；**均已修**（`platform/windows.rs`，main 179480e），本脚本可复跑：
#   * 样本标度（M-g）：crate 内部统一用 int16 标度（±32768，RNNoise / wavio / metrics 口径），
#     而 WASAPI 共享模式交付/消费的是 [-1,1] float。修复前边界两侧都没换算，喂给模型的信号
#     比训练分布低约 90 dB（VAD 形同失效），`--record-in` / `--record-out` 落盘取整成 0
#     （数字静音），注入电平同错。现入口 ×32768、上环前 ÷32768（`INT16_SCALE`）。
#   * 编码校验（L9）：`check_mix_format` 原先只查 `wBitsPerSample == 32`（容器位数），
#     32bit 容器也可能是 32bit **PCM 整型**。现按 `wFormatTag` / `SubFormat` 拒绝非 IEEE float。
#   ⚠ 这两条症状**不是**"端点走 16bit PCM"：`GetMixFormat` 实测就是 IEEE float 32bit/2ch/48k，
#     注册表里的 16bit 是设备格式，引擎自己会转。
#
# ⚠ 2026-09-16：**当前瓶颈不在 agent，在驱动的环回。** `inject --endpoint vdev` +
#   `capture --endpoint vdev` 连跑，会在几轮后从理论值 −6.02 dBFS 翻到**满幅垃圾**
#   （peak −0.06 ~ −1.8 dBFS，且与注入幅度无关：0.10 / 0.25 / 0.50 得到同一个坏读数），
#   翻坏后持续坏；频谱上是被硬削顶的 1 kHz（3 kHz 分量比基频还高）。**只用 CLI 就能复现**，
#   与 mic-agent 无关。所以本脚本加了一个环回自检门（见 Assert-Loopback）：
#   自检不通过 = 环回坏了，整轮判不通过，免得把驱动的问题算到 agent 头上。
#
# ⚠ 另一处已修：本脚本原先的 `-Input` 参数与 PowerShell 自动变量 `$Input` 同名，
#   它在函数作用域里求值为「空枚举」而不是参数值，于是传给 agent 的是 `--input ""`。
#   空串能被任何设备名 `contains` 命中 ⇒ 选到枚举出的第一个采集端点，
#   在本机恰好就是 `Microphone (vdev 虚拟声卡)` 本身 ⇒ 测的是 agent 的自反馈环，
#   不是「物理声源 → agent → 虚拟麦克风」这条验收链路。
#   现改名 `-InputDevice`，并在每轮跑完后硬断言采集端点不是 vdev 自己。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-mic-live-e2e.ps1
param(
    [string]$RepoRoot,
    [string]$OutDir,
    [string]$Endpoint = 'vdev',
    [string]$ToneEndpoint = 'Realtek',
    [string]$InputDevice = 'Stereo Mix',
    [string]$MicAgent,
    [string]$AudioCli
)
$ErrorActionPreference = 'Continue'
$here = $PSScriptRoot
if (-not $RepoRoot) { $RepoRoot = Split-Path -Parent (Split-Path -Parent $here) }
if (-not $OutDir) { $OutDir = Join-Path $env:TEMP 'vdev-acceptance' }
if (-not $AudioCli) {
    $AudioCli = Join-Path $RepoRoot 'crates\vdev-audio-win\target\x86_64-pc-windows-msvc\release\vdev-audio-win.exe'
}
if (-not $MicAgent) {
    $MicAgent = Join-Path $RepoRoot 'target\x86_64-pc-windows-msvc\release\vdev-mic-agent.exe'
}
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
foreach ($p in @($AudioCli, $MicAgent)) {
    if (-not (Test-Path $p)) { throw "找不到可执行文件：$p（先 cargo build --release）" }
}
$scratch = $OutDir
$agent = $MicAgent
$cli = $AudioCli
$failed = $false

# Start-Process 把 -ArgumentList 数组按空格拼接、不替我们加引号：
# 含空格的取值必须自带引号，否则会被切成分开的参数（`--input Stereo Mix` → 多出一个 `Mix`）。
function Quote([string]$s) { '"' + $s + '"' }

# 环回自检门：设备侧 render → capture 环对 0.5 幅度 1 kHz 正弦必须给出 peak −6.02 dBFS。
# 驱动环回不稳时（实测会翻到满幅垃圾），后面每一列读数都没有意义 —— 直接判不通过。
function Assert-Loopback {
    $job = Start-Job -ScriptBlock { param($c, $e) & $c inject --endpoint $e --tone 1000 --amplitude 0.5 --duration 5 } -ArgumentList $cli, $Endpoint
    Start-Sleep -Milliseconds 700
    $r = (& $cli capture --endpoint $Endpoint --duration 3 --skip 1 --json) | ConvertFrom-Json
    Wait-Job $job | Out-Null; Remove-Job $job -Force
    $dev = [math]::Abs($r.peak_dbfs - (-6.02))
    Write-Output ("  环回自检: peak {0:N2} dBFS（期望 −6.02，偏差 {1:N2} dB）" -f $r.peak_dbfs, $dev)
    if ($dev -gt 3) {
        Write-Output "  !! 驱动环回偏离理论值 >3 dB —— 环回已损坏，本轮结论无效"
        $script:failed = $true
    }
}

function RunCase([string]$name, [string[]]$extraArgs) {
    Write-Output "########## $name"
    # 先排空驱动环：上一轮（或上一支测试，比如探针的 chirp 标记）可能留下满幅内容，
    # 进环后要一段时间才排净 —— 不排空会让本轮开头读到旧内容。
    & $cli capture --endpoint $Endpoint --duration 3 | Out-Null
    $capJson = Join-Path $scratch "mic-live-$name-cap.json"
    $liveJson = Join-Path $scratch "mic-live-$name.json"
    $liveLog = Join-Path $scratch "mic-live-$name.log"
    $outWav = Join-Path $scratch "mic-live-$name-out.wav"

    $cap = Start-Process -FilePath $cli `
        -ArgumentList 'capture','--duration','16','--skip','4','--json' `
        -PassThru -NoNewWindow -RedirectStandardOutput $capJson
    Start-Sleep -Milliseconds 500

    $liveArgs = @('live','--seconds','20','--vdev',$Endpoint,'--input',(Quote $InputDevice),
                  '--record-out',(Quote $outWav),'--report',(Quote $liveJson)) + $extraArgs
    $live = Start-Process -FilePath $agent -ArgumentList $liveArgs `
        -PassThru -NoNewWindow -RedirectStandardOutput $liveLog
    Start-Sleep -Seconds 2

    & $cli inject --endpoint $ToneEndpoint --tone 1000 --amplitude 0.5 --duration 12 | Out-Null
    Wait-Process -Id $cap.Id -ErrorAction SilentlyContinue
    Wait-Process -Id $live.Id -ErrorAction SilentlyContinue

    $capOut = Get-Content $capJson -Raw -Encoding UTF8 | ConvertFrom-Json
    Write-Output ("  vdev 麦克风: rms {0:N1} dBFS / peak {1:N1} dBFS / {2} 帧" -f $capOut.rms_dbfs, $capOut.peak_dbfs, $capOut.frames)
    if (Test-Path $liveJson) {
        $rep = Get-Content $liveJson -Raw -Encoding UTF8 | ConvertFrom-Json
        Write-Output ("  mic-agent : 帧 " + $rep.frames + " / 输入 " + $rep.capture.name +
                      (" / wet {0:N2} / CPU {1:N3}% 单核 / drop {2} starve {3} / p99 {4:N4} ms" -f `
                       $rep.wet_ratio_mean, $rep.cpu_percent_of_one_core, `
                       $rep.ring_dropped_samples, $rep.ring_starved_samples, $rep.frame_ms_p99))
        Write-Output ("  record-out: " + $outWav)

        # 硬断言：采集端点不能是虚拟设备自己，否则量到的是自反馈环（见文件头说明）
        $devName = $null
        if ($rep.vdev.name -match '\(([^()]+)\)\s*$') { $devName = $Matches[1] }
        if ($devName -and $rep.capture.name -like "*$devName*") {
            Write-Output ("  !! 采集端点 " + $rep.capture.name + " 就是虚拟设备本身 —— 自反馈环，不是验收链路")
            $script:failed = $true
        } elseif ($rep.ring_dropped_samples -gt 0) {
            Write-Output ("  !! 环丢样 " + $rep.ring_dropped_samples + "，此轮数据不可用")
            $script:failed = $true
        }
    } else {
        Write-Output "  (未生成 live 报告，见 $liveLog)"
        $script:failed = $true
    }
}

Assert-Loopback
RunCase 'dry' @('--mix','0')
RunCase 'denoise' @('--adaptive')

if ($failed) { Write-Output '结论：不通过'; exit 1 }
Write-Output '结论：通过'
exit 0
