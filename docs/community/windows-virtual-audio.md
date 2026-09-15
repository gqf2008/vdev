# 用 Rust 把虚拟声卡写进 Windows 内核：PortCls/WaveRT 实战与蓝屏级踩坑实录

> 本文是 [vdev](https://github.com/gqf2008/vdev) 虚拟设备驱动开发系列之一。全套含 macOS 摄像头/声卡/键鼠/虚拟屏与 Windows 摄像头/显示器/声卡/HID 九篇。
> 代码引用约定：本文所有 `文件:行号` 均相对**仓库根**（如 `crates/.../foo.rs:12`），行号为写作时基线；代码演进后行号会漂移，按符号名搜索为准。

## 一、虚拟声卡的两条路线：为什么我们选了最难走的那条

想在 Windows 上"凭空"多出一张声卡（把系统播放环回成麦克风输入，供会议/录制软件抓取），大致三条路：

1. **用户态虚拟设备**：Windows 对音频没有这个选项。摄像头有 Media Foundation 虚拟摄像头（vdev 系列的 `vdev-camera-win` 走的正是用户态 DirectShow 源过滤器，完全免签名），显示器有 IddCx 的 UMDF 用户态驱动路线，唯独声卡和内核 HID 没有官方用户态捷径。
2. **AVStream**：通用内核流框架，什么媒体类型都能做，但音频要自己把 KS 语义、时钟、DMA 全部拼起来，音频栈的胶水代码量反而更大。
3. **PortCls/WaveRT**：微软为音频定制的端口类（port class）驱动模型。系统自带 `portcls.sys`，负责 KS 自动化、格式协商、位置跟踪这些"普通话"，驱动只写一个 miniport（小端口）回答"我有什么 pin、什么格式、缓冲区在哪"。现代 WDM 音频驱动的事实标准，微软官方样例 sysvad 就是它。

vdev 的 `vdev-audio-win` 选了第三条。语义上等同 macOS 侧的自研 BlackHole：虚拟扬声器（render 端点）把系统播放写入环形缓冲，虚拟麦克风（capture 端点）从同一块缓冲读出，**输出环回输入**。

内核路线的成本要提前认清：任何一次野指针、池越界、错误的 IRQL 假设，代价都不是段错误而是 **BSOD**；内核环境没有 CRT、没有堆抽象、不能 panic 展开，大量用户态习得的"防御性编程"在这里直接失效。本文第 5 节的六个案例，全部是独立审查在这个驱动里真实抓出来的 blocker——修复前的代码，以当时状态加载几乎必蓝屏。

顺带澄清一个仓库里的措辞：本文早期草稿沿用设计文档把它称作 "KMDF 内核驱动"，但代码实际**没有用任何 WDF 框架**——`driver/src/lib.rs` 的模块注释写得很清楚：手写精简绑定路线，跳过 WDF 的函数表机制，驱动入口直接调 PortCls 的初始化/子设备注册函数。它是一个纯 WDM 风格的 PortCls 小端口驱动。

## 二、PortCls/WaveRT 最小知识

读懂这个驱动只需要五个概念：

**1. 适配器驱动骨架。** 驱动的 `DriverEntry` 不自己填 `DRIVER_OBJECT`，而是交给 PortCls：

```rust
// crates/vdev-audio-win/driver/src/lib.rs:132-142
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PUNICODE_STRING,
) -> NTSTATUS {
    crate::kdbg!("DriverEntry\n");
    unsafe { PcInitializeAdapterDriver(driver_object, registry_path, Some(add_device)) }
}
```

PnP 子系统为设备调 `AddDevice`，我们在里面用 `PcAddAdapterDevice` 注册 StartDevice 回调，并声明子设备对象数上限——本驱动注册 4 个子设备（Wave 捕获/渲染 + Topology 捕获/渲染），所以 `MAX_SUBDEVICES = 4`（crates/vdev-audio-win/driver/src/lib.rs:67）。这个数必须覆盖**全部**子设备，槽位耗尽后 `PcRegisterSubdevice` 会失败。

**2. 端口 + 小端口 = 子设备。** StartDevice 里按 sysvad 的 `InstallDevice` 顺序组装：`PcNewPort(&CLSID_PortWaveRT)` 创建端口对象 → 创建自己的 miniport COM 对象 → 调端口的 `Init`（vtable 第 4 槽）把两者绑在一起 → `PcRegisterSubdevice` 以名字（`WaveRender-0`、`WaveCapture-0`、`TopologyRender-0`、`TopologyCapture-0`）注册成 KS filter。名字必须与 INF 里 `AddInterface=%KSCATEGORY_*%,%KSNAME_*%` 的模板逐字节一致——仓库里有一个宿主单测专门盯这件事（`crates/vdev-audio-win/driver/src/endpoint_names.rs:53–108`）。

**3. 过滤器描述符是声明式自描述。** PortCls 启动时会调 miniport 的 `GetDescription`，拿一张 `PCFILTER_DESCRIPTOR`：有哪些 pin、数据方向（`KSPIN_DATAFLOW_IN/OUT`）、通信语义（SINK/SOURCE）、pin 类别 GUID（`KSNODETYPE_SPEAKER`/`KSNODETYPE_MICROPHONE`）、支持的格式范围。渲染 pin 声明成 `DataFlow=OUT / Communication=SINK / Category=KSNODETYPE_SPEAKER`，捕获 pin 反之（`crates/vdev-audio-win/driver/src/miniport.rs:836–875`）。

**4. WaveRT 是"实时"轮转缓冲。** 传统 WavePci 每个小数据包都触发内核/用户态往返；WaveRT 则由 miniport 分配一段物理连续的 DMA 环形缓冲，把用户态地址直接映射给音频引擎（AudioEng.exe），引擎自己搬数据，内核不再经手每一段波形。miniport 只需回答三件事：分配缓冲（`AllocateAudioBuffer`）、硬件延迟、以及——最微妙的——**当前播放位置**。

**5. GetPosition 契约。** `IMiniportWaveRTStream::GetPosition` 返回的 `PlayOffset/WriteOffset` 必须是**环形缓冲内的偏移**（对 `dma_size` 取模），而不是自流开始以来的累计字节数。没有硬件位置寄存器的虚拟设备，应让 `GetPositionRegister` 返回 `STATUS_NOT_SUPPORTED`（0xC00000BB），PortCls 便会退回"按通知周期反复轮询 GetPosition"的模式。这五个字节的契约，后面会看到我们怎样把它违反了两次。

## 三、用 Rust 写内核驱动：构建形态与绑定方法论

**双模式 crate。** 整个驱动是一个 crate，靠 feature 切换两种形态（`crates/vdev-audio-win/driver/src/lib.rs:1`）：

```rust
#![cfg_attr(feature = "kernel", no_std)]
```

- 默认（无 `kernel`）：普通 cdylib，纯逻辑模块（环形缓冲、位置数学、拓扑描述符、端点常量）无条件编译，`cargo test` 在 macOS 宿主就能跑；
- `--features kernel`：`no_std` 生效，链接 `ntoskrnl`/`portcls`/`ks` 等，产出真正的 `.sys`。

链接脚本在 `driver/build.rs`：`/ENTRY:DriverEntry`、`/SUBSYSTEM:NATIVE`、`/DRIVER:WDM`，并用三个 `/NODEFAULTLIB` 排除 `libcmt`/`libucrt`/`libvcruntime`——内核没有用户态 CRT。同时开 `/INTEGRITYCHECK` 强制 PE 校验和（内核映像加载要求）。workspace 的 dev/release profile 都设了 `panic = "abort"`；`no_std` 下还要自己提供 panic handler，本驱动的选择很直接（crates/vdev-audio-win/driver/src/lib.rs:110–119）：**直接** `KeBugCheckEx(0xDEAD_DEAD, …)`——内核里 panic 就该干脆地蓝屏，而不是把系统挂在未定义状态（`kdbg!` 默认编译为空，不要把「会先落日志」当既成事实）。

关于"要不要自己补运行时符号"：这里要如实说——**本驱动没有提供 `memcmp/memcpy` 之类的 shim**。`no_std` + `panic=abort` 之后，core 库剩下的链接期依赖只有一个：一个返回 0 的空函数 `__CxxFrameHandler3`（crates/vdev-audio-win/driver/src/lib.rs:122–126），用于满足链接器对 MSVC 异常处理器的引用。（同一仓库的 display 驱动在 bindgen 场景确实踩过 `memcmp/memcpy` 签名校验的坑，那是另一条链路的故事。）

**绑定全部手写，与 WDK 头文件逐字段对照。** `driver/src/sys/` 下是手写的 WDM/PortCls/KS 绑定子集：`types.rs`（基础类型 + KS/PC 描述符）、`portcls.rs`（PortCls 导出函数与 IID/CLSID）、`mem.rs`（`ExAllocatePool2`/`ExFreePoolWithTag` 薄封装）、`log.rs`（默认静默的 `kdbg!` 宏，仅接受字面量消息，规避 varargs ABI）。方法论写在 `topology.rs` 文件头：vtable 槽序对照 WDK `portcls.h` 行号、GUID 值逐条从 `ksmedia.h`/`ks.h` 抄录并在旁注明行号、结构布局推演 x64 尺寸后用 `#[cfg(test)]` 的 `size_of!/offset_of!` 断言钉死。宿主跑不了内核代码，但**布局断言跑得动**——这是 Rust 内核驱动在非 Windows 开发机上仅有的几种验证手段之一（另两种是编译期检查和纯逻辑抽离单测）。

## 四、核心机制解析

### 4.1 SPSC 环形缓冲：两个单调索引，计数只派生不存储

扬声器和麦克风两个流共享一块非分页池（`ExAllocatePool2`，flags `0x40` = `POOL_FLAG_NON_PAGED`——`GetPosition` 会在 `DISPATCH_LEVEL` 被轮询，任何分页内存都是禁忌）。缓冲本身是无锁 SPSC：裸指针数据区 + `read`/`write` 两个 `AtomicUsize` **单调索引**（存取时对 capacity 取模），`count` 不再是独立原子量、而是由 `write - read` 派生（`crates/vdev-audio-win/driver/src/ringbuffer.rs:25–32`）。`GetPosition` 内无锁、无分页内存、无阻塞调用，DISPATCH_LEVEL 合规。

之所以改成这个形态，是 2026-09-15 的复审推翻了本文此前「单写单读 + 原子 RMW 即可」的结论：渲染流满载时 `write_drop_oldest` 要从**写者线程**推进 read 索引腾位，于是 read 有了两个写者；两端各自 load-compute-store 互相覆盖，read 会**回退**，并与独立存储的 `count` 永久脱同步（`fetch_sub` 还会让 count 下溢回绕）——不是丢帧，是环回流的结构性损坏。修复后的纪律是：**read 索引两端都可以推进，但一律 `fetch_max` 单调递增、绝不回退**（读者正常读、写者满载丢最旧，`crates/vdev-audio-win/driver/src/ringbuffer.rs:124,147`）；`count` 由索引派生，从根上消灭「索引与计数脱同步」这一类错误，`read <= write` 恒成立。数据面仍是尽力而为的环回链路（满载丢最旧的窗口内读者可能拷到写者正在覆盖的字节，个别撕裂帧），但索引/计数永不错位。两个并发回归测试把这个不变式钉死：无压力并发读写 20 万样本逐字节保序、持续满载丢最旧压力下不变式始终成立（`crates/vdev-audio-win/driver/src/ringbuffer.rs:332–429`）。

### 4.2 时间锚定的 GetPosition：sysvad 同款

虚拟设备没有硬件位置寄存器，位置必须"演"出来。做法与 sysvad 一致：进入 `KSSTATE_RUN` 时记一个 `KeQueryPerformanceCounter` 时间锚点 + 位置锚点；每次被轮询，把流逝的 tick 换算成应推进的字节数，**只处理 `[last_processed, target)` 这一段 DMA 窗口**，并且单次推进不超过一个 DMA 周期（防止引擎久未查询时一次搬爆，`crates/vdev-audio-win/driver/src/miniport.rs:298–344`）。

时间→字节、环跨度切分、位置取模这三块纯数学被抽进 `position.rs`，该模块不做任何内核调用、无条件编译——于是这组最容易出现 off-by-one 的公式反而在 macOS 宿主上有完整单测兜底：

```rust
// crates/vdev-audio-win/driver/src/position.rs:54-62
pub const fn split_ring_span(start: usize, n: usize, size: usize) -> (usize, usize) {
    if size == 0 {
        return (0, 0);
    }
    let off = start % size;
    let first = if size - off < n { size - off } else { n };
    (off, first)
}
```

欠载/过载策略也在这里：捕获侧读不足就补零（`read_zero_fill`，否则 DMA 尾部残留旧数据，麦克风会循环回放陈旧音频）；渲染侧满载就丢最旧数据（`write_drop_oldest`，丢新数据会让渲染时钟停滞）。

**2026-09-16 补：时间→字节之后还要向下对齐到整帧。** `bytes_for_interval()` 按 QPC 换算出的字节数是**任意整数**（1 ms @ 192000 B/s = 192，可是 7.3 ms = 1401），而 DMA 缓冲与环形缓冲的读写栅格是 `block_align`（16bit 立体声 = 4 字节）。直接拿这个数推进 `last_processed`，`last_processed % dma_size`（DMA 缓冲偏移）与环形缓冲读写索引就带上了一字节相位：引擎按帧解码时高/低字节互换。症状极隐蔽——注入 1 kHz 正弦，环回连跑几轮后从 −6.02 dBFS 翻到满幅垃圾、主频变 19 kHz，而约一半轮次是好的（0.5 ms 的换算量恰好是 4 的倍数）。修法是推进前用 `frame_aligned_advance()` 向下取整到整帧（余数由 QPC 锚点下一轮补齐，不累积漂移）。

> 教训：凡是“按时间算字节、再当索引用”的地方，都要问一句“这是帧的整数倍吗”——字节地址空间上的缓冲区不等分成帧。

### 4.3 IMiniportTopology：把端点真正"装"进系统

这是审查抓出的第 7 个 blocker 之外、但同样致命的一课：**只有 Wave 子设备时，设备管理器里能看到声卡，控制面板里却没有播放/录音设备**。Windows 音频端点（AudioEndpointBuilder/WASAPI 可枚举的那层）需要每个端点配一个 topology filter，把 Wave 的桥接 pin 连到音量/静音终端节点，音频栈才能建成端点。

所以驱动实现了完整的 `IMiniportTopology`（`topology.rs`，约千行）。**渲染与采集的拓扑形状不一样，且必须以同机可用设备的实测为准**：

- **渲染拓扑直通**：`NodeCount=0` + 一条 pin→pin 连接（对照 sysvad `speakertoptable.h` 的 `NodeCount=0`，以及同机第三方虚拟声卡实测——`KSPROPERTY_TOPOLOGY_NODES` 返回空、`KSPROPERTY_TOPOLOGY_CONNECTIONS` 只 1 条）。渲染侧的音量/静音由音频引擎在软件混音层处理，驱动不需要替它做节点；
- **采集拓扑串音量/静音**：`NodeCount=2`、3 条连接（`PCFILTER_NODE → volume → mute → PCFILTER_NODE`），节点挂 `KSPROPERTY_AUDIO_VOLUMELEVEL`（Id=4）与 `KSPROPERTY_AUDIO_MUTE`（Id=13）各一项，共用一个属性处理器，按 `Verb` 分派 GET/SET/BASICSUPPORT 三路（BASICSUPPORT 按 KS 协议两级应答：40 字节出完整 `KSPROPERTY_DESCRIPTION`，4 字节只出 AccessFlags）。音量/静音值用 `AtomicI32` 存内存——属性语义是真的，DSP 效果是假的，这对虚拟声卡刚好够用。

> 这一条是**装机之后**才纠过来的：第一版按 sysvad 的"节点中转"形状写渲染拓扑（经 `KSNODETYPE_AUDIO_ENGINE` 节点），端点能枚举但打不开（见案例七～十一）。把渲染拓扑改成直通、`NodeCount` 归零之后，端点才真正可用。

最后用 `PcRegisterPhysicalConnection` 把 wave↔topology 两对桥接 pin 接成物理连接（渲染路径 wave→topology，捕获路径 topology→wave），任一步失败沿注册逆序 teardown：先 `IUnregisterPhysicalConnection` 解除物理连接，再 `IUnregisterSubdevice` 注销子设备，否则已注册的 port 反向引用即将被释放的适配器内存，是一个窗口期 UAF（`crates/vdev-audio-win/driver/src/adapter.rs:529–595；失败路径 teardown 在 458–510`）。

## 五、踩坑实录：六个蓝屏级案例

以下是独立审查在这个驱动里抓出的真实缺陷（修复前的代码），每一个单独拎出来都足以让驱动在设备启动或首次流式传输时 BSOD。它们有很强的共性：**编译期零报错，错误只在内核里兑现**。

### 案例一：环形缓冲结构体存在栈帧上——DISPATCH_LEVEL 一碰就蓝屏

最初 `RingBuffer::new` 按值返回结构体，初始化代码把它存在 `adapter_init` 的局部变量里，再把**这个栈变量的地址**塞进适配器：

```rust
// 修复前的样子（adapter.rs）
let ring = RingBuffer::new(ring_mem.cast(), RING_SIZE); // 值，存于栈局部变量
(*this).ring = &ring as *const RingBuffer as *mut RingBuffer; // 栈帧地址！
```

`adapter_init` 一返回，这个指针就是悬垂的。三重后果：`stream_get_position` 在 DISPATCH_LEVEL 经它读写已被复用的内核栈；释放路径对悬垂地址 `ExFreePoolWithTag` 造成池损坏 bugcheck；真正的 1 MB 池分配永久泄漏。

修法（`crates/vdev-audio-win/driver/src/adapter.rs:168–197`）：`RingBuffer` 结构体和数据区合并成**一次**池分配——分配 `size_of::<RingBuffer>() + 1MB`，`ptr::write` 把结构体放置到池基址，数据区紧随其后，释放时只 free 唯一的池指针。教训：内核里任何"会被别的执行流摸到"的对象，生命周期必须挂到池上，栈上构造 + 存地址是标准送命姿势。

### 案例二：回绕首段长度公式写反——现有单测全从 0 起写，所以漏网

环形缓冲写入要切两段：首段从当前写位置到缓冲末尾。最初写成了：

```rust
let first = (w % self.capacity).min(n);      // 错：首段是"当前偏移"
let first = (self.capacity - w % self.capacity).min(n); // 对：到末尾的剩余空间
```

前者在 offset < capacity/2 时不越界但写错位置（静默数据损坏），跨尾界时直接 slice 越界 panic——内核态就是 `KeBugCheckEx`。最扎心的是测试：修复前仅有的 3 个单测**全部从 offset 0 或恰好 capacity/2 起写**，这个 bug 在宿主上零触发，是审查者用同逻辑写 PoC（capacity=8、offset=6、写 4 字节）实证的。

修复后补了三类用例（`crates/vdev-audio-win/driver/src/ringbuffer.rs:205–263`）：写跨尾界（尾 5 写 6 → 5+1 切分）、读跨尾界、整圈回绕；再加一个 4096 步的 XorShift 随机性质测试，环形实现逐字节比对线性参考实现（`crates/vdev-audio-win/driver/src/ringbuffer.rs:293–327`）。教训：**回绕逻辑的测试必须从非零偏移、跨尾界的形态开始写**，全零起点的用例天然抓不到这类错。

### 案例三：COM vtable 少一级间接——调用即野跳转

`IPortWaveRTStream` 是 PortCls 侧的 COM 对象。调用它分配 DMA 页时，最初写成了：

```rust
// 错：把对象本体当 vtable
let vtbl = &*(ps.port_stream as *const c_void as *const IPortWaveRTStreamVtbl);
// 对：COM 对象首字段才是 vtable 指针，必须双重间接
let vtbl: &IPortWaveRTStreamVtbl =
    unsafe { &*(*(ps.port_stream as *const *const IPortWaveRTStreamVtbl)) };
```

错的那版把 PortCls 流对象的内部数据当函数指针表，`allocate_pages_for_mdl`（vtable 偏移 24）读到一个数据值就"调用"了它——跳任意地址。讽刺的是同一仓库 `crates/vdev-audio-win/driver/src/adapter.rs:380–390` 里对 `IPort` 的写法是正确的双重间接，两处对照才让问题显形。COM 是"指针的指针"这件事，在 Rust 里没有类型系统替你兜底，只能靠纪律。

### 案例四：GetPosition 的双重契约违反

初版 `stream_get_position` 同时违反了第 2 节说的两条契约：每次被轮询都把**整个** DMA 缓冲搬运一遍（PortCls 按通知周期反复轮询，等于每个周期重复拷贝同一份数据），并把返回位置累加整缓冲长度、**不取模**——第二个缓冲周期后返回的偏移就越界了，PortCls 的位置跟踪随之错乱。

修复后的形态（`crates/vdev-audio-win/driver/src/miniport.rs:292–344`）：QPC 时间锚点换算目标位置 → 只处理 `[last_processed, target)` 窗口 → `PlayOffset/WriteOffset` 一律 `position % dma_size`。这与 macOS 侧踩过的"GetZeroTimeStamp 必须锚定真实流逝时间"是同一族坑：**虚拟设备的时钟不能靠"每次被问就推一段"来演，必须锚定物理时间**，否则轮询频率一变，时钟就跟着变。

### 案例五：NTSTATUS 手算十进制错——格式协商静默死亡

绑定层的 NTSTATUS 常量最初是手算的十进制补码值，7 个常量错了 5 个。最致命的一个：

| 常量 | 注释声称 | 实际写成的值 | 正确值 |
|---|---|---|---|
| `STATUS_BUFFER_TOO_SMALL` | 0xC0000023 | **0xC000000D** | 0xC0000023 |

`STATUS_BUFFER_TOO_SMALL` 是 KS 数据交集协议里"第一次给 NULL 缓冲查尺寸"的**约定返回值**（本驱动 `miniport_data_range_intersection` 就靠它走两段式查询，`crates/vdev-audio-win/driver/src/miniport.rs:619–624`）。写成 0xC000000D（恰好是 `STATUS_INVALID_PARAMETER` 的值），PortCls 把尺寸查询当硬错误——pin 格式协商直接失败，而且没有任何日志，流就是建不起来。

修法分三层（`crates/vdev-audio-win/driver/src/sys/types.rs:15–35`）：一律改十六进制字面量直书后转型（`0xC000_0023u32 as i32`，"回绕即本意"），彻底消灭手算十进制；`const _: () = assert!(...)` 编译期钉死"错误级 NTSTATUS 转 i32 必为负"这一失败判据依赖的不变式；再加一个逐常量比对官方值的回归测试。教训浓缩成一句：**常量值永远抄 hex，不要心算补码**。

> **可复核性说明**：本案例「最初手算、7 个错 5 个」是对排障时原始错误值的描述；该版本已被后续修复覆盖，当前仓库历史中查不到，无法逐值复核。**可复核的是当前代码**——值一律十六进制直书，且有编译期断言 + 逐常量比对官方值的回归测试。本案例请按「当时的排障记录」而非「可复现证据」阅读。

### 案例六：KS 描述符凭记忆写布局——PortCls 读 Category 即野指针

这组坑和案例五同根，是手写 FFI 的系统性风险。最初的 `KSPIN_DESCRIPTOR` 凭记忆书写：
> **可复核性说明**：这段「初版结构体字段写错」的细节（漏字段、按值/按指针混淆、杜撰字段）同样来自当时排障记录，原始定义已不可考。**可复核的是修复后的状态**：`size_of!`/`offset_of!` 断言与 `DEFINE_GUID` 逐字节比对都在测试里且能跑。
漏了 `DataFlow` 字段、`Category`/`Name` 按值内嵌 GUID（真布局是 `*const GUID` 指针）、字段顺序错位。PortCls 按真 ABI 在固定偏移读 `Category` 指针时，读到的是 GUID 值的前 8 字节——`KSCATEGORY_AUDIO` 开头是 `0x6994AD04`，一个非规范地址，解引用即蓝屏。`PCPIN_DESCRIPTOR` 的层次也写错：真结构是三个实例计数 + 自动化表 + **按值内嵌**的 `KSPIN_DESCRIPTOR`，初版写成了指针。`PCFILTER_DESCRIPTOR` 更是杜撰出了不存在的 `ConnectionSize` 和尾部 5 个 GUID 字段。

另外同一批还有一处"抄写错"：`IID_IMiniport`/`IID_IPort` 曾被错写成完全不相干的 GUID 值，PortCls 以真 IID 查询会拿到 `E_NOINTERFACE`——修复后测试直接把 WDK 头文件里的 `DEFINE_GUID` 展开成 16 字节数组逐字节比对（`crates/vdev-audio-win/driver/src/sys/portcls.rs:281–306`）。连音频主格式 GUID `KSDATAFORMAT_TYPE_AUDIO`（`73647561-…`，即小端的 `'aud '`）都曾是个杜撰值——格式协商从第一字节的字节序起就得对。

修法没有捷径：下载对应版本 WDK 的 `ks.h`/`ksmedia.h`/`portcls.h`/`ntstatus.h`，逐字段推演 x64 布局（ULONG 4B、指针 8B、C 枚举 4B、匿名联合写全变体撑起真实尺寸——`KSPIN_DESCRIPTOR` 尾部那个联合的第二变体恰好把结构撑到 88 字节，省略它整体短 16 字节会被 `GetDescription` 拒收，`crates/vdev-audio-win/driver/src/sys/types.rs:204–223`），重写后每个结构配 `size_of!`/`offset_of!` 断言：

```rust
// crates/vdev-audio-win/driver/src/sys/types.rs:505-518（节选）
#[test]
fn ks_pin_descriptor_layout_x64() {
    assert_eq!(size_of::<KSPIN_DESCRIPTOR_TAIL>(), 16);
    assert_eq!(size_of::<KSPIN_DESCRIPTOR>(), 88);
    assert_eq!(offset_of!(KSPIN_DESCRIPTOR, DataFlow), 48);
    assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Category), 56);
    assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Reserved), 72);
}
```

这一族教训在仓库全局规则里已经沉淀成一句话：**内核/COM ABI 结构与常量，权威只有 WDK 头文件；一律"头文件 + 布局断言"双保险，禁止凭记忆**。

顺带一提同批修复的两个小案例：`PcAddAdapterDevice` 绑定少写了一个参数，x64 调用约定下第 6 参从栈上未初始化槽位读入野值（对照 `portcls.h` 补全 5 参签名，`crates/vdev-audio-win/driver/src/sys/portcls.rs:130–143`）；无硬件位置寄存器时 `GetPositionRegister` 曾返回 `STATUS_SUCCESS` + 空寄存器，可能诱使 PortCls 进入寄存器模式对地址 0 做 MMIO 读——改为返回 `STATUS_NOT_SUPPORTED` 退回轮询模式（`crates/vdev-audio-win/driver/src/miniport.rs:427–441`）。

## 六、装机之后：五个"能枚举却打不开"的运行时缺陷

上一批是静态审查抓出来的，这一批正好相反：**门禁全绿、设备管理器里有声卡、控制面板里两个端点都在，`IMMDevice::Activate` 也成功——但第一个真正要打开 KS pin 的调用就失败**。它们只在真机上暴露，靠三件事定位：逐调用探测、跟同机可用设备做 KS 属性对照、必要时扫第三方驱动的二进制描述符（方法见本章末）。

### 案例七：wave 开流 pin 没暴露 WaveRT 的环回流接口

**现象**：`IAudioClient::Activate` 返回 `S_OK`，紧接着的 `GetDevicePeriod` 返回 `0x80070491`（`ERROR_NO_MATCH`，KS 的 `STATUS_NO_MATCH` 映射），之后 `GetMixFormat` / `IsFormatSupported` / `Initialize`（共享与独占）全部失败。同机 ToDesk/Realtek 端点同一步是 `hr=0`（`default=100000, min=30000`）。

**根因**：`KSPROPERTY_PIN_INTERFACES` 返回的是 `KSINTERFACE_STANDARD_STREAMING(0)`，而 audiodg 打开 WaveRT 端点时按 `KSPIN_INTERFACE{KSINTERFACESETID_Standard, KSINTERFACE_STANDARD_LOOPED_STREAMING}` 选接口——选不到就 `STATUS_NO_MATCH`。对照：同机第三方虚拟声卡该属性实测 `id=1`；sysvad/VDA 干脆不声明 `Interfaces`（`InterfacesCount=0`），由 PortCls 补默认集（默认集里含 LOOPED_STREAMING）。

**修法**：`PIN_INTERFACES` 的 `Id` 改成 `KSINTERFACE_STANDARD_LOOPED_STREAMING`（`crates/vdev-audio-win/driver/src/miniport.rs`）。

### 案例八：数据范围缺"音频信号处理模式"属性列表

Win10 的音频引擎给端点定"设备格式/混音格式"时，会按**信号处理模式**匹配数据范围；范围上没有模式属性列表就匹配不到任何 (模式, 格式) 组合。参考实现（sysvad `endpoints.h::PinDataRangeSignalProcessingModeAttribute`）的做法是：

```c
// 数据范围本身置 KSDATARANGE_ATTRIBUTES，且 DataRanges 指针数组为 [范围, KSATTRIBUTE_LIST]
static KSATTRIBUTE PinDataRangeSignalProcessingModeAttribute =
    { sizeof(KSATTRIBUTE), 0, STATICGUIDOF(KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE) };
```

我们补齐了 `KSDATARANGE_ATTRIBUTES(2)` + 一条 `KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE`（`E1F89EB5-…`）属性，并把 `DataRangesCount` 从 1 改成 2。

### 案例九：`KSPROPERTY_PIN_PROPOSEDATAFORMAT` 的 SET 被当成"照单全收"

**现象**：引擎对这个属性的 **SET 调用计数一路涨到 1600+ 次**（自定义只读计数器实测 `prop_set=1624`），却始终不调 `NewStream`（`new_stream=0`）。

**根因**：参考实现（sysvad `PropertyHandlerProposedFormat → IsFormatSupported`）里，SET 是**校验语义**——按驱动自己的设备格式表回状态：支持回 `STATUS_SUCCESS`、不支持回 `STATUS_NO_MATCH`，**不回填缓冲**。我们第一版为了"别让引擎判定设备不可用"，对任何 PCM/EXTENSIBLE 提案都回成功、还把设备格式写回缓冲，等于对每个候选格式都说"行"。

**修法**：改成校验语义（只回状态），并按实例里的属性列表校验请求的模式。改完这一批提案**直接归零**（`prop_set=0`）。

### 案例十：WaveRT 的格式随 `NewStream` 传入，stream `SetFormat` 根本不会被调

**现象**：端点已经能开流（`Initialize`、`Start` 全部 `S_OK`），但**不出声**：渲染侧 `GetCurrentPadding` 恒等于缓冲满值（实测 48000 不变）、采集侧 `GetNextPacketSize` 恒为 0。计数器现场很直白：`new_stream=17` 而 `set_format=0`。

**根因**：WaveRT 模型里格式是随 `IMiniportWaveRT::NewStream(..., DataFormat)` 传进来的（sysvad 也是如此：`NewStream → stream->Init(..., DataFormat, ...)`），PortCls 之后不会再下发 stream `SetFormat`。只把格式解析写在 `SetFormat` 里，`bytes_per_sec` 就永远为 0，`GetPosition` 里的 QPC 推进被判为不可用、位置恒 0——引擎看不到数据流动，于是缓冲永不排空。

**修法**：抽出 `apply_stream_format(stream, data_format)`，`SetFormat` 与 `NewStream` 两条路径都调用；NewStream 里格式非法时释放刚建的流并返回错误。

### 案例十一：KS 结构的"线上长度"不能用 Rust 的 `size_of`

`KSDATAFORMAT_WAVEFORMATEX` 的**线上长度**是 `64 + 18 = 82` 字节；Rust 侧同名结构体因为 `KSDATAFORMAT` 的 8 字节对齐会被 padding 成 84/88。用 `size_of::<KSDATAFORMAT_WAVEFORMATEX>()` 当阈值，会把引擎发来的合法 82 字节设备格式（16bit/48k/2ch PCM）判死——现场特征是 `set_format=54` 但 `set_format_ok=16`，`last_size=0x52`。改成显式常量 82 之后，独占模式也随即可用（`Initialize(exclusive)` 从 `0x8889000F` 变 `S_OK`）。

> 同族提醒：`WAVEFORMATEX`/`WAVEFORMATEXTENSIBLE` 在 `windows` crate 里是 1 字节对齐（packed）类型，既不能取字段引用（E0793），也不能"把 18 字节结构拷到栈上再去读偏移 24 的 SubFormat"——那是越界读栈垃圾。CLI 里这个 bug 的现场是：float32 混音格式被误判成"位深 32 不支持"。

### 定位方法论（比这五个缺陷更值钱）

1. **逐调用探测**：按 `Activate → GetDevicePeriod → GetMixFormat → IsFormatSupported → GetBufferSize → Initialize → GetService → Start → Initialize(exclusive)` 的顺序逐个打印 HRESULT，找**第一个**失败的调用。接口方法要加 `[PreserveSig]`（C#）或等价手段，否则 COM 互操作会把非 0 HRESULT 抛成异常，看不到是哪一步。
2. **跟同机可用设备做逐属性对照**：`IOCTL_KS_PROPERTY` 对同一 pin 查 `CINSTANCES / DATAFLOW / COMMUNICATION / CATEGORY / INTERFACES / MEDIUMS / DATARANGES / PROPOSEDATAFORMAT(2) / DATAINTERSECTION`，与可用的第三方虚拟声卡逐格 diff——**"别人的设备回什么"比"文档说该回什么"更快**。
3. **必要时扫第三方驱动的二进制**：按结构里 GUID 的内存字节序搜（如 `KSDATAFORMAT_TYPE_AUDIO` = `61 75 64 73 00 00 10 00 80 00 00 aa 00 38 9b 71`），看 GUID 前 16 字节的 `FormatSize/Flags`，就能还原对方声明了什么。
4. **判定顺序**：①接口/属性形状（KS 层 diff）→ ②`GetDevicePeriod` 是否 0 → ③`GetMixFormat`/`Initialize` 是否 0 → ④`GetCurrentPadding` 是否下降 → ⑤capture 是否有包。每一步都能独立归因，不要一次改多处。

## 七、构建与安装

前置：Windows 机器 + Visual Studio 2022（C++ 桌面负载）+ WDK 10.0.26100 + Rust stable（MSVC target）。仓库的 `crates/vdev-audio-win` 是独立 workspace。

```powershell
# 产出内核驱动（kernel feature 打开 no_std + 内核链接）
cargo build --release --features kernel -p vdev-audio-driver

# 打包 + 签名（scripts/stage-sign-audio.ps1）：
# vdev_audio.dll 改名 vdev_audio.sys，连 INF/CLI 拷入 target\dist，
# Inf2Cat 生成 vdev-audio.cat，signtool 用证书 Subject 签 sys 和 cat
.\scripts\stage-sign-audio.ps1
```

签名现状必须如实说：这是自签名/测试签名路线。先用 `New-SelfSignedCertificate -Type CodeSigningCert` 做一张代码签名证书，`certutil -addstore` 装进 TrustedPublisher 和 Root；若加载仍被拦，开测试签名（`bcdedit /set testsigning on`，需重启）。签名脚本里 signtool 的 `/n` 参数直接取查到证书的 `$cert.Subject`——这个脚本曾被从显示驱动复制过来、证书名硬编码错配，也是审查修掉的点之一。

安装走 CLI（自动 UAC 提权，参数转义处理了含空格路径）：

```text
vdev-audio-win.exe install --inf-dir target\dist   # 装驱动（需管理员）
vdev-audio-win.exe status                          # 查看安装状态（--json 出机器可读格式）
vdev-audio-win.exe inject --tone 1000 --duration 4 # 向「vdev 扬声器」注入（宿主推流）
vdev-audio-win.exe capture --duration 6 --skip 4   # 从「vdev 麦克风」采集并报电平
vdev-audio-win.exe uninstall                       # 卸载
```

宿主 GUI（`crates/vdev-app-win`，独立 workspace）的「虚拟声卡」页把上面这套做成按钮：安装/卸载/刷新 +
「注入音频」/「环回自测」（后者并发跑 `capture ‖ inject` 并把 RMS/峰值写回面板与日志）。
GUI 不直接调驱动，而是委托 `vdev-audio-win.exe`（找不到时设 `VDEV_AUDIO_WIN_EXE` 或放到同目录）。

INF 是 Media 类（`ClassGuid={4d36e96c-…}`）、`Root\vdev-audio` 硬件 ID，`Include=ks.inf,wdmaudio.inf` 借用系统注册节。有个值得单独说的细节：**INF 必须是 UTF-16 LE（带 BOM）**——SetupAPI 只认这个编码，仓库的宿主单测会读 INF 字节流校验 BOM、CRLF 行尾，以及四个 `KSNAME_*` 接口模板与驱动里 `PcRegisterSubdevice` 注册名逐字节一致（这个 INF 曾是 UTF-8+双 BOM，还是审查抓出来的）。

> 装机时记得升 `DriverVer`：`pnputil /add-driver /install` 对"版本没变"的包会直接报
> `Driver package is up-to-date on device`，`System32\drivers\vdev_audio.sys` 时间戳不变——
> 看着装上了，跑的还是旧驱动。

CI 现状：GitHub Actions 的 Windows 用户态矩阵对 `vdev-audio-win` 跑 fmt/check/test/clippy（默认 feature，纯逻辑测试全部可跑）；依赖 WDK 的驱动本体构建由 `windows-driver-wdk` job 覆盖（runner 上装 WDK + LLVM 后 `cargo check --workspace`），该 job 起初以 `continue-on-error` 顾问形式上线、已于 2026-09 升为**硬门禁**。

## 八、现状与局限

如实交底：

- **已真机验证通过（2026-09-14，Win10 19045 x64 + 测试签名）。** 端点从"能枚举但打不开"修到**完全可用**：
  - 渲染/采集端点与同机 Realtek / ToDesk 端点**逐调用一致**：`GetDevicePeriod hr=0`（`default=10ms / min=3ms`）、`GetMixFormat` 返回 float32/48k、`IsFormatSupported`、`Initialize`（共享 `AUTOCONVERTPCM` 与独占）、`GetBufferSize=48000`、`GetService`、`Start` 全 `S_OK`；
  - **环回实测通过**：向「vdev 扬声器」注入 0.5 幅度 1 kHz 正弦，从「vdev 麦克风」采回 **RMS −6.0 dBFS / 峰值 −6.0 dBFS**（正是 0.5 幅度正弦的理论值，逐样本搬运），基线静音 −93.8 dBFS；
  - CLI 有了注入/采集入口（见下），GUI 有「虚拟声卡」页。
- **格式固定**：48 kHz / 16 bit / 双声道 PCM，`apply_stream_format` 硬校验，别的不收（引擎负责在混音格式与设备格式之间转换）。
- **位置走轮询**：无硬件位置寄存器（明确返回 `STATUS_NOT_SUPPORTED`），依赖 PortCls 的轮询模式 + QPC 时间锚。**时钟能否推进取决于 `NewStream` 里有没有拿到格式**（案例十）。
- **音量/静音是"记事本"**：采集 topology 的节点属性真实可读写，但没有 DSP 效果（渲染拓扑直通，音量由引擎软件混音层处理）。
- **环回有固定积压**：两个流共享非分页环形缓冲，按**设备格式** 16bit/48k/2ch = 192000 B/s 算，1 MB ≈ **5.46 s**（真机实测 ≈5.14 s；0.3.10.0 起改为 256 KB ≈ 1.37 s）。所以"顺序执行：先注入、再采集"读到的多是环里的历史数据；要量当次注入必须**并发**采集（CLI 与 GUI 的环回自测都是这么做的），或把缓冲调小。
- **驱动本身没有用户态注入接口**：环回纯内核内完成，驱动不暴露 IOCTL/WriteFile 面（安全面干净）。宿主"注音"是从**端点侧**完成的——`vdev-audio-win inject` 就是往「vdev 扬声器」推流（WASAPI 共享模式 + 端点混音格式），`capture` 从「vdev 麦克风」拉流；两者并发就是环回自测，GUI 的「环回自测」按钮把它做成了一键（后台线程跑 CLI，结果回填面板与日志）：

```powershell
vdev-audio-win inject --tone 1000 --amplitude 0.5 --duration 4   # 也可 --wav file.wav 循环播放
vdev-audio-win capture --duration 6 --skip 4 --json               # 报 RMS/峰值 dBFS，可 --wav 存盘
```
- **待办**：Driver Verifier 专项（special pool + DDI compliance）还没跑；环形缓冲已从 1 MB（≈5.46 s）调到 256 KB（≈1.37 s），仍可按需再调；多虚拟屏场景未涉及（本设备与显示器无关）。

## 九、写在最后：内核驱动"编译过 ≠ 能跑"

这个驱动的六次蓝屏级缺陷，没有一个是编译器能抓的：栈悬垂是合法的裸指针运算，回绕公式错是合法的下标，vtable 少一级间接是合法的转型，NTSTATUS 错值是合法的 i32，KS 描述符错布局是合法的 `repr(C)`。`unsafe` 的自由把类型系统的保护全部让渡给了 ABI——而 ABI 的权威不在任何一门语言里，只在 WDK 头文件和官方样例里。

方法论的沉淀就三条：

1. **官方样例与 WDK 头文件是唯一权威。** sysvad 的安装顺序、连接表条目、端点拓扑，逐行照抄不丢人；结构布局、GUID、NTSTATUS、vtable 槽序，逐字段对照头文件，注释里写明行号来源。
2. **把"内核才能验证的"压缩到最小，其余全部下沉为宿主可测。** 纯数学抽模块（`position.rs`）、纯常量抽模块（`endpoint_names.rs`）、布局断言进 `#[cfg(test)]`——本驱动最终有 6 个驱动测试模块（含 CLI 共 7 个）可以在 macOS 宿主跑，其中两个（回绕性质测试、NTSTATUS/GUID 值回归）正是案例二和案例五的疫苗。静态描述符这类"数据即 ABI"的东西，再加一层描述符自洽性断言（计数、越界、DataFlow 方向）。
3. **"能枚举"离"能用"很远，真机验证要按调用链一层层量。** 前六个缺陷是审查抓的，后面五个只有真机才暴露：设备管理器有节点、控制面板有端点、`Activate` 返回 `S_OK`——但 `GetDevicePeriod` 就已经在报 `0x80070491` 了。**别把"看得见"当"验证过"**：把 HRESULT 打印到第一个失败点，再拿同机可用设备逐属性对照，比读文档猜快得多。
4. **官方样例也要对版本与平台核对。** sysvad 的"节点中转"拓扑、`MsHidKmdf.inf`（Win11 才有）这类依赖，在 Win10 上要么装不上、要么能用但没必要；`KSPROPERTY_PIN_PROPOSEDATAFORMAT` 的语义只有读源码才对得准。
5. **Driver Verifier 仍要做。** special pool + DDI compliance 会把池越界和 IRQL 违规在第一时间变成可定位的 bugcheck，而不是等池损坏在别处爆炸。功能链路（装机、枚举、开流、环回）已验证，Verifier 专项是下一步。

内核驱动的世界没有"先跑起来再说"。每一个凭记忆写出的字节，最终都会以蓝屏的形式找你复核。

---

*本文代码均来自 [gqf2008/vdev](https://github.com/gqf2008/vdev) 仓库 `crates/vdev-audio-win`（截至 2026-09，全部缺陷已修复）。参考实现：Microsoft sysvad 样例。*
