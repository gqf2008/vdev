# vdev-display-win — Windows 虚拟显示器（IddCx UMDF，Rust）

在 Windows x64 上提供**真虚拟显示器**（Indirect Display Driver，IddCx + UMDF 用户态驱动）：

- 系统识别为第二块显示器，可扩展/镜像桌面、设置分辨率与刷新率；
- 驱动是 UMDF（用户态），崩溃不影响内核；WUDFHost 进程隔离；
- 完全用 Rust 编写（绑定层来自 MIT 项目 virtual-display-rs，见 THIRD_PARTY.md）。

## 组成

| crate | 说明 |
|---|---|
| `driver` | 虚拟显示器驱动 DLL（`vdev_display.dll`，UMDF + IddCx），含命名管道 IPC 服务 |
| `cli` | `vdev-display-win.exe`：安装/卸载/状态 + 显示器增删改查（SetupAPI + IPC） |
| `wdf-umdf-sys` | bindgen 生成的 UMDF + IddCx 绑定（MIT，原样） |
| `wdf-umdf` | WDF/IddCx 安全封装（MIT，原样） |
| `driver-ipc` | 驱动 IPC 协议（named pipe + serde_json，MIT，原样） |
| `driver-logger` | Windows 事件日志 + DebugView 日志（MIT，原样） |

## 构建

前置：Visual Studio 2022（含 C++ 桌面负载）、WDK 10.0.26100（`winget install Microsoft.WindowsWDK.10.0.26100`）、
LLVM（bindgen 用 libclang，`winget install LLVM.LLVM`）、Rust stable（MSVC target）。

```powershell
cd crates\vdev-display-win
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo build --release
```

产物：`target\x86_64-pc-windows-msvc\release\vdev_display.dll` 与 `vdev-display-win.exe`。

## 签名与安装

UMDF 驱动包需要签名；本机开发用自签名代码签名证书（装进 TrustedPublisher + Root）：

```powershell
# 1) 生成自签名代码签名证书（一次性）
New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=vdev Virtual Display Driver" `
  -FriendlyName "vdev-driver" -CertStoreLocation Cert:\CurrentUser\My `
  -KeyExportPolicy Exportable -KeySpec Signature -KeyUsage DigitalSignature `
  -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3")

# 2) 打包 + 签名 DLL + 生成并签名目录文件（vdev-display.cat）
.\scripts\stage-sign.ps1          # 输出到 target\dist

# 3) 把证书装进系统根 + 受信任发布者（管理员）
certutil -addstore -f TrustedPublisher vdev-driver-cert.cer
certutil -addstore -f Root vdev-driver-cert.cer

# 4) 安装驱动（管理员；SetupAPI 创建设备节点 Root\vdev-display 并从 INF 安装）
vdev-display-win.exe install --inf-dir target\dist
```

> 说明：虚拟显示器是 UMDF（用户态）驱动，自签名证书通常即可，无需开测试签名。
> 若安装报签名错误，可开启测试签名（`bcdedit /set testsigning on` + 重启）。

## 使用

```powershell
vdev-display-win.exe status          # 驱动安装状态
vdev-display-win.exe add 1920x1080   # 添加一块 1920x1080@60 虚拟屏
vdev-display-win.exe add 3840x2160@120 1280x720@60/120 --name "vdev-4k"
vdev-display-win.exe list            # 列出当前虚拟屏
vdev-display-win.exe set-mode 0 2560x1440@144
vdev-display-win.exe remove 0        # 移除
vdev-display-win.exe remove-all
vdev-display-win.exe persist         # 把配置写入注册表（HKCU\SOFTWARE\vdev-display）
vdev-display-win.exe uninstall       # 卸载驱动（管理员）
```

添加后系统设置/`EnumDisplayDevices` 会看到新显示器，桌面可扩展过去（IddCx 由 OS 直接渲染）。

## 与 macOS 版语义对照

| macOS（CGSVirtualDisplay） | Windows（本驱动） |
|---|---|
| 创建/销毁虚拟屏 | `add` / `remove` |
| 分辨率/刷新率 | `add 1920x1080@60/120`、`set-mode` |
| 屏幕内容由宿主推入 | OS 直接把桌面渲染进虚拟屏（远程推流场景可扩展 swap chain 注入） |

## 验收

```powershell
cargo fmt --all -- --check
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"; cargo clippy --all-targets -- -D warnings
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"; cargo test
```
