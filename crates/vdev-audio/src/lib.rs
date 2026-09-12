//! vdev-audio：100% Rust 虚拟声卡（CoreAudio HAL `AudioServerPlugIn`）。
//! 一个设备含输出流 + 输入流，输出环回输入（像 BlackHole/Soundflower）。
//! 系统只认 HAL 插件（/Library/Audio/Plug-Ins/HAL/*.driver），语言无关。

// 档位对齐 vdev-audio-win/driver：CoreAudio C 头镜像结构与 DSP 数值转换
// 属 FFI 惯例，pedantic 中与之冲突的子集在 crate 根放开。
#![allow(clippy::cast_possible_truncation)] // FFI 结构字段宽度转换
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)] // 采样数/时长换算的整数↔浮点
#![allow(clippy::struct_field_names)] // CoreAudio C 头结构体 m 前缀字段按原样镜像

use std::ffi::c_void;

mod dsp;
mod vtable;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use vtable::{
    pid_t, AudioObjectID, AudioObjectPropertyAddress, AudioServerPlugInClientInfo,
    AudioServerPlugInDriverInterface, AudioServerPlugInDriverRef, AudioServerPlugInHostRef,
    AudioServerPlugInIOCycleInfo, AudioStreamBasicDescription, AudioValueRange, Boolean,
    CFUUIDBytes, OSStatus, HRESULT, LPVOID, REFIID, ULONG,
};

// ---------------- 对象 ID 固定分配（2 个虚拟设备） ----------------
const OBJ_PLUGIN: AudioObjectID = 1;
const OBJ_BOX: AudioObjectID = 2;
// 设备 A
const DEV_A: AudioObjectID = 3;
const A_OUT: AudioObjectID = 4;
const A_IN: AudioObjectID = 5;
const A_VOL: AudioObjectID = 6;
const A_MUTE: AudioObjectID = 7;
// 设备 B
const DEV_B: AudioObjectID = 8;
const B_OUT: AudioObjectID = 9;
const B_IN: AudioObjectID = 10;
const B_VOL: AudioObjectID = 11;
const B_MUTE: AudioObjectID = 12;
const N_DEVICES: usize = 2;
// 兼容旧名（单设备版遗留别名；多设备重构后 props.rs 已改用 DEV_A/A_OUT 等，
// 刻意保留作旧引用/文档对照，不删除）
#[allow(dead_code)]
const OBJ_DEVICE: AudioObjectID = DEV_A;
#[allow(dead_code)]
const OBJ_STREAM_OUTPUT: AudioObjectID = A_OUT;
#[allow(dead_code)]
const OBJ_STREAM_INPUT: AudioObjectID = A_IN;
#[allow(dead_code)]
const OBJ_VOLUME: AudioObjectID = A_VOL;
#[allow(dead_code)]
const OBJ_MUTE: AudioObjectID = A_MUTE;

// 设备元数据
pub(crate) struct DevMeta {
    pub name: &'static str,
    pub uid: &'static str,
}
pub(crate) const DEVS: [DevMeta; N_DEVICES] = [
    DevMeta {
        name: "vdev-audio A",
        uid: "vdev-audio-A-device",
    },
    DevMeta {
        name: "vdev-audio B",
        uid: "vdev-audio-B-device",
    },
];

// 对象 ID → 设备索引（0=A, 1=B）；插件/盒返回 None
pub(crate) fn dev_index(obj: AudioObjectID) -> Option<usize> {
    match obj {
        DEV_A | A_OUT | A_IN | A_VOL | A_MUTE => Some(0),
        DEV_B | B_OUT | B_IN | B_VOL | B_MUTE => Some(1),
        _ => None,
    }
}
// 对象是否属于某设备（设备本身 / 流 / 控制）；多设备辅助 API，暂无调用点，刻意保留
#[allow(dead_code)]
pub(crate) fn obj_in_device(obj: AudioObjectID, idx: usize) -> bool {
    dev_index(obj) == Some(idx)
}
// 设备是否在运行（有活跃 IO 客户端）
pub(crate) fn device_running(obj: AudioObjectID) -> u32 {
    u32::from(dev_index(obj).is_some_and(|i| IO_CLIENTS[i].load(Ordering::SeqCst) > 0))
}

// ---------------- 全局状态 ----------------
static HOST: Mutex<Option<usize>> = Mutex::new(None);
static SAMPLE_RATE: AtomicU64 = AtomicU64::new(48_000);
static ZERO_SEED: AtomicU64 = AtomicU64::new(1);
// GetZeroTimeStamp —— BlackHole 同款：锚定 + 环缓冲量化 + 追赶推进。
// sample = N * ZTS_PERIOD；host = anchor + N * ticks_per_period。
// 只在“计划下一拍已到”时推进一拍（Float64 累计 ticks），IO 停止期间不推进，
// 恢复后从断点继续，coreaudiod 看到的时钟永远连续（切换输入设备不再失声）。
const ZTS_PERIOD_FRAMES: u64 = 16384; // kAudioDevicePropertyZeroTimeStampPeriod（≥10923）

