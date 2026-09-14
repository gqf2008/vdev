# 把 vdev 的 HID 节点收敛成"键盘 / 鼠标各一个"：
#   1) CLI uninstall（会移除它认得的节点）
#   2) 兜底：把 ROOT\HIDCLASS 下残留的 vdev 节点逐个 pnputil /remove-device，最多 3 轮
#      （上一轮 CLI 在 CM_Query_And_Remove_SubTreeW 上吃到 ERROR_NOT_READY(0x17)，只删掉一部分）
#   3) CLI install 一次 → 期望恰好 2 个节点；再跑一次注入冒烟
# 需要管理员（会自动请求 UAC）。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-converge.ps1
param(
    [string]$RepoRoot,
    [string]$OutDir,
    [string]$HidCli,
    [string]$HidDist
)
$ErrorActionPreference = 'Continue'
$here = $PSScriptRoot
if (-not $RepoRoot) { $RepoRoot = Split-Path -Parent (Split-Path -Parent $here) }
if (-not $OutDir) { $OutDir = Join-Path $env:TEMP 'vdev-acceptance' }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $isAdmin) {
    Write-Output '需要管理员权限，正在请求 UAC …（请在弹窗上点“是”）'
    $fwd = @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', "`"$PSCommandPath`"",
        '-RepoRoot', "`"$RepoRoot`"", '-OutDir', "`"$OutDir`"")
    if ($HidCli) { $fwd += @('-HidCli', "`"$HidCli`"") }
    if ($HidDist) { $fwd += @('-HidDist', "`"$HidDist`"") }
    Start-Process powershell.exe -Verb RunAs -ArgumentList $fwd
    exit 0
}

Start-Transcript -Path (Join-Path $OutDir 'hid-converge.log') -Force | Out-Null

$root = $RepoRoot
$hidCli = if ($HidCli) { $HidCli } else {
    Join-Path $root 'crates\vdev-hid-win\target\x86_64-pc-windows-msvc\release\vdev-hid-win.exe'
}
$hidDist = if ($HidDist) { $HidDist } else { Join-Path $root 'crates\vdev-hid-win\target\dist' }
foreach ($p in @($hidCli, (Join-Path $hidDist 'vdev-hid.inf'))) {
    if (-not (Test-Path $p)) { throw "缺少 $p（先 cargo build --release / stage-sign-hid.ps1，或用 -HidCli/-HidDist 指定）" }
}

function List-VdevHid([string]$tag) {
    Write-Output "---- $tag"
    Get-PnpDevice -Class HIDClass -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -like 'ROOT\HIDCLASS*' -and $_.FriendlyName -match 'vdev' } |
        Select-Object Status, FriendlyName, InstanceId | Format-Table -AutoSize | Out-String -Width 120
}

List-VdevHid '清理前'
& $hidCli kernel uninstall 2>&1 | Select-Object -Last 3
Start-Sleep -Seconds 3

for ($round = 1; $round -le 3; $round++) {
    $nodes = @(Get-PnpDevice -Class HIDClass -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -like 'ROOT\HIDCLASS*' -and $_.FriendlyName -match 'vdev' })
    if ($nodes.Count -eq 0) { Write-Output "第 $round 轮：已无残留节点"; break }
    Write-Output "第 $round 轮：移除 $($nodes.Count) 个残留节点"
    foreach ($n in $nodes) {
        pnputil /remove-device $n.InstanceId 2>&1 | Select-Object -Last 1
    }
    Start-Sleep -Seconds 3
}

List-VdevHid '清理后 / 重装前'
& $hidCli kernel install --inf-dir $hidDist 2>&1 | Select-Object -Last 4
Start-Sleep -Seconds 4
List-VdevHid '重装后（期望恰好 2 个）'

Write-Output '---- 状态与注入冒烟'
& $hidCli kernel status 2>&1 | Select-Object -First 10
& $hidCli kernel key a 2>&1 | Select-Object -Last 2
Write-Output ("key_exit=$LASTEXITCODE")

Stop-Transcript | Out-Null
