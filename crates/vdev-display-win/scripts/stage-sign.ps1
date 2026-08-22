# stage-sign.ps1 — 把 vdev 虚拟显示器驱动打包到 target/dist 并签名
$ErrorActionPreference = "Stop"
$root = Join-Path $PSScriptRoot ".."
$rel = Join-Path $root "target\x86_64-pc-windows-msvc\release"
$dist = Join-Path $root "target\dist"
$kit = "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0"
$signtool = Join-Path $kit "x64\signtool.exe"
$inf2cat = Join-Path $kit "x86\Inf2Cat.exe"

Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dist | Out-Null
Copy-Item (Join-Path $root "driver\vdev-display.inf") $dist
Copy-Item (Join-Path $rel "vdev_display.dll") $dist
Copy-Item (Join-Path $rel "vdev-display-win.exe") $dist

$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.FriendlyName -eq "vdev-driver" } | Select-Object -First 1
if (-not $cert) { throw "找不到 vdev-driver 证书，请先运行 New-SelfSignedCertificate" }

& $signtool sign /s my /n "vdev Virtual Display Driver" /fd sha256 /q (Join-Path $dist "vdev_display.dll")
if ($LASTEXITCODE -ne 0) { throw "signtool dll failed" }

Push-Location $dist
& $inf2cat /driver:$dist /os:10_X64
Pop-Location
if ($LASTEXITCODE -ne 0) { throw "inf2cat failed" }

& $signtool sign /s my /n "vdev Virtual Display Driver" /fd sha256 /q (Join-Path $dist "vdev-display.cat")
if ($LASTEXITCODE -ne 0) { throw "signtool cat failed" }

Write-Host "=== dist ready ==="
Get-ChildItem $dist | Select-Object Name, Length

