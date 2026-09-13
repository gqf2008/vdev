# 用 100% Rust 写 macOS 虚拟声卡：AudioServerPlugIn 从加载到环回出声

> 本文是 [vdev](https://github.com/gqf2008/vdev) 虚拟设备驱动开发系列之一。全套含 macOS 摄像头/声卡/键鼠/虚拟屏与 Windows 摄像头/显示器/声卡/HID 九篇。

## 一、虚拟声卡是干什么的

虚拟声卡解决的是一个看似简单的问题：**把"播放"变成"录音"**。视频播放器把音轨推给一张虚拟设备的输出端，会议软件把"麦克风"选成同一张设备的输入端，声音就从一个 App 流进了另一个 App——全程不经过扬声器，也不过麦克风。典型用途：

- **环回采集**：录系统声音、录远端会议音频；
- **给推流/会议当麦克风**：把任意 App 的输出伪装成一支麦克风；
- **音频路由与处理**：在输出→输入的通路上插 EQ、增益、限幅，就是一个系统级效果器。

macOS 上这条路的标杆是开源的 [BlackHole]（更早是 Soundflower）：安装后系统里多出一台 2/16/64/128 声道的虚拟设备，输出环回输入。vdev 的 `vdev-audio` 做的是同一件事，差别在于：**整个驱动是一个没有任何 C 代码的 Rust crate**，并且在 BlackHole 的骨架之外加了两台 8 声道设备、一张跨设备路由矩阵和一条实时 DSP 管线。本文把这个 crate 从加载、枚举、时钟到 IO 的完整机制拆开讲，最后是六个真实的踩坑案例。

[BlackHole]: https://github.com/ExistentialAudio/BlackHole

## 二、AudioServerPlugIn：跑在 coreaudiod 里的"驱动"

很多人对 macOS 音频的印象停在 App 层的 AVAudioPlayer/AudioUnit。但所有 App 的音频最终都汇入 **coreaudiod**——系统音频守护进程，由它统一调度设备、混音、时钟。App 层看到的一台"设备"，是 coreaudiod 背后某个驱动暴露出来的对象。

给 coreaudiod 添加设备，官方入口就是 **HAL 插件**：一个放在 `/Library/Audio/Plug-Ins/HAL/*.driver` 的 CFPlugIn bundle。coreaudiod 扫描该目录，按 Info.plist 里的 `CFPlugInTypes`（AudioServerPlugIn 类型 UUID）与 `CFPlugInFactories`（工厂函数名）加载并调用你导出的工厂，之后所有交互都走一张 C 函数表——`AudioServerPlugInDriverInterface`。它不是内核扩展：没有 IOKit 匹配、没有内核签名审批，崩溃了也只是拖垮 coreaudiod 而不是内核。但代价是**驱动级实时契约**：

- `DoIOOperation` 在 coreaudiod 的每设备 IO 线程上按周期调用（512 帧 @48kHz 约 10.7ms 一个周期），错过 deadline 就是可闻的爆音或设备被拔;
- 这条路径上不能分配内存、不能拿会被属性线程长期占住的锁、更不能写盘（在 `DoIOOperation` 里 fopen 调试日志是我们真实踩过并写进教训的坑——它不只是慢，还会改变 coreaudiod 的行为，制造假象）;
- 时钟不是自己说了算：设备必须通过 `GetZeroTimeStamp` 把自己的 sample time 锚定到 host time（`mach_absolute_time()` 的 ticks）上，coreaudiod 拿它对齐所有客户端。时钟模型稍微不合意（比如跳变），coreaudiod 的反应不是报错，而是直接停掉 IO——这是后面坑 3 的伏笔。

一个 HAL 插件要向宿主描述一棵对象树：plug-in → box → device → stream/control，每个对象都靠"属性"通信。宿主通过 `HasProperty / IsPropertySettable / GetPropertyDataSize / GetPropertyData / SetPropertyData` 五个入口查询和设置一切——名字、UID、流格式、采样率、音量……缺一个关键属性，轻则设备显示不全，重则初始化失败。

## 三、100% Rust 布一个 C++ vtable

`vdev-audio` 的 Cargo 配置只有一行关键声明：`crate-type = ["cdylib"]`。难点全在 ABI：

**1. 产物必须是 MH_BUNDLE。** cargo cdylib 默认产出 MH_DYLIB（filetype=6），coreaudiod 会直接跳过。Makefile 用裸链接参数改产物类型：

```bash
# crates/vdev-audio/Makefile（build 目标，节选）
cargo rustc -p vdev-audio --lib --release \
  -C link-arg=-Wl,-bundle \
  -C link-arg=-Wl,-undefined -C link-arg=-Wl,dynamic_lookup
```

`-undefined dynamic_lookup` 让 CoreFoundation/CoreAudio 的符号留到加载时解析——bundle 是被 coreaudiod 加载的，宿主进程里本来就有这些符号。

**2. 签名。** macOS 26 的 coreaudiod 拒绝未签名和 adhoc 签名的驱动，必须 Developer ID（Makefile 里检测到证书就用真签名，否则打警告）。

**3. 工厂返回的是"指针的指针"。** Info.plist 的 `CFPlugInFactories` 把工厂 UUID 指到 `vdev_audio_create`，它必须返回 `AudioServerPlugInDriverRef`——类型是 `AudioServerPlugInDriverInterface**`，即指向"接口指针变量"的地址，BlackHole 同款语义：

```rust
// crates/vdev-audio/src/lib.rs:367-374,404-405
#[no_mangle]
pub extern "C" fn vdev_audio_create(
    _allocator: *const c_void,
    _type_id: *const c_void,
) -> *mut c_void {
    // AudioServerPlugInDriverRef = &interface_ptr
    (&raw mut VTABLE_PTR).cast::<c_void>()
}

#[no_mangle]
pub static mut VTABLE_PTR: *mut AudioServerPlugInDriverInterface = &raw mut VTABLE;
```

`VTABLE_PTR` 必须真的存在于镜像里并加 `#[no_mangle]`——注释里写得很直白：防止编译器把 `&VTABLE_PTR` 优化成 `VTABLE_PTR` 的值。值语义差一层解引用，宿主第一次调用就是野跳转。

**4. 手工铺 vtable。** `AudioServerPlugInDriverInterface` 的布局必须与 C 头文件逐字段一致：`IUNKNOWN_C_GUTS`（`_reserved` + `QueryInterface` + `AddRef` + `Release`）后跟 19 个插件方法（`crates/vdev-audio/src/vtable.rs:105-267`）。Rust 侧用 `#[repr(C)]` 结构体 + `Option<unsafe extern "C" fn ...>` 逐项镜像，`lib.rs:376` 的 `static mut VTABLE` 把每个槽位填上实现。两个 ABI 细节值得单独记住：

- **REFIID 按值传**。`QueryInterface` 的第二个参数是 `CFUUIDBytes`（16 字节结构体），在 arm64 上拆进 x1:x2 两个寄存器——不是指针。vtable.rs:19 的注释："否则 out 取到 UUID 高位直接崩"。
- **引用计数可以不实现**。`AddRef/Release` 恒返回 1（lib.rs:440-445）：接口对象是插件镜像里的静态变量，宿主 Release 到 0 也不会卸载，BlackHole 同款。`QueryInterface` 则只接受驱动接口 IID（`kAudioServerPlugInDriverInterfaceUUID`，lib.rs:409）和 IUnknown，其余一律 `E_NOINTERFACE`。

## 四、核心机制：sample-time 环形缓冲 + 追赶时钟

这是整个驱动最值得抄走的部分，也是我们 rewriting 三轮才稳定下来的部分。

### 4.1 环形缓冲按 sample time 定位，不是 FIFO

v1 的环回用 FIFO（写一次、读一次）实现，很快在真实使用中暴露问题：输出客户端和输入客户端的生命周期不同步——QuickTime 切走输入设备再切回来，读写顺序对不上，FIFO 语义直接失效。BlackHole 的做法是把环形缓冲当成**时间轴上的窗口**：65536 帧 × 8 声道的 f32 缓冲（约 1.4s @48kHz），写侧按 IO cycle 的 `mOutputTime.mSampleTime` 定位写入，读侧按 `mInputTime.mSampleTime` 定位读取，位置对容量取模：

```rust
// crates/vdev-audio/src/lib.rs:263-264
fn ring_write_out(idx: usize, data: &[f32], out_sample_time: f64, frames: u32) {
    let start =
        ((out_sample_time as i64).rem_euclid(RING_FRAMES as i64)) as usize * CHANNELS;
    // ... 处理回绕的两段 copy_from_slice，更新 RING_LAST_OUTPUT_BITS
```

读侧（`ring_read_in`，lib.rs:282）维护三个原子量：`RING_LAST_OUTPUT_BITS`（上次输出写到的结束 sample time）、`RING_IS_CLEAR`、`RING_RESYNC`。输出还没写到输入要读的位置（`last_output - frames < in_sample_time`）就静音并清空——**宁可静音，不放旧账**。残留旧音频的检测见坑 4。

IO 主循环 `plugin_do_io_operation`（lib.rs:626）里还有两个防御：入口把宿主请求的 frames 夹紧到 ring 容量（异常大的请求不再让切片越界），cycle 缺失时 READ_INPUT 直接输出静音。整条 RT 路径没有分配、没有可能长持锁的 Mutex——跨线程同步全靠原子量。

### 4.2 GetZeroTimeStamp：锚定 + 量化 + 追赶

`GetZeroTimeStamp` 每个周期被宿主调用，回答"设备的 sample time 此刻对应哪个 host time"。返回 `(sample, host, seed)`，seed 变化代表时钟不连续——宿主会当作设备重新上电。v1 的实现基于 Initialize 锚点 + 墙钟连续累计，结果就是坑 3 的"切麦克风失声"。现在的实现完全对齐 BlackHole：

```rust
// crates/vdev-audio/src/lib.rs:174-181
// 纯函数：计划下一拍已到（anchor + prev + period <= now）则推进一拍
fn zts_next_beat(anchor: u64, prev_ticks: f64, period_ticks: f64,
                 now_ticks: u64) -> Option<f64> {
    if anchor + prev_ticks as u64 + period_ticks as u64 <= now_ticks {
        Some(prev_ticks + period_ticks)
    } else {
        None
    }
}
```

语义：时钟按 16384 帧一拍量化（`ZTS_PERIOD_FRAMES`，即属性 `'ring'` 的值，BlackHole 要求 ≥10923），**只有当"计划中的下一拍"在真实时间上已经到达时，才推进一拍**。IO 停止期间没人调用，就不推进；恢复后从断点继续——`host = anchor + prev_ticks`，`sample = count * 16384`，两者永远步进一致，时钟在宿主眼里连续。IO 长停顿后"追赶"也只每次查询推进一拍（lib.rs:800 的单测锁死了这个语义），不会一口气跳几十万帧。

实现上，`Zts` 的三个字段（锚点、拍数、上次拍点的 f64 ticks）全部原子化，`query` 用快照 + CAS 循环推进（lib.rs:102-173）——因为 `GetZeroTimeStamp` 跑在宿主的定时线程上，不能与 `start_io` 的控制线程互相持锁。`zts_next_beat` 被抽成纯函数，边界条件（含"等于"算到拍）有确定性单测。host time 一律用 `mach_absolute_time()` 的 ticks、经 `mach_timebase_info` 换算，**不是纳秒**——返回纳秒会让 coreaudiod 认为设备时钟异常，IO 只跑几个周期就停。

### 4.3 设备的生与死：StartIO/StopIO 计数

设备从空闲到活跃（第一个 IO 客户端出现）时重置时钟锚点并清空 ring；`stop_io` 用 `fetch_update` 做饱和递减，未配对的 stop 不会让计数回绕到 `u32::MAX`（lib.rs:511-536）。这两个细节保证"每次重新开始都是干净的时间线"。

## 五、两台设备、一张路由矩阵、一份 DSP 的教训

单设备稳定后，项目把它扩成两台 8 声道设备（`vdev-audio A/B`，对象 ID 固定 3..7 与 8..12，lib.rs:26-41），并加了三样东西，每样都附带一个架构教训。

**路由矩阵。** `route[src][dst]` 表示 src 设备的输出混入 dst 设备输入的增益，默认对角阵（各自环回）。RT 读侧要无锁，于是每行两个 f32 的位模式打包进一个 `AtomicU64`，`ReadInput` 一次 load 取整行快照：

```rust
// crates/vdev-audio/src/lib.rs:214-219,221-231（节选）
static ROUTE_ROWS: [AtomicU64; N_DEVICES] = [
    AtomicU64::new(ROUTE_ROW_UNIT_GAIN | (ROUTE_ROW_UNROUTED << 32)), // A→A
    AtomicU64::new(ROUTE_ROW_UNROUTED | (ROUTE_ROW_UNIT_GAIN << 32)), // B→B
];
const fn route_row_pack(gains: [f32; N_DEVICES]) -> u64 {
    (gains[0].to_bits() as u64) | ((gains[1].to_bits() as u64) << 32)
}
```

宿主通过自定义属性 `'vrut'` 写入（CFString `"r00,r01,r10,r11"`），写侧整行一次 store。跨设备读侧（`ring_peek`，lib.rs:330）窥视别的设备的 ring——代码注释诚实地标注这是**非实时安全的实验特性**：若 src 写侧整圈反超（读侧停顿超过约 1.4s），会读到新旧混合样本，但 f32 字宽读写不撕裂，是音频伪影而非内存安全问题。

**DSP。** RBJ cookbook 三段 EQ（120Hz 低架 / 1kHz 峰值 / 8kHz 高架）+ 总增益 + tanh 软限幅（`crates/vdev-audio/src/dsp.rs`），系数只在设参时重算，处理路径纯乘加。教训在于放置位置：多设备重构时 DSP 一度是全局单例，结果两台设备的 biquad 状态互相串扰、属性线程与两个 IO 线程挤一把锁（见坑 6）。现在是每设备一个 `OnceLock<Mutex<Dsp>>`（lib.rs:203-207），各 IO 线程只锁自己的实例。

**自定义属性。** `'vdsp'`（DSP 参数）与 `'vrut'`（路由）都走 CoreAudio 自定义属性协议：在设备的 `'cust'`（`kAudioObjectPropertyCustomPropertyInfoList`）里注册 `CustomPropertyInfo`，dataType 用 `'cfst'`（props.rs:482-490），值是 CFString。为什么是字符串——见坑 5。控制类对象（音量/静音）也搭齐了（`vlme`/`mute` 类、`vlsc`/`lcdv`/`mute` 选择器），Audio MIDI Setup 里推子可见可用。

## 六、踩坑实录

以下六个案例全部来自真实 commit 与独立审查记录，按"现象 → 定位 → 修法"复述。

### 坑 1：HAL 错误码是四字码，十进制值手打必错

**现象**：审查发现 props.rs 里 4 个 HAL 错误码常量（`kAudioHardwareBadObjectError` 等）的十进制值全是编造的——注释与真实值对不上，`'who?'` 这类错误码被写成了毫不相干的数。
**定位**：CoreAudio 的 OSStatus 错误码大量是 FourCharCode：四个 ASCII 字节按首字节在高位打包。`'who?'` = `0x77686F3F` = 2003332927。手打十进制没有任何机制防错，错一位就是一个不存在的错误。
**修法**：改成十六进制四字码字面量，并加回归测试用 ASCII 逐字断言（commit `e7caea0`）：

```rust
// crates/vdev-audio/src/props.rs:194-197
const BAD_OBJ: OSStatus = 0x216F_626A_u32 as i32;  // '!obj'
const BAD_PROP: OSStatus = 0x7768_6F3F_u32 as i32; // 'who?'
const BAD_SIZE: OSStatus = 0x2173_697A_u32 as i32; // '!siz'
const BAD_SEL: OSStatus = 0x756E_6F70_u32 as i32;  // 'unop'

// crates/vdev-audio/src/lib.rs:730-741：四字码 → OSStatus 的纯函数断言
const fn fcc(s: [u8; 4]) -> i32 {
    (((s[0] as u32) << 24) | ((s[1] as u32) << 16)
        | ((s[2] as u32) << 8) | s[3] as u32) as i32
}
// test_error_code_four_char_codes: assert_eq!(BAD_PROP, fcc(*b"who?"))
```

**教训**：抄 SDK 常量时保留四字码形式（或直接用 `'x'` 打包函数），十进制只出现在注释里对照。

### 坑 2：kAudioObjectPropertyOwner 的值在 macOS 26 变了

**现象**：单设备版一切正常；扩成两台设备后，coreaudiod 反复初始化、CPU 空转，设备时有时无。
**定位**：逐属性对照 BlackHole 后发现 macOS 26 的 `kAudioObjectPropertyOwner` 是 `'stdv'`（0x73746476），而代码按旧头文件用了 `'owne'`（0x6F776E65）。单设备时宿主容忍 owner 缺失，多设备时它依赖 owner 建立 plug-in → box → device 的对象图，图建不起来就反复重试。
**修法**：`SEL_OWNE` 改为 `'stdv'`（crates/vdev-audio/src/props.rs:18，commit `3330b86`），同时补齐路由矩阵。变量名保留 `SEL_OWNE` 只是历史痕迹。
**教训**：四字码常量的"值"不是跨版本契约，`bcls/clas/owne/ownd` 这类对象图属性，装机异常时第一个 diff。

### 坑 3：时钟跳变——切走麦克风再切回来，永远没声音

**现象**：QuickTime 切到别的输入设备再切回 vdev-audio，没声音；重启 QuickTime 无效，只有重启 coreaudiod 才恢复。
**定位**：v1 的 `GetZeroTimeStamp` 基于 Initialize 锚点 + 墙钟连续累计。设备 IO 停止期间（切走的那几秒）墙钟照走，切回后 sample time 一次性跳变几十万帧且 seed 不变——coreaudiod 判定设备时钟异常，IO 永久停摆。
**修法**：两步。`df4d52e` 先改成"被调用时才增量推进"；`b8a60b5` 再完整对齐 BlackHole：锚定 + 16384 帧量化 + 追赶推进（即 4.2 节的状态机），`start_io` 空闲→活跃时才重置锚点。
**教训**：sample time 的连续性是宿主眼中的"设备还活着"；任何基于墙钟外推的时钟模型，都过不了"客户端反复启停"这一关。

### 坑 4：切回后先放约 1 秒旧音频

**现象**：坑 3 修完后，切回 vdev-audio 会先放出切走之前/期间写入的旧声音（约 1 秒），然后才是新音频。
**定位**：sample-time 环形缓冲下，切回时 input 时间戳落后 output，按时间窗读到的自然是旧位置的残留数据。
**修法**（commit `e0f7bda`）：`ring_read_in` 开头检测不同步——

```rust
// crates/vdev-audio/src/lib.rs:286-295（节选）
if last_output - in_sample_time > rate {
    if !RING_RESYNC[idx].swap(true, Ordering::SeqCst) {
        ring_clear(idx);
    }
    out.fill(0.0);
    return;
}
```

落后超过 1 秒采样率就清空缓冲一次、静音直到追平。一个反直觉的细节写在了注释和教训里：检测信号要用 **input 落后 output 的差距**，而不是 input 自身相邻两次的跳变——切回后宿主可能让 input 从切走前的位置平滑继续，自身跳变只有一帧，抓不住。
**教训**：时间窗缓冲的"陈旧数据"问题要在读侧用时间差显式检测，寄希望于宿主重置时间戳是不可靠的。

### 坑 5：自定义属性跨进程传不过去，客户端读到 'who?'

**现象**：`'vdsp'` 属性最初注册为 `dataType=None` + 裸 `4×f32`。驱动侧 Set 正常，但客户端进程（CLI）读永远返回 `kAudioHardwareUnknownPropertyError`。
**定位**：属性值要经 coreaudiod 在驱动进程与客户端进程之间 marshal，`dataType=None` 的裸二进制没有跨进程表示。
**修法**（commit `b8f7ef6`）：在 `'cust'` 列表里以 `dataType='cfst'` 注册，值改为 CFString `"gain,low,mid,high"`；顺带修正了 `AudioServerPlugInCustomPropertyInfo` 的字段数（三个字段，含 `mQualifierDataType`）。实测 CLI set/get/reset 通畅，gain+6dB 精确 +6dB。
**教训**：HAL 自定义属性的类型，按宿主能 marshal 的来选——CFString 是最省事的通用载体。

### 坑 6：全局单例 DSP 与共享 scratch 的跨设备竞争

**现象**：独立审查（commit `e7caea0` 一并修复）指出：多设备化之后 DSP 仍是全局单例——两台设备的 biquad 滤波状态 `s1/s2` 互相串扰；混音 scratch 缓冲 `MIX_BUF` 全设备共享，两个 IO 线程写写竞争，属 UB + 音频损坏。
**定位**：coreaudiod **每设备一个 IO 线程**，任何"每设备一份才对"的状态放成全局，都立刻变成数据竞争。
**修法**：DSP 改 `OnceLock<Mutex<Dsp>>` 每设备一份（lib.rs:203），scratch 改 `MIX_BUFS[idx]` 每设备一份（lib.rs:246），RT 路径上每设备只碰自己的槽位；同设备属性线程与 IO 线程的短暂锁竞争，注释里明说"持锁窗口为单周期乘加，接受"。
**教训**：多设备驱动的默认架构是"每设备一份状态 + 原子量共享全局配置"，不要先全局再拆。

另有一条贯穿始终的调试纪律，值得单独写一行：**coreaudiod 一旦空转，立即停手**——卸载问题驱动、重启一次 coreaudiod，仍不行就重启电脑；反复 `killall -9` 加装卸驱动会把它的 XPC 状态搞坏到系统级损坏。正确的姿势是先在独立进程里用 dlopen + 工厂 + Initialize 的 C 测试跑通全链路，再装系统。

## 七、构建与运行

`make` 环回自测除 Rust 工具链外还需要 `ffmpeg` 与 `python3`（见 `test_loopback.sh`）；安装需要管理员权限（osascript 提权，含 `killall coreaudiod`）：

```bash
cd crates/vdev-audio
make install      # 构建 MH_BUNDLE + Developer ID 签名 + 装 /Library/Audio/Plug-Ins/HAL + 重启 coreaudiod
make uninstall    # 卸载
make test         # 环回自测：440Hz 正弦播到输出流，同时从输入流录 3s，非静音 > 20% 即 PASS
```

装好后任意 App 的音频设备列表里会出现 **vdev-audio A / vdev-audio B**（各 8 进 8 出），输出当播放端、输入当麦克风端。控制 CLI 与驱动同 crate 交付：

```bash
cargo build -p vdev-audio --bin vdev-audio-ctl --release
target/release/vdev-audio-ctl                  # 读 DSP 参数（gain/low/mid/high dB）
target/release/vdev-audio-ctl set 6 0 0 0      # 总增益 +6dB
target/release/vdev-audio-ctl reset            # 全部归零（直通）
target/release/vdev-audio-ctl route            # 读路由矩阵
target/release/vdev-audio-ctl route 1 0 0 1    # 默认：A/B 各自环回
target/release/vdev-audio-ctl route 1 1 0 0    # A 的输出同时送进 B 的输入
```

## 八、现状与局限

如实交代，方便读者评估要不要抄：

- **格式**：32-bit float 交错、8 声道（7.1 布局），采样率白名单 44.1/48 kHz（`'nsr#'`），缓冲帧大小固定报 512（Set `'fsiz'` 接受但不变更）。
- **跨设备路由是非实时安全的实验特性**：默认路由不触发该路径；读侧停顿超过一个 ring 时长会读到新旧混合样本（代码注释明示）。
- **控制是"UI 级"的**：音量/静音对象让 Audio MIDI Setup 可见可动，Set 直接成功但值不参与信号链。
- **静态设备模型**：不支持宿主动态 `CreateDevice`（返回 `'unop'`）；`QueryInterface` 只认两个 IID；引用计数是形式上的。
- **`vdev-audio-ctl` 只操作枚举到的第一台 vdev-audio 设备**（按名字包含 `vdev-audio` 匹配），DSP/路由参数是每设备属性，操作 B 需要自行改匹配逻辑。
- 内存模型依赖"每设备一个 IO 线程"的宿主契约（ring 为 `static mut`，靠该契约把并发约束在"每设备串行 + 少量原子量跨线程"内）；入口处对宿主请求的 frames 做了夹紧兜底。

Windows 侧的对应实现（PortCls/WaveRT 内核驱动 `vdev-audio-win`）见系列另一篇，两边的挑战完全不同：macOS 侧的难点是宿主契约与时钟，Windows 内核侧的难点是 ABI 与蓝屏。

## 九、写在最后

回看整个过程，`vdev-audio` 的三轮重构（FIFO → 墙钟 → sample-time 环形缓冲 + 追赶时钟）本质上是在逐步逼近 BlackHole 已经给出的答案。给想写同类驱动的读者三条建议：

1. **先抄对样本**。BlackHole 是完整的、经过大规模部署验证的实现——时钟模型、属性清单、打包方式都能逐项 diff。我们的每个大坑（时钟、owner、环回）都是"没对齐它的地方"。
2. **隔离验证优先于装机实测**。一个 dlopen 加载测试（工厂 → QueryInterface → Initialize → 属性全查询 → StartIO → IO 回调）能在不碰 coreaudiod 的情况下覆盖 90% 的正确性，剩下的 10% 才值得冒"系统音频服务损坏、重启电脑"的风险。
3. **对宿主保持敬畏**。coreaudiod 不会告诉你哪里错了——它只会沉默、空转或停摆。错误码、属性、时钟语义这些"官面契约"，一个字节都不能凭印象写。

仓库与完整代码：[gqf2008/vdev](https://github.com/gqf2008/vdev)（`crates/vdev-audio`，约 2700 行 Rust 含驱动、CLI 与测试）。系列其余八篇见 `docs/community/`。
