# 清理 driver store 里"不再被任何 vdev 设备使用"的历史驱动包。
#
# 为什么需要：反复装驱动会往 store 里堆几十个历史包（本机实测 28 个 vdev-audio 包），
# 功能不受影响，但 `pnputil /enum-drivers` 变噪音、"现役是哪个包"难判断。
# 本脚本自动识别**当前绑在 vdev 设备上的包**并保留，其余 vdev-*.inf 包删除。
#
# 用法：
#   powershell -File scripts\acceptance\drivers-cleanup.ps1 -DryRun   # 只列不删（不需要管理员）
#   powershell -File scripts\acceptance\drivers-cleanup.ps1            # 真删（自动请求 UAC）
param(
    [string]$OutDir,
    [string[]]$KeepInf,     # 额外保留的包（如 oem135.inf）
    [switch]$DryRun
)
$ErrorActionPreference = 'Continue'
if (-not $OutDir) { $OutDir = Join-Path $env:TEMP 'vdev-acceptance' }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

$isAdmin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole(
    [Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $DryRun -and -not $isAdmin) {
    Write-Output '需要管理员权限，正在请求 UAC …（请在弹窗上点“是”）'
    $fwd = @('-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', "`"$PSCommandPath`"", '-OutDir', "`"$OutDir`"")
    if ($KeepInf) { $fwd += @('-KeepInf', ($KeepInf -join ',')) }
    try {
        $child = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -ArgumentList $fwd
        exit $child.ExitCode
    } catch {
        Write-Output ("❌ 提权失败或被取消：{0}（未做任何改动）" -f $_.Exception.Message)
        exit 1
    }
}

$log = Join-Path $OutDir ("drivers-cleanup-{0}.log" -f (Get-Date -Format 'yyyyMMdd-HHmmss'))
Start-Transcript -Path $log -Force | Out-Null
Write-Output ("模式：{0}" -f $(if ($DryRun) { 'DryRun（只列不删）' } else { '真实删除' }))

# ① 现役包：vdev 设备当前绑定的 INF（绝不动）
$keep = @{}
if ($KeepInf) { foreach ($k in $KeepInf) { $keep[$k.Trim().ToLower()] = 'KeepInf 参数' } }
Get-PnpDevice -ErrorAction SilentlyContinue |
    Where-Object { $_.FriendlyName -match 'vdev' -and $_.InstanceId -match '^ROOT\\(HIDCLASS|MEDIA|DISPLAY)' } |
    ForEach-Object {
        $inf = (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName DEVPKEY_Device_DriverInfPath -ErrorAction SilentlyContinue).Data
        if ($inf) { $keep[$inf.ToLower()] = $_.InstanceId }
    }
Write-Output '① 保留的包（vdev 设备当前绑定）'
foreach ($k in $keep.Keys) { Write-Output ("   {0}  <- {1}" -f $k, $keep[$k]) }

# ② 枚举 store 里的 vdev 包
$enumText = pnputil /enum-drivers 2>&1 | Out-String -Width 200
$targets = @()
foreach ($block in (($enumText -split 'Published Name:') | Select-Object -Skip 1)) {
    $lines = $block -split "`r?`n"
    $published = $lines[0].Trim()
    $original = (($lines | Select-String 'Original Name:') -replace '.*Original Name:\s*', '').Trim()
    if ($original -notmatch '^vdev-(hid|audio|display)\.inf$') { continue }
    if ($keep.ContainsKey($published.ToLower())) { continue }
    $ver = (($lines | Select-String 'Driver Version:') -replace '.*Driver Version:\s*', '').Trim()
    $targets += [pscustomobject]@{ Published = $published; Original = $original; Version = $ver }
}
Write-Output ("② 待删包（{0} 个）" -f $targets.Count)
foreach ($t in $targets) { Write-Output ("   {0} | {1} | {2}" -f $t.Published, $t.Original, $t.Version) }

if ($DryRun) {
    Stop-Transcript | Out-Null
    Write-Output 'DryRun 结束（未做任何改动）'
    exit 0
}

# ③ 删除
Write-Output '③ 逐个删除'
$fail = 0
foreach ($t in $targets) {
    $out = pnputil /delete-driver $t.Published /uninstall /force 2>&1 | Out-String
    $code = $LASTEXITCODE
    Write-Output ("   {0}: exit={1} {2}" -f $t.Published, $code, ($out -replace "`r?`n", ' ').Trim())
    if ($code -ne 0) { $fail++ }
}

# ④ 复查：剩余 vdev 包 + 现役设备状态
Write-Output '④ 清理后剩余的 vdev 包'
$enumText2 = pnputil /enum-drivers 2>&1 | Out-String -Width 200
foreach ($block in (($enumText2 -split 'Published Name:') | Select-Object -Skip 1)) {
    $lines = $block -split "`r?`n"
    $original = (($lines | Select-String 'Original Name:') -replace '.*Original Name:\s*', '').Trim()
    if ($original -notmatch '^vdev-(hid|audio|display)\.inf$') { continue }
    $ver = (($lines | Select-String 'Driver Version:') -replace '.*Driver Version:\s*', '').Trim()
    Write-Output ("   {0} | {1} | {2}" -f $lines[0].Trim(), $original, $ver)
}
Write-Output '⑤ 现役设备状态（应仍为 OK）'
Get-PnpDevice -ErrorAction SilentlyContinue |
    Where-Object { $_.FriendlyName -match 'vdev' } |
    Select-Object Status, Class, FriendlyName | Format-Table -AutoSize | Out-String -Width 120

Stop-Transcript | Out-Null
Write-Output ("删除失败 {0} 个；日志：{1}" -f $fail, $log)
if ($fail -gt 0) { exit 1 }
exit 0
