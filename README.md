# vdev — 用 Rust 造虚拟设备（macOS + Windows）

> 用 Rust 在 **macOS（Apple Silicon）** 与 **Windows（x64）** 上实现的虚拟设备集合——
> 键盘/鼠标、摄像头、显示器、声卡，每个都让操作系统"相信"有一个真设备存在，
> 可被任意 App 当真实硬件使用。**全 Rust，系统绑定全部手写**。

| 虚拟设备 | macOS（Apple Silicon） | Windows（x64） |
|---|---|---|
| 虚拟键盘 / 鼠标 | CGEventPost 用户态注入（`vdev-hid`）✅ | SendInput 用户态 + **KMDF 内核 HID minidriver**（`vdev-hid-win`）🔧 |
| 虚拟摄像头 | **CMIOExtension，100% Rust**（`vdev-camera-ext`）✅ | **DirectShow 源过滤器**（用户态 COM，免签名）（`vdev-camera-win`）✅ |
| 虚拟显示器 | CGVirtualDisplay 私有 API（`vdev-screen`）✅ | **IddCx UMDF 间接显示驱动**（`vdev-display-win`）🔧 |
| 虚拟声卡 | CoreAudio HAL AudioServerPlugIn，100% Rust（`vdev-audio`）✅ | **PortCls / WaveRT miniport（KMDF 内核）**（`vdev-audio-win`）🔧 |

图例：✅ 已可用（实测）　🔧 代码已合入 main、构建与自测通过；真机安装验证进行中（Windows 驱动需签名，见下）。

