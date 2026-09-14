# sign-common.ps1 — 三个 stage-sign*.ps1 共用的"找 WDK 工具 / 找签名证书"逻辑。
#
# 证书来源优先级（本地与 CI 同一套脚本，靠环境变量切换）：
#   1. $env:VDEV_SIGN_PFX      指向 .pfx（配 $env:VDEV_SIGN_PFX_PASSWORD）→ 导入 CurrentUser\My 后使用；
#      $env:VDEV_SIGN_TRUST=1 时同时导入 LocalMachine 的 TrustedPublisher + Root（需管理员，CI runner 有）。
#   2. $env:VDEV_SIGN_THUMBPRINT  直接指定指纹（CI 里现生成自签证书时用这条）。
#   3. 本地默认：CurrentUser\My 里 FriendlyName 等于 $env:VDEV_SIGN_FRIENDLY_NAME（默认 vdev-driver）
#      的第一张代码签名证书——与仓库 README 的制备步骤一致。
#
# WDK 工具链目录：优先 $env:VDEV_WDK_BIN，否则取 Windows Kits\10\bin 下版本号最大的一个。

function Get-VdevWdkBin {
    if ($env:VDEV_WDK_BIN -and (Test-Path $env:VDEV_WDK_BIN)) {
        return $env:VDEV_WDK_BIN
    }
    $base = "C:\Program Files (x86)\Windows Kits\10\bin"
    $cands = Get-ChildItem $base -Directory -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '^\d+\.\d+\.\d+\.\d+$' } |
        Sort-Object { [version]$_.Name } -Descending
    foreach ($c in $cands) {
        if (Test-Path (Join-Path $c.FullName "x64\signtool.exe")) { return $c.FullName }
    }
    throw "找不到 WDK 的 signtool（装 WDK 10 或设 VDEV_WDK_BIN）"
}

function Import-VdevPfx {
    param([Parameter(Mandatory = $true)][string]$Path)
    $pwd = ConvertTo-SecureString -String ($env:VDEV_SIGN_PFX_PASSWORD | ForEach-Object { $_ }) -AsPlainText -Force
    $cert = Import-PfxCertificate -FilePath $Path -CertStoreLocation Cert:\CurrentUser\My -Password $pwd
    if ($env:VDEV_SIGN_TRUST -eq "1") {
        # 让"装了这张证书"的机器能直接 pnputil 安装（CI 里用于自检装机包；本地一般已在 TrustedPublisher/Root）
        certutil -addstore -f TrustedPublisher $Path | Out-Null
        certutil -addstore -f Root $Path | Out-Null
    }
    return $cert
}

function Get-VdevSigningCert {
    if ($env:VDEV_SIGN_PFX) {
        if (-not (Test-Path $env:VDEV_SIGN_PFX)) { throw "VDEV_SIGN_PFX 指向的文件不存在：$env:VDEV_SIGN_PFX" }
        return Import-VdevPfx -Path $env:VDEV_SIGN_PFX
    }
    if ($env:VDEV_SIGN_THUMBPRINT) {
        $c = Get-Item "Cert:\CurrentUser\My\$($env:VDEV_SIGN_THUMBPRINT)" -ErrorAction SilentlyContinue
        if (-not $c) { throw "VDEV_SIGN_THUMBPRINT 指定的证书不在 CurrentUser\My：$env:VDEV_SIGN_THUMBPRINT" }
        return $c
    }
    $name = if ($env:VDEV_SIGN_FRIENDLY_NAME) { $env:VDEV_SIGN_FRIENDLY_NAME } else { "vdev-driver" }
    $cert = Get-ChildItem Cert:\CurrentUser\My -CodeSigningCert | Where-Object { $_.FriendlyName -eq $name } | Select-Object -First 1
    if (-not $cert) {
        throw "找不到签名证书（CurrentUser\My 里 FriendlyName=$name 的代码签名证书）。本地按 README 的 New-SelfSignedCertificate 制备，或设 VDEV_SIGN_PFX / VDEV_SIGN_THUMBPRINT。"
    }
    return $cert
}

# 把证书公钥导出到 dist，随装机包一起发布——目标机装上它才能信任测试签名
function Export-VdevSigningCert {
    param(
        [Parameter(Mandatory = $true)]$Cert,
        [Parameter(Mandatory = $true)][string]$Dist,
        [string]$Name = "vdev-test-signing.cer"
    )
    Export-Certificate -Cert $Cert -FilePath (Join-Path $Dist $Name) -Type CERT -Force | Out-Null
    Write-Host "已导出证书公钥：$Name（目标机需 certutil -addstore TrustedPublisher/Root 后安装）"
}
