# vdev — macOS 虚拟设备（Rust）

用 Rust 在 **现代 macOS（Apple Silicon）** 上实现的虚拟设备集合：

| 设备 | 技术路线 | 状态 |
|---|---|---|
| 虚拟 HID（键鼠） | `cgevents` / CGEventPost（用户态事件注入） | ✅ 注入 + 监听可用 |
| 虚拟摄像头 | ~~CoreMediaIO DAL~~ → **CMIOExtension**（Swift 薄壳 + Rust 帧核心） | 🚧 已构建可编译，待 Apple 描述文件激活 |
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

## 虚拟摄像头：CMIOExtension（进行中）

**实测结论（macOS 26.5）**：DAL 插件已被系统停载（12.3 弃用），现代路线是 CMIOExtension。

现状：
- ✅ Rust 核心新增 BGRA32 C ABI（`vdev_camera_render_bgra32`）
- ✅ `crates/vdev-camera/extension/`：CMIOExtension 系统扩展（Swift 薄壳 + Rust 帧源，1280x720@30fps SMPTE）
- ✅ `crates/vdev-camera/host/`：宿主 App（安装/卸载按钮）
- ✅ XcodeGen + xcodebuild 构建通过，Developer ID 手动签名验证通过
- ⏳ **阻塞**：系统扩展激活需要宿主 App 带 `com.apple.developer.system-extension.install`
  受限 entitlement，必须配套含 System Extension capability 的**描述文件**（AMFI 无 profile 直接杀进程）。
  需在 Xcode 登录 Apple 开发者账号后 `make build-autosign`。

```bash
cd crates/vdev-camera
make build-autosign   # 前提：Xcode → Settings → Accounts 已登录开发者账号
```
`crates/vdev-camera/dal/` 的旧 DAL 插件保留作学习样本。

## 权限说明

- **注入按键**：`CGEventPost` 无需辅助功能权限（macOS 10.15+ 对合成事件放行）。
- **拦截/监听**（后续功能）：需要「辅助功能」权限。
- **虚拟摄像头**：宿主 App（QuickTime/Zoom）需要摄像头权限；插件装到 `/Library/CoreMediaIO/Plug-Ins/DAL/`。
- **虚拟屏幕**：使用私有 API，仅供学习研究，不同 macOS 版本可能行为不同。

## 路线图

- [x] 调研三条技术路线（见 docs/RESEARCH.md）
- [ ] vdev-hid：键码/文本/鼠标注入 CLI 可用
- [ ] vdev-screen：创建/列出/销毁虚拟显示器
- [ ] vdev-camera：Rust 帧核心（彩条/渐变）+ 帧服务
- [ ] vdev-camera：DAL 插件薄壳接通 Rust 核心，QuickTime 可见
- [ ] 组合玩法：虚拟屏幕 + 摄像头串流（配合 SFU 经验）
