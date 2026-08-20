# 调研笔记：macOS 虚拟设备技术路线

结论：现代 macOS（Apple Silicon）上三个虚拟设备都不需要内核驱动。

## 1. 虚拟 HID

- **方案**：CGEventPost（Quartz Event Services）合成键盘/鼠标/滚轮事件。
- **Rust crate**：[cgevents](https://github.com/doom-fish/cgevents-rs)（0.4，纯 Rust 绑定，零 Swift）。
- 注入不需要辅助功能权限；拦截（CGEventTap）需要。
- 替代路线：DriverKit `IOUserHIDDevice`（C++ only，Rust 需 C ABI 桥，后续再说）。

## 2. 虚拟摄像头（CoreMediaIO DAL 插件）

- **形态**：`.plugin` bundle，装到 `/Library/CoreMediaIO/Plug-Ins/DAL/`（或用户目录）。
- **入口**：`PlugInMain(CFAllocatorRef, CFUUIDRef)` 返回 `CMIOHardwarePlugInInterface`（C vtable，约 80 个方法）。
- **参考实现**：
  - johnboiles/coremediaio-dal-minimal-example（最简，MIT）
  - johnboiles/obs-mac-virtualcam（OBS 虚拟摄像头，生产级）
- **Rust 策略**：vtable 主体留在 ObjC++ 薄壳（改自 dal-minimal，重命名类 + 换 UUID），帧生成/逻辑用 Rust 静态库（C ABI）提供。

## 3. 虚拟屏幕（CGVirtualDisplay 私有 API）

- **来源**：CoreGraphics.framework 私有头，DisplayLink 等厂商在用，跨版本较稳定。
- **调用方式**（ObjC 消息，Rust 用 objc2 直接发）：
  - `CGVirtualDisplayMode`：`initWithWidth:height:refreshRate:`
  - `CGVirtualDisplayDescriptor`：vendorID/productID/serialNumber/name/sizeInMillimeters/maxPixelsWide/maxPixelsHigh/红绿蓝白 primary/queue
  - `CGVirtualDisplaySettings`：modes/hiDPI/rotation
  - `CGVirtualDisplay`：`initWithDescriptor:` + `applySettings:`，暴露 `displayID`
- **镜像到物理屏**：公开 API `CGBeginDisplayConfiguration` + `CGConfigureDisplayMirrorOfDisplay` + `CGCompleteDisplayConfiguration`。
- **参考实现**：sammcj/force-hidpi（Swift，含完整私有头）、enfp-dev-studio/node-mac-virtual-display（Node native）。
- **注意**：私有 API 仅供学习；屏幕捕获走 ScreenCaptureKit，勿滥用。
