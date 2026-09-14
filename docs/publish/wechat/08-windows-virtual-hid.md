# 用 Rust 写 Windows 内核虚拟键鼠：从 KMDF HID minidriver 到 Virtual HID Framework

**内核 HID minidriver 的正确性大头不在语法，而在三重契约：INF 接线、IOCTL 编号、结构体布局——编译器全都看不见。**

> 本文是 vdev 虚拟设备驱动开发系列之一（共 9 篇）。完整源码与代码位置标注见仓库对应文章。

> **2026-09-14 更新：内核路线已从 minidriver 换成 VHF（Virtual HID Framework）。** 原因很硬：
> minidriver 路线要 `Include=MsHidKmdf.inf`，而该文件只随 **Windows 11（build 22000+）** 提供——
> Win10 19045 的 `%SystemRoot%\inf` 下没有它，安装直接报 `0xE0000219`。
> **VHF 自 Win10 1607 起随系统提供**，跨版本可用，且免去"自己应答 `IOCTL_HID_*`、从
> `Irp->UserBuffer` 掏 `HID_XFER_PACKET`"这一整类契约风险：描述符与输入报告上行都由 VHF 代管，
> 驱动只需 `VhfCreate`/`VhfStart` 建虚拟 HID 设备、`VhfReadReportSubmit` 交报告；
> 用户态用 `HidD_SetFeature` 写厂商 Feature 报告即注入（`WriteFile` 路线返回 `ERROR_INVALID_FUNCTION`），
> INF 里则需要 `HKR,,LowerFilters,0x00010000,"vhf"`（缺它 `VhfCreate` 直接返回
> `STATUS_INVALID_DEVICE_REQUEST(0xC0000010)`）。
> **下文保留了 minidriver 版的完整实现与踩坑**——它是一条真实的失败路径，价值在"为什么走不通"
> 与"哪些契约在 VHF 下依然适用"（结构布局、报告描述符、键码映射三节完全不失效）。
> 实测结果见文末"现状与局限"。

## 1. 为什么要"软件造一套键盘鼠标"

让程序替人打字、移动鼠标，是最常见的一类系统级需求：UI 自动化测试要模拟真实按键序列、远程协助要把对端的键鼠事件"回放"到本机、无障碍工具要把眼动/语音转成点击。Windows 上实现它有三条典型路线：

1. **SendInput（用户态 API）**：几十行代码搞定，`vdev-hid-win` 的默认模式就是它。但它注入的事件在底层钩子处带"软件注入"标记（`LowLevelKeyboardProc`/`LowLevelMouseProc` 里的 `LLKHF_INJECTED`/`LLMHF_INJECTED`），且它只是"向系统投递事件"，系统里并不存在一个对应的输入设备。
2. **现成的开源虚拟 HID 驱动（vmulti 等）**：直接提供可安装的 sys+INF，拿来即用；但项目普遍年久失修，报告格式、设备身份等想改就得自己动 C 代码。
3. **自己写 KMDF HID minidriver**：在内核里注册一个真正的 HID 设备。报告从 HID 类驱动（hidclass）进入系统，与物理 USB 键鼠同一条输入栈；设备管理器里出现的是货真价实的键盘/鼠标节点。

第三条路线的工程量最大（内核驱动、签名门槛），但只有它给出"真实设备级"的仿真：Raw Input 按设备枚举时能看到它，面向 HID 设备的诊断工具能看到它，事件来源与物理输入同源。需要说明的是，这种"难以与物理设备区分"的特性同样处于反作弊等输入审计系统的关注范围内——本文只讨论它在自动化测试、远控、无障碍等正当场景下的技术实现，请遵守目标软件的服务条款。

