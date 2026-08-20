# Windows 驱动开发有趣方向

Windows 与 macOS 相反：内核驱动生态**依然健在且非常活跃**（虽然有 HVCI/内存完整性等限制，也有严格的签名门槛）。下面是按「有趣程度 + 上手难度」整理的主流方向。

## 技术路线（决定玩法）

| 路线 | 说明 | 现状 |
|---|---|---|
| **WDM（旧）** | 传统内核驱动模型，自己处理 IRP | 已被 WDF 取代，仅老驱动/特殊场景 |
| **KMDF（内核模式驱动框架）** | WDF 提供对象/电源/PNP 管理，写起来省心 | 现役主流，通用设备驱动首选 |
| **UMDF（用户模式驱动框架）** | 驱动跑在用户态，崩了不蓝屏 | 适合 USB 等低性能要求设备 |
| **Minifilter（文件系统微过滤）** | 文件读写/加密/杀毒/EDR 的黄金模型 | 最经典、最值得学的内核驱动类型 |
| **NDIS / WFP** | 网卡过滤驱动 / Windows 过滤平台（网络包监控、VPN） | 网络方向主流 |
| **AVStream / IddCx** | 摄像头过滤 / 间接显示驱动（虚拟显示器） | 虚拟摄像头、虚拟显示器靠这个 |
| **用户态插件冒充驱动** | Win32 API 钩子、虚拟设备 SDK、服务 | 很多「虚拟设备」其实不用写驱动 |

关键点：Windows 上「写内核驱动」的门槛主要在**签名**（测试签名 / EV 证书 / WHQL / 微软 attestation signing），而不在技术本身。

## 推荐方向（按有趣程度）

### 1. 虚拟手柄 / 虚拟 HID ⭐ 最经典
- **ViGEmBus**（Nefarius）：内核虚拟总线驱动，能凭空造出 Xbox 360 / DS4 手柄，是模拟器、串流、自动化输入的神器。
- 配套 **HidHide**：隐藏真实设备、拦截 HID 报告，做输入重定向。
- 玩法：键盘鼠标转手柄、云游戏输入映射、机器人自动化操作。

### 2. 虚拟音频 / 虚拟摄像头 ⭐ 实用性强
- **Scream**（开源网络音频驱动）：把系统音频编码成 RTP 推到网络，是「网络声卡」的极佳范本。
- **OBS Virtual Camera**：AVStream 内核过滤器，把任意画面变成系统摄像头。
- 玩法：虚拟麦克风/扬声器、音频环回（类似 macOS BlackHole）、会议/直播音视频管道、把 WebRTC 流喂给系统摄像头。

### 3. 虚拟显示器（IddCx）⭐ 最「黑科技」
- **IddCx（Indirect Display Driver）**：微软官方框架，让你写一个「虚拟显示器」——系统会认为插了一台真显示器，画面通过 GPU 编码发到你指定的通道。
- 开源范本：**itsmikethetech/VirtualDisplayDriver**、usbmmIdd。
- 玩法：无头服务器远程桌面、VR/串流虚拟屏、多屏扩展、屏幕采集（配合你熟悉的 SFU/串流场景非常搭）。

### 4. Minifilter 文件系统 / EDR ⭐ 最经典硬核
- **Minifilter** 是 Windows 内核开发的「必修课」：注册回调就能看到每个文件的打开/读写/改名/删除。
- 玩法：透明加密盘、勒索软件防护、文件审计、沙箱、EDR 的核心组件。
- 配套：进程/线程回调（PsSetCreateProcessNotifyRoutine）、注册表过滤（CmRegisterCallback）、对象回调（ObRegisterCallbacks）——组合起来就是一个迷你安全软件。
- 微软官方有大量 minifilter 示例（WDK samples），从 zero 到能跑不难。

### 5. 网络方向（NDIS / WFP）⭐ 门槛较高但很值
- **WFP Callout Driver**：在 TCP/IP 栈里挂钩子，做 VPN、流量分流、内容过滤、防火墙。
- **NDIS Filter Driver**：网卡层驱动，可做虚拟网卡、流量镜像、包注入。
- 玩法：透明代理、抓包工具（替代 WinPcap 的部分场景）、局域网游戏加速。

