# vdev — 用 Rust 造虚拟设备（macOS + Windows）

用 Rust 在 **macOS（Apple Silicon）** 与 **Windows（x64）** 上实现的虚拟设备集合——
键盘/鼠标、摄像头、显示器、声卡，每个都让操作系统"相信"有一个真设备存在，可被任意 App
当真实硬件使用。**全 Rust**：macOS 绑定手写；Windows 侧 display / HID 由 WDK 头经 bindgen
生成，audio 为手写精简绑定。

| 虚拟设备 | macOS（Apple Silicon） | Windows（x64） |
|---|---|---|
| 键盘 / 鼠标 | CGEventPost 用户态注入（`vdev-hid`）✅ | 用户态 CLI + **VHF 虚拟 HID 内核驱动**（`vdev-hid-win`）✅ |
| 摄像头 | CMIOExtension，100% Rust（`vdev-camera-ext`）✅ | DirectShow 源过滤器，用户态免签名（`vdev-camera-win`）✅ |
| 显示器 | CGVirtualDisplay 私有 API（`vdev-screen`）✅ | IddCx UMDF 间接显示驱动（`vdev-display-win`）✅ |
| 声卡 | CoreAudio HAL AudioServerPlugIn（`vdev-audio`）✅ | PortCls / WaveRT miniport，WDM 内核（`vdev-audio-win`）✅ |