vdev 的内核路线（仓库内叫"路线 B"）以微软官方样例 **vhidmini2** 为蓝本。工程分两个 crate：用户态 CLI `vdev-hid-win`（`crates/vdev-hid-win`）与内核驱动 `vdev-hid-driver`（独立 workspace `crates/vdev-hid-win/kernel`）；内核绑定是 vendored、由 bindgen 生成的路由（`kernel/vendor/wdk-sys`）。下文所有 IOCTL 值、结构体布局、INF 段名均出自该仓库源码，文末附踩坑实录。

## 2. HID 协议最小知识

HID 设备用**报告描述符（Report Descriptor）**自描述数据格式。它是一段字节码，由 Usage Page（设备类别，如 Generic Desktop 0x01、Key Codes 0x07）、Usage（类别内语义，如 Keyboard 0x06、Mouse 0x02）、Report Size/Count、Input/Output/Feature 等指令组成；`Collection (Application)` 划出一个独立的"应用集合"，对应操作系统里的一个顶层设备功能。

键盘的经典 8 字节输入报告布局：第 0 字节是 8 个修饰键（左 Ctrl 到右 Win）的位图，第 1 字节保留，第 2–7 字节是最多 6 个按下的按键码（数组型字段，每个字节填一个 Key Codes usage）。鼠标则常用 1 字节键位（3 bit + 5 bit 填充）加 X/Y/滚轮三个 int8 相对值，共 4 字节。

vdev 的键盘描述符（完整 60 字节）有一个关键设计——**厂商输出管道**：

`rust
// crates/vdev-hid-win/kernel/driver/src/contract.rs
pub static KEYBOARD_REPORT_DESCRIPTOR: [u8; 60] = [
 0x05, 0x01, // Usage Page (Generic Desktop)
 0x09, 0x06, // Usage (Keyboard)
 0xA1, 0x01, // Collection (Application)
 // ……修饰键位图 + 保留字节 + 6 按键码（Input）……
 0x05, 0x01, // Usage Page (Generic Desktop)
 0x09, 0x00, // Usage (Undefined)
 0x26, 0xFF, 0x00, // Logical Maximum (255)
 0x95, 0x08, // Report Count (8)
 0x91, 0x00, // Output (Data, Array, Absolute) —— 注入管道
 0xC0, // End Collection
];
`

这个 8 字节的 Output 字段声明了一个输出管道：用户态对 HID 接口 `WriteFile` 的数据会沿它抵达驱动（`IOCTL_HID_WRITE_REPORT`）。驱动把它**当作输入报告**投递回 hidclass——于是"写进去一个键，系统就收到一个键"。鼠标描述符（67 字节，）同理带一条 4 字节输出管道。

还有一个值得注意的细节：按键码数组的 Usage/Logical Maximum 写到 `0x73`（115）而不是常见的 `0x65`（101），因为 F13–F24 的 usage 落在 0x68–0x73，上限给低了这些键会被 hidclass 静默丢弃——这是审查阶段抓出来的真实 bug（见第 6 节）。

## 3. 第一版：KMDF HID minidriver 架构（minidriver 路线，已弃用）

Windows 的 HID 栈是标准的类驱动/微型驱动分层：

- **hidclass.sys**（类驱动）：向上暴露设备接口（`GUID_DEVINTERFACE_HID`），承接用户态的 `CreateFile`/`ReadFile`/`WriteFile`/`HidD_*`，把解析后的请求以内部 IOCTL 的形式发给下层；
- **minidriver**：只负责应答这组内部 IOCTL（给描述符、交报告、收报告），完全不碰 PnP/电源/缓冲管理的杂务；
- **mshidkmdf.sys**（KMDF HID 映射驱动，Win11 build 22000 起由 `MsHidKmdf.inf` 安装）：站在 hidclass 与 KMDF minidriver 之间的"翻译层"，把 hidclass 的 WDM 请求转成 WDF 队列回调。

vdev 驱动的自我定位很有意思：它把自己挂成设备的**下层过滤器**，真正的功能驱动是继承自 `MsHidKmdf.inf` 的 mshidkmdf：

