# macOS 驱动开发有趣方向

macOS 驱动开发正处于「传统内核扩展（kext）退场、DriverKit 用户态驱动登场」的转型期。下面按「有趣程度 + 上手难度」整理主流方向。

## 技术路线（决定玩法）

| 路线 | 说明 | 现状 |
|---|---|---|
| **kext（IOKit 内核扩展）** | 传统方式，C++，跑在内核态 | 已废弃，Apple Silicon 上基本装不了，必须关 SIP 才能在 Intel 上玩 |
| **DriverKit（dext，系统扩展）** | 官方新路线，驱动跑在**用户态**（独立进程），用 C++ 的 IOKit 子集 | 现役推荐，但只开放 USB/PCI/串口/HID/音频/以太网/EndpointSecurity 等几个 family |
| **用户态插件冒充驱动** | CoreMediaIO DAL 插件、CoreAudio HAL 插件、HID Event System | 很多「虚拟设备」其实不用写驱动，靠这个就能实现 |

关键点：**macOS 里很多你以为的驱动，其实是用户态插件**。这本身就是个值得研究的「降维打击」思路。

## 推荐方向（按有趣程度）

### 1. 虚拟 HID 设备（键盘/鼠标/手柄）⭐ 最经典
- **Karabiner-Elements / Karabiner-DriverKit-VirtualHIDDevice**（pqrs-org）：用 DriverKit 造虚拟键盘+鼠标，实现按键映射、宏、组合键。这是 DriverKit 虚拟设备最完整的开源范本。
- 可扩展：虚拟游戏手柄、快捷键注入、输入监控/拦截（注意 TCC 权限）、「键盘固件模拟器」。

### 2. 虚拟音频驱动 ⭐ 实用性强
- **BlackHole**（ExistentialAudio）：虚拟声卡，把系统音频「环回」给录音软件。开源、代码清晰，是学 CoreAudio + 驱动打包的绝佳样本。
- 玩法：虚拟声卡做音频路由、DSP 效果器、实时转写管道、音频桥接（如把 Zoom 声音接进 OBS）。
- 技术点：CoreAudio HAL 插件 / AudioServerPlugIn，比内核驱动安全得多。

### 3. 虚拟摄像头 ⭐ 最「黑客」
- **iVirtualCamera**（基于 CoreMediaIO DAL）：不写内核驱动，只做 CoreMediaIO 插件就能让系统「多出一个摄像头」，视频流由你任意喂。
- 玩法：AI 虚拟形象、屏幕/游戏画面伪装摄像头、视频流滤镜、把 RTSP/WebRTC 流变成本地摄像头。

### 4. 真实硬件驱动（USB / PCIe / 串口）⭐ 最硬核
- **USB 自定义设备驱动**：用 DriverKit 写 USB HID / USB 串口 / 复合设备驱动，接自己的开发板、传感器、机械键盘。
- **PCIe 驱动**：**mrmidi/ASFireWire** —— 用 DriverKit 从零实现 FireWire 主机控制器（OHCI），涉及 PCIe 探测、DMA、中断、异步/等时传输，是当前最硬核的开源 DriverKit 项目之一。
- **qemu-vfio-apple**：QEMU 的 fork，在 macOS 上做 PCI passthrough，需要写 dext 编程 DART（Apple 的 IOMMU）——IOMMU 方向非常进阶。
- 技术点：IOUserClient、共享内存、DMA 映射、IOBufferMemoryDescriptor。

### 5. 网络方向 ⭐ 门槛较高但很值
- **Network Extension**（NEPacketTunnelProvider / NEAppProxyProvider）：VPN、透明代理、内容过滤的官方用户态方案。做 SFU/音视频的话，写「虚拟网卡 + 流量镜像」会很有意思。
- 传统 **NKE**（内核网络扩展）已死，别碰。

### 6. 安全 / 逆向方向 ⭐ 最刺激
- **Endpoint Security Framework（ESF）**：拿到进程启动、文件读写、网络连接等系统级事件，做 EDR/沙箱监控。
- **IOKit 攻击面研究**：翻老 kext（Intel 时代）的漏洞、研究 IOUserClient 的越权。
- **Hackintosh 生态**：Lilu、VirtualSMC、WhateverGreen 这些 kext 是「逆向 Apple 内核接口」的教科书，虽然官方路线已死，但研究价值极高。

### 7. 其他冷门但有趣的
- Force Touch / 触控板手势驱动（IOHIDFamily 层拦截）
- macFUSE（用户态文件系统，当年是 kext，现在有 FUSE-T 纯用户态方案）
- 虚拟屏幕/显示器 EDID 伪造
- Hypervisor.framework + 虚拟化框架（严格说不是驱动，但做「驱动测试虚拟机」必备）

## 入门路线

1. **先看官方**：Apple DriverKit 文档 + WWDC 视频（搜 "DriverKit"、"Introducing DriverKit"、"USB/PCI/HID DriverKit" 相关 session）。
2. **从虚拟设备切入**：不用真硬件，先 clone Karabiner-DriverKit-VirtualHIDDevice 或 BlackHole，改着玩。
3. **理解部署链路**：dext 要打包成 System Extension、正确签名（Developer ID + 专用 entitlements）、用户手动批准、`systemextensionsctl` 管理——**这部分比写驱动本身更容易踩坑**。
4. **工具链**：Xcode 模板、`ioreg`（看设备树）、`log show`（看系统日志）、`kmutil`（kext 管理）、LLDB 内核调试（配合 macOS 虚拟机）。
5. **测试环境**：建议在 macOS 虚拟机里折腾（Virtualization.framework / UTM），避免把主力机搞挂——驱动开发最痛的永远是「一崩全机崩」和「签名被拒」。

## 一句话总结

> 入门玩 **虚拟 HID / 虚拟音频 / 虚拟摄像头**（用户态为主，安全且见效快）；进阶玩 **USB/PCIe 真实设备驱动 + IOMMU/DMA**（硬核）；科研向玩 **ESF/安全逆向**。
