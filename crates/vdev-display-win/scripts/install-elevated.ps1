# install-elevated.ps1 — vdev 虚拟显示器驱动一键安装（需管理员）
$ErrorActionPreference = "Continue"
$root = Split-Path $PSScriptRoot -Parent
$dist = Join-Path $root "target\dist"
$exe = Join-Path $dist "vdev-display-win.exe"
$log = "$env:TEMP\vdev-display-install.log"
Remove-Item $log -ErrorAction SilentlyContinue
Start-Transcript -Path $log -Force

Write-Host "== 1/4 证书 =="
$cer = "$env:TEMP\vdev-driver-cert.cer"
if (Test-Path $cer) {
    certutil -addstore -f TrustedPublisher $cer | Out-Null
    certutil -addstore -f Root $cer | Out-Null
    Write-Host "cert installed"
}

Write-Host "== 2/4 清理残留 =="
$pkgs = pnputil /enum-drivers 2>&1 | Out-String
[regex]::Matches($pkgs, "Published Name:\s+(\S+\.inf)") | ForEach-Object {
    $inf = $_.Groups[1].Value
    $path = "C:\Windows\INF\$inf"
    if (Test-Path $path) {
        $txt = Get-Content $path -Raw -ErrorAction SilentlyContinue
        if ($txt -match "vdev-display") { pnputil /delete-driver $inf /uninstall /force 2>&1 | Out-Null }
    }
}
Remove-Item "C:\Windows\System32\drivers\UMDF\vdev_display.dll" -Force -ErrorAction SilentlyContinue
Write-Host "cleanup done"

Write-Host "== 3/4 安装 =="
& $exe install --inf-dir $dist
Write-Host "install exit: $LASTEXITCODE"

Write-Host "== 4/4 状态 =="
& $exe status

# 失败时补充诊断
$pnp = Get-PnpDevice -Class Display -ErrorAction SilentlyContinue | Where-Object { $_.InstanceId -match "vdev" }
if (-not $pnp) {
    Write-Host "--- setupapi.dev.log tail (vdev) ---"
    Get-Content "C:\Windows\INF\setupapi.dev.log" -Tail 200 -ErrorAction SilentlyContinue | Select-String -Pattern "vdev|!!!" | Select-Object -Last 25 | ForEach-Object { $_.Line }
}
Stop-Transcript