`ini
; crates/vdev-hid-win/kernel/driver/vdev-hid.inf
[vdev_hid_kbd_Install.NT]
CopyFiles=Drivers_Dir
Include=MsHidKmdf.inf
Needs=MsHidKmdf.NT

[vdev_hid_kbd_Install.NT.Services]
Include=MsHidKmdf.inf
Needs=MsHidKmdf.NT.Services
; minidriver 服务 flag 0：关联服务由 Needs 继承的 mshidkmdf 承担
AddService = vdev_hid, 0x00000000, vdev_hid_Service_Inst

[vdev_hid_kbd_Install.NT.Filters]
AddFilter=vdev_hid,,vdev_hid_Filter_Install
`

驱动侧与之呼应，`evt_device_add` 里先调 `WdfFdoInitSetFilter` 声明过滤器身份，随后建两条队列：**默认并行队列**接 hidclass 发来的 `IOCTL_HID_*`（回调 `evt_io_internal_device_control`），**manual 队列**挂起暂无数据可交的读请求。

一个驱动服务两套设备：INF 写了两条模型行 `Root\vdev-hid`（键盘）与 `Root\vdev-hid-mouse`（鼠标），安装时经注册表硬件键写入 `Role` 值（0=键盘，1=鼠标），驱动在设备添加时读回（`read_role`，），按角色选择各自的 HID 描述符、报告描述符与报告长度。VID/PID 也由角色决定：`VID=0x5644`（"VD"），键盘 `PID=0x4849`（"HI"），鼠标 `PID=0x484D`（"HM"）。

全局状态用一把 **WDF 框架自旋锁**保护：两个实例（各自的队列句柄、最近一次注入的 8 字节报告、`report_ready` 标志）都在锁内访问。`StateCell` 用 `UnsafeCell` + 手写 `unsafe impl Sync` 承载，RAII 守卫在 `Drop` 里 `WdfSpinLockRelease`——为什么不用手工 `KSPIN_LOCK`，踩坑第 ⑤ 条有交代。

## 4. IOCTL 处理：HID 内部契约全家福（minidriver 版，已弃用）

> 本节是**第一版 minidriver 实现**的契约细节（Win11 可用、Win10 装不上）。当前 VHF 实现不再
> 自己应答这些内部 IOCTL——但"数值必须可溯源、可断言"这条纪律与实现选型无关，且报告描述符、
> 键码映射、结构布局三节在 VHF 下**完全继续适用**。

minidriver 的全部工作就是一个 `match`。hidclass 与 minidriver 之间的 IOCTL 定义在 `hidport.h` 与 `hidclass.h`，都是 `CTL_CODE(FILE_DEVICE_KEYBOARD=0x0B, 功能码, Method, FILE_ANY_ACCESS)` 的展开。vdev 把这组常量放在 windows-free 的 `contract.rs` 里，宏展开对应关系如下（数值可在宿主机单测中断言，）：