> **状态图例**：✅ 已可用（真机实测）　🔧 代码已合入 main、构建与自测通过（真机验证进行中）
> 　🚧 开发中。**双平台四类设备当前均为 ✅**：Windows 侧四个驱动都在 Win10 19045 x64
> （测试签名）上装机验证过——显示器出第二块屏、声卡端点可开流且环回有数据、键盘鼠标出现在
> 设备管理器且实弹注入生效、摄像头 ffmpeg/VLC 可取流。逐个组件的现状与局限见
> [文档导航](#文档导航) 与 [docs/](docs/README.md)。

**为什么不用 kext / DriverKit**：kext 在 Apple Silicon 上已死（需关 SIP，Intel-only）；
DriverKit 只支持 C++，Rust 只能做 C ABI 内核、工程成本高。macOS 虚拟设备的正统玩法是
**用户态插件 / 事件服务**——HID 走 `CGEventPost`、摄像头走 CMIOExtension、屏幕走
`CGVirtualDisplay` 私有 API、声卡走 CoreAudio HAL `AudioServerPlugIn`。Windows 同理但门槛
分布不同：摄像头有用户态 DirectShow 捷径，显示器有官方 IddCx UMDF，只有声卡与内核 HID
必须走内核。选型细节见 [`docs/dev/macos-route-survey.md`](docs/dev/macos-route-survey.md)。

## 快速开始

```bash
cargo build --release

# 键盘 / 鼠标（注入无需辅助功能权限；listen 需要）
vdev hid type "hello from vdev"
vdev hid key space                 # 键名只收名字，全表见 vdev hid key --help
vdev hid move 100 100 && vdev hid click 100 100 --button left
vdev hid listen --seconds 10       # 无权限时立即报错、非零退出

# 虚拟屏幕（私有 API，仅供学习）
vdev screen list
vdev screen create --width 1920 --height 1080 --name vdev-demo

# 摄像头出帧核心
vdev camera frame --out /tmp/frame.ppm
```

三块需要"装机"的设备各自一条命令，细节见对应文档：

```bash
make -C crates/vdev-camera install-rust   # 虚拟摄像头 → /Applications/VDCamera.app（打开后点「安装虚拟摄像头」）
make -C crates/vdev-audio  install        # 虚拟声卡 → /Library/Audio/Plug-Ins/HAL（需管理员）
cargo build -p vdev-app --release         # 宿主 App（摄像头推流 / 虚拟屏 / 状态面板）
```

## 现状

| 组件 | 平台 | 状态 | 说明 |
|---|---|---|---|
| `vdev-hid` | macOS | ✅ 可用 | 键码注入 / 文本 / 鼠标 / 监听 |
| `vdev-screen` | macOS | ✅ 可用 | 私有 API，仅供学习；不同 macOS 版本行为可能不同 |
| `vdev-camera-ext` | macOS | ✅ 可用 | 实测 macOS 26.5：QuickTime/会议可见，1920×1080@60 稳定 |
| `vdev-audio` | macOS | ✅ 可用 | 两台设备 A/B，输出环回输入 |
| `vdev-mic-agent` | 双平台 | macOS 可用 / Windows 门禁绿、真机音频行为待验 | 端侧 RNNoise 降噪注入虚拟麦克风（其 Windows 依赖的 `vdev-audio-win` 环回驱动已真机验证） |
| `vdev-camera-win` | Windows | ✅ 可用 | 用户态 COM，免签名，`install` 即用 |
| `vdev-display-win` | Windows | ✅ 可用 | IddCx UMDF，自签名通常即可；`add/list/set-mode/remove` 需管理员（管道 SDDL 只给 BA/SY） |
| `vdev-audio-win` | Windows | ✅ 可用 | PortCls/WaveRT WDM，需测试签名；CLI `inject`/`capture` + GUI 环回自测 |
| `vdev-hid-win` | Windows | ✅ 可用 | VHF 虚拟 HID 驱动（Win10/11 通用），需测试签名；键鼠注入实测生效 |
| `vdev-app-win` | Windows | ✅ 可用 | 宿主 GUI：摄像头推流 / 显示器增删 / 声卡注入与环回自测 / 键鼠注入 |

CI（`.github/workflows/ci.yml`）全部为**硬门禁**：macOS 主 workspace 的
fmt / check / test / clippy `-D warnings` / `build --release`；Windows 五个用户态 workspace
与 `vdev-mic-agent` 的 fmt / check / clippy（test 按包）；`windows-driver-wdk` 构建
（display 的 driver + bindgen、hid 的 wdk-build，runner 上装 WDK + LLVM）。

> 最后一步 `build --release` 不能省：check/test/clippy 都不做最终链接，FFI 声明错框架
> （如把 AudioUnit 符号挂在 CoreAudio 上）只有真 build 才在 ld 阶段报未定义符号。

## 仓库结构

```
crates/                         # 主 workspace（macOS-only）
  vdev-hid/        键盘/鼠标：键码注入、文本、鼠标移动/点击/滚动（CGEventPost）
  vdev-screen/     虚拟屏幕：CGVirtualDisplay 私有 API 封装
  vdev-camera/     摄像头：共享帧生成核心（lib）
  vdev-camera-ext/ 摄像头：CMIOExtension 全 Rust 扩展（手写 objc2 绑定 + FrameChannel + 滤镜）
  vdev-audio/      声卡：CoreAudio HAL AudioServerPlugIn（输出环回输入，自研 BlackHole）
  vdev-filter/     实时图像滤镜管线（美颜 / 背景替换，Vision）
  vdev-mic-agent/  AI 虚拟麦克风端侧链路（CoreAudio / WASAPI 双后端）
  vdev-app/        macOS 宿主 App（Rust + Slint）
  vdev-host/       统一命令行入口（二进制名 vdev）
crates/*-win/       # Windows 侧：各自独立 workspace
  vdev-camera-win/  摄像头：DirectShow 源过滤器（用户态 COM）
  vdev-display-win/ 显示器：IddCx UMDF 驱动 + CLI + driver-ipc
  vdev-audio-win/   声卡：PortCls/WaveRT miniport（WDM）+ CLI
  vdev-hid-win/     键盘/鼠标：SendInput 用户态 + VHF 虚拟 HID 内核驱动
  vdev-app-win/     Windows 宿主 App（Rust + Slint）
docs/                文档：README.md 索引；community/ 对外系列；dev/ 内部开发笔记
```

## 构建与安装

### macOS

```bash
cargo build --release            # 三个纯用户态设备（HID / 屏幕 / 摄像头出帧核心）

make -C crates/vdev-camera install-rust   # 摄像头宿主 + 扩展 → /Applications/VDCamera.app
make -C crates/vdev-audio  install        # 声卡 HAL 插件（Developer ID 签名；需管理员）
make -C crates/vdev-audio  test           # 环回自测（需 ffmpeg + python3）
```

摄像头推流：安装后在 App 里点「屏幕推流」，或用 `crates/vdev-camera/tools/push_frames.swift`
推图片 / 屏幕 / 视频；扩展监听 `127.0.0.1:27890`，收 36 字节小端头 + BGRA32 的帧。
设备侧滤镜由扩展进程的环境变量配置（`VDEV_FILTER` / `VDEV_BG`）。

### Windows

各 `*-win` 是独立 workspace。前置：Rust stable（MSVC）+ Visual Studio 2022（C++ 桌面负载）；
**只有构建 display / audio / HID 的驱动**才额外需要
[WDK 10.0.26100](https://learn.microsoft.com/windows-hardware/drivers/download-the-wdk) 与
LLVM（bindgen 用 libclang）——camera-win / app-win 是纯用户态组件，不需要 WDK。

```powershell
# 摄像头（免签名，直接可用）
cd crates\vdev-camera-win
cargo build --release
.\target\release\vdev-camera-win.exe install
ffmpeg -f dshow -list_devices true -i dummy     # 应看到 "vdev-camera"

# 显示器 / 声卡 / HID 驱动：需先签名（见下），再 install
vdev-display-win.exe install --inf-dir target\dist
vdev-display-win.exe add 1920x1080                      # 增删改查虚拟屏（需管理员）
vdev-display-win.exe list / set-mode 0 2560x1440@144 / remove 0

# 虚拟声卡：装机后可直接注入 / 采集验证（一条命令跑环回自测）
cd crates\vdev-audio-win; cargo build --release
vdev-audio-win.exe inject --tone 1000 --amplitude 0.5 --duration 4   # 注入到「vdev 扬声器」
vdev-audio-win.exe capture --duration 6 --skip 4                     # 从「vdev 麦克风」采集并报电平

# 内核 HID（VHF）：装好后即可注入
vdev-hid-win.exe kernel install && vdev-hid-win.exe kernel status
vdev-hid-win.exe kernel key a / vdev-hid-win.exe kernel mouse move 20 0
```

驱动签名（一次性制备证书，Subject / FriendlyName 必须与 `crates/*/scripts/stage-sign*.ps1`
的选择条件一致）：

```powershell
$cert = New-SelfSignedCertificate -Type CodeSigningCert `
-Subject "CN=vdev Virtual Display Driver" -FriendlyName "vdev-driver" `
-CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy Exportable `
-KeySpec Signature -KeyUsage DigitalSignature `
-TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3")
Export-Certificate -Cert $cert -FilePath vdev-cert.cer
certutil -addstore -f TrustedPublisher vdev-cert.cer     # 管理员
certutil -addstore -f Root vdev-cert.cer                 # 管理员
bcdedit /set testsigning on                              # 声卡 / 内核 HID 需要，重启生效
```

宿主 GUI（`vdev-app-win`）四个页签对应四类设备：摄像头推流、显示器增删、声卡注入/环回自测、
键鼠注入。它不直接调驱动，而是委托各 `*-win` CLI（找不到 exe 时设 `VDEV_*_EXE` 或放到同目录）：

```powershell
cd crates\vdev-app-win; cargo build --release; .\target\release\vdev-app-win.exe
```

## 发版与签名

**打 tag 即发版**（`.github/workflows/release.yml`）：CI 在 Windows runner 上装 WDK + LLVM、
构建三个内核驱动与用户态工具、调用各 crate 的 `scripts/stage-sign*.ps1` 打包签名、
跑 `scripts/verify-dist.ps1` 自检（文件齐全 + `signtool verify /pa`）、压 zip 并挂到 GitHub Release
（同时生成 `SHA256SUMS.txt`）。手动触发 `workflow_dispatch` 会出一份 prerelease，用于验证流水线本身。

```bash
git tag v0.3.9.0 && git push origin v0.3.9.0     # 触发 release.yml
```

发布包（每个 zip 内含已签名的驱动 + INF + CAT + 对应 CLI，可能含 `vdev-test-signing.cer`）：
`vdev-hid-win-*.zip`、`vdev-audio-win-*.zip`、`vdev-display-win-*.zip`、`vdev-tools-win-*.zip`。

**两种签名模式**（同一套脚本，见 `scripts/sign-common.ps1`）：

| 模式 | 证书来源 | 产物能装在哪 |
|---|---|---|
| 测试签名（默认） | CI 现生成自签证书，公钥导出成包内 `vdev-test-signing.cer` | 目标机需 `certutil -addstore` 信任该证书 **且**开启 `bcdedit /set testsigning on` |
| 官方签名 | 仓库 secrets `VDEV_SIGN_PFX_BASE64` + `VDEV_SIGN_PFX_PASSWORD`（EV / Azure Trusted Signing 证书） | 任意默认配置的 Windows——需先走微软 attestation/WHQL 提交流程把 `.cat` 交给微软重签 |

本地等价操作（不依赖 CI，也不依赖某个 worktree——任意检出都能跑）：

```powershell
# 证书来源由环境变量决定；不设则用 CurrentUser\My 里 FriendlyName=vdev-driver 的证书
$env:VDEV_SIGN_PFX = "C:\path\to\vdev.pfx"     # 可选：用 PFX（配 VDEV_SIGN_PFX_PASSWORD）
$env:VDEV_SIGN_TRUST = "1"                     # 可选：同时导入 LocalMachine 的 TrustedPublisher+Root（管理员）

powershell -File crates\vdev-audio-win\scripts\stage-sign-audio.ps1       # → target\dist
powershell -File scripts\verify-dist.ps1 -Dist crates\vdev-audio-win\target\dist `
  -Inf vdev-audio.inf -Binary vdev_audio.sys -Cat vdev-audio.cat -RequireCer
```

> 自签产物仅供开发/测试机使用；对外分发必须由微软签名（attestation signing 不需要 HLK，
> 但要 EV 证书或 Azure Trusted Signing，并持有 Partner Center 提交凭据——这一步目前是手工/后续接线点）。

## 文档导航

文档分两层：[`docs/README.md`](docs/README.md) 是索引。**`docs/community/` 是对外发布的社区系列**
（9 篇 + 公告，含每个设备的完整实现与踩坑），**`docs/dev/` 是内部开发笔记**（可能滞后于实现，
冲突时以代码为准）。组件级构建/签名/验收说明在各 crate 内。

| 想了解什么 | 去哪看 |
|---|---|
| 快速了解项目能做什么 | [`docs/community/announcement-ai-mic.md`](docs/community/announcement-ai-mic.md) |
| 各设备怎么写出来的（含踩坑） | [`docs/community/README.md`](docs/community/README.md)（系列总目录） |
| Windows 声卡（驱动/CLI/GUI/验收） | [`crates/vdev-audio-win/README.md`](crates/vdev-audio-win/README.md) |
| 显示器驱动构建/签名/CLI/验收 | [`crates/vdev-display-win/README.md`](crates/vdev-display-win/README.md) |
| 内核 HID（VHF）驱动构建/注入 | [`crates/vdev-hid-win/kernel/driver/README.md`](crates/vdev-hid-win/kernel/driver/README.md) |
| AI 虚拟麦克风 | [`crates/vdev-mic-agent/README.md`](crates/vdev-mic-agent/README.md) |
| macOS 路线调研 / 选型过程 | [`docs/dev/`](docs/dev) |

## 路线图

- [x] macOS：HID / 屏幕 / 摄像头（CMIOExtension 全链路）/ 声卡 可用
- [x] macOS：虚拟屏 + 摄像头串流 + SFU 端到端；设备侧滤镜（美颜 / 背景替换）
- [x] Windows：DirectShow 虚拟摄像头（用户态免签名）可用
- [x] Windows：IddCx UMDF 显示器 / PortCls WaveRT 声卡 / VHF 虚拟 HID —— 代码与门禁就绪
- [x] Windows：三驱动真机安装验证（测试签名）——显示器第二块屏、声卡端点可开流且环回有数据、
  键鼠出现在设备管理器且实弹注入生效
- [x] Windows：CLI `inject`/`capture` 与 GUI「虚拟声卡」页（注入 + 一键环回自测）
- [x] Windows：虚拟声卡环回积压从 5.46 s（1 MB）调到 1.37 s（256 KB，0.3.10.0）
- [ ] 双平台 UI 宿主统一（`vdev-app` ↔ `vdev-app-win`）

## 内容与授权

- 代码遵循 **MIT**（见各 crate `LICENSE` / `THIRD_PARTY.md`；部分绑定层来自 MIT 第三方项目，保留版权声明）。
- 作者在写 **《用 Rust 写驱动》/「老高写代码」** 驱动开发系列（公众号 / 知乎），以本仓库双栈实现为素材。
  若你打算基于本仓库写文章、教程或课程，欢迎先通过 Issue / Discussions 联系作者，可提供内容审阅与协作。
- 作者另在写《用 Rust 手写 RTOS 内核》书稿（基于 [Xtask](https://github.com/gqf2008/Xtask)），
  以及开源多模态 AI 输入法 [verba-ime](https://github.com/gqf2008/verba-ime)。
