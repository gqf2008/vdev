# 回归：故意制造重复节点，验证 uninstall 能一次清空并断言无残留。
#   1) install（期望 4 个节点：原一对 + 新一对）
#   2) uninstall（期望移除全部 4 个；旧实现在 CM 0x17 上 bail，只删一部分）
#   3) 断言 0 个；再 install 一次 → 恰好 2 个
# 需要管理员（会自动请求 UAC）。
#
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-uninstall-regression.ps1
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
    # 用 -Wait -PassThru 把**子进程的退出码**透传出去（UAC 被取消时不能静默 exit 0）
    try {
        $child = Start-Process powershell.exe -Verb RunAs -ArgumentList $fwd -Wait -PassThru
        exit $child.ExitCode
    } catch {
        Write-Output ("❌ 提权失败或被取消：{0}（未做任何改动）" -f $_.Exception.Message)
        exit 1
    }
}

Start-Transcript -Path (Join-Path $OutDir 'hid-uninstall-regression.log') -Force | Out-Null

$root = $RepoRoot
$hidCli = if ($HidCli) { $HidCli } else {
    Join-Path $root 'crates\vdev-hid-win\target\x86_64-pc-windows-msvc\release\vdev-hid-win.exe'
}
$hidDist = if ($HidDist) { $HidDist } else { Join-Path $root 'crates\vdev-hid-win\target\dist' }

function Nodes([string]$tag) {
    $n = @(Get-PnpDevice -Class HIDClass -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -like 'ROOT\HIDCLASS*' -and $_.FriendlyName -match 'vdev' })
    Write-Output ("---- {0}: {1} 个节点" -f $tag, $n.Count)
    $n | Select-Object FriendlyName, InstanceId | Format-Table -AutoSize | Out-String -Width 100
    return $n.Count
}

Write-Output '==== 1) 再装一次，制造重复节点'
& $hidCli kernel install --inf-dir $hidDist 2>&1 | Select-Object -Last 2
Start-Sleep -Seconds 4
$n1 = Nodes '制造重复后'

Write-Output '==== 2) uninstall（被测实现）'
& $hidCli kernel uninstall 2>&1 | Select-Object -Last 6
Write-Output ("uninstall_exit=$LASTEXITCODE")
Start-Sleep -Seconds 3
$n2 = Nodes '卸载后（期望 0）'
if ($LASTEXITCODE -eq 0 -and $n2 -eq 0) { Write-Output '✅ 回归通过：一次清空且无残留' } else { Write-Output '❌ 回归未通过' }

Write-Output '==== 3) 恢复：装回一对'
& $hidCli kernel install --inf-dir $hidDist 2>&1 | Select-Object -Last 2
Start-Sleep -Seconds 4
$n3 = Nodes '恢复后（期望 2）'
& $hidCli kernel status 2>&1 | Select-Object -First 6

Stop-Transcript | Out-Null
