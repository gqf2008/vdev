# verify-dist.ps1 — 装机包自检：文件齐全 + 签名验证（`/pa`，默认 Authenticode/驱动策略）。
#
# 用法：
#   pwsh -File scripts/verify-dist.ps1 -Dist crates/vdev-audio-win/target/dist `
#        -Inf vdev-audio.inf -Binary vdev_audio.sys -Cat vdev-audio.cat -RequireCer
#
# 说明：`signtool verify /pa` 要求证书链被本机信任。CI 里签名用的自签证书已由
# scripts/sign-common.ps1 在 VDEV_SIGN_TRUST=1 时导入 LocalMachine 的
# TrustedPublisher + Root —— 所以自签产物也能通过校验，这正是要验的：
# "目标机装上 .cer 之后能验签"。
param(
    [Parameter(Mandatory = $true)][string]$Dist,
    [Parameter(Mandatory = $true)][string]$Inf,
    [Parameter(Mandatory = $true)][string]$Binary,
    [Parameter(Mandatory = $true)][string]$Cat,
    [Switch]$RequireCer
)
$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "sign-common.ps1")
$kit = Get-VdevWdkBin
$signtool = Join-Path $kit "x64\signtool.exe"

$script:fail = 0
function Check([bool]$cond, [string]$msg) {
    if ($cond) { Write-Host "  [ok] $msg" } else { Write-Host "  [FAIL] $msg"; $script:fail++ }
}

Write-Host "== 装机包自检：$Dist"
Check (Test-Path $Dist) "目录存在"
Check (Test-Path (Join-Path $Dist $Inf)) "$Inf 存在"
Check (Test-Path (Join-Path $Dist $Binary)) "$Binary 存在"
Check (Test-Path (Join-Path $Dist $Cat)) "$Cat 存在"
if ($RequireCer) {
    Check (Test-Path (Join-Path $Dist "vdev-test-signing.cer")) `
        "vdev-test-signing.cer 已导出（目标机装它才信任测试签名）"
}

foreach ($f in @($Binary, $Cat)) {
    $p = Join-Path $Dist $f
    Write-Host "== signtool verify /pa $f"
    & $signtool verify /pa /v $p | Out-String -Stream |
        Where-Object { $_ -match 'Successfully verified|Issued to|Hash of file' } |
        Select-Object -First 4 | ForEach-Object { "     " + $_.Trim() }
    Check ($LASTEXITCODE -eq 0) "$f 通过 /pa 校验"
}

if ($script:fail -gt 0) { throw "装机包自检失败：$script:fail 项" }
Write-Host "== 自检通过"
