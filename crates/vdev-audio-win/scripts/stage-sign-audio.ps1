# stage-sign-audio.ps1 — 把 vdev 虚拟声卡驱动打包到 target/dist 并签名（内核驱动）
$ErrorActionPreference = "Stop"
$root = Join-Path $PSScriptRoot ".."
$rel = Join-Path $root "target\x86_64-pc-windows-msvc\release"
$dist = Join-Path $root "target\dist"
$kit = "C:\Program Files (x86)\Windows Kits\10\bin\10.0.26100.0"
$signtool = Join-Path $kit "x64\signtool.exe"
$inf2cat = Join-Path $kit "x86\Inf2Cat.exe"

Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$dll = Join-Path $rel "vdev_audio.dll"
if (-not (Test-Path $dll)) {
    Write-Host "未找到 vdev_audio.dll，先执行: cargo build --release --features kernel -p vdev-audio-driver"
    exit 1
}
Copy-Item $dll (Join-Path $dist "vdev_audio.sys")
Copy-Item (Join-Path $root "driver\vdev-audio.inf") $dist
Copy-Item (Join-Path $rel "vdev-audio-win.exe") $dist

$cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.FriendlyName -eq "vdev-driver" } | Select-Object -First 1
if (-not $cert) { throw "找不到 vdev-driver 证书" }

& $signtool sign /s my /n "vdev Virtual Display Driver" /fd sha256 /q (Join-Path $dist "vdev_audio.sys")
if ($LASTEXITCODE -ne 0) { throw "signtool sys failed" }

Push-Location $dist
& $inf2cat /driver:$dist /os:10_X64
Pop-Location
if ($LASTEXITCODE -ne 0) { throw "inf2cat failed" }

& $signtool sign /s my /n "vdev Virtual Display Driver" /fd sha256 /q (Join-Path $dist "vdev-audio.cat")
if ($LASTEXITCODE -ne 0) { throw "signtool cat failed" }

Write-Host "=== dist ready ==="
Get-ChildItem $dist | Select-Object Name, Length