- **IOCTL**：`IOCTL_HID_GET_DEVICE_DESCRIPTOR`　**功能码 / Method**：0 / NEITHER　**值**：`0x000B0003`　**用途**：返回 `HID_DESCRIPTOR`
- **IOCTL**：`IOCTL_HID_GET_REPORT_DESCRIPTOR`　**功能码 / Method**：1 / NEITHER　**值**：`0x000B0007`　**用途**：返回报告描述符
- **IOCTL**：`IOCTL_HID_READ_REPORT`　**功能码 / Method**：2 / NEITHER　**值**：`0x000B000B`　**用途**：hidclass 取一个输入报告
- **IOCTL**：`IOCTL_HID_WRITE_REPORT`　**功能码 / Method**：3 / NEITHER　**值**：`0x000B000F`　**用途**：WriteFile 注入入口
- **IOCTL**：`IOCTL_HID_GET_STRING`　**功能码 / Method**：4 / NEITHER　**值**：`0x000B0013`　**用途**：字符串索引（本设备不实现）
- **IOCTL**：`IOCTL_HID_GET_DEVICE_ATTRIBUTES`　**功能码 / Method**：9 / NEITHER　**值**：`0x000B0027`　**用途**：返回 `HID_DEVICE_ATTRIBUTES`（VID/PID/版本）
- **IOCTL**：`IOCTL_HID_GET_FEATURE`　**功能码 / Method**：100 / OUT_DIRECT　**值**：`0x000B0192`　**用途**：无 feature 报告，返回 NOT_SUPPORTED
- **IOCTL**：`IOCTL_HID_SET_FEATURE`　**功能码 / Method**：100 / IN_DIRECT　**值**：`0x000B0191`　**用途**：同上
- **IOCTL**：`IOCTL_HID_SET_OUTPUT_REPORT`　**功能码 / Method**：101 / IN_DIRECT　**值**：`0x000B0195`　**用途**：`HidD_SetOutputReport`，与 WRITE_REPORT 共用注入臂
- **IOCTL**：`IOCTL_HID_GET_INPUT_REPORT`　**功能码 / Method**：104 / OUT_DIRECT　**值**：`0x000B01A2`　**用途**：返回当前报告状态

（hidport.h 里另有功能码 7=ACTIVATE/8=DEACTIVATE/10=IDLE，本驱动未处理，落默认臂返回 `STATUS_INVALID_DEVICE_REQUEST`。）

分发主干只有一屏：

`rust
// crates/vdev-hid-win/kernel/driver/src/lib.rs（节选）
unsafe fn dispatch_ioctl(...) -> DispatchResult {
 let Some(role) = role_for_queue(queue) else {
 return DispatchResult::Complete(STATUS_INVALID_DEVICE_REQUEST);
 };
 match code {
 hid::IOCTL_HID_GET_DEVICE_DESCRIPTOR => {
 DispatchResult::Complete(unsafe { copy_to_output(request, hid_descriptor(role)) })
 }
 hid::IOCTL_HID_READ_REPORT => unsafe { handle_read_report(role, request) },
 hid::IOCTL_HID_WRITE_REPORT | hid::IOCTL_HID_SET_OUTPUT_REPORT => {
 DispatchResult::Complete(unsafe { handle_inject(role, request, input_len) })
 }
 hid::IOCTL_HID_GET_FEATURE | hid::IOCTL_HID_SET_FEATURE => {
 DispatchResult::Complete(STATUS_NOT_SUPPORTED) // 0xC00000BB
 }
 // ……
 }
}
`

### HID_XFER_PACKET 要从 `Irp->UserBuffer` 取（WDM 逃逸）

写报告类请求携带的不是裸缓冲，而是一个 `HID_XFER_PACKET` 结构（`hidclass.h`：`reportBuffer` 指针 + `reportBufferLen` + `reportId`，）。vhidmini2 的 util.c 有一句关键注释：hidclass 把**包结构体的指针放在 `Irp->UserBuffer`**，KMDF 的 `WdfRequestRetrieveInputMemory`/`OutputMemory` 对这类请求取到的是别的东西——必须经 WDM 逃逸，先拿 IRP 再读 `UserBuffer`：

`rust
// crates/vdev-hid-win/kernel/driver/src/lib.rs
unsafe fn retrieve_packet(
 request: WDFREQUEST, buffer_len: usize, packet: &mut HID_XFER_PACKET,
) -> NTSTATUS {
 if buffer_len < size_of::<HID_XFER_PACKET> {
 return STATUS_INVALID_BUFFER_SIZE;
 }
 let irp = unsafe { call_unsafe_wdf_function_binding!(WdfRequestWdmGetIrp, request) };
 unsafe {
 core::ptr::copy_nonoverlapping(
 (*irp).UserBuffer.cast::<u8>,
 (packet as *mut HID_XFER_PACKET).cast::<u8>,
 size_of::<HID_XFER_PACKET>,
 );
 }
 STATUS_SUCCESS
}
`