### 6. 用户态文件系统 ⭐ 安全且实用
- **WinFsp / Dokan**：用户态文件系统框架，不用碰内核就能「造一个盘符」。
- 玩法：挂载云盘、内存盘、加密盘、FUSE 生态移植（很多 Linux 工具能直接搬过来）。

### 7. 安全 / 反作弊 / Hypervisor ⭐ 最刺激
- **Hypervisor 开发**：**SimpleVisor**（Alex Ionescu）是一个极简开源 hypervisor，教你用 VT-x 做 CPU 虚拟化——理解 HVCI/虚拟化安全的基础。
- **内核安全研究**：DKOM（直接内核对象操作）、rootkit 原理、BYOVD（驱动漏洞利用）——攻防两边都极有教育价值，注意只在虚拟机里玩。
- **反作弊内核驱动**：内存完整性校验、句柄保护（ObRegisterCallbacks 经典应用）。
- Windows 11 的 VBS / HVCI / Memory Integrity 会挡掉大量老套路，研究「在新安全模型下怎么做防御」本身就是热点。

### 8. 真实硬件驱动（USB / PCIe / 蓝牙）⭐ 最硬核
- **KMDF/UMDF USB 驱动**：接开发板、传感器、机械键盘、自研外设。
- **PCIe 驱动**：网卡/采集卡/GPU 厂商的日常，需要理解 DMA、BAR、中断（MSI-X）。
- **蓝牙**：蓝牙 HID 过滤、自研 BLE 外设配对。

### 9. 其他冷门但有趣的
- **键盘/鼠标过滤驱动**（KPH / Nefarius 工具链）：按键监听、鼠标轨迹处理
- **屏幕亮度/电源管理 ACPI 驱动**
- **UEFI / 引导级驱动**（Bootkit 研究，纯安全学习向）
- **游戏内 FPS 监控 / 反作弊对抗**（研究向，注意红线）

## 入门路线

1. **搭环境**：Windows 11 虚拟机（Hyper-V / VMware）+ Visual Studio 2022 + **WDK（Windows Driver Kit）** + WinDbg Preview。
2. **开测试签名**：`bcdedit /set testsigning on`，或用 Hyper-V 内核调试（`kdnet` / VirtualKD）。
3. **从 Sample 开始**：WDK 自带海量示例（minifilter、KMDF 虚拟设备、AVStream、IddCx 等），改着玩。
4. **学会调试**：WinDbg 双机调试、`!analyze`、Driver Verifier（驱动验证器，能抓内存池越界/IRQL 错误）——**Windows 驱动最容易死在这上面**。
5. **理解签名链路**：测试签名 → 自签 → EV 证书 → WHQL / attestation signing（Win11 后内核驱动必须有微软签名，这是最大的现实门槛）。
6. **遵守红线**：所有内核实验在 VM 里做；别写/别跑恶意驱动。

## 一句话总结

> 入门玩 **ViGEmBus 虚拟手柄 / 虚拟音频摄像头**；进阶玩 **Minifilter + WFP（安全/网络）**；最前沿玩 **IddCx 虚拟显示器 + Hypervisor**；硬核玩 **KMDF/WDF 真实 USB/PCIe 驱动**。

## macOS vs Windows 对比

| 维度 | macOS | Windows |
|---|---|---|
| 内核驱动现状 | kext 已死，DriverKit 用户态为主 | 内核驱动（WDF）依然主流、生态活跃 |
| 虚拟设备玩法 | DriverKit / CoreMediaIO / CoreAudio | ViGEmBus / AVStream / IddCx / WinFsp |
| 签名门槛 | Developer ID + entitlements + 用户批准 | 测试签名起步，正式发布需 EV/WHQL/微软签名 |
| 调试 | LLDB + 虚拟机 | WinDbg 双机调试 + Driver Verifier（更成熟） |
| 最独特方向 | 用户态插件冒充驱动（DAL/HAL） | Minifilter / WFP / Hypervisor / IddCx |
