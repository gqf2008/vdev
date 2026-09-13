# 用纯 Rust 写一个 Windows 虚拟摄像头：DirectShow 源过滤器全解析

> 本文是 [vdev](https://github.com/gqf2008/vdev) 虚拟设备驱动开发系列之一。全套含 macOS 摄像头/声卡/键鼠/虚拟屏与 Windows 摄像头/显示器/声卡/HID 九篇。
> 代码引用约定：本文所有 `文件:行号` 均相对**仓库根**（如 `crates/.../foo.rs:12`），行号为写作时基线；代码演进后行号会漂移，按符号名搜索为准。

虚拟摄像头是虚拟设备里最"平易近人"的一种：它不要求理解内核电源管理，也不要求签发驱动包，却覆盖了从 COM、跨进程共享内存到多媒体框架协商的完整知识面。本文以 vdev 仓库中的 `vdev-camera-win`（约 3100 行 Rust，含注释与测试）为例，讲清楚一条完整的实现路径：如何用纯 Rust 写一个 DirectShow 源过滤器（Source Filter），把它注册成系统摄像头，并把另一个进程推送的帧安全地送到任意 App 的摄像头画面里。

## 一、三条路线，为什么选 DirectShow

在 Windows 上造一个"假摄像头"，大体有三条路线：

| 路线 | 形态 | 门槛 |
|---|---|---|
| 内核驱动（AVStream 类） | 内核态过滤器驱动，OBS Virtual Camera 走这条路 | 驱动签名（测试签名 / EV 证书 / WHQL），蓝屏风险在内核 |
| Media Foundation 虚拟摄像头 | 较新的多媒体框架路线，依赖较新的系统版本 | 系统版本与生态位限制，排障面在框架内部 |
| **DirectShow 源过滤器（本仓选择）** | **用户态 COM DLL**，注册到"视频捕获源"类别 | **无驱动、无驱动签名门槛**，regsvr32 / 自注册即装即用 |

选 DirectShow 的理由很直接：

1. **无签名门槛**。它是一个被 `regsvr32`（或程序自身）写进注册表的进程内 COM 组件（InprocServer32），不经过驱动符号检查，也没有内核稳定性责任。
2. **用户态可调试**。整个过滤器运行在消费 App 的进程里，可以用 `selftest` 子命令在自己的进程内建一张 `源过滤器 → NullRenderer` 的图来验证帧流动，不需要真机内核调试环境。
3. **生态兼容面最大**。DirectShow 虽老，但 `ffmpeg -f dshow`、VLC、OBS、Zoom、Teams 等仍然枚举"视频捕获源"类别，Windows 自带的 `quartz.dll` 提供了 Filter Graph 的全部基础设施。

代价是你要直面 COM 和 DirectShow 的年代感设计——这也是本文后半部分踩坑实录的来源。

## 二、DirectShow 最小知识

只需要四个概念：

- **Filter Graph（过滤器图）**：一张由"过滤器"（Filter）组成的数据流图。摄像头画面从 **Source Filter**（源过滤器）流出，经过可能的转换过滤器，到达渲染器或编码器。App 通过 Filter Graph Manager（`CLSID_FilterGraph`，实现在 `quartz.dll`）建图、连线和控制状态。
- **Pin 与媒体协商**：每个过滤器有若干 Pin（引脚）。源过滤器暴露一个**输出 Pin**，下游渲染器提供输入 Pin。连线时双方协商一份 `AM_MEDIA_TYPE`（主类型/子类型/格式块，比如 `MEDIATYPE_Video` + `MEDIASUBTYPE_YUY2` + `VIDEOINFOHEADER`）。协商过程是 `IPin::Connect` → 对端 `ReceiveConnection` → 分配器协商（`IMemAllocator`），随后数据以 `IMediaSample` 为载体，由源过滤器调用下游的 `IPin::Receive`（准确说是 `IMemInputPin::Receive`）逐样本推送。
- **状态机**：过滤器有 `Stopped / Paused / Running` 三态。对推源过滤器有一个关键细节：**Pause 时就要开始送预滚帧**，下游渲染器收到第一帧才能完成自己的 Pause，图才能进入 Run。我们的实现把推流线程启动放在 `Pause`，`Run` 只更新时间起点（`crates/vdev-camera-win/src/dshow/filter.rs:229–249`）。
- **时间戳**：每个 `IMediaSample` 用 `SetTime` 打上时间戳，且必须填**流时间**（相对 Run 起点、按帧间隔单调递增），渲染器拿它与参考时钟比较后按时呈现。时间戳填错时间域或干脆不打，下游会卡帧或黑屏。

虚拟摄像头的本质因此非常朴素：**写一个"长得像摄像头"的 Source Filter**——实现 `IBaseFilter`（及继承链 `IMediaFilter`/`IPersist`）、一个输出 Pin（`IPin`），在 Pin 上声明自己会输出什么格式，然后周期性地往下游 `Receive` 样本。真正的画面数据则从进程外来。

## 三、纯 Rust 写 COM 组件

### 3.1 IUnknown、vtable 与引用计数

COM 的核心契约是 `IUnknown`：`QueryInterface` / `AddRef` / `Release`，加上一张二进制 vtable。手工实现这些既繁琐又容易错，本仓使用微软官方的 `windows` crate 0.62 + `windows-core` 的 `#[implement(...)]` 宏：宏为结构体生成 COM 兼容的对象布局、vtable 和引用计数，我们只需按 trait 实现各接口方法（方法签名固定为 `&self` + 裸指针，`unsafe` 解引用收敛在这些实现内部，全 crate 的 `unsafe` 都带 `SAFETY` 注释）。

过滤器与 Pin 分别声明为：

```rust
// crates/vdev-camera-win/src/dshow/filter.rs:149
#[implement(IBaseFilter, IMediaFilter, IPersist, IAMFilterMiscFlags)]
pub struct VirtualCameraFilter { pub inner: Arc<FilterInner> }

// crates/vdev-camera-win/src/dshow/pin.rs:127
#[implement(IPin, IAMStreamConfig, IKsPropertySet)]
pub struct OutputPin { pub inner: Arc<PinInner> }
```

一个 Rust 结构体同时实现多个 COM 接口，`QueryInterface` 由宏按列表分发；`Arc<...Inner>` 是真正承载状态的共享内部，COM 对象只是它的"壳"。这在后面讲引用环时会很关键。

### 3.2 类厂与 DLL 导出

COM 组件由**类厂**（`IClassFactory`）创建。消费 App 调 `CoCreateInstance(CLSID)` 时，COM 运行时读注册表找到 DLL，`LoadLibrary` 后调用导出函数 `DllGetClassObject` 拿类厂，再 `CreateInstance` 出过滤器对象。四个标准导出在 `crates/vdev-camera-win/src/lib.rs:48–70`，全部只有一行转发：

```rust
// crates/vdev-camera-win/src/lib.rs
#[no_mangle]
pub extern "system" fn DllGetClassObject(
    rclsid: *const GUID, riid: *const GUID, ppv: *mut *mut c_void,
) -> HRESULT {
    com::dll::get_class_object(rclsid, riid, ppv)   // 校验 CLSID，返回 FilterClassFactory
}
// DllCanUnloadNow / DllRegisterServer / DllUnregisterServer 同型
// 完整实现见 crates/vdev-camera-win/src/lib.rs:57–70 与 crates/vdev-camera-win/src/com/mod.rs:95–141
```

两个值得注意的工程决策：`DllCanUnloadNow` 恒返回 `S_FALSE`（拒绝卸载，避免 DLL 在使用中被拔掉，`crates/vdev-camera-win/src/com/mod.rs:119–121`）；所有线程入口（包括推流线程）都用一个 `ComInit` RAII 守卫初始化 COM，当宿主线程已经以别的模式初始化过（返回 `S_FALSE` 或 `RPC_E_CHANGED_MODE`）时**复用而不卸载**，避免破坏宿主的 COM 状态（`crates/vdev-camera-win/src/com/mod.rs:25–53`）——虚拟摄像头 DLL 是活在别人的进程里的，这类礼貌是必须的。

### 3.3 自注册：DllRegisterServer 里到底注册了什么

`register_filter()`（`crates/vdev-camera-win/src/camera.rs:46–62`）写三组注册表键：

```text
HKCR\CLSID\{E4C01F0D-A9FC-4352-8590-F0E5AD2BFFCE}            # 过滤器 CLSID（crates/vdev-camera-win/src/dshow/filter.rs:27）
    默认值      = "vdev-camera"                              # 友好名
HKCR\CLSID\{E4C01F0D-...}\InprocServer32
    默认值      = C:\...\vdev_camera_win.dll                  # 本 DLL 绝对路径
    ThreadingModel = Both
HKCR\CLSID\{860BB310-5D01-11D0-BD3B-00A0C911CE86}\Instance\{E4C01F0D-...}
    FriendlyName = "vdev-camera"     # 缺它设备枚举直接跳过（关键！）
    CLSID        = {E4C01F0D-...}
    FilterData   = REG_BINARY（REGFILTER2 v2 序列化，88 字节）
```

第二组是 COM 侧的"我是谁、代码在哪"；第三组才是"我是一个摄像头"——`{860BB310-...}` 是系统固定的 `CLSID_VideoInputDeviceCategory`（视频捕获源类别），设备枚举器 `ICreateDevEnum` 扫的就是这个类别下的 `Instance` 键。三个细节来自实战：

- **`FriendlyName` 必须存在**，否则设备枚举器直接跳过此设备，而 `CoCreateInstance` 又能成功——极具迷惑性（`crates/vdev-camera-win/src/camera.rs:146–157` 注释）。
- **`FilterData` 是 REGFILTER2 v2 的二进制布局**（`0pi3`/`0ty3` 签名的 pin/媒体类型记录 + 偏移引用的 GUID 存储），我们的 pin 声明为单个 YUY2 输出、`MERIT_DO_NOT_USE`（不参与自动建图，只能被显式选中——虚拟摄像头不该被 Graph 自动连到随便什么渲染器上）。序列化在 `serialize_filter_data()`（`crates/vdev-camera-win/src/camera.rs:174–207`），字节级对照了 Wine 的 `FM2_WriteFilterData` 与真实样例，并有布局回归测试。
- **注册顺序是先 HKLM（系统级，需管理员）失败自动回退 HKCU（当前用户级）**，HKCR 是两者的合并视图，所以免管理员也能装（`crates/vdev-camera-win/src/camera.rs:106–124`）。

另外，若同目录存在 32 位 DLL（`vdev_camera_win32.dll`），会同时注册 `Software\Classes\WOW6432Node` 视图——32 位进程（如 32 位 VLC）经 WOW64 重定向只能看到这个视图（`crates/vdev-camera-win/src/camera.rs:51–59`）。

## 四、帧通道：从推流进程到 IMediaSample

推流与取流是**两个独立进程**：你的推流程序（或 GUI 宿主）持有 `CameraServer`，而过滤器 DLL 被加载进消费 App 的进程。两者之间用三个命名内核对象通信（`crates/vdev-camera-win/src/com/shm.rs:32–36`）：

- `Local\vdev-camera-win-frames`：共享内存（文件映射），64 字节头 + 两个帧槽；
- `Local\vdev-camera-win-frame-event`：自动重置事件，新帧信号；
- `Local\vdev-camera-win-publish-lock`：命名互斥体，多生产者并发的发布锁。

头部是一个 `#[repr(C)]` 结构，`seq` 与 `ready` 是跨进程原子（`crates/vdev-camera-win/src/com/shm.rs:48–58`）：

```rust
// crates/vdev-camera-win/src/com/shm.rs
#[repr(C)]
struct Header {
    magic: u32,
    width: u32,
    height: u32,
    stride: u32,
    buf_len: u32,
    seq: AtomicU32,    // 发布序号，Release/Acquire 配对
    ready: AtomicU32,
    pad: [u32; 5],     // 头部对齐到 64 字节
}
```

**生产者** `publish()`（`crates/vdev-camera-win/src/com/shm.rs:177–252`）的顺序是：有界等待发布锁（500ms，正常持锁窗口是微秒级 memcpy）→ 读当前 `seq` 加一 → 把帧整块拷进**另一个槽**（`seq & 1` 双缓冲，写者写的永远是读者没在读的槽）→ 以 `Release` 顺序发布新 `seq`、置 `ready` → `SetEvent` 唤醒消费者 → `ReleaseMutex`。

**消费者** `latest()`（`crates/vdev-camera-win/src/com/shm.rs:268–297`）是一次标准的 **seqlock 读**：

```rust
// crates/vdev-camera-win/src/com/shm.rs（latest 读循环，精简）
for _ in 0..MAX_SEQLOCK_ATTEMPTS {          // 上限 8 轮，防活锁
    let seq_before = header.seq.load(Ordering::Acquire);
    // 读头部字段 width/height/buf_len …
    let copy_len = (buf_len as usize).min(MAX_BUF);  // 校验前不可信，按槽容量截断
    out.copy_from_slice(&slot_buf[..copy_len]);      // 拷贝负载
    let seq_after  = header.seq.load(Ordering::Acquire); // 拷贝【之后】采样
    match judge_seqlock_read(seq_before, seq_after, …) {
        SeqlockVerdict::Accept  => return Some((width, height)),
        SeqlockVerdict::NoFrame => return None,   // 序号稳定但头部自洽校验失败
        SeqlockVerdict::Retry   => continue,      // 撕裂，重读
    }
}
```

防撕裂有两个关键点：一是**`seq_after` 必须在负载拷贝完成之后采样**——序号括弧要罩住"头部 + 负载"全部读取。若只在拷贝前采样，写方在拷贝期间连发两帧回到同一槽（奇偶相同，如 7→9）的撕裂像素会漏检并通过校验；二是拷贝长度在通过 `buf_len == w*h*4` 自洽校验之前不可信，必须按单槽容量截断，防崩溃残留的垃圾 `buf_len` 造成越界。这两条裁决被抽成了零 Windows 依赖的纯函数 `judge_seqlock_read`（`crates/vdev-camera-win/src/com/channel_logic.rs:86–107`），连同重读收敛、双发布回归等 9 个测试可以直接在任意平台 `rustc --test` 跑——**把无锁算法的"决策逻辑"与"系统调用"分离，是这类代码能被充分单测的关键**。

**过滤器侧**的推流线程（`crates/vdev-camera-win/src/dshow/streaming.rs:62–166`）把通道数据变成 DirectShow 样本：`wait_frame` 等新帧（超时=一帧时长）→ `latest` 取帧，生产者分辨率与协商格式不一致时最近邻缩放，完全无帧则回退棋盘格测试图案（摄像头永远不能"没有画面"）→ BGRA 转 YUY2 → `allocator.GetBuffer` 取下游样本 → `GetPointer` 拿缓冲区、`copy_nonoverlapping` 填帧（这就是 FillBuffer）→ `SetTime` 打**流时间**戳、`SetSyncPoint(true)` → 下游 `IMemInputPin::Receive`。其中分配器协商镜像了 `CBaseOutputPin::DecideAllocator` 的协议：先试下游提供的分配器，`VFW_E_NO_ALLOCATOR`（ffmpeg 的 sink 就这样）或属性被拒时自建 `CLSID_MemoryAllocator` 并 `NotifyAllocator`（`crates/vdev-camera-win/src/dshow/pin.rs:527–549`）。

顺带一提兼容性选型：输出子类型选 **YUY2 而非 RGB32**，因为 DirectShow 摄像头生态以 YUV 为主，VLC 3.0 无法从 RGB32 媒体类型提取 fourcc，直接报 unsupported format；`biHeight` 用**正数**（OBS/libdshowcapture 同款），负值会被 VLC 塞进 unsigned 字段溢出成黑屏（`crates/vdev-camera-win/src/dshow/media_type.rs:24,63-70`）。

## 五、踩坑实录

以下是四个真实发生过、且都能在代码里看到"案发现场注释"的坑。前三个来自一次独立审查（commit `aeb5200`，"DirectShow 过滤器审查修复——死锁与泄漏清零"），属于"能跑 demo 但上不了生产"的级别。

### 坑 1：Mutex 的 WAIT_ABANDONED——持锁进程崩溃后，你必须"接管"

跨进程命名互斥体的等待只认 `WAIT_OBJECT_0` 是经典错误。原实现里推流方进程在持锁瞬间崩溃，内核会把互斥体标记为 abandoned，后续等待者的 `WaitForSingleObject` 返回 `WAIT_ABANDONED(0x80)`——**这个返回码的语义是"所有权已移交给本次等待"，等待者此刻已经拿到锁了**。把它当失败返回且不 `ReleaseMutex`，锁就永远滞留在死进程名下，之后所有生产者的推帧调用永久失败。

正确姿势是按裸返回码分派（`crates/vdev-camera-win/src/com/channel_logic.rs:53–62`）：

```rust
// crates/vdev-camera-win/src/com/channel_logic.rs
pub fn judge_lock_wait(code: u32) -> LockWaitVerdict {
    match code {
        WAIT_OBJECT_0_CODE   => LockWaitVerdict::Acquired,
        // 内核已把所有权移交本次等待：必须按已获锁继续，绝不能当失败
        WAIT_ABANDONED_CODE  => LockWaitVerdict::AcquiredAbandoned,
        WAIT_TIMEOUT_CODE    => LockWaitVerdict::TimedOut,
        _                    => LockWaitVerdict::Failed,
    }
}
```

接管后的两条配套：一、既已获锁，正常路径末尾的 `ReleaseMutex` 照常执行（`crates/vdev-camera-win/src/com/shm.rs:250`）；二、锁保护的数据可能被崩溃写撕坏，读侧用上面的 seqlock 序号裁决兜底丢弃坏帧。同批还把 `INFINITE` 等待改成了 500ms 有界等待——正常持锁是微秒级，超时即持锁方异常，放弃本帧由下一帧自然重试，别让推流线程陪一个挂死的进程一起挂死。

### 坑 2：COM 对象的自引用环——引用计数永不归零的泄漏

`#[implement]` 生成的 COM 对象在引用计数归零时才析构内部结构。原实现里 filter 和 pin 各自把"**自己的** COM 接口" `clone` 一份缓存在共享状态里（`self_base: Option<IBaseFilter>` 之类），而 `windows-core` 接口的 `clone` 是显式 `AddRef`——"对象 → 自己的接口"是天然的引用环，外部引用全部释放后计数仍停在 1，filter + pin 整套永不析构。对虚拟摄像头来说这意味着**每开一次摄像头泄漏一套对象**。

解法是规范所有权：唯一持久的强引用放 owner（`FilterInner::pin_com` 持有 pin 的初始 `IPin`），所有"回指"——filter 看自己、pin 看自己、pin 看 owner——一律 `Interface::downgrade()` 存 `Weak`，用的时候 `upgrade`，失败即对象正在析构，返回错误或空指针而不是悬垂（`crates/vdev-camera-win/src/dshow/filter.rs:128–146`、`crates/vdev-camera-win/src/dshow/pin.rs:59–63,83-94`）。强引用链变成无环的 `F → FI → pin`，外部释放后整条链可回收。

### 坑 3：Stop 的顺序——先 join 后 Decommit，反了会挂死

停流要做的两件事：停推流线程（置 stop 标志 + `join`），以及对分配器 `Decommit`（与 Pause 时的 `Commit` 配对）。顺序是不可变更的不变量：**必须先等线程死，再 Decommit**。反过来的话，`Decommit` 返回后推流线程还活着，可能正好进入下一轮循环去 `GetBuffer` 访问已 decommit 的分配器。现实现的 `stop_streaming`（`crates/vdev-camera-win/src/dshow/filter.rs:309–329`）里 `t.stop()`（join 返回）在前、`Decommit` 在后，并有一段注释把推演写全。

这里有一个**有意接受的残余风险**，值得写驱动/过滤器的人体会：`GetBuffer` 在下游不还样本时可能无超时阻塞，join 因此可能等。之所以可以接受，是因为 DirectShow 全图 Stop 有顺序性——渲染器先停、样本释还，join 因而有界。你依赖的是框架的协议，而不是自己的运气。

### 坑 4：注册表里写错的 DLL 路径——`GetModuleFileNameW(None)` 拿的不是"我的 DLL"

自注册要往 `InprocServer32` 写 DLL 绝对路径。原实现用 `GetModuleFileNameW(None)` 推导——但 `None` 的语义是"**当前进程的 exe**"。`regsvr32` 下恰好碰对（调用约定使然），可同样的代码嵌进其他宿主（比如一个 64/32 双视图注册器进程），写进注册表的就是宿主 exe 的路径，之后 `CoCreateInstance` 加载的自然是垃圾。

正解是以"模块内任意地址"反查所属模块：在本模块放一个静态符号作锚点，`GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | ..._UNCHANGED_REFCOUNT, 地址, &hmodule)` 取真句柄，再 `GetModuleFileNameW(Some(h))`（`crates/vdev-camera-win/src/com/registry.rs:125–154`）。`UNCHANGED_REFCOUNT` 表示只借句柄不配对 `FreeLibrary`。这个锚点在 regsvr32/LoadLibrary 宿主下解析为 DLL 自身，在 CLI（rlib 进 exe）下解析为 exe——两种宿主一套代码。

## 六、构建、安装与运行

以下命令与仓库 README 一致（Windows x64，需 Rust MSVC 工具链）：

```powershell
cd crates\vdev-camera-win
cargo build --release
# 产物：target\release\vdev_camera_win.dll（过滤器）+ vdev-camera-win.exe（CLI）

# 安装：优先 HKLM（管理员），失败自动回退 HKCU（免管理员）
.\target\release\vdev-camera-win.exe install
.\target\release\vdev-camera-win.exe list        # 列出视频捕获源，应见 vdev-camera

# 验证（ffmpeg 侧）
ffmpeg -f dshow -list_devices true -i dummy            # 应看到 "vdev-camera" (video)
ffmpeg -f dshow -list_options true -i video=vdev-camera # 应列出 3 档格式

# 推流（终端 1）与取流（终端 2）是两个独立进程
.\target\release\vdev-camera-win.exe push --width 640 --height 360 --fps 30 --seconds 120
ffmpeg -f dshow -i "video=vdev-camera" -c:v libx264 -f mp4 out.mp4

# 自测：进程内 DirectShow 图（源 → NullRenderer），3 秒应交付约 80 帧
.\target\release\vdev-camera-win.exe selftest --seconds 3

# 卸载
.\target\release\vdev-camera-win.exe uninstall
```

`install` 走的是程序内自注册（CLI 直接调 `register_filter()`）。等价地，也可以用系统标准方式触发 DLL 的 `DllRegisterServer` 导出——两者最终执行同一段注册代码（`crates/vdev-camera-win/src/com/mod.rs:123–131` → `camera.rs`）：

```powershell
regsvr32 .\target\release\vdev_camera_win.dll      # 注册（无管理员权限时同样回退 HKCU）
regsvr32 /u .\target\release\vdev_camera_win.dll   # 注销（DllUnregisterServer）
```

要兼容 32 位消费进程时，额外编一份 32 位 DLL 放到 64 位 DLL 同目录，`install` 会检测到并自动注册 WOW6432Node 视图：

```powershell
rustup target add i686-pc-windows-msvc
cargo build --release --target i686-pc-windows-msvc
Copy-Item target\i686-pc-windows-msvc\release\vdev_camera_win.dll `
           target\release\vdev_camera_win32.dll
```

仓库里还带一个 32 位诊断探针（`crates/vdev-camera-win/examples/probe.rs`）：用 `CoCreateInstance` 加载真实 DLL，按 VLC/ffmpeg 打开设备的调用序列逐步执行并打印进度，崩溃即定位到具体接口——排障 32 位兼容时非常好用。

## 七、现状与局限

**已实测**：README 将本组件标为 ✅ 可用（实测）。具体经过真机排障验证的点包括：ffmpeg 的设备枚举、格式枚举与取流录制；VLC 打开（为此修掉了 RGB32 fourcc、负 `biHeight`、时间戳时间域三个兼容性问题）；进程内 `selftest` 图 3 秒约 80 帧；共享内存往返与 FilterData 布局的单元测试。OBS/Zoom 等走同一"视频捕获源"枚举路径，属设计目标而非逐一实测项。

**当前限制**（与 README 一致）：

- 固定输出 YUY2，三档格式（1920x1080 / 1280x720 / 640x480）@30fps；`IAMStreamConfig::SetFormat` 仅允许在未连接时调用，运行中动态改格式未做。
- 推流端是棋盘格测试图案 + 共享帧通道；音频、配置 UI、多分辨率动态协商留待后续。
- 免签名指的是**没有驱动签名要求**；分发 DLL 时仍建议做代码签名，否则 SmartScreen 与杀软可能提示。
- `MERIT_DO_NOT_USE` 意味着它不会出现在任何自动建图里——这是有意的：虚拟摄像头只应被用户显式选中。

## 八、写在最后

这个项目里我们认为最可复用的经验有两条。

其一，**"安全封装优先"的分层**：所有 Win32/COM 调用收敛到带 `SAFETY` 注释的封装模块（`com/`、`dshow/`），业务层（CLI）零 `unsafe`；再把无锁协议这类易错逻辑抽成零依赖纯函数（`channel_logic.rs`），让最危险的并发决策在任何平台上都能跑回归测试。Rust 写 COM 的可行性与舒适度，很大程度上取决于这条分界线画得清不清。

其二，**虚拟设备是"协议复读机"**：DirectShow 的坑几乎都不在"怎么写代码"，而在"有没有按 `CBaseOutputPin::DecideAllocator`、`IMediaFilter::Stop` 这些半官方契约办事"。每个坑修完都值得把契约本身写回注释——这也是我们把踩坑实录直接写进源码注释与本文的原因。

系列其他篇章（macOS 摄像头/声卡、Windows 虚拟显示器/声卡/HID）见仓库 [gqf2008/vdev](https://github.com/gqf2008/vdev)。欢迎按 README 的门禁跑通后提 PR。
