# Windows 虚拟显示器 + 虚拟声卡（内核驱动路线，Rust）

> 状态：调研完成，待实现。用户已拍板：接受开测试签名 + 装 WDK，走内核驱动路线；
> 实现语言必须 Rust（C/C++ 能实现的 Rust 一样可以实现）。

## 1. 目标与验收标准

在 Windows（x64）上提供与 macOS 版 vdev 语义对齐的**真虚拟设备**：

| 设备 | 语义（macOS 对照） | Windows 验收标准 |
|---|---|---|
| 虚拟显示器 | CGSVirtualDisplay 创建/镜像/销毁 | 设备管理器出现虚拟显示适配器；系统设置/`EnumDisplayDevices` 看到第二块显示器；可设分辨率/刷新率；桌面可扩展到虚拟屏；OBS/录屏可见 |
| 虚拟声卡 | CoreAudio 输出环回输入（自研 BlackHole） | 控制面板出现「vdev 扬声器」+「vdev 麦克风」两个端点；播放到扬声器的声音被麦克风录到；宿主可注入音频到麦克风（推流） |

约束：
- 全部业务代码 Rust；`unsafe` 隔离在 sys 绑定与最小封装层，业务层零 unsafe（沿用仓库规范）。
- 遵循仓库开发流程：worktree + issue + PR + 独立审查 + 合并 + 清理。
- 构建不依赖 MSBuild/WDK VSIX（cargo 直接驱动），仅依赖 WDK 头文件与库文件。

## 2. 技术路线总览

Windows **没有**用户态虚拟显示器/声卡官方 API，两条路线都必须走驱动：

| 设备 | 驱动类型 | 内核？ | 签名要求 | Rust 可行性 |
|---|---|---|---|---|
| 虚拟显示器 | **IddCx（Indirect Display Driver Class eXtension）** | **UMDF 用户态驱动** | 驱动包签名；自签名证书装进 TrustedPublisher+Root 即可（**可能无需开测试签名**） | ✅ 有完整 Rust 参考（virtual-display-rs） |
| 虚拟声卡 | **PortCls 音频 miniport（WaveRT + Topology）** | **KMDF 内核驱动** | **必须测试签名**（`bcdedit /set testsigning on` + 重启）或正式签名 | ⚠️ 无现成 Rust 参考，按 AudioMirror(C++)/sysvad 结构移植 |

关键认知：
- **虚拟显示器 = IddCx UMDF**：MS 官方 IndirectDisplay 示例就是 UMDF（用户态）。驱动以 DLL 形式被
  WUDFHost 进程加载，崩溃不影响内核。参考项目 virtual-display-rs 证明 **Rust 写 IddCx UMDF 驱动完全可行**，
  且其发布版仅靠自签名证书安装（未强制要求测试签名）。
- **虚拟声卡没有用户态捷径**：WASAPI 环回只是采集，不是虚拟设备；VB-Cable/Voicemeeter/AudioMirror 全是
  内核驱动。因此虚拟声卡是本批次里唯一**必须开测试签名 + 重启**的组件。

## 3. 虚拟显示器：IddCx UMDF 驱动（优先做）

### 3.1 原理

- 驱动注册为 Display 类、Root 枚举设备（硬件 ID 如 `Root\vdev-display`）。
- 通过 IddCx API 创建 adapter → 创建 monitor → 上报 EDID/模式列表 → 分配 swap chain。
- **OS 把虚拟屏当作真显示器渲染**：桌面可扩展/复制到虚拟屏，OS 通过 swap chain 把渲染好的帧交给驱动
  （驱动默认什么都不做也能用——这就是"多一块屏"）。
- 驱动也可以**往 swap chain 表面写帧**来显示外部内容（远程/推流场景，vdev 后续扩展点）。
- 硬件光标：IddCxMonitorSetupHardwareCursor（或软件光标）。

### 3.2 参考项目（均已核实，MIT 可移植）