取到包后再校验 `reportBuffer` 非空、长度不小于本角色报告长度，把报告字节拷进状态并调用 `inject_report`。这短短十几行是整个移植里最容易写错、错了还会蓝屏的地方（踩坑第 ③ 条）。

## 5. 注入链路：从一条 CLI 命令到系统收键

以 `vdev-hid-win kernel key a` 为例，整条链路是：

1. **纯逻辑层**（windows-free，macOS 也能跑单测）：`key_to_hid("a")` 把键名映射为 usage `0x04`，`make_report` 组装 8 字节报告 `[0,0,0x04,0,0,0,0,0]`（修饰键、保留、键码三段布局，）；
2. **找设备**：`HidD_GetHidGuid` 拿 HID 接口类 → `SetupDiEnumDeviceInterfaces` 枚举全部 HID 接口 → 逐个打开并 `HidD_GetAttributes` 核对 VID/PID，命中 `0x5644/0x4849` 即键盘；
3. **写入**：`CreateFileW` 打开接口后直接 `WriteFile` 8 字节：

`rust
// crates/vdev-hid-win/src/kernel.rs（节选）
let handle = unsafe {
 CreateFileW(PCWSTR(wide.as_ptr), GENERIC_WRITE.0,
 FILE_SHARE_READ | FILE_SHARE_WRITE, None, OPEN_EXISTING,
 FILE_ATTRIBUTE_NORMAL, None)
}?;
let mut written = 0u32;
let ok = unsafe { WriteFile(handle, Some(report), Some(&mut written), None) };
`

4. **内核侧**：WriteFile 到 HID 接口 → hidclass 组 `IOCTL_HID_WRITE_REPORT` + `HID_XFER_PACKET` → 默认队列回调 → `handle_inject` 经 WDM 逃逸取包 → `inject_report` 把报告写入实例状态；
5. **交付**：`inject_report` 尝试从 manual 队列取一个挂起的 `READ_REPORT` 请求，有则立刻用报告完成它；没有就置 `report_ready`，等 hidclass 的下一次读请求到来时立即投递（`handle_read_report`，）。hidclass 对一个设备同时只挂一个读，所以 manual 队列至多一个请求；转发请求与注入到达之间存在一小段竞态窗口，代码在 `WdfRequestForwardToIoQueue` 之后再检查一次 `report_ready`，把可能"刚进门"的注入取回并自行完成，避免报告永久滞留；
6. **系统消费**：完成的读请求沿 hidclass 上行，报告被翻译成普通键盘事件进 Win32 输入栈——与插上一个真实键盘无异。

`HidD_GetInputReport`（`IOCTL_HID_GET_INPUT_REPORT`）走同一条路的反向：把实例里缓存的最近报告拷回调用方缓冲，可用于自检"驱动眼里当前按键状态"。

## 6. 踩坑实录：编译器看不见的三重契约

以下是这个驱动从"构建全绿但链路不通"到"可安装可注入"修复轮里的真实案例。它们共同指向一个事实：**内核驱动的正确性大头不在语法，而在与系统组件的契约——INF 接线、结构体布局、IRP 缓冲区槽位，编译器全都看不见。**

### 坑 ①：INF 不接 hidclass/mshidkmdf，全部 IOCTL 处理器是死代码

初版 INF 只 `AddService` 了自身驱动，没有 `Include=MsHidKmdf.inf` + 三段 `Needs`，也没有 `[*.NT.Filters]AddFilter` + `FilterPosition=Lower`。结果是：设备节点能出现（只证明 `Class=HIDClass` 生效），但 mshidkmdf/hidclass 功能驱动从未进栈，`GUID_DEVINTERFACE_HID` 接口无人注册，用户态按 HidD GUID 枚举必为空——驱动里那一整块 IOCTL 处理代码一行都不会被执行。另一个连带条款：已经 `Needs=MsHidKmdf.NT.Services` 继承了关联服务的 INF，自己的 `AddService` 必须 flag 0（非 `SPSVCINST_ASSOCSERVICE`），否则双关联服务，轻则安装报错，重则 mshidkmdf 被顶掉、`HidRegisterMinidriver` 失败。

