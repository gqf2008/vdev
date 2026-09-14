# mic-agent Windows live 端到端（两次对照）：
#   A) --mix 0    直通（dry）：证明 capture → frames → ring → 注入 vdev 扬声器 → vdev 麦克风采回 这条链是通的
#   B) --adaptive 降噪（wet）：同一信号经 RNNoise，电平应明显下降 —— 证明降噪真在工作
# 信号源：往 Realtek 扬声器播 1kHz 正弦，Stereo Mix 采到它（无需物理麦克风）。
#
# 实测基线（2026-09-14）：dry −5.8 dBFS → denoise −34.1 dBFS。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-mic-live-e2e.ps1
param(
    [string]$RepoRoot,
    [string]$OutDir,
    [string]$Endpoint = 'vdev',
    [string]$ToneEndpoint = 'Realtek',
    [string]$Input = 'Stereo Mix',
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

    $liveArgs = @('live','--seconds','20','--vdev',$Endpoint,'--input',"`"$Input`"",
                  '--record-out',"`"$outWav`"",'--report',"`"$liveJson`"") + $extraArgs
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
        Write-Output ("  mic-agent : 帧 " + $rep.frames)
        Write-Output ("  报告键: " + (($rep.PSObject.Properties.Name) -join ', '))
    } else {
        Write-Output "  (未生成 live 报告，见 $liveLog)"
    }
}

RunCase 'dry' @('--mix','0')
RunCase 'denoise' @('--adaptive')