// ---- 每设备状态 ----
// ZTS 字段全部原子化（M4）：GetZeroTimeStamp 在每个 IO 周期被宿主定时线程调用，
// 不能与 initialize/start_io 的控制线程路径互相持锁；快照读取 + CAS 推进一拍。
struct Zts {
    anchor_ticks: AtomicU64,
    count: AtomicU64,
    prev_ticks_bits: AtomicU64, // f64 位模式（Float64 累计 ticks）
}
impl Zts {
    const fn new() -> Self {
        Self {
            anchor_ticks: AtomicU64::new(0),
            count: AtomicU64::new(0),
            prev_ticks_bits: AtomicU64::new(0),
        }
    }
    // 复位（initialize/start_io 调用，控制线程）：count/prev 先清零、anchor 最后写
    //（anchor != 0 是“已初始化”标记，读侧先读 anchor，SeqCst 保证看到新 anchor
    // 就看不到旧的 count/prev）
    fn reset(&self, anchor: u64) {
        self.count.store(0, Ordering::SeqCst);
        self.prev_ticks_bits.store(0, Ordering::SeqCst);
        self.anchor_ticks.store(anchor, Ordering::SeqCst);
    }
    // 快照查询 + 到点推进一拍（每周期最多推进一拍，BlackHole 同款语义）。
    // 返回 (sample, host_ticks)；anchor == 0（未初始化）返回 (0, now)。
    // CAS 循环保证并发推进恰好一次；period_ticks 由调用方按采样率换算传入
    //（不涉及系统调用，可对本地实例做确定性单测）。
    fn query(&self, now_ticks: u64, period_ticks: f64) -> (f64, u64) {
        let anchor = self.anchor_ticks.load(Ordering::SeqCst);
        if anchor == 0 {
            return (0.0, now_ticks);
        }
        let mut count = self.count.load(Ordering::SeqCst);
        let mut prev_ticks = f64::from_bits(self.prev_ticks_bits.load(Ordering::SeqCst));
        if let Some(mut next) = zts_next_beat(anchor, prev_ticks, period_ticks, now_ticks) {
            let mut cur = prev_ticks.to_bits();
            loop {
                match self.prev_ticks_bits.compare_exchange_weak(
                    cur,
                    next.to_bits(),
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    // 推进成功：count 与推进一一对应地 +1
                    Ok(_) => {
                        count = self.count.fetch_add(1, Ordering::SeqCst) + 1;
                        prev_ticks = next;
                        break;
                    }
                    Err(actual) => {
                        prev_ticks = f64::from_bits(actual);
                        if let Some(newer) =
                            zts_next_beat(anchor, prev_ticks, period_ticks, now_ticks)
                        {
                            // 别的线程也在推进且仍未到位：按其最新值重算下一拍再试
                            next = newer;
                            cur = actual;
                        } else {
                            // 别的线程已把拍子推进到位：跟随其快照返回
                            count = self.count.load(Ordering::SeqCst);
                            break;
                        }
                    }
                }
            }
        }
        // sample 时间不回转；count*ZTS_PERIOD_FRAMES 在实际时间尺度内不会溢出，
        // 用 wrapping_mul 避免理论溢出 panic
        (
            count.wrapping_mul(ZTS_PERIOD_FRAMES) as f64,
            anchor + prev_ticks as u64,
        )
    }
}
// 纯函数：计划下一拍已到（anchor + prev + period <= now）则推进一拍，返回新 prev
fn zts_next_beat(anchor: u64, prev_ticks: f64, period_ticks: f64, now_ticks: u64) -> Option<f64> {
    if anchor + prev_ticks as u64 + period_ticks as u64 <= now_ticks {
        Some(prev_ticks + period_ticks)
    } else {
        None
    }
}
static ZTS: [Zts; N_DEVICES] = [Zts::new(), Zts::new()];
static IO_CLIENTS: [std::sync::atomic::AtomicU32; N_DEVICES] = [
    std::sync::atomic::AtomicU32::new(0),
    std::sync::atomic::AtomicU32::new(0),
];
// ---- BlackHole 式环形缓冲：按 sample time 定位（不是 FIFO head/tail）----
// ring 容量 65536 帧 × 8ch；输出（WriteMix）写入 mOutputTime 对应位置，
// 输入（ReadInput）读取 mInputTime 对应位置；输出未跟上时输出静音并清空。
// 同设备环形读写都在该设备自己的 IO 线程内串行执行（coreaudiod 每设备一个
// IO 线程）；唯一的跨线程访问是跨设备路由（ring_peek 读别的设备的 ring，
// 见 do_io_operation 内的说明与 ring_peek 文档）。
const RING_FRAMES: usize = 65536;
const CHANNELS: usize = 8;
#[allow(static_mut_refs)]
static mut RING_BUFS: [[f32; RING_FRAMES * CHANNELS]; N_DEVICES] =
    [[0.0; RING_FRAMES * CHANNELS]; N_DEVICES];

// DSP 管线（EQ + 增益 + 软限幅），参数由自定义属性 'vdsp' 控制。
// 每设备一份实例（M3）：滤波状态（biquad s1/s2）不跨设备串扰；每设备一把锁，
// 各设备 IO 线程只锁自己的实例，无跨设备竞争（属性线程与同设备 RT 线程的
// 短暂竞争接受，持锁窗口为单周期乘加）。
static DSPS: [std::sync::OnceLock<Mutex<dsp::Dsp>>; N_DEVICES] =
    [std::sync::OnceLock::new(), std::sync::OnceLock::new()];
pub(crate) fn dsp(idx: usize) -> &'static Mutex<dsp::Dsp> {
    DSPS[idx].get_or_init(|| Mutex::new(dsp::Dsp::default()))
}
// 路由矩阵：route[src][dst] = src 设备输出 → dst 设备输入的增益（0=不路由）。
// RT 路径无锁（M4）：每行（src）两个 f32 的位模式打包进一个 AtomicU64，
// ReadInput 一次 load 取整行快照；'vrut' 属性写侧逐行 store。默认对角线
// [[1,0],[0,1]]：各自独立环回。
const ROUTE_ROW_UNIT_GAIN: u64 = 0x3F80_0000; // 1.0f32 位模式
const ROUTE_ROW_UNROUTED: u64 = 0; // 0.0f32 位模式
static ROUTE_ROWS: [AtomicU64; N_DEVICES] = [
    // [1.0, 0.0]：A→A 环回、A→B 断开
    AtomicU64::new(ROUTE_ROW_UNIT_GAIN | (ROUTE_ROW_UNROUTED << 32)),
    // [0.0, 1.0]：B→B 环回、B→A 断开
    AtomicU64::new(ROUTE_ROW_UNROUTED | (ROUTE_ROW_UNIT_GAIN << 32)),
];
// 纯函数：一行路由（到各 dst 的增益）打包/按 dst 解包（位模式精确往返）
const fn route_row_pack(gains: [f32; N_DEVICES]) -> u64 {
    (gains[0].to_bits() as u64) | ((gains[1].to_bits() as u64) << 32)
}
fn route_row_lane(row: u64, dst: usize) -> f32 {
    let bits = if dst == 0 {
        row as u32
    } else {
        (row >> 32) as u32
    };
    f32::from_bits(bits)
}
// 'vrut' 属性写侧：整行一次原子 store
pub(crate) fn route_set_row(src: usize, gains: [f32; N_DEVICES]) {
    ROUTE_ROWS[src].store(route_row_pack(gains), Ordering::SeqCst);
}
// 属性读侧：两行快照（非 RT 路径，逐行 load 已足够）
pub(crate) fn route_rows() -> [u64; N_DEVICES] {
    [
        ROUTE_ROWS[0].load(Ordering::SeqCst),
        ROUTE_ROWS[1].load(Ordering::SeqCst),
    ]
}
// IO scratch：route 混音的临时缓冲，每设备一份（M2）——各设备 IO 线程只写自己
// 索引的槽位，消除原先共享单块的跨线程写写竞争（UB + 音频损坏）。
#[allow(static_mut_refs)]
static mut MIX_BUFS: [[f32; RING_FRAMES * CHANNELS]; N_DEVICES] =
    [[0.0; RING_FRAMES * CHANNELS]; N_DEVICES];
