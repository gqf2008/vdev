# 调研笔记：macOS 虚拟设备技术路线

结论：现代 macOS（Apple Silicon）上三个虚拟设备都不需要内核驱动。

## 1. 虚拟 HID

- **方案**：CGEventPost（Quartz Event Services）合成键盘/鼠标/滚轮事件。
- **Rust crate**：[cgevents](https://github.com/doom-fish/cgevents-rs)（0.4，纯 Rust 绑定，零 Swift）。
- 注入不需要辅助功能权限；拦截（CGEventTap）需要。
- 替代路线：DriverKit `IOUserHIDDevice`（C++ only，Rust 需 C ABI 桥，后续再说）。

## 2. 虚拟摄像头（⚠️ 旧路：CoreMediaIO DAL 已死，现代走 CMIOExtension）

### DAL 插件（已实测失败，2026-08-20 / macOS 26.5）

- **形态**：`.plugin` bundle，装到 `/Library/CoreMediaIO/Plug-Ins/DAL/`。
- **入口**：`PlugInMain` 返回 `CMIOHardwarePlugInInterface`（C vtable，约 80 个方法）。
- **实测结论**：插件可构建、可签名（Developer ID + Team ID）、可手动 dlopen + PlugInMain 返回有效接口，但
  `cameracaptured` 在 macOS 26 上**完全不加载**第三方 DAL 插件（AVFoundation / CMIO 枚举均不可见）。
- **根因**：macOS 12.3 弃用 DAL，13+ 由 **CMIOExtension（Camera Extension）** 取代；OBS 虚拟摄像头在 macOS 13+
  已切换（obsproject/obs-studio#7777）。签名、权限、UUID 均不是问题。
- 结论：**不要再投入 DAL**。旧 DAL 插件代码（`crates/vdev-camera/dal/`）已于 2026-08-21 清理删除，调研结论见上。

### CMIOExtension（现代路线）

- **形态**：System Extension（`CMIOExtensionProvider`），随宿主 App 安装，用户手动批准。
- **语言**：Swift / ObjC（`CMIOExtension.framework`，macOS 13+）。
- **Rust 策略**：extension provider 薄壳调 Rust 静态库（C ABI），与 DAL 相同的「薄壳 + Rust 核心」结构。
- **参考**：Apple 官方示例（WWDC22 "Create a camera extension with Core Media IO"）、obs-mac-virtualcam 的
  `mac-virtualcam` extension 实现。

#### 已跑通的最小实现（vdev-camera，macOS 26，QuickTime 可见 Rust 彩条）

- 宿主 App（含 `com.apple.developer.system-extension.install`）+ 扩展 `.systemextension`（文件名 = bundle ID）。
- 激活三要素：
  1. 宿主与扩展 Info.plist 都有 `NSSystemExtensionUsageDescription`；
  2. 宿主与扩展 entitlements 都有**同名** `com.apple.security.application-groups`；
  3. `CMIOExtensionMachServiceName` = `$(TeamIdentifierPrefix)$(PRODUCT_BUNDLE_IDENTIFIER)`，且必须以
     App Group 为前缀（例：group=`XFXU84HVK3.com.vdev.camera`，service=`XFXU84HVK3.com.vdev.camera.host.extension`）。
- 扩展 Info.plist **不要** `NSExtension` 块（对齐 Apple 模板/SimCam），只要 `CMIOExtension` + usage description。
- 运行时五件事：
  1. `CMIOExtensionProvider` 进程级单例，全进程只建一个（ProviderSource 里建，别处复用）；
  2. `device.addStream(stream)` **先于** `provider.addDevice(device)`（否则零流设备，能枚举但 0 帧）；
  3. `legacyDeviceID` 填 UUID 字符串；
  4. `startStream()` 里启动帧定时器，`stream.send(...)` 发 `CMSampleBuffer`（BGRA32 + IOSurface）；
  5. 换二进制必须递增 `CFBundleVersion` 强制 sysextd 替换，否则一直跑旧 staging。
- 批准入口（macOS 26）：系统设置 → 通用 → 登录项与扩展 → 摄像头扩展。
- 排障：`systemextensionsctl list` + `/usr/bin/log show --predicate 'process == "sysextd"' --info --debug`；
  出帧用 QuickTime 新建影片录制验证最可信（沙盒扩展里 NSLog 可能不进 unified log）。

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
