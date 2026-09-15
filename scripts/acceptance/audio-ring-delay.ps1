# 用 mic-agent 做连续写者把驱动环形缓冲灌满，再打一个标记音，量「渲染写入 → 采集读出」的积压上限。
# 环容量决定这个数：设备格式 16bit/48k/2ch = 192000 B/s ⇒ 1 MB≈5.46 s、256 KB≈1.37 s
# （实测：1 MB 环 5.14 s、256 KB 环 1.48 s —— 量的是后沿，见 audio-ring-delay-analysis.py）。
#
# 前置：声卡驱动已安装；信号源用「立体声混音 / Stereo Mix」代替物理麦克风。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-ring-delay.ps1 -Tag before
#
# ⚠ 2026-09-15：`-Input` 改名 `-InputDevice`。`$Input` 与 PowerShell 自动变量同名，
#   函数作用域里求值为空枚举 ⇒ 传给 agent 的 `--input ""` 会被任意设备名命中，
#   选到第一个采集端点（本机即 vdev 麦克风自身），量的是自反馈环而非物理链路。
param(
    [string]$Tag = 'after',
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
$root = $RepoRoot
$cli = $AudioCli
$agent = $MicAgent
$wav = Join-Path $OutDir "ring-delay-$Tag.wav"

# Start-Process 把 -ArgumentList 数组按空格拼接、不替我们加引号：
# 含空格的取值必须自带引号，否则会被切成分开的参数。
function Quote([string]$s) { '"' + $s + '"' }

# 排空环（2s 采集）
& $cli capture --endpoint $Endpoint --duration 2 | Out-Null

# 连续写者（14s dry 直通；先不采集 → 环灌满）
$live = Start-Process -FilePath $agent `
    -ArgumentList 'live','--mix','0','--seconds','14','--vdev',$Endpoint,'--input',(Quote $InputDevice) `
    -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $OutDir "ring-delay-$Tag-live.log")
Start-Sleep -Seconds 3

# 采集 10s → WAV
$cap = Start-Process -FilePath $cli `
    -ArgumentList 'capture','--endpoint',$Endpoint,'--duration','10','--wav',(Quote $wav) `
    -PassThru -NoNewWindow -RedirectStandardOutput (Join-Path $OutDir "ring-delay-$Tag-cap.log")
Start-Sleep -Seconds 2

# 标记音 1.5s（从真实声卡出 → 立体声混音采到 → 穿过整条链路）
& $cli inject --endpoint $ToneEndpoint --tone 1000 --amplitude 0.8 --duration 1.5 | Out-Null
Write-Output '标记音已注入（采集开始后约 2.0s）'
Wait-Process -Id $cap.Id -ErrorAction SilentlyContinue
Wait-Process -Id $live.Id -ErrorAction SilentlyContinue

python (Join-Path $here 'audio-ring-delay-analysis.py') $wav $Tag --expect-tail 2.0