// 上次输出写入的“结束 sample time”（f64 位模式）+ 缓冲是否干净
static RING_LAST_OUTPUT_BITS: [AtomicU64; N_DEVICES] = [AtomicU64::new(0), AtomicU64::new(0)];
static RING_IS_CLEAR: [AtomicBool; N_DEVICES] = [AtomicBool::new(true), AtomicBool::new(true)];
// 输入/输出时间戳不同步（切换设备后 input 落后 output）→ 清空缓冲并静音直到追平
static RING_RESYNC: [AtomicBool; N_DEVICES] = [AtomicBool::new(false), AtomicBool::new(false)];

#[allow(static_mut_refs)]
fn ring_clear(idx: usize) {
    unsafe {
        RING_BUFS[idx].fill(0.0);
    }
    RING_IS_CLEAR[idx].store(true, Ordering::SeqCst);
}

// 输出（WriteMix）：把混合数据写入 output sample time 对应的 ring 位置
fn ring_write_out(idx: usize, data: &[f32], out_sample_time: f64, frames: u32) {
    let start = ((out_sample_time as i64).rem_euclid(RING_FRAMES as i64)) as usize * CHANNELS;
    let n = data.len();
    unsafe {
        let ring = &mut RING_BUFS[idx];
        if start + n <= RING_FRAMES * CHANNELS {
            ring[start..start + n].copy_from_slice(data);
        } else {
            let first = RING_FRAMES * CHANNELS - start;
            ring[start..].copy_from_slice(&data[..first]);
            ring[..n - first].copy_from_slice(&data[first..]);
        }
    }
    let end = out_sample_time + f64::from(frames);
    RING_LAST_OUTPUT_BITS[idx].store(end.to_bits(), Ordering::SeqCst);
    RING_IS_CLEAR[idx].store(false, Ordering::SeqCst);
}

// 输入（ReadInput）：从 input sample time 对应的 ring 位置读；输出未跟上则静音+清空
fn ring_read_in(idx: usize, out: &mut [f32], in_sample_time: f64, frames: u32) {
    let last_output = f64::from_bits(RING_LAST_OUTPUT_BITS[idx].load(Ordering::SeqCst));
    let rate = SAMPLE_RATE.load(Ordering::SeqCst) as f64;
    // 切换设备后 input 时间戳会落后 output（读到的是切走前/切走期间的旧音频）。
    if last_output - in_sample_time > rate {
        if !RING_RESYNC[idx].swap(true, Ordering::SeqCst) {
            ring_clear(idx);
        }
        out.fill(0.0);
        return;
    }
    if RING_RESYNC[idx].load(Ordering::SeqCst) && (last_output - in_sample_time) <= rate {
        RING_RESYNC[idx].store(false, Ordering::SeqCst);
    }
    if last_output - f64::from(frames) < in_sample_time {
        out.fill(0.0);
        if !RING_IS_CLEAR[idx].load(Ordering::SeqCst) {
            ring_clear(idx);
        }
        return;
    }
    let start = ((in_sample_time as i64).rem_euclid(RING_FRAMES as i64)) as usize * CHANNELS;
    let n = out.len();
    unsafe {
        let ring = &RING_BUFS[idx];
        if start + n <= RING_FRAMES * CHANNELS {
            out.copy_from_slice(&ring[start..start + n]);
        } else {
            let first = RING_FRAMES * CHANNELS - start;
            out[..first].copy_from_slice(&ring[start..]);
            out[first..].copy_from_slice(&ring[..n - first]);
        }
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}
// 跨设备路由读侧（非本设备 IO 线程）窥视 src 设备 ring 的指定窗口。
// 并发契约（M2，非实时安全的实验特性）：写侧（src 设备 IO 线程 ring_write_out）
// 在写完数据后才以 SeqCst 发布“已写结束 sample time”（RING_LAST_OUTPUT_BITS），
// 本函数开头对该原子量做一次快照——正常情况下已保证读窗口 [in, in+frames) 完全
// 落在已写区域内、与写侧正在写的区域不相交。若 src 写侧整圈反超（读侧停顿超过
// ring 时长约 1.4s@48kHz），会读到新旧混合样本（音频伪影，非内存安全问题：
// f32 为字宽读写不撕裂，且读到的引用只做逐元素拷贝）。默认路由不触发该路径。
#[allow(static_mut_refs)]
fn ring_peek(idx: usize, out: &mut [f32], in_sample_time: f64) {
    let frames = (out.len() / CHANNELS) as f64;
    let last_output = f64::from_bits(RING_LAST_OUTPUT_BITS[idx].load(Ordering::SeqCst));
    if last_output - frames < in_sample_time {
        out.fill(0.0);
        return;
    }
    let start = ((in_sample_time as i64).rem_euclid(RING_FRAMES as i64)) as usize * CHANNELS;
    let n = out.len();
    unsafe {
        let ring = &RING_BUFS[idx];
        if start + n <= RING_FRAMES * CHANNELS {
            out.copy_from_slice(&ring[start..start + n]);
        } else {
            let first = RING_FRAMES * CHANNELS - start;
            out[..first].copy_from_slice(&ring[start..]);
            out[first..].copy_from_slice(&ring[..n - first]);
        }
    }
}

fn mach_now_ticks() -> u64 {
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
    }
    unsafe { mach_absolute_time() }
}
fn mach_ns_per_tick() -> f64 {
    unsafe extern "C" {
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    }
    let mut info = MachTimebaseInfo { numer: 1, denom: 1 };
    unsafe { mach_timebase_info(&raw mut info) };
    f64::from(info.numer) / f64::from(info.denom)
}

