# mic-agent Windows live 端到端（两次对照）：
#   A) --mix 0    直通（dry）：证明 capture → frames → ring → 注入 vdev 扬声器 → vdev 麦克风采回 这条链是通的
#   B) --adaptive 降噪（wet）：同一信号经 RNNoise，电平应明显下降 —— 证明降噪真在工作
# 信号源：往 Realtek 扬声器播 1kHz 正弦，Stereo Mix 采到它（无需物理麦克风）。
#
# ⚠ 本脚本的输出目前只用来证明「链路能跑」，**不能当电平基线**（2026-09-15 真机复测）：
#   * 设备侧是好的：`vdev-audio-win.exe inject --endpoint vdev` + `capture --endpoint vdev`
#     对 0.5 幅度正弦给出 peak −6.02 dBFS，与理论值逐位吻合。
#   * agent 侧不对：`platform/windows.rs` 把设备缓冲当 **f32** 读写（drain_capture /
#     fill_render_ring 里的 `*const f32` / `*mut f32`），而 vdev 端点是 **16bit PCM**
#     （cli/src/wasapi.rs 就是按 i16 / 2 字节步长处理的，所以 CLI 精确）。结果是
#     注入电平不可信（实测把 −24.9 dBFS 的源灌成 0.0 dBFS 削顶）、
#     `--record-in` / `--record-out` 两个 WAV 全是数字静音。
#     修 `check_mix_format`（它只查了 wBitsPerSample==32，没有校验子格式是 IEEE float）
#     与两处指针转换之前，A/B 两组数字无意义。
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

function RunCase([string]$name, [string[]]$extraArgs) {
    Write-Output "########## $name"
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
        Write-Output ("  报告键: " + (($rep.PSObject.Properties.Name) -join ', '))

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

RunCase 'dry' @('--mix','0')
RunCase 'denoise' @('--adaptive')

if ($failed) { Write-Output '结论：不通过'; exit 1 }
Write-Output '结论：通过（电平基线待 platform/windows.rs 的样本布局修好后重测）'
exit 0
