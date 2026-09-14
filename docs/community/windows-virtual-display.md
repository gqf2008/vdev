> 本文是 [vdev](https://github.com/gqf2008/vdev) 虚拟设备驱动开发系列之一。全套含 macOS 摄像头/声卡/键鼠/虚拟屏与 Windows 摄像头/显示器/声卡/HID 九篇。

# 用 Rust 写一个 Windows 虚拟显示器驱动：IddCx UMDF 从绑定层到 IPC 的完整实录

> 对应仓库：[`crates/vdev-display-win`](https://github.com/gqf2008/vdev/tree/main/crates/vdev-display-win)。文中所有代码片段与 file:line 均来自 main 分支当前状态（commit `1332023`）。
> 代码引用约定：本文所有 `文件:行号` 均相对**仓库根**（如 `crates/.../foo.rs:12`），行号为写作时基线；代码演进后行号会漂移，按符号名搜索为准。

## 一、引言：为什么是虚拟显示器，为什么是 IddCx

虚拟显示器是一块"操作系统认为存在、但没有物理面板"的屏幕。典型用途：

- **无头渲染**：服务器上没有插显示器，但渲染管线需要一个桌面；
- **投屏 / 远控**：把一块独立分辨率的屏扩展出去，再推流给另一台机器，分辨率不受物理屏限制；
- **AI Agent 与自动化**：给无头进程一个"真屏幕"，让依赖窗口系统的软件照常工作。

Windows 上做虚拟显示器，历史上有两条路线：

1. **旧式镜像驱动（Mirror Driver）**：XP 时代的内核显示过滤驱动，微软已明确标记为废弃，在新版 Windows 上基本不可用；
2. **IddCx 间接显示驱动（Indirect Display Driver）**：Windows 10 1607 起微软提供的官方路线，基于 UMDF（用户态驱动框架）+ IddCx（Indirect Display 扩展）。USB 显示芯片厂商、各类虚拟屏软件现在都走这条路。

选型几乎没有悬念。IddCx 的优势正好踩在我们的需求上：

- **官方支持**：OS 原生把桌面渲染进虚拟屏，`EnumDisplayDevices`、系统设置里它就是一块正常的显示器；
- **用户态隔离**：驱动是运行在 `WUDFHost.exe` 宿主进程里的 DLL，崩溃不影响内核，调试也像普通用户态程序；
- **签名门槛低**：UMDF 驱动包用自签名代码签名证书（装进 TrustedPublisher + Root）通常即可安装，不必开测试签名，更不必 EV 证书（四类设备的签名门槛见仓库 [README](https://github.com/gqf2008/vdev#readme) 的构建与安装章节：摄像头免签名、显示器自签名即可、声卡与内核 HID 需测试签名）。

代价是：**这是一个真正的驱动项目**。哪怕代码在用户态，它面对的是 C ABI、框架管理的对象生命周期、和一份只在装了 WDK 的 Windows 主机上才存在的头文件体系。本文剩下的篇幅，就是把这条链路完整走一遍。

## 二、IddCx/UMDF 最小知识

写代码前只需要理解三件事。

**宿主进程模型。** UMDF 驱动编译成一个 DLL（本项目的产物是 `vdev_display.dll`），由 `WUDFHost` 进程加载。入口不是你写的 `DriverEntry`，而是框架的 `FxDriverEntryUm`——`crates/vdev-display-win/driver/src/lib.rs:22–34` 用 `#[link(name = "WdfDriverStubUm", modifiers = "+whole-archive")]` 静态链接了 UMDF 的 stub 库，由它转发到我们导出的 `DriverEntry`。所有 WDF/IddCx API 调用最终穿过 stub 查一张函数表进入框架。

**三个抽象。** IddCx 把"虚拟显卡"拆成三层：

- `IddCxAdapter`——虚拟显卡本身，在 D0Entry（设备上电）里用 `IddCxAdapterInitAsync` 初始化；
- `IddCxMonitor`——插在显卡上的一块屏，驱动主动 `IddCxMonitorCreate` + `IddCxMonitorArrival` 告诉 OS"我插了块屏"（这就是 **arrival**）；
- `IddCxSwapChain`——OS 决定真的往这块屏渲染后，通过 `EvtIddCxMonitorAssignSwapChain` 回调把一条 swap chain **指派（assign）** 给驱动，驱动从这里拿到每一帧。

模式协商走三个回调：`EvtIddCxParseMonitorDescription`（解析 EDID 报告显示器支持的模式）、`EvtIddCxMonitorQueryTargetModes`（报告传输能力）、`EvtIddCxAdapterCommitModes`（OS 拍板）。本项目里 EDID 是驱动自己生成的，并把 EDID 的序列号字段当作显示器 ID 使用——这样 `parse_monitor_description` 回调里反解序列号就能对回 IPC 层登记的显示器（`crates/vdev-display-win/driver/src/edid.rs:62–100`，校验和按 EDID 规范重算）。

**回调都是 C ABI。** 框架从 C++ 侧调进 Rust，所有导出回调都是 `extern "C-unwind"`，这一点贯穿全文。

## 三、用 bindgen 直取 WDK 头做绑定层

`wdf-umdf-sys` 不是手写的，是**构建时对本机 WDK 头文件跑 bindgen 生成的**。`crates/vdev-display-win/wdf-umdf-sys/build.rs:20–21` 钉死版本：

```rust
// wdf-umdf-sys/build.rs
const UMDF_V: &str = "2.31";
const IDDCX_V: &str = "1.4";
```

build.rs 做四件事：读注册表 `KitsRoot10` 找到 WDK；从 SDK 的多个版本目录里挑最高的一个；把 `IddCx.h` 和 UMDF 头喂给 bindgen；链接 `WdfDriverStubUm` 与 `IddCxStub` 两个静态 stub 库（`crates/vdev-display-win/wdf-umdf-sys/build.rs:204,217`）。

**为什么不手写？** 同仓库的 `vdev-audio-win`（PortCls/WaveRT 内核声卡）就是手写绑定的，代价写在经验库里：凭记忆写出的 KS 描述符杜撰出不存在的字段、按值内嵌的结构写成指针、NTSTATUS 常量抄错——编译期零报错，加载后才发现。ABI 结构（结构体布局、函数表索引、常量值）的权威只有 WDK 头文件，bindgen 直取等于把"人肉对照头文件"这一步交给机器，结构性杜绝这类事故。

**C-unwind 全量覆盖。** 驱动的回调要导出成 `extern "C-unwind"`（这是 Rust 与 C++ 异常/ unwind 语义交互的现实选择），绑定层生成的函数指针类型必须与之匹配，否则签名对不上。build.rs 用一行正则把所有生成项统一改 ABI（`crates/vdev-display-win/wdf-umdf-sys/build.rs:273`）：

```rust
let mut builder = bindgen::Builder::default()
    // ... 省略头文件与 include 路径 ...
    .blocklist_item("NTSTATUS")      // 用自己的 ntstatus.rs 实现
    // CRT 内存/串函数随 WDK 头混进生成结果，且被下面的 override_abi(CUnwind, ".*")
    // 一并声明成 extern "C-unwind"——rustc 对 std 依赖的运行时符号有签名校验
    // （必须 extern "C"），C-unwind 声明直接硬错误（CI WDK job 首跑实测）。
    .blocklist_item("memcmp")
    .blocklist_item("memcpy")
    .blocklist_item("memset")
    .blocklist_item("memmove")
    .blocklist_item("strlen")
    .override_abi(Abi::CUnwind, ".*")
```

注释里那句"CI WDK job 首跑实测"是个真实的坑，第四节末尾展开。

**`WdfIsFunctionAvailable!` 与官方公式逐字核对。** UMDF 的 API 按"客户端版本 vs 框架版本"可用性分档，官方头 `wdffuncenum.h` 提供一个宏公式来判断"这个函数在当前框架上是否存在"。绑定层把它复刻成 Rust 宏（`crates/vdev-display-win/wdf-umdf-sys/src/lib.rs:13-30`），宏体注释里挂着官方源码链接，移植时与 `microsoft/Windows-Driver-Frameworks` 的 `WDF_IS_FUNCTION_AVAILABLE` 逐字核对过一致（公式：索引小于"永远可用"计数，或客户端版本不高于框架版本，或索引小于框架实际函数计数）。`IddCxIsFunctionAvailable!` 同构。

这些宏不是摆设。`wdf-umdf` 安全封装层的 `WdfCall!` 宏（`crates/vdev-display-win/wdf-umdf/src/wdf.rs:67-124`）在第一次调用某个 WDF 函数时，按 `WDFFUNCENUM::<名字 TableIndex>` 取函数表索引、先查可用性、再从 `WdfFunctions_02031` 函数表读出函数指针缓存进 `OnceLock`，最后以 `f(WdfDriverGlobals, ...)` 的形式调用并把 `NTSTATUS` 翻译成 `Result`。手写这一层最容易错的"索引偏移、全局表名字、参数顺序"全部来自生成代码，不靠人记。

## 四、安全上下文管理：让"零初始化槽位"成为合法表示

WDF 的对象模型里，驱动可以在每个框架对象（设备/适配器/显示器）上挂一块自定义**上下文（context）**内存：创建对象时通过 `WDF_OBJECT_ATTRIBUTES.ContextTypeInfo` 注册类型信息，之后用 `WdfObjectGetTypedContextWorker` 取回指针。关键点是：**这块内存由框架分配，且保证零初始化**——微软文档 "Framework Object Context Space" 原话："When the framework allocates context space for an object, it also zero-initializes the context space."

`wdf-umdf` 用 `WDF_DECLARE_CONTEXT_TYPE!` 宏（`crates/vdev-display-win/wdf-umdf/src/wdf.rs:145-526`）把这个机制封装成 Rust 风格的 API。驱动侧的使用极其朴素（`crates/vdev-display-win/driver/src/context.rs:57–58`）：

```rust
WDF_DECLARE_CONTEXT_TYPE!(pub DeviceContext);
WDF_DECLARE_CONTEXT_TYPE!(pub MonitorContext);
```

宏为类型生成 `new`/`init`/`clone_into`/`get`/`get_mut`/`drop`/`get_type_info` 一族方法。内部表示是一个 `Arc<RwLock<T>>`：设备对象的 `init` 存 `Strong`，显示器、适配器对象通过 `clone_into` 存 `Weak`——整个上下文只有一份堆分配，框架销毁设备对象时 drop 那个 `Strong`，其余全部失效。

真正的设计难点在**生命周期错位**：`driver_add`（`crates/vdev-display-win/driver/src/entry.rs:136–193`）里的顺序是 `WdfDeviceCreate` → `IddCxDeviceInitialize` → `context.init(device)`。如果 `IddCxDeviceInitialize` 失败，函数直接返回错误，`init` 从未执行；但 WDF 销毁设备对象时会触发我们注册的 `EvtCleanupCallback`，清理回调会调用 `DeviceContext::drop(handle)` 对上下文槽做 `drop_in_place`——**对一块全零内存按含 `Arc` 的枚举析构，就是对空指针/垃圾引用做 drop，是 UB**。

第一直觉是用 `Option<ArcPointer<T>>`，靠 niche 填充让全零等于 `None`——但这依赖"Option<枚举> 的全零字节恰好是 None"这种**语言不保证**的布局假设。最终方案是把槽位类型本身做成零值合法的 `repr(C)` 枚举（`crates/vdev-display-win/wdf-umdf/src/wdf.rs:197-202`）：

```rust
/// M4：显式 `Uninit` 变体让「从未被 init/clone_into 写过的上下文」有合法表示。
/// WDF 框架为对象分配上下文空间时保证零初始化。
/// `repr(C)` 保证 discriminant 位于偏移 0、首变体 tag 为 0，因此全零字节
/// 恰好解码为 `Uninit`；对 `Uninit` drop_in_place 是 no-op。
#[repr(C)]
enum ArcPointer<T> {
    Uninit,
    Strong(::std::sync::Arc<T>),
    Weak(::std::sync::Weak<T>),
}
```

`repr(C)` 枚举的 discriminant 位于偏移 0、首变体 tag 为 0 是**语言级保证**：全零字节严格解码为 `Uninit`，析构它是 no-op，访问路径（`get`/`get_mut`/`clone_into`）遇到 `Uninit` 返回显式的 `NotInitialized` 错误。这个布局论证不是口头断言——宏体内置了回归测试 `zeroed_slot_decodes_as_uninit`（`crates/vdev-display-win/wdf-umdf/src/wdf.rs:209–243`），用 `transmute_copy` 把全零字节按槽位类型解码并断言得到 `Uninit`，同时固化"尺寸 = tag + 裸指针 = 2×usize"。

## 五、数据面：swap chain 线程、MMCSS 与命名管道 IPC

**Swap chain 处理。** OS 把 swap chain 指派给显示器时（`crates/vdev-display-win/driver/src/callbacks.rs:396–419` 的 `assign_swap_chain`），驱动为它起一条专用线程（`crates/vdev-display-win/driver/src/swap_chain_processor.rs:47–92`）。线程先给自己挂 MMCSS（Multimedia Class Scheduler Service）优先级——`AvSetMmThreadCharacteristicsW(w!("Distribution"), ...)`，让高 CPU 负载下帧处理仍被调度器优待——然后进入核心循环：`IddCxSwapChainReleaseAndAcquireBuffer` 取帧，返回 `E_PENDING` 说明新帧未就绪，就 `WaitForSingleObject` 最多 16 ms 等事件或超时后重试；拿到缓冲就调 `IddCxSwapChainFinishedProcessingFrame` 归还。线程退出时负责 `WdfObjectDelete` 掉 swap chain 对象；`SwapChainProcessor` 的 `Drop` 实现置一个 `AtomicBool` 终止标志并 `join` 线程，保证指派/取消指派的干净交接。注意当前实现是**直通**——拿到帧立即归还，尚未做画面注入或捕获，这是留给推流场景的扩展点。

一个小而典型的修复：MMCSS 挂失败时原实现直接 `return`，跳过了后面的 `WdfObjectDelete(swap_chain)`，泄漏整个 swap chain 对象；现在降级为普通优先级继续跑，收尾清理照常（`crates/vdev-display-win/driver/src/swap_chain_processor.rs:61–73` 注释）。

**命名管道 IPC。** 驱动在 `adapter_init_finished` 回调里启动 listener 线程（`crates/vdev-display-win/driver/src/ipc.rs:136`），在 `\\.\pipe\vdev-display` 上循环接受连接。协议是 `driver-ipc` 定义的一组 serde 类型（`DriverCommand::{Notify,Remove,RemoveAll}` / `RequestCommand::State`，见 `crates/vdev-display-win/driver-ipc/src/core.rs:25-51`），JSON 序列化、以 `0x04`（EOT）字节分帧；多客户端间用 tokio broadcast 通道广播变更事件。

这个管道曾经是这个驱动**最大的本地攻击面**，修复分三步，都写在 `ipc.rs`：

1. **NULL DACL → SDDL**（`crates/vdev-display-win/driver/src/ipc.rs:138–164`）。原实现（继承自上游）的注释明写 "These security attributes will allow anyone access"：`SetSecurityDescriptorDacl(..., true, None, false)` 设置 NULL DACL，任何本地低权限用户都能连上管道注入增删显示器命令。现在改为显式 SDDL 描述符（`crates/vdev-display-win/driver/src/ipc.rs:147`）：

   ```rust
   // D:P           DACL 存在且受保护（不被继承 ACL 穿透）
   // (A;;GA;;;BA)  允许 BUILTIN\Administrators 完全访问
   // (A;;GA;;;SY)  允许 LOCAL SYSTEM 完全访问
   let sddl = w!("D:P(A;;GA;;;BA)(A;;GA;;;SY)");
   ```

   副作用是 CLI 也必须以管理员运行才能连驱动——这是一次"收紧到可用性边界"的取舍，与下一道防线配套。

2. **管道抢占防护**（`crates/vdev-display-win/driver/src/ipc.rs:180–201`）。恶意进程可以先建同名管道（pipe squatting），让我们的实例创建失败、或让客户端连到攻击者的管道。修复是给第一个实例加 `FILE_FLAG_FIRST_PIPE_INSTANCE` 语义（tokio `ServerOptions::first_pipe_instance(true)`）：首实例创建成功后其余实例才允许建，抢注者反而会让**我们**的首建失败——但此时不再 panic，而是记日志 + 每秒退避重试（原实现在这里是无条件 `unwrap`，抢注场景等于远程击落驱动宿主）。SDDL 转换失败同样直接放弃启动 listener，绝不退回无保护管道。

3. **输入校验收进驱动**。新增的 `driver/src/validate.rs` 是一个零依赖纯函数模块，把所有上限集中定义（`crates/vdev-display-win/driver/src/validate.rs:14–36`）：显示器 ≤16、每显示器模式 ≤64、每模式刷新率 ≤64、分辨率 ∈ [64, 16384]、刷新率 ∈ [1, 1000]、单条消息 ≤512 KiB（512 KiB 是按最坏合法 Notify 的 JSON 体量 378,748 字节推算出来的，有一个单测 `worst_case_legal_notify_fits_in_max_msg_bytes` 把这个数字固化）。读循环里字节积累超限即清空缓冲（`crates/vdev-display-win/driver/src/ipc.rs:250–256`），防无界内存增长；模式数值溢出全部改到 `u64`/`checked`/`saturating` 域计算（`crates/vdev-display-win/driver/src/callbacks.rs:82–98`，像素时钟公式在大分辨率高刷新率下会击穿 u32，注释里给了具体组合：2160p@1000Hz ≈ 4.68e9 ≥ u32::MAX）。

纯函数化的副产品：`validate.rs` 不依赖任何 FFI 类型，可以在 macOS 宿主上用 `rustc --test` 直接跑回归测试——这个 workspace 整体只能在 Windows 主机构建，宿主测试能力是刻意设计出来的（下文《构建》节详述）。

## 六、CLI 与安装：INF、SetupAPI 与 UAC 提权

**INF 的关键段**（`driver/vdev-display.inf`）：设备类是 Display（`ClassGUID = {4D36E968-E325-11CE-BFC1-08002BE10318}`）；`.hw` 段把 `IndirectKmd` 挂成上层过滤器——这是 IddCx 的内核侧伴生驱动，UMDF 驱动必须借它才接到显示栈；`[*.Wdf]` 段声明 `UmdfService=vdev-display` 与 `UmdfLibraryVersion=2.31.0`、`UmdfExtensions = IddCx0102`。这里有一处真实修复：INF 原来写 `UmdfLibraryVersion=2.25.0`，而 build.rs 链接的是 UMDF 2.31 的 stub（代码经 `WdfFunctions_02031` 函数表调用）——版本声明低于实际链接版本，Windows 可能按错误的宿主版本加载导致驱动装不上。修复时顺带核对了微软官方 `IddSampleDriver.inf`：`IddCx0102` 的写法与官方样例一致；而 `IddMinimumVersionRequired` 是 IddCx stub 要求的**链接期符号**（本项目在 `wdf-umdf-sys/src/bindings.rs` 以 `#[no_mangle] static = 4` 提供，对应 IddCx 1.4），不是 INF 指令，不应写进 INF（`vdev-display.inf:48-62` 的注释完整记录了这三个结论）。`DriverVer` 也与 `driver/Cargo.toml` 的 `0.4.0` 做了同步。

**安装流程**（`crates/vdev-display-win/cli/src/install.rs:170–260`）走 SetupAPI 五步：`SetupDiGetINFClassW` 从 INF 取类 GUID/类名 → `SetupDiCreateDeviceInfoList` 建设备信息集 → `SetupDiCreateDeviceInfoW`（`DICD_GENERATE_ID` 自动生成实例 ID）→ `SetupDiSetDeviceRegistryPropertyW` 设硬件 ID `Root\vdev-display` → `SetupDiCallClassInstaller(DIF_REGISTERDEVICE)` 注册节点 → `DiInstallDriverW` 把 INF 装入驱动存储并安装。安装前先清残留设备节点，保证 IddCx 单实例语义。

**UAC 自提权**（`crates/vdev-display-win/cli/src/main.rs:276–345`）有三个容易踩的坑，修复后的实现都处理了：

- **参数引用**：`args.join(" ")` 拼出来的命令行，含空格的 `--inf-dir "C:\Program Files\..."` 会被拆错位。现在逐参数按 MSVCRT argv 规则加引号转义（`quote.rs` 单独成模块，有宿主单测）；
- **工作目录**：`ShellExecuteExW` 的 `runas` 子进程 CWD 默认是 `System32`，相对路径 `--inf-dir target\dist` 提权后必然失效。现在把 `lpDirectory` 显式设为当前目录（`crates/vdev-display-win/cli/src/main.rs:299–307`）；
- **退出码**：提权子进程失败、父进程报成功是最糟的失败模式。现在 `SEE_MASK_NOCLOSEPROCESS` 拿到句柄，`WaitForSingleObject` + `GetExitCodeProcess` 透传真实退出码。

**persist 的教训**：上游曾有一个 `persist` 子命令把显示器列表写进 `HKCU\SOFTWARE\vdev-display`，帮助文本承诺"重启后恢复"。审查发现全仓**没有任何代码读这个键**——只写不读的"恢复"是虚假承诺，修复选择了直接删除该功能，而不是补一个没人验证过的读取方。文档承诺的功能必须有代码兑现，否则删掉比留着诚实。

## 七、踩坑实录

以下六个问题全部是真实发生、真实修复的案例，按"最值得记住"排序。

### 坑 1：未初始化 context 被 drop → UB

即第四节的 M4。复盘时最值得注意的是**触发条件的隐蔽性**：只有"设备创建成功、`IddCxDeviceInitialize` 失败"这条罕见失败路径上，框架才会对一个从未写入的上下文槽触发清理回调。正常路径全绿，测试全过，UB 藏在错误处理路径里等着。修法（`repr(C)` 枚举首变体 `Uninit` 零值合法化）没有选择移动 `init` 调用顺序——那只是把问题从一条路径挪到另一条——而是让"零初始化"本身成为类型的合法表示，从表示层消灭这一类 UB。另外记录一个被否决的方案：`Option<ArcPointer<T>>` 的 niche 填充恰好让全零等于 `None` 看似可行，但那是实现细节不是语言保证，换编译器版本就可能翻车。

### 坑 2：D0Entry 不幂等，睡眠唤醒后显示器全消失

`EvtDeviceD0Entry` 在每次设备上电时都会调用——睡眠唤醒、重新插拔都会再来一次。原实现无条件执行 `init_adapter` → `IddCxAdapterInitAsync` 重建适配器，而全局 `ADAPTER` 槽第二次 `set` 必然失败，返回 `STATUS_ADAPTER_HARDWARE_ERROR`：**resume 之后虚拟显示器集体消失**。修复是在 `init_adapter` 开头加幂等护栏（`crates/vdev-display-win/driver/src/context.rs:87–92`）：

```rust
pub fn init_adapter(&mut self) -> Result<(), ContextError> {
    // M6：D0Entry 可多次发生（睡眠唤醒/重新上电）。adapter 只允许初始化一次，
    // 否则每次 D0 都会 IddCxAdapterInitAsync 重建 adapter 对象：
    // 旧 adapter 对应的上下文 Weak 泄漏、已创建的显示器全部失效（唤醒后显示器消失）。
    if self.adapter.is_some() {
        return Ok(());
    }
```

驱动回调没有"只会调用一次"的假设，每个带全局副作用的初始化都值得问一句"再来一次会怎样"。

### 坑 3：bindgen 的 CRT 符号在 CI 首跑炸出硬错误

第三节贴的那段 blocklist 注释，背后是 bindgen + `override_abi(CUnwind, ".*")` 组合的真实事故：`override_abi` 的全量正则无差别命中了**所有**生成函数，包括 WDK 头顺带声明的 `memcmp/memcpy/memset/strlen`——它们被改写成 `extern "C-unwind"` 之后，rustc 编译期拒绝："invalid definition of the runtime symbol used by the standard library"（std 要求这四个运行时符号必须是 `extern "C"` 签名，编译期有签名校验）。这个错误**只在真实生成绑定的环境暴露**——开发机是 macOS，交叉编译路径上 build.rs 直接跳过 bindgen，宿主侧零症状；CI 的 WDK job 首跑才第一次真实编译。修复是 blocklist 掉这五个符号（crate 内无直接调用点，`memmove` 属同类预防）。教训是：`override_abi(".*")` 这类全量正则要主动想想它还命中了谁，凡是与 std 共存的绑定产物，std 运行时符号一律先屏蔽。

### 坑 4：宏定义而未实例化，宏体错误零可见

同一个 CI 首跑暴露的第二类问题：`WDF_DECLARE_CONTEXT_TYPE!` 宏体内有 9 处裸 unsafe 操作（unsafe fn 体内解引用未包 unsafe 块），workspace 配了 `unsafe_op_in_unsafe_fn = deny`（`Cargo.toml:17-18`）却一直没报——因为 **rustc 只检查实例化后的代码**。宏在定义 crate `wdf-umdf` 内从未被调用过，宏体就是纯 token 树，语法/类型/lint 错误一律"不存在"；而第一次展开它的恰是 `cfg(test)` 里的布局探针（`crates/vdev-display-win/wdf-umdf/src/wdf.rs:687–705`），于是 9 个 E0133 全从 test 目标爆出，lib 目标反而是绿的。修法两条：库导出的 `macro_rules!` 必须在 crate 内自带 `#[cfg(test)]` 最小展开探针（顺便做布局断言，第四节那个 `zeroed_slot_decodes_as_uninit` 正是这个探针）；修宏体错误要按"所有调用方展开都受影响"推演一次修完，不能等编译器逐轮喂。"宏定义 crate 与宏调用 crate 分离"的组合，lint 门禁对宏体是天然盲区。

### 坑 5：NULL DACL + 管道抢占（M1）

第五节已详述修复。补充审查视角：UMDF 驱动宿主 `WUDFHost` 是系统级进程，"驱动对管道输入零校验 + 管道对任何用户开放"的组合意味着任何低权限本地用户都能操控虚拟显示器、灌爆驱动宿主内存，甚至抢注管道让驱动在 `unwrap` 处 panic。三条修复（SDDL、首实例保护、驱动侧集中校验）是同一次审查里一起落地的——**攻击面要整条链一起收**，只修一半等于没修。

### 坑 6：把 `DRIVER_OBJECT*` 当 `WDFDEVICE` 传（M5）

事件日志服务启动晚于驱动时，重试线程 5 分钟超时分支里曾把 `DRIVER_OBJECT*` 强转成 `WDFDEVICE` 调 `WdfDeviceSetFailed`——句柄类型混淆，且 `DriverEntry` 阶段根本不存在任何设备对象。FFI 里裸指针换算编译器帮不了你，这句柄在运行期指向什么只有类型系统之外的事实能回答。修复是删除该调用：那个阶段本来就没有合法的设备句柄可传，"记录超时事实、不做对象操作"才是正确语义（`crates/vdev-display-win/driver/src/entry.rs:96–98` 留了完整注释）。顺带一提，重试线程里的 `Duration::from_mins(5)` 用的是 Rust 1.91 才稳定的 std API，这一行代码本身就钉住了 toolchain 下限——仓库没有 rust-toolchain 固定文件时，这类细节要主动核对。

## 八、构建、签名与运行

前置依赖（详见 [crate README](https://github.com/gqf2008/vdev/blob/main/crates/vdev-display-win/README.md)）：Visual Studio 2022（C++ 桌面负载）、WDK 10.0.26100、LLVM（bindgen 的 libclang）、Rust stable MSVC target。**必须在 Windows 主机构建**——bindgen 需要 WDK 头文件，`driver/.cargo/config.toml` 也把 target 固定为 `x86_64-pc-windows-msvc`：

```powershell
cd crates\vdev-display-win
$env:LIBCLANG_PATH = "C:\Program Files\LLVM\bin"
cargo build --release          # 产物 vdev_display.dll + vdev-display-win.exe
.\scripts\stage-sign.ps1       # 打包 + 签名 DLL + 生成/签名目录文件 → target\dist
```

签名现状如实说明：UMDF 驱动包需要签名，本机开发用自签名代码签名证书（`New-SelfSignedCertificate -Type CodeSigningCert` 生成，`certutil -addstore` 装进 TrustedPublisher + Root）通常即可安装，**无需开测试签名**；WHQL/正式签名未做。安装与使用：

```powershell
vdev-display-win.exe install --inf-dir target\dist   # 非管理员会自动 UAC 提权
vdev-display-win.exe status
vdev-display-win.exe add 1920x1080
vdev-display-win.exe add 3840x2160@120 1280x720@60/120 --name "vdev-4k"
vdev-display-win.exe list / set-mode 0 2560x1440@144 / remove 0 / remove-all
vdev-display-win.exe uninstall
```

添加后系统设置里立刻出现新显示器，桌面可扩展过去（OS 直接渲染）。`add` 的模式语法是 `宽x高@刷新率1/刷新率2`，缺省刷新率补 60。

**测试的门禁矩阵**值得单独一提：`driver-ipc` 的协议/状态测试全部依赖 tokio 的 Windows named pipe，只能随 Windows `cargo test` 跑；为了不让 macOS 宿主零覆盖，纯逻辑被刻意抽成零依赖模块（`validate.rs`、`mode_check.rs`、`quote.rs`、`state.rs`、ArcPointer 布局镜像），由 `scripts/state-tests-host.sh` 用 `rustc --test` 在宿主直跑。CI 里 `windows-user` job 门禁 WDK-free 的用户态包；`windows-driver-wdk` job（runner 上 winget 装 WDK + LLVM 后 `cargo check --workspace`）起初以 `continue-on-error` 的**顾问 job** 形式上线——坑 3、坑 4 都是它首跑逮到的，顾问期物超所值；实测连续多次稳定通过、且历史红灯都是真实驱动编译错误后，已于 2026-09 升为**硬门禁**（`.github/workflows/ci.yml:122-150`）。

## 九、现状与局限

**已真机验证通过（2026-09-14，Win10 19045 x64）**：装机后 `add 1920x1080` 让系统多出一块屏
（`\\.\DISPLAY223` 1920x1080，Monitor 类出现 "Generic PnP Monitor"），`set-mode 0 2560x1440`
返回 0，`list` 能枚举到该虚拟屏；静态审查、CI 门禁（含 `windows-driver-wdk` 硬门禁）、协议层
单测都已就绪。README 的状态框已随之从"装机验证中"改为 ✅ 可用。

已知局限，如实列出：

- swap chain 处理是**直通**（取帧即还），画面注入/捕获是后续工作；
- 硬件光标能力**刻意未声明**：曾声明支持却不喂数据导致光标更新丢失，现回退为 OS 软件光标（`crates/vdev-display-win/driver/src/context.rs:283–287` 注释），绑定层的 `IddCxMonitorSetupHardwareCursor` 封装保留备用；
- INF 仅声明 `NTamd64`，而 build.rs 已支持 ARM64 目标——两侧尚未对齐；
- **CLI 需要管理员**：驱动侧管道 SDDL 只给 `BA`/`SY`，所以 `add`/`list`/`set-mode`/`remove` 这类
  要连驱动命令都必须提权（CLI 只在 `install`/`uninstall` 上自动 UAC，其余子命令会直接失败）。
  日常频繁增删显示器的场景可以考虑放宽为交互用户 + 仍保留首实例防护；
- **多虚拟屏会崩**：追加第二块以上的节点时 UMDF 侧不稳定（实测崩），当前按"单虚拟屏"使用；
- 驱动侧对 `adapter_commit_modes` 目前直接返回成功，未记录 OS 实际提交的模式。

## 十、写在最后

这个项目的绑定层与驱动本体移植自 MIT 项目 [virtual-display-rs](https://github.com/MolotovCherry/virtual-display-rs)（归属见 `THIRD_PARTY.md`），vdev 在其上做了大量加固：SDDL/首实例管道防护、集中式输入校验、回调 panic 兜底、上下文槽零值合法化、D0 幂等、对象清理回调补全、CLI 提权修复。回头看，最有普适价值的三条经验：

1. **ABI 结构的权威只有头文件**——bindgen 直取 WDK 头比手写绑定便宜得多，也安全得多；
2. **失败路径才是 UB 的老家**——"初始化失败后框架仍触发清理"这种跨框架的失败序，要在数据表示层（零值合法化）而非调用顺序上解决；
3. **从未真实编译过的代码等于没写过**——宏体、feature 门控、平台专属包，都需要一条真实触达的构建路径，哪怕先是顾问 job。

Windows 侧的虚拟摄像头（DirectShow 用户态、免签名）、虚拟声卡（WDM PortCls/WaveRT，不使用 WDF）与内核 HID 驱动的姊妹篇见仓库 `docs/` 与 README。如果这篇文章帮你少踩一个坑，欢迎到 [vdev](https://github.com/gqf2008/vdev) 点个 star。