// ---------------- 工厂函数（Info.plist CFPlugInFactories 指向） ----------------
#[no_mangle]
pub extern "C" fn vdev_audio_create(
    _allocator: *const c_void,
    _type_id: *const c_void,
) -> *mut c_void {
    // 返回 AudioServerPlugInDriverRef = &interface_ptr（AudioServerPlugInDriverInterface**）
    (&raw mut VTABLE_PTR).cast::<c_void>()
}

static mut VTABLE: AudioServerPlugInDriverInterface = AudioServerPlugInDriverInterface {
    _reserved: std::ptr::null_mut(),
    query_interface: Some(plugin_query_interface),
    add_ref: Some(plugin_add_ref),
    release: Some(plugin_release),
    initialize: Some(plugin_initialize),
    create_device: Some(plugin_create_device),
    destroy_device: Some(plugin_destroy_device),
    add_device_client: Some(plugin_add_device_client),
    remove_device_client: Some(plugin_remove_device_client),
    perform_device_config_change: Some(plugin_perform_device_config_change),
    abort_device_config_change: Some(plugin_abort_device_config_change),
    has_property: Some(plugin_has_property),
    is_property_settable: Some(plugin_is_property_settable),
    get_property_data_size: Some(plugin_get_property_data_size),
    get_property_data: Some(plugin_get_property_data),
    set_property_data: Some(plugin_set_property_data),
    start_io: Some(plugin_start_io),
    stop_io: Some(plugin_stop_io),
    get_zero_time_stamp: Some(plugin_get_zero_time_stamp),
    will_do_io_operation: Some(plugin_will_do_io_operation),
    begin_io_operation: Some(plugin_begin_io_operation),
    do_io_operation: Some(plugin_do_io_operation),
    end_io_operation: Some(plugin_end_io_operation),
};

// 指向接口结构体的指针（BlackHole 同款语义：factory 返回 AudioServerPlugInDriverRef）。
// #[no_mangle] 导出，防止编译器把 &VTABLE_PTR 优化成 VTABLE_PTR 的值（值语义不同）。
#[no_mangle]
pub static mut VTABLE_PTR: *mut AudioServerPlugInDriverInterface = &raw mut VTABLE;

// ================= IUnknown =================
// kAudioServerPlugInDriverInterfaceUUID（AudioServerPlugIn.h，已对本机 SDK 头文件核对）
const IID_PLUGIN_INTERFACE: [u8; 16] = [
    0xEE, 0xA5, 0x77, 0x3D, 0xCC, 0x43, 0x49, 0xF1, 0x8E, 0x00, 0x8F, 0x96, 0xE7, 0xD2, 0x3B, 0x17,
];
// IUnknown：00000000-0000-0000-C000-000000000046
const IID_IUNKNOWN: [u8; 16] = [0, 0, 0, 0, 0, 0, 0, 0, 0xC0, 0, 0, 0, 0, 0, 0, 0x46];
// M6：仅接受本插件驱动接口与 IUnknown（纯函数，回归见 tests::test_query_interface_iid）
fn iid_supported(uuid: &CFUUIDBytes) -> bool {
    uuid.m_data == IID_PLUGIN_INTERFACE || uuid.m_data == IID_IUNKNOWN
}
unsafe extern "C" fn plugin_query_interface(
    _driver: *mut c_void,
    uuid: REFIID, // CFUUIDBytes 按值传递（x1:x2），out_interface 在第 4 寄存器 x3
    out_interface: *mut LPVOID,
) -> HRESULT {
    if out_interface.is_null() {
        return 0x8000_4003_u32 as HRESULT /* E_POINTER */;
    }
    // M6：其余 IID 一律按 COM 契约返回 E_NOINTERFACE（原实现对任何 UUID 都返回 S_OK）
    if !iid_supported(&uuid) {
        return 0x8000_4002_u32 as HRESULT /* E_NOINTERFACE */;
    }
    // SAFETY：IUnknown 契约保证 out_interface 非空时指向宿主提供的可写 LPVOID 槽位
    let vtable = &raw mut VTABLE_PTR;
    unsafe {
        *out_interface = vtable as LPVOID;
    }
    0 // S_OK
}
// AddRef/Release 恒返回 1：接口对象是插件镜像内的静态生命周期（&VTABLE_PTR 指向
// 静态 VTABLE），宿主 Release 到 0 也不卸载/销毁（BlackHole 同款语义），
// 故不实现真实引用计数。
unsafe extern "C" fn plugin_add_ref(_driver: *mut c_void) -> ULONG {
    1
}
unsafe extern "C" fn plugin_release(_driver: *mut c_void) -> ULONG {
    1
}

// ================= 生命周期 =================
unsafe extern "C" fn plugin_initialize(
    _driver: AudioServerPlugInDriverRef,
    in_host: AudioServerPlugInHostRef,
) -> OSStatus {
    *HOST
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(in_host as usize);
    let anchor = mach_now_ticks();
    for zts in &ZTS {
        zts.reset(anchor);
    }
    0
}

unsafe extern "C" fn plugin_create_device(
    _driver: AudioServerPlugInDriverRef,
    _desc: *const c_void,
    _client: *const AudioServerPlugInClientInfo,
    _out_id: *mut AudioObjectID,
) -> OSStatus {
    // M7：本插件只提供 initialize 阶段静态声明的两台设备（'dev#' 固定返回
    // DEV_A/DEV_B），不支持宿主动态创建设备，一律返回
    // kAudioHardwareUnsupportedOperationError（原实现对任何请求都回 DEV_A）。
    BAD_SEL
}
unsafe extern "C" fn plugin_destroy_device(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
) -> OSStatus {
    0
}
unsafe extern "C" fn plugin_add_device_client(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    0
}
unsafe extern "C" fn plugin_remove_device_client(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    0
}
unsafe extern "C" fn plugin_perform_device_config_change(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _action: u64,
    _info: *mut c_void,
) -> OSStatus {
    0
}
unsafe extern "C" fn plugin_abort_device_config_change(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _action: u64,
    _info: *mut c_void,
) -> OSStatus {
    0
}