> **Windows 侧状态（2026-09-09）**：`vdev-camera-win` 是用户态 COM 组件、无签名门槛，可直接安装试用；
> `vdev-display-win` / `vdev-audio-win` / `vdev-hid-win` 是驱动（UMDF/KMDF），需自签名证书或测试签名后装机验证（进行中）。
> 详见 [Windows 侧](#windows-侧x64)。

## 为什么用 Rust 写"虚拟设备"、为什么不用 kext / DriverKit

先回答 macOS 侧最常见的问题：为什么不用内核驱动？

- **kext 在 Apple Silicon 上已死**（需关 SIP，Intel-only）。
- **DriverKit（dext）只支持 C++**，Rust 只能做 C ABI 内核，工程成本高。
- macOS 虚拟设备的"正统玩法"其实是**用户态插件/事件服务**：
  - 虚拟 HID → CGEventPost / IOHID 事件注入
  - 虚拟摄像头 → CMIOExtension（bundle，无内核组件）
  - 虚拟屏幕 → CoreGraphics 私有 API `CGVirtualDisplay`（DisplayLink 等厂商同款）
  - 虚拟声卡 → CoreAudio HAL `AudioServerPlugIn`

Windows 侧的路线选择同理、但门槛分布不同：虚拟摄像头有用户态 DirectShow 路线（免签名），
虚拟显示器有官方 IddCx UMDF 用户态驱动路线，只有虚拟声卡与内核 HID 没有用户态捷径、必须走内核
（KMDF）——详见 [Windows 侧](#windows-侧x64)。

## 仓库结构

```
crates/                         # 主 workspace（macOS-only；根 Cargo.toml 成员，零仓库外依赖）
  vdev-hid/        虚拟键盘/鼠标：键码注入、文本输入、鼠标移动/点击/滚动（CGEventPost）
  vdev-camera/     虚拟摄像头：Rust 帧生成核心（lib）+ CMIOExtension 全 Rust 扩展（vdev-camera-ext）
  vdev-screen/     虚拟屏幕：CGVirtualDisplay 私有 API 封装
  vdev-audio/      虚拟声卡：CoreAudio HAL AudioServerPlugIn（输出环回输入，自研 BlackHole）
  vdev-host/       宿主进程 / 统一命令行入口（二进制名 vdev）
  vdev-app/        macOS 宿主 App（Rust + Slint）
  vdev-filter/     实时图像滤镜管线（美颜 / 背景替换，Vision）
  vdev-mic-agent/  AI 虚拟麦克风端侧链路：物理麦克风 → 降噪 → 注入 vdev 麦克风（用户态）
crates/*-win/       # Windows 侧：各自独立 workspace（不影响 macOS 主仓库）
  vdev-hid-win/     虚拟键盘/鼠标：SendInput 用户态 + KMDF 内核 HID minidriver（内核虚拟 HID 路线 B）
  vdev-camera-win/  虚拟摄像头：DirectShow 源过滤器（用户态 COM，免签名）
  vdev-display-win/ 虚拟显示器：IddCx UMDF 间接显示驱动 + CLI + driver-ipc
  vdev-audio-win/   虚拟声卡：PortCls/WaveRT miniport（KMDF 内核）+ CLI
  vdev-app-win/     Windows 宿主 App（Rust + Slint）
docs/RESEARCH.md    技术路线调研笔记与参考项目
```

---

# macOS 侧（Apple Silicon）

## 快速开始

```bash
cargo build --release

# 虚拟 HID
vdev hid type "hello from vdev"
vdev hid key space        # 空格（键名只收名字，全表见 vdev hid key --help）
vdev hid move 100 100
vdev hid click 100 100 --button left

# 虚拟屏幕（私有 API，仅供学习）
vdev screen list
vdev screen create --width 1920 --height 1080 --name vdev-demo

# 虚拟摄像头（先出帧核心）
vdev camera frame --out /tmp/frame.ppm

# 监听键盘/鼠标（需要辅助功能权限；无权限时立即报错、以非零退出码退出）
vdev hid listen --seconds 10
```

## 虚拟摄像头：CMIOExtension（✅ 已可用）

**实测结论（macOS 26.5）**：DAL 插件已被系统停载（12.3 弃用），现代路线是 CMIOExtension。

### 已知注意事项（开发/排障）

- **扩展版本铁律**：扩展代码不变就**不要升扩展版本号**。升级宿主 App 不影响扩展；
  升扩展会触发 macOS launchd 替换竞态（`Submit job failed: Operation already in progress`，
  扩展显示已启用但进程不启动、摄像头消失），并通常需要重新批准一次。
- **已内置自动修复**：激活完成但 15s 内摄像头未出现时，App 自动「停用→重新启用→轮询」；
  需要批准会自动打开系统设置。也可 CLI 触发：`/Applications/VDCamera.app/Contents/MacOS/vdev-camera --selftest-recover`。
- **虚拟屏无互斥**：CLI `vdev screen create` 与 App 可各自创建，彼此不加锁；
  同时创建第二块虚拟屏可能失败、也可能得到两块（取决于 macOS 版本与当前系统状态）。
- **崩溃排查**：App UI 回调有 `catch_unwind` 防护，panic 会落盘 `$HOME/vdev-panic.log`
  （沙盒 App 写不了 /tmp，沙盒容器里即 `~/Library/Containers/com.vdev.camera.host/Data/vdev-panic.log`）；
  崩溃报告在 `~/Library/Logs/DiagnosticReports/vdev-camera-*.ips`。
- **文件选择器（视频推流）依赖沙盒文件权限**：宿主 App 是沙盒应用，entitlements 必须带
  `com.apple.security.files.user-selected.read-only`，否则 `NSOpenPanel` 返回 NULL、
  选择器弹不出来（历史上就是这原因）。改过 entitlements 后记得 `make install-rust` 重新签名安装。
- **视频推流彩条/视频交替（已修复）**：扩展端超过 2s 没收到新注入帧就回落到彩条。
  视频放在 ossfs/FUSE 网络挂载上时，AVAssetReader 读盘会随机停顿 1.5~2.8s+（本地文件仅
  18~20ms），停顿超过 2s 摄像头就"彩条↔视频"反复交替。宿主端已加保活：最后发送超过 500ms
  就重发最后一帧（带新时间戳），推流真正结束才允许回落彩条。
  已知残余：网络挂载上 AVAssetReader 初始化（读 moov/索引）可能耗时 10~30s，期间显示彩条属正常。
- **屏幕/虚拟屏推流静止后出彩条（已修复）**：CGDisplayStream 在画面静止后几乎不再回调
  （COMPLETE/IDLE 都停），之前"收到 IDLE 才重发最后一帧"的修复在纯静态画面下失效
  （实测虚拟屏 145s 只发 20 帧）。已改为独立保活线程：无新帧超过 500ms 就重发最后一帧，
  与视频推流同款机制（实测静态虚拟屏 2fps 稳定出帧）。
  另修复：FrameClient 连接断开后 send 失败会重置客户端，下一帧自动重连，避免"连接死了
  永久彩条"。
- **旧版本残留**：多次迭代留下的僵尸扩展 `[terminated waiting to uninstall on reboot]` 无害，
  重启一次自动清理。

### 使用步骤（普通用户）

1. 打开 `/Applications/VDCamera.app`，点「**安装虚拟摄像头**」。
2. 首次会在 系统设置 → 通用 → 登录项与扩展 → 扩展 → 按类别 → 相机扩展 里需要批准
   （App 会自动打开设置页并引导）。
3. 状态变为「✓ 已安装，摄像头可用」后，在任何 App 的摄像头列表里选择 **vdev-camera**：
   - **QuickTime**：文件 → 新建影片录制 → 摄像头选 vdev-camera
   - **Zoom / FaceTime / 腾讯会议**：设置 → 摄像头 → vdev-camera
4. 卸载：打开 VDCamera.app 点「**卸载虚拟摄像头**」。

### 开发构建

宿主 App 是 **Rust + Slint + slint-pixel**（`crates/vdev-app`），扩展是 **100% Rust**
（`crates/vdev-camera-ext`，手写 CMIOExtension objc2 绑定 + FrameChannel + 帧管线，零 Swift）：

```bash
cd crates/vdev-camera
make install-rust     # cargo 编宿主 App + 编 Rust 扩展 + 组装签名 + 装 /Applications（无 xcodebuild）
# 产物：/Applications/VDCamera.app，打开后点「安装虚拟摄像头」
```

### 自测（引擎级，无需点 UI）

```bash
cargo build -p vdev-app --release
./target/release/vdev-app --ui-selftest         # UI 回调接线：建虚拟屏/推流 开始/停止
./target/release/vdev-app --selftest-screen --dur 8    # 屏幕推流（CGDisplayStream → TCP）
./target/release/vdev-app --selftest-video --file x.mp4 --dur 8  # 视频推流（AVAssetReader）
/Applications/VDCamera.app/Contents/MacOS/vdev-camera --selftest-openpanel  # 沙盒内验证 NSOpenPanel 可创建（不弹窗）
/Applications/VDCamera.app/Contents/MacOS/vdev-camera --selftest-sysext  # 安装/卸载委托回调
```

### 真实画面推流通道（✅ 可用）

扩展开启后监听 `127.0.0.1:27890`，外部工具把 BGRA32 帧推进来，摄像头就显示真实画面；
超过 0.5s 没有新帧自动回落到 Rust 彩条。

```bash
# 推送一张图片（循环，方便验证通道）
cd crates/vdev-camera/tools
swift push_frames.swift image /path/to/pic.png --fps 60

# 推送真实屏幕画面（默认 1080p@30；首次需授权 屏幕录制）
swift push_frames.swift screen [--display <id>] --fps 30

# 推送视频文件（AVAssetReader 解码逐帧推）
swift push_frames.swift video /path/to/video.mp4 --fps 60
```

也可以直接用宿主 App：安装完成后点「**屏幕推流**」按钮，无需命令行。

- 默认主格式 **1920×1080@60**；推流端任意尺寸都会被通道接受（工具默认按主格式缩放）
- 屏幕采集默认 30fps（1080p60 会导致 WindowServer 过载卡顿）
- 组合玩法：`vdev screen create` 建虚拟屏幕 → `swift push_frames.swift screen --display <虚拟屏ID>`，
  摄像头即显示虚拟屏幕内容（可再接 SFU/WebRTC 做远程串流）

帧协议：36 字节小端头（magic "VDFR" / version / width / height / stride / ptsNs / payloadLen）
+ `stride*height` 字节 BGRA32（见 `crates/vdev-camera-ext/src/frame_channel.rs`）。

#### 设备侧滤镜（美颜 / 背景替换）

滤镜在**设备侧**做——虚拟摄像头对推来的帧自己成像，所以任何推帧方（宿主 App、
`push_frames.swift`、外部桥）都得到同一套处理，推流方不必各自实现。
参数配在**扩展进程的环境**里（见 `crates/vdev-camera-ext/src/filters.rs`）：

```bash
VDEV_FILTER="brightness,contrast,saturation,green,sharpen,beauty,whiten"
VDEV_BG=blur        # 开启背景模糊（Vision 人像分割）

# brightness -1..1（0=不变） / contrast 0..2（1=不变） / saturation 0..2（1=不变）
# green ≥0（0=关；绿幕抠像阈值） / sharpen 0..2 / beauty,whiten 0..1（0=关）
```

没配任何滤镜时整段跳过，走原样直通路径（零额外开销）。

### 踩坑记录（已沉淀）

- 激活校验链：`.systemextension` 文件名=bundle ID → 宿主+扩展都要
  `NSSystemExtensionUsageDescription` → 宿主+扩展要有同名 `application-groups` →
  `CMIOExtensionMachServiceName` 必须以 App Group 为前缀 → 换二进制必须递增版本号。
- 运行时：`CMIOExtensionProvider` 进程级单例只能建一个；`device.addStream` 必须先于
  `provider.addDevice`（否则零流设备、能枚举但 0 帧）；`legacyDeviceID` 填 UUID 字符串。
- 详见 `docs/RESEARCH.md` 与 `~/.agents/rules/LESSON_CMIOExtension虚拟摄像头激活与出帧的连环坑.md`。

历史遗留已清理：旧 DAL 插件（`dal/`）、旧 Swift 宿主（`host/`）、旧 Swift 扩展壳
（`crates/vdev-camera/extension/`）均已删除/替换；现在宿主与扩展 **100% Rust**，
构建只走 `cargo build` + 手工组装签名（无 xcodebuild）。

## 虚拟声卡 vdev-audio（✅ 已可用）

100% Rust 的 CoreAudio HAL 驱动：**一个虚拟设备 = 输出流 + 输入流，输出环回输入**
（同 BlackHole/Soundflower）。App 把视频音轨推到 vdev-audio 输出，会议/录制软件把
「麦克风」选成 vdev-audio 就能收到，**无 VB-Cable/BlackHole 的周期爆音**。

```bash
cd crates/vdev-audio
make install      # cargo 编 bundle(MH_BUNDLE) + Developer ID 签名 + 装 /Library/Audio/Plug-Ins/HAL + 重启 coreaudiod
make uninstall    # 卸载
make test         # 环回自测：播放 440Hz → 输出流，同时从输入流录制 3s，非静音 > 20% 即 PASS
```

- 使用：任意 App 的音频设备里选择 **vdev-audio**（输出=播放端，输入=麦克风端）。
- App 音频推流已自动优先 vdev-audio（找不到再回退 BlackHole/VB-Cable）；
  App 状态面板会检测并显示虚拟声卡状态（刷新状态 / 启动时自动检测，
  CLI 验证：`/Applications/VDCamera.app/Contents/MacOS/vdev-camera --selftest-audio`）。
- 驱动技术点（macOS 26 踩坑，见 `docs/RESEARCH.md`）：
  - 产物必须是 **MH_BUNDLE**（`-Wl,-bundle`），cargo cdylib 默认 MH_DYLIB 会被 coreaudiod 跳过；
  - 必须 **Developer ID 签名**（adhoc 也被跳过）；
  - `AudioServerPlugInDriverRef` = `&interface_ptr`（工厂返回指针的指针），且 `QueryInterface`
    的 `REFIID` 是 **CFUUIDBytes 按值传 x1:x2**（不是指针）；
  - `GetZeroTimeStamp` 的 host time 必须用 `mach_absolute_time()`（ticks），sample time 用
    timebase 换算——用纳秒会让 coreaudiod 认为时钟异常、IO 只跑几个周期就停；
  - 输出 IO 操作是 `kAudioServerPlugInIOOperationWriteMix`（`'rite'`），不是 `'writ'`；
  - 数组属性（`pfta`/`sfma`/`nsr#`/`ctrl`/`ownd`）在 inDataSize 不足时要**截断返回**而非报错；
  - 必须实现 `bcls`/`clas`/`owne`/`ownd`/`lnam`/`lmod`/`lmak`/`ring`/`cstb`/`clkd` 等属性。

## 权限说明（macOS）

- **注入按键**：`CGEventPost` 无需辅助功能权限（macOS 10.15+ 对合成事件放行）。
- **拦截/监听**（`vdev hid listen`）：需要「辅助功能」权限；无权限时立即报错退出（退出码非 0）。
- **虚拟摄像头**：宿主 App 需要摄像头权限（仅用于检测安装状态）；扩展需在系统设置中批准；
  使用方（QuickTime/Zoom 等）各自需要摄像头权限。
- **虚拟屏幕**：使用私有 API，仅供学习研究，不同 macOS 版本可能行为不同。

## 组合玩法：虚拟屏幕 + SFU 串流（✅ 实测通过）

配合 aerodesk（str0m WebRTC SFU）把虚拟屏幕远程分发，完整链路已验证：
虚拟屏(0x12) → 发布端采集(HEVC 1080p) → SFU 收流 → 观看端解码 327 帧。

```bash
# 1. 起本地 SFU + signal（aerodesk 仓库）
cd /Volumes/Workspace/GitHub/aerodesk
TURN_SECRET=devsecret ./target/release/aerodesk-sfu      # 3002 + 媒体 3478
./target/release/aerodesk-signal                          # WS 3003 / WSS 3001

# 2. 建虚拟屏（保持进程；--hold 控制存活秒数）
cd ~/Documents/GitHub/vdev
target/release/vdev screen create --width 1920 --height 1080 --name vdev-sfu-demo --hold 3600

# 3. 发布端：--display 是索引（0=主屏），虚拟屏是第 2 个 → 1
cd /Volumes/Workspace/GitHub/aerodesk
cargo run -p aerodesk-agent -- --role publisher --encoder screen --display 1 \
  --room vdev-demo --signal ws://127.0.0.1:3003/ws

# 4. 观看端（另一终端）
cargo run -p aerodesk-agent -- --role viewer --room vdev-demo --layer f \
  --signal ws://127.0.0.1:3003/ws
# 日志出现 RECEIVED/DECODED 即成功；浏览器可访问 https://<host>:3000
```

要点：`vdev screen list` 给的是 CGDisplayID（十六进制），aerodesk `--display` 要的是
**显示器索引**（枚举顺序）；虚拟屏通常是第 2 个 → 索引 1。

---

# Windows 侧（x64）

Windows **没有**用户态虚拟显示器/声卡的官方 API，四条路线里三条要碰驱动，只有摄像头有用户态捷径。
按"签名门槛从低到高"排：

| 设备 | 路线 | 驱动类型 | 签名要求 |
|---|---|---|---|
| 虚拟摄像头 | DirectShow 源过滤器（`vdev-camera-win`） | 用户态 COM | **免签名**（regsvr32/自注册） |
| 虚拟显示器 | IddCx 间接显示（`vdev-display-win`） | **UMDF 用户态驱动** | 驱动包签名（自签名证书装 TrustedPublisher+Root 通常即可） |
| 虚拟声卡 | PortCls/WaveRT miniport（`vdev-audio-win`） | **KMDF 内核驱动** | 测试签名或正式签名 |
| 虚拟键盘/鼠标 | KMDF HID minidriver（`vdev-hid-win`） | **KMDF 内核驱动** | 测试签名或正式签名 |

> 状态：`vdev-camera-win` 已可用（用户态，可直接安装）；`vdev-display-win` / `vdev-audio-win` /
> `vdev-hid-win` 代码已合入 main、构建与单测通过，真机安装验证进行中（需先按下方"签名"节准备）。

## vdev-camera-win — DirectShow 虚拟摄像头（✅ 用户态，免签名）

一个 **DirectShow 源过滤器**（用户态 COM 组件，无内核驱动、无签名门槛），把跨进程推送的
BGRA 帧变成系统里的一个"视频捕获源"，任意 App（ffmpeg / OBS / Zoom / Teams / 微信）都能当摄像头选。

- 架构（**安全封装优先**）：所有系统 API 先收敛到带 `SAFETY` 注释的安全封装模块
  （`com/`：COM 初始化/类工厂/注册表 RAII/无锁共享帧通道；`dshow/`：媒体类型/过滤器/Pin），
  业务层零 `unsafe`。用微软官方 `windows` crate 0.62 + `implement` 宏，100% Rust。
- 推流与取流是**两个独立进程**，经命名共享内存（双缓冲 + 序号，无锁）通信；
  无新帧回退棋盘格；尺寸不一致自动最近邻缩放。
- 固定输出 **YUY2**，3 档分辨率（1080p / 720p / 640x480）@30fps。
- CLI：`install / uninstall / list / push / selftest`；支持 64 位 + 32 位双视图注册。
- 自测：进程内 DirectShow 图（源 → NullRenderer）3s 交付约 80 帧；`cargo test`（SHM 往返 + FilterData 布局）。
- 关键踩坑（详见 `docs/windows-virtual-camera.md`）：Instance 键必须带 `FriendlyName`（否则枚举不到但
  CoCreateInstance 能成功）；输出 pin 必须实现 `IKsPropertySet` 返回 `PIN_CATEGORY_CAPTURE`（否则 ffmpeg
  报 Could not find output pin）；推源必须自建并 `Commit` 内存分配器；不要 `SetTime` 样本时间戳（否则
  下游按参考时钟等待、帧率掉到 ~0）；用 **YUY2** 别用 RGB32（VLC 无法提取 fourcc）；`biHeight` 用正数
  （VLC 负数会溢出成黑屏）；样本必须有时间戳（VLC 用其做 PTS）。

```powershell
cd crates\vdev-camera-win
cargo build --release
.\target\release\vdev-camera-win.exe install     # 优先 HKLM，失败回退 HKCU（免管理员）
ffmpeg -f dshow -list_devices true -i dummy      # 应看到 "vdev-camera" (video)
.\target\release\vdev-camera-win.exe push --width 640 --height 360 --fps 30 --seconds 120
ffmpeg -f dshow -i "video=vdev-camera" -c:v libx264 -f mp4 out.mp4
```

## vdev-display-win — IddCx UMDF 虚拟显示器（🔧 装机验证中）

Windows 官方 **Indirect Display Driver（IddCx）**，**UMDF 用户态驱动**：系统识别为第二块显示器，
可扩展/镜像桌面、设置分辨率与刷新率。驱动以 DLL 被 WUDFHost 进程加载，崩溃不影响内核。

- 组成：`driver`（`vdev_display.dll`，IddCx UMDF + 命名管道 IPC）、`cli`（`vdev-display-win.exe`：
  install/uninstall/status + 显示器增删改查）、`wdf-umdf-sys` + `wdf-umdf`（来自 MIT 项目
  virtual-display-rs 的绑定与安全封装）、`driver-ipc`（named pipe + serde_json）、`driver-logger`
  （事件日志）。第三方来源见 `crates/vdev-display-win/THIRD_PARTY.md`。
- 绑定：手写 `wdf-umdf-sys`（UMDF + IddCx），业务层经安全封装调用。
- CLI 示例：

```powershell
vdev-display-win.exe add 1920x1080          # 添加一块虚拟屏
vdev-display-win.exe add 3840x2160@120 1280x720@60/120 --name "vdev-4k"
vdev-display-win.exe list / set-mode 0 2560x1440@144 / remove 0 / remove-all
vdev-display-win.exe install --inf-dir target\dist
```

- 与 macOS 版语义对照：macOS `screen create` ↔ Windows `add`；`screen list` ↔ `list`；
  macOS 由宿主向虚拟屏推内容，Windows 由 OS 直接渲染进虚拟屏（远程推流场景可扩展 swap chain 注入）。

## vdev-audio-win — PortCls/WaveRT 虚拟声卡（🔧 构建绿，实测待测试签名）

**KMDF 内核驱动**（虚拟设备无用户态捷径），PortCls 端口类加载手写 WaveRT miniport + Topology：
虚拟扬声器（render）把系统播放写入环形缓冲，虚拟麦克风（capture）从同一缓冲读出 → **输出环回输入**
（AudioMirror 语义，等价 macOS 侧自研 BlackHole）；宿主可从用户态向环形缓冲注入/采集音频。

- 组成：`driver`（`vdev_audio.sys`：AdapterCommon + WaveRT miniport + Topology + RingBuffer +
  手写 WDM/PortCls/KS 绑定）、`cli`（install/uninstall/status/注入/采集）、INF + 签名脚本。
- 参考：Microsoft sysvad（官方 C++）、AudioMirror（MIT WaveRT 环回）。
- 构建与门禁全绿（fmt/clippy/test）；实测需开测试签名（见下）。

## vdev-hid-win — KMDF 内核 HID 键盘/鼠标（🔧 构建绿，实测待测试签名）

用户态 SendInput 之外的内核路线（路线 B）：**KMDF HID minidriver** 注册到 HIDCLASS，
设备管理器 HID 类出现「vdev 虚拟键盘」（`Root\vdev-hid`）与「vdev 虚拟鼠标」（`Root\vdev-hid-mouse`）。

- 报告描述符含厂商输出管道：用户态 `WriteFile` 即注入——键盘 8 字节报告（1 修饰键 + 6 按键）、
  鼠标 4 字节报告（键位 + X/Y + 滚轮，相对值）；经 manual 队列投递给 hidclass 作为真实 HID 消费。
- 手写 `wdk-sys`（WDF/GPIO/SPB/USB…）内核绑定（vendor 目录，随仓）。
- CLI：`vdev-hid-win kernel install/status/uninstall` + `key/mouse` 注入命令。
- 构建 + 报告格式单测通过（键→HID usage/修饰位/鼠标报告布局）；实测需开测试签名。

## Windows 通用：构建 / 签名 / 安装

各 `*-win` crate 是**独立 workspace**（不与 macOS 主 workspace 混构）。构建前置：
Visual Studio 2022（C++ 桌面负载）+ **WDK 10.0.26100**（`winget install Microsoft.WindowsWDK.10.0.26100`）
+ LLVM（bindgen 用 libclang）+ Rust stable（MSVC target）。

```powershell
# 驱动签名：自签名代码签名证书（一次性）→ 签名 DLL/SYS + 生成 .cat
New-SelfSignedCertificate -Type CodeSigningCert -Subject "CN=vdev Driver" `
  -CertStoreLocation Cert:\CurrentUser\My -KeyExportPolicy Exportable `
  -KeySpec Signature -KeyUsage DigitalSignature `
  -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3")
# 把证书装进 TrustedPublisher + Root（管理员）
certutil -addstore -f TrustedPublisher vdev-cert.cer
certutil -addstore -f Root vdev-cert.cer

# 若加载仍被签名策略拦：开测试签名（管理员，需重启）
bcdedit /set testsigning on
```

- 虚拟显示器是 UMDF，自签名证书通常即可、无需测试签名；虚拟声卡 / 内核 HID 需测试签名或正式签名。
- 虚拟摄像头是用户态 COM，**完全免签名**，`install` 即可。

## Windows 侧当前状态与待办

- [x] `vdev-camera-win`：DirectShow 源过滤器（代码 + 自测 + ffmpeg 生态踩坑已修），可用
- [x] `vdev-display-win`：IddCx UMDF 驱动（含 CLI/UAC 提权），构建/单测通过
- [x] `vdev-audio-win`：PortCls WaveRT 驱动，构建/门禁全绿
- [x] `vdev-hid-win`：KMDF HID minidriver，构建/报告单测通过
- [ ] 装机验证（display/audio/hid）：开测试签名/装证书后真机安装、枚举、端到端实测
- [ ] `vdev-app-win` 宿主 GUI 集成页完善（虚拟显示器页；声卡页已有）

CI（`.github/workflows/ci.yml`，push main + PR 触发）：
- **硬门禁**：macOS 主 workspace（fmt/check/test/clippy -D warnings）；Windows 用户态五个
  独立 workspace（camera-win / hid-win / app-win / audio-win / display-win 用户态包）各自的
  fmt/check/clippy（test 按包有则跑）。
- **顾问 job（continue-on-error，红不阻塞）**：`windows-driver-wdk`——依赖 WDK 的驱动构建
  （display-win 的 driver + wdf-umdf-sys bindgen、hid-win/kernel 的 wdk-build）。托管 runner
  上 winget 装 WDK + LLVM 的组合未实测（wdk-build 可能还需 EWDK/WDKContentRoot），替代路径为
  Windows 真机本地构建；稳定转绿后可去掉 continue-on-error 升级为硬门禁。

---

# 文档导航

| 文档 | 内容 |
|---|---|
| `docs/RESEARCH.md` | 三条技术路线的调研笔记与参考项目 |
| `docs/windows-virtual-camera.md` | Windows 虚拟摄像头（DirectShow）设计与踩坑 |
| `docs/windows-virtual-display-audio.md` | Windows 虚拟显示器 + 虚拟声卡（驱动路线）设计 |
| `docs/Windows驱动开发有意思方向.md` | Windows 侧"值得写"的内容线索 |
| `docs/macOS驱动开发有意思方向.md` | macOS 侧"值得写"的内容线索 |
| `crates/vdev-display-win/README.md` | 虚拟显示器驱动（构建/签名/CLI/验收） |
| `crates/vdev-hid-win/kernel/driver/README.md` | 内核 HID 驱动（构建/注入） |

# 路线图

- [x] 调研三条技术路线（macOS：见 `docs/RESEARCH.md`）
- [x] macOS：vdev-hid / vdev-screen / vdev-camera（CMIOExtension 全链路）/ vdev-audio 可用
- [x] macOS：组合玩法（虚拟屏 + 摄像头串流 + SFU 端到端）+ 设备侧滤镜（美颜/背景替换）
- [x] 远端桥（WebRTC 流 → 虚拟摄像头/声卡）已迁至 aerodesk 仓库（`aerodesk-vdev-bridge`）
- [x] Windows：DirectShow 虚拟摄像头（用户态免签名）可用
- [x] Windows：IddCx UMDF 虚拟显示器 / PortCls WaveRT 虚拟声卡 / KMDF 内核 HID —— 代码与门禁就绪
- [ ] Windows：三驱动真机安装验证（测试签名）+ GUI 集成收尾
- [ ] 双平台 UI 宿主统一（`vdev-app` ↔ `vdev-app-win`）

# 内容与授权

- 本仓库**代码**遵循 MIT 协议（见各 crate `LICENSE` / `THIRD_PARTY.md`；部分绑定层来自 MIT 第三方项目，保留版权声明）。
- 作者在写 **《用 Rust 写驱动》/「老高写代码」驱动开发系列** 内容（公众号 / 知乎），以本仓库双栈实现为素材——
  如果你打算基于本仓库写文章、教程或课程，欢迎先通过 GitHub Issue / Discussions 联系作者，
  可提供内容审阅与协作，避免内容冲突。
- 作者另在写《用 Rust 手写 RTOS 内核》书稿（基于 [Xtask](https://github.com/gqf2008/Xtask)），
  以及开源多模态 AI 输入法 [verba-ime](https://github.com/gqf2008/verba-ime)。