### 坑 ②：`HID_DESCRIPTOR` 必须 packed——10 字节 vs 线格式 9 字节

hidport.h 的 `_HID_DESCRIPTOR` 整体被 `pshpack1.h` 包裹（1 字节对齐）：`bLength(1) + bDescriptorType(1) + bcdHID(2) + bCountryCode(1) + bNumDescriptors(1) + 描述符列表项(1+2) = 9 字节`。Rust 侧初版用 `#[repr(C)]` 自然对齐：列表项里的 `u16` 要求 2 字节对齐，偏移 7 处插入 1 字节填充，`size_of` 变 10，`bLength` 也跟着写成 10。hidclass 按打包偏移去读 `wDescriptorLength`，读到的是错位字节（实测错读成 15360），报告描述符协商直接崩。修复是 `#[repr(C, packed)]` + 编译期断言兜底：

`rust
// crates/vdev-hid-win/kernel/driver/src/hid.rs
#[repr(C, packed)]
pub struct HID_DESCRIPTOR {
 pub bLength: UCHAR,
 pub bDescriptorType: UCHAR,
 pub bcdHID: u16,
 pub bCountryCode: UCHAR,
 pub bNumDescriptors: UCHAR,
 pub DescriptorList: [HID_DESCRIPTOR_LIST_ENTRY; 1],
}
const _: = assert!(size_of::<HID_DESCRIPTOR> == 9);
`

packed 结构体在 Rust 里禁止取字段引用，所以字段一律按值读写或整结构体字节拷贝——这也是 `copy_to_output` 以 `&T` 整体拷出的原因。

### 坑 ③：`HID_XFER_PACKET` 取错 IRP 槽位，注入必败甚至蓝屏

初版用 `WdfRequestRetrieveInputMemory` 取包。这些 IOCTL 标着 METHOD_NEITHER，但 hidclass 真正放包的位置是 `Irp->UserBuffer`；KMDF 的 Retrieve*Memory 对这类请求拿到的不是它，最坏情况是对一个可能为 NULL 的 `Type3InputBuffer` 解引用——注入必失败，蓝屏收场。修法即第 4 节那段"WDM 逃逸"：`WdfRequestWdmGetIrp` 拿 IRP，先按方向的 BufferLength 做包大小下限校验，再按字节整包拷出。vhidmini2 的 util.c（`RequestGetHidXferPacket_ToRead/WriteToDevice`）就是这么写的，移植时逐行对照即可，不要"按理解重写"。

### 坑 ④：数字键 usage 错位一位

`'1'..'9'` 的 Key Codes usage 是 `0x1E..0x26`，`'0'` 是 `0x27`——不是连续的 `0x1E + (c - '0')`。初版恰是后者：按 `0` 出来的是 `1`，`1`–`9` 整体偏移 +1，而且被一个"把错误当期望值"的单测固化了。修复与回归：

`rust
// 数字行：'1'..'9' → 0x1E..0x26；'0' 特判为 0x27
let usage = if c == '0' { 0x27 } else { 0x1E + (c as u8 - b'1') };
`

同族问题还有键盘描述符 Usage Max 从 0x65 扩到 0x73（F13–F24），并用"注入侧全部 usage 不得超过描述符 Usage Max"的单测锁死（`report.rs` 测试 `all_key_usages_within_descriptor_range`）。

### 坑 ⑤：自制自旋锁在 DISPATCH_LEVEL 的同 CPU 死锁窗口