// ================= IO =================
unsafe extern "C" fn plugin_start_io(
    _driver: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _client: u32,
) -> OSStatus {
    let Some(idx) = dev_index(id) else { return 0 };
    if IO_CLIENTS[idx].fetch_add(1, Ordering::SeqCst) == 0 {
        // 设备从空闲→活跃：重置时间锚点 + 清空 ring（BlackHole 同款）
        ZTS[idx].reset(mach_now_ticks());
        ring_clear(idx);
    }
    0
}
unsafe extern "C" fn plugin_stop_io(
    _driver: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _client: u32,
) -> OSStatus {
    if let Some(idx) = dev_index(id) {
        // M5：饱和递减——未配对的 stop_io 不允许计数回绕到 u32::MAX
        //（fetch_update 闭包返回 None 即已是 0，不写回）
        let _ =
            IO_CLIENTS[idx].fetch_update(Ordering::SeqCst, Ordering::SeqCst, |c| c.checked_sub(1));
    }
    0
}
unsafe extern "C" fn plugin_get_zero_time_stamp(
    _driver: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _client: u32,
    out_sample: *mut f64,
    out_host: *mut u64,
    out_seed: *mut u64,
) -> OSStatus {
    let Some(idx) = dev_index(id) else {
        // SAFETY：out 指针仅在非空时写入，宿主保证指向可写内存
        if !out_sample.is_null() {
            unsafe {
                *out_sample = 0.0;
            }
        }
        if !out_host.is_null() {
            unsafe {
                *out_host = mach_now_ticks();
            }
        }
        if !out_seed.is_null() {
            unsafe {
                *out_seed = ZERO_SEED.load(Ordering::SeqCst);
            }
        }
        return 0;
    };
    // BlackHole 同款：host 用 mach ticks；sample 按 ZTS_PERIOD 量化；
    // 只在计划下一拍已到（anchor + prevTicks + periodTicks <= now）时推进一拍。
    // RT 路径无锁（M4）：原子快照 + CAS 推进，见 Zts::query。
    let now_ticks = mach_now_ticks();
    let rate = SAMPLE_RATE.load(Ordering::SeqCst) as f64;
    let ns_per_tick = mach_ns_per_tick();
    let ticks_per_frame = 1e9 / ns_per_tick / rate; // Float64
    let period_ticks = ticks_per_frame * ZTS_PERIOD_FRAMES as f64;
    let (sample, host) = ZTS[idx].query(now_ticks, period_ticks);
    // SAFETY：out 指针仅在非空时写入，宿主保证指向可写内存
    if !out_sample.is_null() {
        unsafe {
            *out_sample = sample;
        }
    }
    if !out_host.is_null() {
        unsafe {
            *out_host = host;
        }
    }
    // sample 时间不回转，seed 保持恒定（HAL 用 seed 变化检测跳变）
    if !out_seed.is_null() {
        unsafe {
            *out_seed = ZERO_SEED.load(Ordering::SeqCst);
        }
    }
    0
}
unsafe extern "C" fn plugin_will_do_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
    op: u32,
    out_do: *mut u8,
    out_in_place: *mut u8,
) -> OSStatus {
    // SAFETY：out 指针仅在非空时写入，宿主保证指向可写 Boolean（u8）
    if !out_do.is_null() {
        unsafe {
            *out_do = match op {
                K_OP_READ_INPUT | K_OP_WRITE_OUTPUT => 1,
                _ => 0,
            };
        }
    }
    if !out_in_place.is_null() {
        unsafe {
            *out_in_place = 1;
        }
    }
    0
}
unsafe extern "C" fn plugin_begin_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
    _op: u32,
    _frames: u32,
    _cycle: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    0
}
unsafe extern "C" fn plugin_do_io_operation(
    _driver: AudioServerPlugInDriverRef,
    id: AudioObjectID,
    _stream: AudioObjectID,
    _client: u32,
    op: u32,
    frames: u32,
    cycle: *const AudioServerPlugInIOCycleInfo,
    main_buf: *mut c_void,
    _sec_buf: *mut c_void,
) -> OSStatus {
    if main_buf.is_null() {
        return 0;
    }
    let Some(idx) = dev_index(id) else { return 0 };
    // M11：夹紧 frames 到 ring 容量——宿主异常大的请求不再让后续 copy_from_slice
    // 越界 panic（正常 HAL 每周期 ≤4096 帧，此处兜底的是契约上限）
    let frames = frames.min(RING_FRAMES as u32);
    let n = frames as usize * CHANNELS; // 多声道交错
                                        // SAFETY：宿主按 CoreAudio IO 契约提供 frames×CHANNELS 个 f32 的可写交错缓冲
    let data = unsafe { std::slice::from_raw_parts_mut(main_buf.cast::<f32>(), n) };
    // BlackHole 同款：用 IO cycle 的 sample time 定位 ring（不是 FIFO）
    let sample_time = if cycle.is_null() {
        -1.0
    } else {
        // SAFETY：cycle 非空时由宿主保证指向可读 AudioServerPlugInIOCycleInfo
        let cycle = unsafe { &*cycle };
        match op {
            K_OP_READ_INPUT => cycle.m_input_time.m_sample_time,
            K_OP_WRITE_OUTPUT => cycle.m_output_time.m_sample_time,
            _ => -1.0,
        }
    };
    match op {
        K_OP_WRITE_OUTPUT => {
            // DSP：EQ + 增益 + 软限幅（实时处理，再写入环；每设备实例，M3）
            if let Ok(mut d) = dsp(idx).lock() {
                d.process(data);
            }
            if sample_time >= 0.0 {
                ring_write_out(idx, data, sample_time, frames);
            } else {
                ring_write_out(idx, data, 0.0, frames);
            }
        }
        // sample_time 无效的 READ_INPUT 与未知 op 一样输出静音（守卫并入 match 臂，行为不变）
        K_OP_READ_INPUT if sample_time >= 0.0 => {
            ring_read_in(idx, data, sample_time, frames);
            // 跨设备路由（M2/M4）：无锁读 src 行快照。跨设备读 src 的 ring 与 src
            // 设备 IO 线程的 ring_write_out 并发，依赖 ring_peek 开头的原子
            // “已写结束时间”快照保证正常情况下读写窗口不相交（详见 ring_peek
            // 的并发契约注释）；该路径为非实时安全的实验特性，默认路由不触发。
            // 混音 scratch 用本设备的 MIX_BUFS[idx]（每设备一份，M2）。
            for (src, row) in ROUTE_ROWS.iter().enumerate() {
                if src == idx {
                    continue;
                }
                let g = route_row_lane(row.load(Ordering::SeqCst), idx);
                if g != 0.0 {
                    // SAFETY：MIX_BUFS[idx] 仅由本设备 IO 线程读写（每设备一线程，
                    // 同设备 IO 周期串行执行），不与其他线程的槽位别名
                    unsafe {
                        MIX_BUFS[idx][..n].fill(0.0);
                    }
                    unsafe {
                        ring_peek(src, &mut MIX_BUFS[idx][..n], sample_time);
                    }
                    unsafe {
                        for i in 0..n {
                            data[i] += g * MIX_BUFS[idx][i];
                        }
                    }
                }
            }
        }
        _ => {
            data.fill(0.0);
        }
    }
    0
}
unsafe extern "C" fn plugin_end_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
    _op: u32,
    _frames: u32,
    _cycle: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    0
}

