# stage-sign.ps1 — 把 vdev 虚拟显示器驱动打包到 target/dist 并签名
$ErrorActionPreference = "Stop"
$root = Join-Path $PSScriptRoot ".."
$rel = Join-Path $root "target\x86_64-pc-windows-msvc\release"
$dist = Join-Path $root "target\dist"
. (Join-Path $PSScriptRoot "..\..\..\scripts\sign-common.ps1")
$kit = Get-VdevWdkBin
$signtool = Join-Path $kit "x64\signtool.exe"
$inf2cat = Join-Path $kit "x86\Inf2Cat.exe"

Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dist | Out-Null
Copy-Item (Join-Path $root "driver\vdev-display.inf") $dist
Copy-Item (Join-Path $rel "vdev_display.dll") $dist
Copy-Item (Join-Path $rel "vdev-display-win.exe") $dist

# 证书来源见 scripts/sign-common.ps1（本地按 vdev-driver 找；CI 走 VDEV_SIGN_PFX / VDEV_SIGN_THUMBPRINT）；
# 这里原来用 signtool /n "vdev Virtual Display Driver" 按 CN 匹配，改成统一用 /sha1 指纹。
$cert = Get-VdevSigningCert
Export-VdevSigningCert -Cert $cert -Dist $dist

& $signtool sign /s my /sha1 $cert.Thumbprint /fd sha256 /q (Join-Path $dist "vdev_display.dll")
if ($LASTEXITCODE -ne 0) { throw "signtool dll failed" }

Push-Location $dist
& $inf2cat /driver:$dist /os:10_X64
Pop-Location
if ($LASTEXITCODE -ne 0) { throw "inf2cat failed" }

& $signtool sign /s my /sha1 $cert.Thumbprint /fd sha256 /q (Join-Path $dist "vdev-display.cat")
if ($LASTEXITCODE -ne 0) { throw "signtool cat failed" }

Write-Host "=== dist ready ==="
Get-ChildItem $dist | Select-Object Name, Length