初版用手工 `KSPIN_LOCK` 保护全局状态。自旋锁不可重入：任何"持锁状态下再次获取同一把锁"的路径，在单 CPU 上就是永久自旋；而这类驱动的回调本就可运行在 DISPATCH_LEVEL，手工管理 IRQL 提升/恢复（OldIrql 的保存与跨分支还原）又平添出错面。审查发现的正是这样的死锁窗口，修复改用 **WdfSpinLock**：IRQL 提升与恢复由框架接管，并把临界区收窄到"纯内存读写 + `WdfIoQueueRetrieveNextRequest`"（后者在 DISPATCH_LEVEL 调用合法），锁本身在 `DriverEntry` 单线程阶段创建、之后只读（ 的 SAFETY 注释完整记录了这条推理）。

### 坑 ⑥：IOCTL 常量"编个号就行"？功能码与 Method 位都要溯源

初版自造了一套功能码（ATTRIBUTES=4、STRING=5、FEATURE=6……），而真实 hidport.h 二十多年未变的编号是 0/1/2/3/4/7/8/9/10（5/6 保留）——4/5 恰好互换，6–9 段根本不存在，`GET_DEVICE_ATTRIBUTES` 落进默认死臂导致 `HidD_GetAttributes` 必失败；feature/报表类 IOCTL 则定义在另一个头文件 hidclass.h（`HID_IN/OUT_CTL_CODE`，功能码与用户态 `HidD_*` 同号），Method 位是 IN/OUT_DIRECT 而非 NEITHER，初版注释还把 `METHOD_IN_DIRECT=1` 写成了 0。更隐蔽的是，连审查意见给的"正确值"也得再溯源一次（曾有意见把自造的功能码 7 当权威，算出的 0xB001D 实为 ACTIVATE_DEVICE）。最终这组常量以三方对照（2003 DDK 头文件 + 现行 WDK + vhidmini2 源码）校直，并写进宿主可跑的单测：每个 IOCTL 断言 `CTL_CODE(...)` 等式与字面值。

## 7. 构建与安装

前置：Visual Studio 2022（C++ 桌面开发负载）+ WDK 10.0.26100 + LLVM（bindgen 需要 libclang，注意 bindgen 0.71 与 libclang 22 不兼容，用 pip 装的 libclang 18 并把 `LIBCLANG_PATH` 指到它）+ Rust stable MSVC 工具链。驱动是独立 workspace：

`powershell
cd crates\vdev-hid-win\kernel
cargo build --release # 产出 vdev_hid.dll（打包时拷为 vdev_hid.sys）
`

打包与签名走 `scripts/stage-sign-hid.ps1`：拷贝 sys+INF 到 `target\dist`，signtool 签 sys，Inf2Cat 生成 `vdev-hid.cat` 再签名。内核驱动安装需要签名可用：要么开测试签名（`bcdedit /set testsigning on` 后重启，当前仓库文档如实标注实测依赖此步），要么自签证书装入 TrustedPublisher/Root。安装/注入（CLI 会自动走 UAC 提权重启并回传真实退出码）：

`powershell
vdev-hid-win kernel install # 需管理员；默认找 exe 同目录的 inf/sys
vdev-hid-win kernel status
vdev-hid-win kernel key a # 注入按键 a（tap：按下 10ms 后抬起）
vdev-hid-win kernel key ctrl --action down
vdev-hid-win kernel mouse move 20 0
vdev-hid-win kernel mouse click # 左键点击
vdev-hid-win kernel mouse wheel 120 # 滚轮向上
vdev-hid-win kernel uninstall
`

安装过程（`src/kernel.rs` 的 `install`）用纯 SetupAPI 完成：清理残留节点 → `SetupDiGetINFClassW` 取设备类 → 依次创建键盘、鼠标两个根枚举节点并 `DIF_REGISTERDEVICE` → `DiInstallDriverW` 装入驱动存储。曾有一个配套 bug：初版只建键盘节点，INF 里的鼠标安装节永远匹配不上，鼠标设备"从不存在"；HWID 匹配也必须逐段精确比较——`Root\vdev-hid` 是 `Root\vdev-hid-mouse` 的前缀，子串匹配会把鼠标误配成键盘（ 的 `hwid_matches` 有完整注释与回归）。

