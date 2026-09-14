# stage-sign-hid.ps1 — 把 vdev 虚拟键盘驱动打包到 target/dist 并签名（内核驱动，测试签名）
$ErrorActionPreference = "Stop"
$root = Join-Path $PSScriptRoot ".."
$rel = Join-Path $root "kernel\target\x86_64-pc-windows-msvc\release"
$dist = Join-Path $root "target\dist"
. (Join-Path $PSScriptRoot "..\..\..\scripts\sign-common.ps1")
$kit = Get-VdevWdkBin
$signtool = Join-Path $kit "x64\signtool.exe"
$inf2cat = Join-Path $kit "x86\Inf2Cat.exe"

Remove-Item $dist -Recurse -Force -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force -Path $dist | Out-Null
$dll = Join-Path $rel "vdev_hid.dll"
if (-not (Test-Path $dll)) {
    Write-Host "未找到 vdev_hid.dll，先执行: cargo build --release（kernel 目录）"
    exit 1
}
Copy-Item $dll (Join-Path $dist "vdev_hid.sys")
Copy-Item (Join-Path $root "kernel\driver\vdev-hid.inf") $dist

# 证书来源见 scripts/sign-common.ps1（本地按 vdev-driver 找；CI 走 VDEV_SIGN_PFX / VDEV_SIGN_THUMBPRINT）
$cert = Get-VdevSigningCert
Export-VdevSigningCert -Cert $cert -Dist $dist

# 用 /sha1（指纹）选证书，不用 /n：signtool 的 /n 匹配主题 **CN 值**
# （如 "vdev Virtual Display Driver"），传 $cert.Subject 这种完整 DN（"CN=..."）
# 会直接报 "No certificates were found that met all the given criteria"。
& $signtool sign /s my /sha1 $cert.Thumbprint /fd sha256 /q (Join-Path $dist "vdev_hid.sys")
if ($LASTEXITCODE -ne 0) { throw "signtool sys failed" }

Push-Location $dist
& $inf2cat /driver:$dist /os:10_X64
Pop-Location
if ($LASTEXITCODE -ne 0) { throw "inf2cat failed" }

# signtool /n 按证书主题名匹配：直接用查到的证书自身 Subject，
# 禁止硬编码名称（原脚本误抄显示驱动的 "vdev Virtual Display Driver"，与 HID 证书不符）
& $signtool sign /s my /sha1 $cert.Thumbprint /fd sha256 /q (Join-Path $dist "vdev-hid.cat")
if ($LASTEXITCODE -ne 0) { throw "signtool cat failed" }

Write-Host "=== dist ready ==="
Get-ChildItem $dist | Select-Object Name, Length