// ================= 属性 =================
include!("props.rs");

// ---------------- 供 props.rs 用 ----------------
pub(crate) const K_OP_READ_INPUT: u32 = 0x7265_6164; // 'read'
pub(crate) const K_OP_WRITE_OUTPUT: u32 = 0x7269_7465; // 'rite' kAudioServerPlugInIOOperationWriteMix

#[cfg(test)]
mod tests {
    use super::*;

    // 四字码 → OSStatus（CoreAudio FourCharCode 打包规则：首字节在高位）
    const fn fcc(s: [u8; 4]) -> i32 {
        (((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | s[3] as u32) as i32
    }

    // M1 回归：错误码必须等于 AudioHardwareBase.h 的四字码（防止十进制值再被编造）
    #[test]
    fn test_error_code_four_char_codes() {
        assert_eq!(BAD_OBJ, fcc(*b"!obj")); // kAudioHardwareBadObjectError
        assert_eq!(BAD_PROP, fcc(*b"who?")); // kAudioHardwareUnknownPropertyError
        assert_eq!(BAD_SIZE, fcc(*b"!siz")); // kAudioHardwareBadPropertySizeError
        assert_eq!(BAD_SEL, fcc(*b"unop")); // kAudioHardwareUnsupportedOperationError
    }

    // M2/M4 回归：路由行打包/解包位模式精确往返（含 0/负值/非整值）
    #[test]
    fn test_route_row_pack_roundtrip() {
        for g0 in [0.0f32, 1.0, 0.5, -1.25, 1234.5] {
            for g1 in [0.0f32, 1.0, 0.5, -1.25, 1234.5] {
                let row = route_row_pack([g0, g1]);
                // 位级精确往返是断言本意
                #[allow(clippy::float_cmp)]
                {
                    assert_eq!(route_row_lane(row, 0), g0);
                    assert_eq!(route_row_lane(row, 1), g1);
                }
            }
        }
    }

    // M2/M4 回归：默认路由是对角线 [[1,0],[0,1]]（各自独立环回，跨设备断开）
    #[test]
    fn test_route_default_matrix_is_identity() {
        let rows = route_rows();
        // 位级相等是断言本意
        #[allow(clippy::float_cmp)]
        {
            assert_eq!(route_row_lane(rows[0], 0), 1.0); // A→A
            assert_eq!(route_row_lane(rows[0], 1), 0.0); // A→B 断开
            assert_eq!(route_row_lane(rows[1], 0), 0.0); // B→A 断开
            assert_eq!(route_row_lane(rows[1], 1), 1.0); // B→B
        }
    }

    // M4 回归：ZTS 到点才推进一拍（边界含等于）；纯函数，无共享状态
    #[test]
    fn test_zts_next_beat_boundary() {
        // 下一拍在 anchor+prev+period = 1000+0+500 = 1500
        assert_eq!(zts_next_beat(1000, 0.0, 500.0, 1499), None);
        assert_eq!(zts_next_beat(1000, 0.0, 500.0, 1500), Some(500.0));
        // 已推进一拍后：下一拍在 2000
        assert_eq!(zts_next_beat(1000, 500.0, 500.0, 1999), None);
        assert_eq!(zts_next_beat(1000, 500.0, 500.0, 2000), Some(1000.0));
    }

    // M4 回归：Zts 原子快照状态机（本地实例驱动，确定性时序）
    #[test]
    fn test_zts_query_state_machine() {
        let z = Zts::new();
        // 未初始化（anchor=0）：sample=0，host=now
        assert_eq!(z.query(999, 500.0), (0.0, 999));
        z.reset(1000);
        // 未到拍：count=0，host=anchor
        assert_eq!(z.query(1499, 500.0), (0.0, 1000));
        // 到拍推进一拍：sample=16384，host=anchor+period
        assert_eq!(z.query(1500, 500.0), (16384.0, 1500));
        // 同一时刻重复查询不重复推进
        assert_eq!(z.query(1500, 500.0), (16384.0, 1500));
        assert_eq!(z.query(1999, 500.0), (16384.0, 1500));
        // 第二拍
        assert_eq!(z.query(2000, 500.0), (32768.0, 2000));
        // IO 长停顿后每次查询仍只推进一拍（BlackHole 同款，时钟连续不跳变）
        assert_eq!(z.query(999_999, 500.0), (49152.0, 2500));
    }

    // M5 回归：stop_io 饱和递减，0 时不下探（不回绕到 u32::MAX）
    #[test]
    fn test_stop_io_saturates_at_zero() {
        // 用 DEV_B 避开其他测试占用的 DEV_A 路径
        IO_CLIENTS[1].store(0, Ordering::SeqCst);
        // SAFETY：入参为空指针/本插件对象 ID，stop_io 仅做原子计数递减
        unsafe { plugin_stop_io(std::ptr::null_mut(), DEV_B, 0) };
        assert_eq!(IO_CLIENTS[1].load(Ordering::SeqCst), 0);
        IO_CLIENTS[1].store(3, Ordering::SeqCst);
        // SAFETY：同上
        unsafe { plugin_stop_io(std::ptr::null_mut(), DEV_B, 0) };
        assert_eq!(IO_CLIENTS[1].load(Ordering::SeqCst), 2);
        IO_CLIENTS[1].store(0, Ordering::SeqCst); // 还原，不污染其他测试
    }

    // M6 回归：未知 IID 返回 E_NOINTERFACE；驱动接口/IUnknown 返回 S_OK 并写回接口指针
    #[test]
    fn test_query_interface_iid() {
        let mut out: LPVOID = std::ptr::null_mut();
        // SAFETY：out 指向本测试栈上的 LPVOID 槽位，函数按 IUnknown 契约写入
        let rc = unsafe {
            plugin_query_interface(
                std::ptr::null_mut(),
                CFUUIDBytes {
                    m_data: [0xAA; 16], // 任意伪造 IID
                },
                &raw mut out,
            )
        };
        assert_eq!(rc, 0x8000_4002_u32 as HRESULT); // E_NOINTERFACE
        assert!(out.is_null());
        for iid in [IID_PLUGIN_INTERFACE, IID_IUNKNOWN] {
            out = std::ptr::null_mut();
            // SAFETY：同上
            let rc = unsafe {
                plugin_query_interface(
                    std::ptr::null_mut(),
                    CFUUIDBytes { m_data: iid },
                    &raw mut out,
                )
            };
            assert_eq!(rc, 0); // S_OK
            assert_eq!(out, &raw mut VTABLE_PTR as LPVOID);
        }
    }

    // M7 回归：静态设备插件拒绝动态创建设备（返回 'unop'，不写 out_id）
    #[test]
    fn test_create_device_unsupported() {
        let mut id: AudioObjectID = 0;
        // SAFETY：desc/client 为空指针，create_device 不解引用也不写 out_id
        let rc = unsafe {
            plugin_create_device(
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                &raw mut id,
            )
        };
        assert_eq!(rc, BAD_SEL);
        assert_eq!(id, 0);
    }

    // M8 回归：'icon' Get 返回 CFURLRef（CFURLGetTypeID 校验类型），尺寸=指针宽度
    #[test]
    fn test_icon_returns_cfurl() {
        // 与 test_icon_undersized_buffer_no_cfurl 共享 ICON_URL_BUILT 计数，须互斥
        let _guard = ICON_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let addr = AudioObjectPropertyAddress {
            m_selector: SEL_ICON,
            m_scope: SCOPE_GLOBAL,
            m_element: 0,
        };
        let mut size: u32 = 0;
        let mut out: *mut c_void = std::ptr::null_mut();
        // SAFETY：addr/size/out 均为有效指针；Get 创建的 CFURL 由宿主（此处测试
        // 进程）持有，泄漏至进程退出无害
        let rc = unsafe {
            plugin_get_property_data(
                std::ptr::null_mut(),
                DEV_A,
                0,
                &raw const addr,
                0,
                std::ptr::null(),
                std::mem::size_of::<*mut c_void>() as u32,
                &raw mut size,
                (&raw mut out).cast::<c_void>(),
            )
        };
        assert_eq!(rc, NO_ERR);
        assert_eq!(size, std::mem::size_of::<*mut c_void>() as u32);
        assert!(!out.is_null());
        // SAFETY：out 是刚创建的 CF 对象，仅做类型查询
        let type_id = unsafe { CFGetTypeID(out) };
        let url_type = unsafe { CFURLGetTypeID() };
        assert_eq!(type_id, url_type, "'icon' 必须返回 CFURLRef");
    }

    // 回归：'icon' 在 data_size < 指针宽度时返回 '!siz' 并回填所需尺寸，且不得
    // 构造 CFURL（修复前 cf_icon_url() 先于尺寸检查求值，白建的 CFURL 无人持有
    // 即泄漏；泄漏本身宿主不可观测，故以构造计数锁死"先检查后构造"顺序）。
    #[test]
    fn test_icon_undersized_buffer_no_cfurl() {
        let _guard = ICON_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ICON_URL_BUILT.store(0, std::sync::atomic::Ordering::Relaxed);
        let addr = AudioObjectPropertyAddress {
            m_selector: SEL_ICON,
            m_scope: SCOPE_GLOBAL,
            m_element: 0,
        };
        let mut size: u32 = 0;
        let mut out: *mut c_void = std::ptr::null_mut();
        // SAFETY：addr/size/out 均为有效指针；data_size=4 故意不足，Get 不得写出
        let rc = unsafe {
            plugin_get_property_data(
                std::ptr::null_mut(),
                DEV_A,
                0,
                &raw const addr,
                0,
                std::ptr::null(),
                4,
                &raw mut size,
                (&raw mut out).cast::<c_void>(),
            )
        };
        assert_eq!(rc, BAD_SIZE, "data_size < 指针宽度须返回 '!siz'");
        assert_eq!(
            size,
            std::mem::size_of::<*mut c_void>() as u32,
            "须回填所需尺寸（指针宽度）"
        );
        assert!(out.is_null(), "不得写出任何数据");
        assert_eq!(
            ICON_URL_BUILT.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "尺寸不足路径不得构造 CFURL（白建即泄漏）"
        );
    }

    // 回归：READ_INPUT 在 cycle 缺失（sample_time<0）时必须输出静音
    // （clippy collapsible_match 修复：守卫并入 match 臂，该路径落入兜底分支）
    #[test]
    fn test_read_input_without_cycle_is_silence() {
        let mut buf = [1.0f32; 8 * CHANNELS];
        // SAFETY：buf 为合法可写切片，插件函数只在本测试进程内读写该缓冲
        let rc = unsafe {
            plugin_do_io_operation(
                std::ptr::null_mut(),
                DEV_A,
                A_IN,
                0,
                K_OP_READ_INPUT,
                8,
                std::ptr::null(),
                buf.as_mut_ptr().cast::<c_void>(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0);
        assert!(buf.iter().all(|&x| x == 0.0), "未静音: {:?}", &buf[..4]);
    }

    // M11 回归：宿主异常大的 frames 在入口被夹紧到 ring 容量，不再越界
    //（修复前按 70000 帧展开 slice 并对 ring 越界 copy，UB/panic）
    #[test]
    fn test_do_io_clamps_huge_frames() {
        let frames = RING_FRAMES as u32 + 4464; // > RING_FRAMES(65536)
        let mut buf = vec![0f32; RING_FRAMES * CHANNELS]; // 恰为夹紧后的合法大小
                                                          // SAFETY：buf 为合法可写切片；夹紧后插件至多按 RING_FRAMES*CHANNELS 访问
        let rc = unsafe {
            plugin_do_io_operation(
                std::ptr::null_mut(),
                DEV_A,
                A_OUT,
                0,
                K_OP_WRITE_OUTPUT,
                frames,
                std::ptr::null(),
                buf.as_mut_ptr().cast::<c_void>(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, 0);
    }

    // 回归：plugin_initialize 复位全部设备 ZTS（M4 原子化后语义不变）
    #[test]
    fn test_initialize_resets_zts() {
        // SAFETY：入参为空指针，plugin_initialize 对 driver/host 仅做存储不解引用
        unsafe { plugin_initialize(std::ptr::null_mut(), std::ptr::null_mut()) };
        for zts in &ZTS {
            assert_eq!(zts.count.load(Ordering::SeqCst), 0);
            assert_eq!(zts.prev_ticks_bits.load(Ordering::SeqCst), 0.0f64.to_bits());
            assert_ne!(zts.anchor_ticks.load(Ordering::SeqCst), 0);
        }
    }

    // M9 回归：'stm#' 尺寸随 scope 变化——glob=全部流（2 个），input/output=单流
    #[test]
    fn test_stm_size_per_scope() {
        for (scope, want) in [
            (
                SCOPE_GLOBAL,
                2 * std::mem::size_of::<AudioObjectID>() as u32,
            ),
            (SCOPE_INPUT, std::mem::size_of::<AudioObjectID>() as u32),
            (SCOPE_OUTPUT, std::mem::size_of::<AudioObjectID>() as u32),
        ] {
            let a = AudioObjectPropertyAddress {
                m_selector: SEL_STM,
                m_scope: scope,
                m_element: 0,
            };
            let mut size: u32 = 0;
            // SAFETY：addr/size 均为有效指针，该函数为纯属性尺寸查询，无系统副作用
            let rc = unsafe {
                plugin_get_property_data_size(
                    std::ptr::null_mut(),
                    DEV_A,
                    0,
                    &raw const a,
                    0,
                    std::ptr::null(),
                    &raw mut size,
                )
            };
            assert_eq!(rc, NO_ERR);
            assert_eq!(size, want, "scope={scope:#x}");
        }
    }

    // M9 回归：'stm#' 三处口径一致——Has 对 glob/input/output 全真；Get 的
    // glob=[输出流,输入流]、input=输入流、output=输出流（HAL 惯例 glob=全部）
    #[test]
    fn test_stm_get_and_has_per_scope() {
        for (scope, want) in [
            (SCOPE_GLOBAL, vec![A_OUT, A_IN]),
            (SCOPE_INPUT, vec![A_IN]),
            (SCOPE_OUTPUT, vec![A_OUT]),
        ] {
            let a = AudioObjectPropertyAddress {
                m_selector: SEL_STM,
                m_scope: scope,
                m_element: 0,
            };
            // SAFETY：addr 为有效指针，纯属性存在性查询
            let has = unsafe { plugin_has_property(std::ptr::null_mut(), DEV_A, 0, &raw const a) };
            assert_eq!(has, 1, "Has scope={scope:#x}");
            let mut size: u32 = 0;
            let mut buf = [0u8; 16];
            // SAFETY：addr/size/out 均为有效指针，纯属性读取
            let rc = unsafe {
                plugin_get_property_data(
                    std::ptr::null_mut(),
                    DEV_A,
                    0,
                    &raw const a,
                    0,
                    std::ptr::null(),
                    16,
                    &raw mut size,
                    buf.as_mut_ptr().cast::<c_void>(),
                )
            };
            assert_eq!(rc, NO_ERR);
            assert_eq!(size as usize, want.len() * 4, "Get size scope={scope:#x}");
            // 流 ID 数组按原生端序逐字节比对（与插件内存布局一致）
            let mut want_bytes = Vec::new();
            for id in &want {
                want_bytes.extend_from_slice(&id.to_ne_bytes());
            }
            assert_eq!(
                &buf[..want_bytes.len()],
                want_bytes.as_slice(),
                "Get scope={scope:#x}"
            );
        }
    }

    // M10 回归：'ownd' 尺寸按 scope 返回真实数量（glob=4，output=3，input=1）
    #[test]
    fn test_ownd_size_per_scope() {
        for (scope, want) in [(SCOPE_GLOBAL, 4), (SCOPE_OUTPUT, 3), (SCOPE_INPUT, 1)] {
            let a = AudioObjectPropertyAddress {
                m_selector: SEL_OWND,
                m_scope: scope,
                m_element: 0,
            };
            let mut size: u32 = 0;
            // SAFETY：addr/size 均为有效指针，纯属性尺寸查询
            let rc = unsafe {
                plugin_get_property_data_size(
                    std::ptr::null_mut(),
                    DEV_B,
                    0,
                    &raw const a,
                    0,
                    std::ptr::null(),
                    &raw mut size,
                )
            };
            assert_eq!(rc, NO_ERR);
            assert_eq!(size, want * 4, "scope={scope:#x}");
        }
    }
}