## 8. 现状与局限

- **状态：已真机验证通过（2026-09-14，Win10 19045 x64 + 测试签名）。** 设备管理器 HID 类出现「vdev 虚拟键盘」（`Root\vdev-hid`）与「vdev 虚拟鼠标」（`Root\vdev-hid-mouse`）两个 `CM_PROB_NONE` 节点；实弹注入逐项生效（字符、修饰键、Ctrl+A、鼠标相对移动/点击/滚轮）。构建、clippy、fmt 全绿；IOCTL 常量/描述符字节/键码映射/报告布局有 15 个宿主单测守护（契约层 windows-free，macOS 上即可运行）。
- **功能边界**：无 feature 报告（`GET/SET_FEATURE` 返回 `STATUS_NOT_SUPPORTED` 而非伪成功）；不实现 HID 字符串（由 INF/devnode 提供）；键盘一次至多 6 键 + 8 修饰位；鼠标只有相对移动（±127）与滚轮（120 的倍数、实际写入 ±127），无绝对坐标模式。
- **工程取舍**：panic 处理器直接 `KeBugCheckEx(0xE2)` 留蓝屏证据而不是自旋挂死（内核 no_std 无法 unwind，）；`HID_DESCRIPTOR` 的 packed 布局断言只能在 Windows 侧编译验证（wdk-build 拒绝非 Windows 主机），契约层则拆出 `contract.rs`/`report.rs` 两个 windows-free 模块让宿主单测真正触达——用户态与内核经 `#[path]` 共享同一份契约定义，单一事实来源。

## 9. 写在最后：三重契约 + 一条选型约束

回看这轮修复，所有问题可以归约为三类契约，本文称之为 **minidriver 三重契约**：

1. **INF 接线**——`Include/Needs` 把 hidclass 与 mshidkmdf 接进驱动栈、`AddService` flag 0 让关联服务唯一、`AddFilter + FilterPosition=Lower` 挂对层级。接错了，代码再对也是死代码；
2. **IOCTL 契约**——功能码、Method 位、缓冲区槽位（谁放 `UserBuffer`、谁在 `SystemBuffer`）逐条对照头文件与官方样例，数值必须可溯源、可断言；
3. **结构布局**——一切跨内核边界的线格式结构体（`HID_DESCRIPTOR`、`HID_XFER_PACKET`、`HID_DEVICE_ATTRIBUTES`）以 WDK 头文件的 pack 语义为准，用编译期断言钉死 `size_of`。

方法论只有一句话：**移植内核驱动以官方样例（vhidmini2）与 WDK 头文件为唯一权威，逐字段对照，禁止按理解重写**；数值类修复做三角验证（双代头文件 + 官方样例源码，必要时加 ReactOS 实现），审查意见与记忆给出的值一律重新溯源。以及验收标准：设备管理器出现节点只是入场券，链路验收必须走到"HidD 枚举可见 + WriteFile 注入生效"。

仓库地址：gqf2008/vdev，本文涉及代码集中在 `crates/vdev-hid-win`（内核驱动 `kernel/driver`、CLI `src/`、INF 与签名脚本）。系列其余八篇（macOS 摄像头/声卡/键鼠/虚拟屏、Windows 摄像头/显示器/声卡、AI 虚拟麦克风）见仓库 docs 目录。

---

**关于 vdev**：一个用 Rust 造虚拟设备的开源项目（摄像头 / 显示器 / 声卡 / HID，macOS + Windows 双栈），本系列共 9 篇，全部基于仓库真实代码与真实排障记录。

- 项目地址：**github.com/gqf2008/vdev**（点击文末"阅读原文"）
- 系列总目录与其余篇目：仓库 `docs/community/`

如果这篇帮你少踩一个坑，欢迎到仓库点个 star。