| 项目 | 说明 |
|---|---|
| [MolotovCherry/virtual-display-rs](https://github.com/MolotovCherry/virtual-display-rs) | **Rust 写的完整 IddCx UMDF 驱动**：多显示器(≤16)、多分辨率/刷新率、控制 App、MSI/WiX 安装器、自签名证书。结构：`wdf-umdf-sys`(bindgen 绑定) + `wdf-umdf`(安全封装) + `virtual-display-driver`(驱动 DLL) + `driver-ipc`(IOCTL IPC) + CLI |
| Microsoft [IndirectDisplay](https://github.com/microsoft/Windows-driver-samples/tree/main/video/IndirectDisplay) | 官方 C++ 示例（UMDF），理解 IddCx 语义的权威来源 |
| RustDeskIddDriver（rustdesk-org） | 现 main 已是 C++（MS 示例搬运）；历史 Rust 实现仅存在于早期 fork，故不作为主参考 |

### 3.3 移植/改造方案（vdev 命名 + 仓库规范）

```
crates/vdev-display-win/
  wdf-umdf-sys/     # bindgen：UMDF + IddCx 绑定（MIT，保留版权声明）
  wdf-umdf/         # 安全封装：WDF 对象/回调 + IddCx 调用（MIT，保留版权声明）
  driver/           # 驱动 DLL（cdylib）：DriverEntry、callbacks、context、swap-chain、IOCTL IPC
    vdev-display.inf
  cli/              # vdev-display-win.exe：install/uninstall/list/add/remove/setmode/status
  Cargo.toml        # 独立 workspace（依赖 windows crate + bindgen，与主 workspace 隔离）
```

- 驱动 DLL 名 `vdev_display.dll`，硬件 ID `Root\vdev-display`，设备名「vdev 虚拟显示器」。
- IPC 通道（driver-ipc）：沿用 virtual-display-rs 的 IOCTL 设计（Named Pipe 或 DeviceIoControl），
  宿主 CLI/GUI 用它增删虚拟屏、设分辨率。
- 宿主集成：`vdev-app-win` GUI 增加「虚拟显示器」页（安装/卸载/增删屏/分辨率/状态），
  与摄像头页同风格；CLI 同时提供无 GUI 操作。
- 默认行为：OS 渲染到虚拟屏（开箱即用，用户可扩展桌面/镜像）。
- 后续扩展：宿主向 swap chain 注入帧（远程内容显示）。

### 3.4 构建（不依赖 MSBuild）

- 工具链：Rust（用 nightly 的 `-Zpre-link-arg`，或 stable 用 `-C link-arg` 传 `/SUBSYSTEM:WINDOWS`
  `/MANIFEST:NO` `/DYNAMICBASE` `/NXCOMPAT`）+ MSVC x64 linker + bindgen/clang。
- 依赖 WDK：`Include\wdf\umdf\2.31\*`、`Include\um\iddcx\1.4\IddCx.h`、
  `Lib\wdf\umdf\x64\2.31\WdfDriverStubUm.lib`、`Lib\um\x64\iddcx\1.4\IddCxStub.lib`。
- 链接：`-C target-feature=+crt-static` + `static=ucrt` + WdfDriverStubUm + IddCxStub。

### 3.5 安装与签名

- 生成自签名代码签名证书（`New-SelfSignedCertificate -Type CodeSigningCert`，
  或参考 virtual-display-rs 的 makecert 流程），签名驱动 DLL + 生成 `.cat`（可先用 inf2cat），
  把证书装进 **TrustedPublisher + Root**。
- 创建设备节点：`nefcon`（nefarius，virtual-display-rs 同款）：
  `nefconc.exe --create-device-node --class-name Display --class-guid {4d36e968-...} --hardware-id Root\vdev-display`
  再 `--install-driver --inf-path vdev-display.inf`。
- 若 UMDF 加载仍被签名策略拦：再开测试签名（见 §5）。

### 3.6 风险

- **与 ToDesk 虚拟显示驱动共存**（已有 LESSON）：ToDesk 的 Virtual Display Adapter 接管主显示后会致盲
  用户态采集（GDI 纯蓝/DXGI 超时）。本驱动默认只作为**附加显示器**，不接管主显示；
  但仍需实测共存场景，验证我们的虚拟屏不干扰 DXGI/GDI 采集。
- UMDF 崩溃只影响 WUDFHost，不蓝屏；但仍要做好 panic hook + 事件日志。

## 4. 虚拟声卡：PortCls 内核驱动（后做）

### 4.1 原理

- KMDF 驱动，PortCls（端口类）加载我们的 **WaveRT miniport + Topology miniport**。
- 虚拟**扬声器**（WaveRT render）把系统播放的音频写入**环形缓冲**；虚拟**麦克风**（WaveRT capture）
  从同一环形缓冲读出 → 输出环回输入（AudioMirror 语义，等价 macOS 自研 BlackHole）。
- 宿主可从用户态向环形缓冲注入音频（推流），或从环形缓冲抓取（采集）。

### 4.2 参考项目

| 项目 | 说明 |
|---|---|
| [JannesP/AudioMirror](https://github.com/JannesP/AudioMirror) | MIT；基于 sysvad 重构的 WaveRT 环回驱动（speaker→mic）。结构：MiniportWaveRT/Stream、MiniportTopology、RingBuffer、Subdevice 等 |
| Microsoft [sysvad](https://github.com/microsoft/Windows-driver-samples/tree/master/audio/sysvad) | 官方虚拟音频示例（C++），权威参考 |

### 4.3 移植方案

```
crates/vdev-audio-win/
  wdk-audio-sys/    # bindgen：wdm/portcls/ks 绑定（内核）
  driver/           # vdev_audio.sys：DriverEntry + MiniportWaveRT + MiniportTopology + RingBuffer
    vdev-audio.inf
  cli/              # vdev-audio-win.exe：install/uninstall/status/注入/采集
```

- 设备名「vdev 虚拟声卡」，端点「vdev 扬声器」/「vdev 麦克风」。
- 业务层零 unsafe；COM 接口（IMiniport*）用 vtables 封装，遵循仓库 unsafe 规范。
- 需要**内核测试签名**：`bcdedit /set testsigning on` + 重启（用户已接受）。
- 开发调试建议：先在 VM 或本机低风险环境用 WinDbg/`verifier` 验证，避免内核崩溃。

### 4.4 风险

- PortCls miniport 的 COM 接口数量多（IMiniportWaveRT、IMiniportStream、IMiniportTopology、
  IPortWaveRT 等），移植工作量大；是整批里最难的部分。
- 内核驱动崩溃 = 蓝屏；必须严格按 sysvad/AudioMirror 的引用计数与 IRP 处理来写。

## 5. 测试签名与 WDK 环境准备

### 5.1 WDK 安装（已确认未装；仅 SDK 10.0.26100 在）

```powershell
# 管理员
winget install --id Microsoft.WindowsWDK.10.0.26100 --exact `
  --accept-source-agreements --accept-package-agreements --silent
```

需要 WDK 提供：`Include\wdf\umdf\2.31`、`Include\um\iddcx\1.4`、`Lib\wdf\umdf\x64\2.31`、
`Lib\um\x64\iddcx\1.4`（UMDF IddCx 构建）；音频驱动需要 `Include\km` 与 PortCls 头（WDK 自带）。

### 5.2 测试签名（仅虚拟声卡内核驱动需要；虚拟显示器先试免测试签名）

```powershell
# 管理员，需重启
bcdedit /set testsigning on
```

### 5.3 签名工具链

- 生成自签名代码签名证书 + 签名 DLL/SYS：`New-SelfSignedCertificate` + `signtool sign /fd sha256`。
- 目录文件：`inf2cat /driver:<dir> /os:10_x64` 或 MSBuild stampinf/inf2cat。
- 证书安装：`certutil -addstore -f root cert.cer` + `certutil -addstore -f TrustedPublisher cert.cer`。

## 6. 里程碑

1. [ ] 环境：装 WDK（winget，提权）；生成签名证书；装 nefcon
2. [ ] 虚拟显示器驱动移植（wdf-umdf-sys + wdf-umdf + driver + CLI），构建出 DLL
3. [ ] 虚拟显示器安装/枚举/多屏/分辨率实测（含与 ToDesk 共存）
4. [ ] 虚拟显示器 GUI 集成（vdev-app-win 新增页）
5. [ ] 开测试签名（需重启）
6. [ ] 虚拟声卡驱动移植（bindgen + miniport + ringbuffer + CLI），构建出 SYS
7. [ ] 虚拟声卡安装/环回/注入实测
8. [ ] 虚拟声卡 GUI 集成
9. [ ] 审查、合并、清理、沉淀 LESSON
