# 干净重装 vdev HID 驱动：删掉 store 里的驱动包 → 重新安装一份 → 打印节点与驱动文件。
#
# 为什么需要它：反复 install/uninstall 会在设备树里留下幽灵/重复节点（ROOT\HIDCLASS\000X），
# 之后 CLI 的写入可能落到无效实例上，表现为「命令成功但注入无反应」。hid-converge.ps1 只能清节点，
# 而同版本驱动包不会替换文件——要彻底复位就得先 pnputil /delete-driver 再装。
#
# 需要管理员（脚本自己请求 UAC）。
# 用法：powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-reinstall.ps1
#   # 可选：-HidCli <exe> -HidDist <dist 目录> -OutDir <日志目录>
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
if (-not $HidCli) {
    $HidCli = Join-Path $RepoRoot 'crates\vdev-hid-win\target\x86_64-pc-windows-msvc\release\vdev-hid-win.exe'
}
if (-not $HidDist) { $HidDist = Join-Path $RepoRoot 'crates\vdev-hid-win\target\dist' }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
foreach ($p in @($HidCli, (Join-Path $HidDist 'vdev-hid.inf'))) {
    if (-not (Test-Path $p)) { throw "缺少 $p（先 cargo build --release / stage-sign-hid.ps1，或用 -HidCli/-HidDist 指定）" }
}

if (-not ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Output '需要管理员权限，正在请求 UAC …（请在弹窗上点“是”）'
    $fwd = @(
        '-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', "`"$PSCommandPath`"",
        '-RepoRoot', "`"$RepoRoot`"", '-OutDir', "`"$OutDir`"",
        '-HidCli', "`"$HidCli`"", '-HidDist', "`"$HidDist`"")
    try {
        $child = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -ArgumentList $fwd
        exit $child.ExitCode
    } catch {
        Write-Output ("❌ 提权失败或被取消：{0}（未做任何改动）" -f $_.Exception.Message)
        exit 1
    }
}

$log = Join-Path $OutDir 'hid-reinstall.log'
Start-Transcript -Path $log -Force | Out-Null

function Show-Nodes([string]$tag) {
    Write-Output "---- $tag"
    Get-PnpDevice -Class HIDClass -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -like 'ROOT\HIDCLASS*' -and $_.FriendlyName -match 'vdev' } |
        Select-Object Status, FriendlyName, InstanceId | Format-Table -AutoSize | Out-String -Width 120
}

Show-Nodes '① 现状'

Write-Output '② 移除现有 vdev 节点（CLI + pnputil 兜底，最多 3 轮）'
& $HidCli kernel uninstall 2>&1 | Select-Object -Last 3
Start-Sleep -Seconds 2
for ($round = 1; $round -le 3; $round++) {
    $nodes = @(Get-PnpDevice -Class HIDClass -ErrorAction SilentlyContinue |
        Where-Object { $_.InstanceId -like 'ROOT\HIDCLASS*' -and $_.FriendlyName -match 'vdev' })
    if ($nodes.Count -eq 0) { Write-Output "   第 $round 轮：已无残留"; break }
    Write-Output "   第 $round 轮：pnputil 兜底 $($nodes.Count) 个"
    foreach ($n in $nodes) { pnputil /remove-device $n.InstanceId 2>&1 | Select-Object -Last 1 }
    Start-Sleep -Seconds 3
}

Write-Output '③ 删除 store 里的 vdev-hid.inf 驱动包（同版本不删不会替换文件）'
$enumText = pnputil /enum-drivers 2>&1 | Out-String -Width 200
$deleted = 0
foreach ($block in (($enumText -split 'Published Name:') | Select-Object -Skip 1)) {
    if ($block -match 'vdev-hid\.inf') {
        $name = (($block -split "`r?`n")[0]).Trim()
        Write-Output "   -> delete-driver $name"
        pnputil /delete-driver $name /uninstall /force 2>&1 | ForEach-Object { "      $_" }
        $deleted++
    }
}
if ($deleted -eq 0) { Write-Output '   （没找到 vdev-hid.inf 包）' }

Write-Output '④ 重新安装'
& $HidCli kernel install --inf-dir $HidDist 2>&1 | Select-Object -Last 6
Start-Sleep -Seconds 5
Show-Nodes '⑤ 重装后（期望恰好 2 个：键盘 + 鼠标）'

Write-Output '⑥ 驱动文件'
Get-Item C:\Windows\System32\drivers\vdev_hid.sys -ErrorAction SilentlyContinue |
    Select-Object LastWriteTime, Length | Format-List
$sys = 'C:\Windows\System32\drivers\vdev_hid.sys'
$distSys = Join-Path $HidDist 'vdev_hid.sys'
if ((Test-Path $sys) -and (Test-Path $distSys)) {
    $same = (Get-FileHash $sys).Hash -eq (Get-FileHash $distSys).Hash
    Write-Output ("   与 dist 的 vdev_hid.sys 同哈希 = {0}" -f $same)
}

Stop-Transcript | Out-Null

$left = @(Get-PnpDevice -Class HIDClass -ErrorAction SilentlyContinue |
    Where-Object { $_.InstanceId -like 'ROOT\HIDCLASS*' -and $_.FriendlyName -match 'vdev' })
if ($left.Count -ne 2) {
    Write-Output ("❌ 重装后节点数为 {0}（期望 2）：{1}" -f $left.Count, ($left.InstanceId -join ', '))
    exit 1
}
Write-Output '✅ 重装完成：恰好一对（键盘 + 鼠标）'
exit 0
