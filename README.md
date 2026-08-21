# vdev — macOS 虚拟设备（Rust）

用 Rust 在 **现代 macOS（Apple Silicon）** 上实现的虚拟设备集合：

| 设备 | 技术路线 | 状态 |
|---|---|---|
| 虚拟 HID（键鼠） | `cgevents` / CGEventPost（用户态事件注入） | ✅ 注入 + 监听可用 |
| 虚拟摄像头 | ~~CoreMediaIO DAL~~ → **CMIOExtension 100% Rust**（手写 objc2 绑定，零 Swift） | ✅ 可用（QuickTime 可见 Rust 彩条 / 真实推流） |
| 虚拟屏幕 | 私有 API `CGVirtualDisplay` + objc2 FFI | ✅ 创建/镜像/销毁可用 |
| 虚拟声卡 | CoreAudio HAL `AudioServerPlugIn`，**100% Rust**（输出环回输入，自研 BlackHole） | ✅ 可用（音频推流走 vdev-audio，无爆音） |

## 为什么不用 kext / DriverKit

- kext 在 Apple Silicon 上已死（需关 SIP，Intel-only）。
- DriverKit（dext）只支持 C++，Rust 只能做 C ABI 内核，工程成本高。
- macOS 虚拟设备的「正统玩法」其实是**用户态插件/事件服务**：
  - 虚拟 HID → CGEventPost / IOHID 事件注入
  - 虚拟摄像头 → CoreMediaIO DAL 插件（bundle，无内核组件）
  - 虚拟屏幕 → CoreGraphics 私有 API `CGVirtualDisplay`（DisplayLink 等厂商同款）

## 工作区结构

```
crates/
  vdev-hid/     虚拟键盘/鼠标：键码注入、文本输入、鼠标移动/点击/滚动
  vdev-camera/  虚拟摄像头：Rust 帧生成核心（lib）+ CMIOExtension 全 Rust 扩展（vdev-camera-ext）
  vdev-screen/  虚拟屏幕：CGVirtualDisplay 私有 API 封装
  vdev-audio/   虚拟声卡：CoreAudio HAL AudioServerPlugIn（输出环回输入，自研 BlackHole）
  vdev-host/    宿主进程 / 统一命令行入口（二进制名 vdev）
docs/RESEARCH.md  三条技术路线的调研笔记与参考项目
```

## 快速开始

```bash
cargo build --release

# 虚拟 HID
vdev hid type "hello from vdev"
vdev hid key 49          # 空格
vdev hid mouse move 100 100
vdev hid mouse click left

# 虚拟屏幕（私有 API，仅供学习）
vdev screen list
vdev screen create --width 1920 --height 1080 --name vdev-demo

# 虚拟摄像头（先出帧核心）
vdev camera frame --out /tmp/frame.ppm

# 监听键盘/鼠标（需要辅助功能权限）
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
- **同屏只能有一个虚拟屏**：CLI `vdev screen create` 与 App 互斥，占用时创建会失败（日志有提示）。
- **崩溃排查**：App 回调全部 `catch_unwind`，panic 会落盘 `$HOME/vdev-panic.log`
  （沙盒 App 写不了 /tmp，沙盒容器里即 `~/Library/Containers/com.vdev.camera.host/Data/vdev-panic.log`）；
  崩溃报告在 `~/Library/Logs/DiagnosticReports/vdev-camera-*.ips`。
- **文件选择器（视频推流）依赖沙盒文件权限**：宿主 App 是沙盒应用，entitlements 必须带
  `com.apple.security.files.user-selected.read-only`，否则 `NSOpenPanel` 返回 NULL、
  选择器弹不出来（历史上就是这原因）。改过 entitlements 后记得 `make install-rust` 重新签名安装。
- **视频推流彩条/视频交替（已修复）**：扩展端超过 2s 没收到新注入帧就回落到彩条。
  视频放在 ossfs/FUSE 网络挂载上时，AVAssetReader 读盘会随机停顿 1.5~2.8s+（本地文件仅
  18~20ms），停顿超过 2s 摄像头就“彩条↔视频”反复交替。宿主端已加保活：最后发送超过 500ms
  就重发最后一帧（带新时间戳），推流真正结束才允许回落彩条。
  已知残余：网络挂载上 AVAssetReader 初始化（读 moov/索引）可能耗时 10~30s，期间显示彩条属正常。
- **屏幕/虚拟屏推流静止后出彩条（已修复）**：CGDisplayStream 在画面静止后几乎不再回调
  （COMPLETE/IDLE 都停），之前“收到 IDLE 才重发最后一帧”的修复在纯静态画面下失效
  （实测虚拟屏 145s 只发 20 帧）。已改为独立保活线程：无新帧超过 500ms 就重发最后一帧，
  与视频推流同款机制（实测静态虚拟屏 2fps 稳定出帧）。
  另修复：FrameClient 连接断开后 send 失败会重置客户端，下一帧自动重连，避免“连接死了
  永久彩条”。
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

## 权限说明

- **注入按键**：`CGEventPost` 无需辅助功能权限（macOS 10.15+ 对合成事件放行）。
- **拦截/监听**（后续功能）：需要「辅助功能」权限。
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

## 路线图

- [x] 调研三条技术路线（见 docs/RESEARCH.md）
- [x] vdev-hid：键码/文本/鼠标注入 CLI 可用
- [x] vdev-screen：创建/列出/销毁虚拟显示器
- [x] vdev-camera：Rust 帧核心 + CMIOExtension 全链路，QuickTime 可见
- [x] 组合玩法：虚拟屏幕 + 摄像头串流（配合 aerodesk SFU，端到端实测）
