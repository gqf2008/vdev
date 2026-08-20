# vdev — macOS 虚拟设备（Rust）

用 Rust 在 **现代 macOS（Apple Silicon）** 上实现的虚拟设备集合：

| 设备 | 技术路线 | 状态 |
|---|---|---|
| 虚拟 HID（键鼠） | `cgevents` / CGEventPost（用户态事件注入） | ✅ 注入 + 监听可用 |
| 虚拟摄像头 | ~~CoreMediaIO DAL~~ → **CMIOExtension**（Swift 薄壳 + Rust 帧核心） | ✅ 可用（QuickTime 可见 Rust 彩条） |
| 虚拟屏幕 | 私有 API `CGVirtualDisplay` + objc2 FFI | ✅ 创建/镜像/销毁可用 |

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
  vdev-camera/  虚拟摄像头：Rust 帧生成核心 + DAL 插件薄壳（dal/）
  vdev-screen/  虚拟屏幕：CGVirtualDisplay 私有 API 封装
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

### 使用步骤（普通用户）

1. 打开 `/Applications/VDCamera.app`，点「**安装虚拟摄像头**」。
2. 首次会在 系统设置 → 通用 → 登录项与扩展 → 扩展 → 按类别 → 相机扩展 里需要批准
   （App 会自动打开设置页并引导）。
3. 状态变为「✓ 已安装，摄像头可用」后，在任何 App 的摄像头列表里选择 **vdev-camera**：
   - **QuickTime**：文件 → 新建影片录制 → 摄像头选 vdev-camera
   - **Zoom / FaceTime / 腾讯会议**：设置 → 摄像头 → vdev-camera
4. 卸载：打开 VDCamera.app 点「**卸载虚拟摄像头**」。

### 开发构建

宿主 App 是 **Rust + Slint + slint-pixel**（`crates/vdev-app`），扩展是 Swift 薄壳（Apple 强制）：

```bash
cd crates/vdev-camera
make install-rust     # cargo 编宿主 App + xcodebuild 编扩展 + 组装签名 + 装 /Applications
# 产物：/Applications/VDCamera.app，打开后点「安装虚拟摄像头」
```

### 自测（引擎级，无需点 UI）

```bash
cargo build -p vdev-app --release
./target/release/vdev-app --ui-selftest         # UI 回调接线：建虚拟屏/推流 开始/停止
./target/release/vdev-app --selftest-screen --dur 8    # 屏幕推流（CGDisplayStream → TCP）
./target/release/vdev-app --selftest-video --file x.mp4 --dur 8  # 视频推流（AVAssetReader）
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
+ `stride*height` 字节 BGRA32（见 `extension/FrameChannel.swift`）。

### 踩坑记录（已沉淀）

- 激活校验链：`.systemextension` 文件名=bundle ID → 宿主+扩展都要
  `NSSystemExtensionUsageDescription` → 宿主+扩展要有同名 `application-groups` →
  `CMIOExtensionMachServiceName` 必须以 App Group 为前缀 → 换二进制必须递增版本号。
- 运行时：`CMIOExtensionProvider` 进程级单例只能建一个；`device.addStream` 必须先于
  `provider.addDevice`（否则零流设备、能枚举但 0 帧）；`legacyDeviceID` 填 UUID 字符串。
- 详见 `docs/RESEARCH.md` 与 `~/.agents/rules/LESSON_CMIOExtension虚拟摄像头激活与出帧的连环坑.md`。

`crates/vdev-camera/dal/` 的旧 DAL 插件保留作学习样本。

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
