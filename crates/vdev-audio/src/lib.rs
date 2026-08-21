//! vdev-audio：100% Rust 虚拟声卡（CoreAudio HAL AudioServerPlugIn）。
//! 一个设备含输出流 + 输入流，输出环回输入（像 BlackHole/Soundflower）。
//! 系统只认 HAL 插件（/Library/Audio/Plug-Ins/HAL/*.driver），语言无关。

use std::ffi::c_void;

mod dsp;
mod vtable;
use vtable::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;


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
// 兼容旧名（props.rs 大量使用）
const OBJ_DEVICE: AudioObjectID = DEV_A;
const OBJ_STREAM_OUTPUT: AudioObjectID = A_OUT;
const OBJ_STREAM_INPUT: AudioObjectID = A_IN;
const OBJ_VOLUME: AudioObjectID = A_VOL;
const OBJ_MUTE: AudioObjectID = A_MUTE;

// 设备元数据
pub(crate) struct DevMeta {
    pub name: &'static str,
    pub uid: &'static str,
}
pub(crate) const DEVS: [DevMeta; N_DEVICES] = [
    DevMeta { name: "vdev-audio A", uid: "vdev-audio-A-device" },
    DevMeta { name: "vdev-audio B", uid: "vdev-audio-B-device" },
];

// 对象 ID → 设备索引（0=A, 1=B）；插件/盒返回 None
pub(crate) fn dev_index(obj: AudioObjectID) -> Option<usize> {
    match obj {
        DEV_A | A_OUT | A_IN | A_VOL | A_MUTE => Some(0),
        DEV_B | B_OUT | B_IN | B_VOL | B_MUTE => Some(1),
        _ => None,
    }
}
// 对象是否属于某设备（设备本身 / 流 / 控制）
pub(crate) fn obj_in_device(obj: AudioObjectID, idx: usize) -> bool {
    dev_index(obj) == Some(idx)
}
// 设备是否在运行（有活跃 IO 客户端）
pub(crate) fn device_running(obj: AudioObjectID) -> u32 {
    dev_index(obj).map(|i| IO_CLIENTS[i].load(Ordering::SeqCst) > 0).unwrap_or(false) as u32
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
struct Zts {
    anchor_ticks: u64,
    count: u64,
    prev_ticks_bits: u64, // f64 位模式（Float64 累计 ticks）
}
impl Zts {
    const fn new() -> Self { Self { anchor_ticks: 0, count: 0, prev_ticks_bits: 0 } }
}
static ZTS: [Mutex<Zts>; N_DEVICES] = [Mutex::new(Zts::new()), Mutex::new(Zts::new())];
static IO_CLIENTS: [std::sync::atomic::AtomicU32; N_DEVICES] =
    [std::sync::atomic::AtomicU32::new(0), std::sync::atomic::AtomicU32::new(0)];
// ---- BlackHole 式环形缓冲：按 sample time 定位（不是 FIFO head/tail）----
// ring 容量 65536 帧 × 2ch；输出（WriteMix）写入 mOutputTime 对应位置，
// 输入（ReadInput）读取 mInputTime 对应位置；输出未跟上时输出静音并清空。
const RING_FRAMES: usize = 65536;
const CHANNELS: usize = 8;
#[allow(static_mut_refs)]
static mut RING_BUFS: [[f32; RING_FRAMES * CHANNELS]; N_DEVICES] =
    [[0.0; RING_FRAMES * CHANNELS]; N_DEVICES];

// DSP 管线（EQ + 增益 + 软限幅），参数由自定义属性 'vdsp' 控制
static DSP: std::sync::OnceLock<Mutex<dsp::Dsp>> = std::sync::OnceLock::new();
pub(crate) fn dsp() -> &'static Mutex<dsp::Dsp> {
    DSP.get_or_init(|| Mutex::new(dsp::Dsp::default()))
}
// 路由矩阵：ROUTE[src][dst] = src 设备输出 → dst 设备输入的增益（0=不路由）
// 默认 [[1,0],[0,1]]：各自独立环回。可由自定义属性 'vrut' 修改。
static ROUTE: Mutex<[[f32; N_DEVICES]; N_DEVICES]> = Mutex::new([[1.0, 0.0], [0.0, 1.0]]);
pub(crate) fn route() -> &'static Mutex<[[f32; N_DEVICES]; N_DEVICES]> { &ROUTE }
#[allow(static_mut_refs)]
static mut MIX_BUF: [f32; RING_FRAMES * CHANNELS] = [0.0; RING_FRAMES * CHANNELS];
// 上次输出写入的“结束 sample time”（f64 位模式）+ 缓冲是否干净
static RING_LAST_OUTPUT_BITS: [AtomicU64; N_DEVICES] = [AtomicU64::new(0), AtomicU64::new(0)];
static RING_IS_CLEAR: [AtomicBool; N_DEVICES] = [AtomicBool::new(true), AtomicBool::new(true)];
// 输入/输出时间戳不同步（切换设备后 input 落后 output）→ 清空缓冲并静音直到追平
static RING_RESYNC: [AtomicBool; N_DEVICES] = [AtomicBool::new(false), AtomicBool::new(false)];

#[allow(static_mut_refs)]
fn ring_clear(idx: usize) {
    unsafe { RING_BUFS[idx].fill(0.0); }
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
    let end = out_sample_time + frames as f64;
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
    if last_output - (frames as f64) < in_sample_time {
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
    unsafe extern "C" { fn mach_absolute_time() -> u64; }
    unsafe { mach_absolute_time() }
}
fn mach_ns_per_tick() -> f64 {
    unsafe extern "C" { fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32; }
    let mut info = MachTimebaseInfo { numer: 1, denom: 1 };
    unsafe { mach_timebase_info(&mut info) };
    info.numer as f64 / info.denom as f64
}

// ---------------- 工厂函数（Info.plist CFPlugInFactories 指向） ----------------
#[no_mangle]
pub extern "C" fn vdev_audio_create(
    _allocator: *const c_void,
    _type_id: *const c_void,
) -> *mut c_void {
    // 返回 AudioServerPlugInDriverRef = &interface_ptr（AudioServerPlugInDriverInterface**）
    &raw mut VTABLE_PTR as *mut c_void
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
unsafe extern "C" fn plugin_query_interface(
    _driver: *mut c_void,
    _uuid: REFIID, // CFUUIDBytes 按值传递（x1:x2），out_interface 在第 4 寄存器 x3
    out_interface: *mut LPVOID,
) -> HRESULT {
    if out_interface.is_null() { return 0x80004003u32 as HRESULT /* E_POINTER */; }
    *out_interface = &raw mut VTABLE_PTR as LPVOID;
    0 // S_OK
}
unsafe extern "C" fn plugin_add_ref(_driver: *mut c_void) -> ULONG { 1 }
unsafe extern "C" fn plugin_release(_driver: *mut c_void) -> ULONG { 1 }

// ================= 生命周期 =================
unsafe extern "C" fn plugin_initialize(
    _driver: AudioServerPlugInDriverRef,
    in_host: AudioServerPlugInHostRef,
) -> OSStatus {
    *HOST.lock().unwrap_or_else(|e| e.into_inner()) = Some(in_host as usize);
    for i in 0..N_DEVICES {
        let mut z = ZTS[i].lock().unwrap_or_else(|e| e.into_inner());
        z.anchor_ticks = mach_now_ticks();
        z.count = 0;
        z.prev_ticks_bits = 0.0f64.to_bits();
    }
    0
}

unsafe extern "C" fn plugin_create_device(
    _driver: AudioServerPlugInDriverRef,
    _desc: *const c_void,
    _client: *const AudioServerPlugInClientInfo,
    out_id: *mut AudioObjectID,
) -> OSStatus {
    if out_id.is_null() { return -1; }
    *out_id = DEV_A;
    0
}
unsafe extern "C" fn plugin_destroy_device(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
) -> OSStatus { 0 }
unsafe extern "C" fn plugin_add_device_client(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: *const AudioServerPlugInClientInfo,
) -> OSStatus { 0 }
unsafe extern "C" fn plugin_remove_device_client(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: *const AudioServerPlugInClientInfo,
) -> OSStatus { 0 }
unsafe extern "C" fn plugin_perform_device_config_change(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _action: u64,
    _info: *mut c_void,
) -> OSStatus { 0 }
unsafe extern "C" fn plugin_abort_device_config_change(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _action: u64,
    _info: *mut c_void,
) -> OSStatus { 0 }

// ================= IO =================
unsafe extern "C" fn plugin_start_io(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
) -> OSStatus {
    let Some(idx) = dev_index(_id) else { return 0 };
    if IO_CLIENTS[idx].fetch_add(1, Ordering::SeqCst) == 0 {
        // 设备从空闲→活跃：重置时间锚点 + 清空 ring（BlackHole 同款）
        let mut z = ZTS[idx].lock().unwrap_or_else(|e| e.into_inner());
        z.anchor_ticks = mach_now_ticks();
        z.count = 0;
        z.prev_ticks_bits = 0.0f64.to_bits();
        drop(z);
        ring_clear(idx);
    }
    0
}
unsafe extern "C" fn plugin_stop_io(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
) -> OSStatus {
    if let Some(idx) = dev_index(_id) {
        IO_CLIENTS[idx].fetch_sub(1, Ordering::SeqCst);
    }
    0
}
unsafe extern "C" fn plugin_get_zero_time_stamp(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
    out_sample: *mut f64,
    out_host: *mut u64,
    out_seed: *mut u64,
) -> OSStatus {
    let Some(idx) = dev_index(_id) else {
        if !out_sample.is_null() { *out_sample = 0.0; }
        if !out_host.is_null() { *out_host = mach_now_ticks(); }
        if !out_seed.is_null() { *out_seed = ZERO_SEED.load(Ordering::SeqCst); }
        return 0;
    };
    // BlackHole 同款：host 用 mach ticks；sample 按 ZTS_PERIOD 量化；
    // 只在计划下一拍已到（anchor + prevTicks + periodTicks <= now）时推进一拍。
    let now_ticks = mach_now_ticks();
    let mut z = ZTS[idx].lock().unwrap_or_else(|e| e.into_inner());
    if z.anchor_ticks != 0 {
        let rate = SAMPLE_RATE.load(Ordering::SeqCst) as f64;
        let ns_per_tick = mach_ns_per_tick();
        let ticks_per_frame = 1e9 / ns_per_tick / rate; // Float64
        let period_ticks = ticks_per_frame * ZTS_PERIOD_FRAMES as f64;
        let mut prev_ticks = f64::from_bits(z.prev_ticks_bits);
        if z.anchor_ticks + prev_ticks as u64 + period_ticks as u64 <= now_ticks {
            z.count += 1;
            prev_ticks += period_ticks;
            z.prev_ticks_bits = prev_ticks.to_bits();
        }
        if !out_sample.is_null() { *out_sample = (z.count * ZTS_PERIOD_FRAMES) as f64; }
        if !out_host.is_null() { *out_host = z.anchor_ticks + prev_ticks as u64; }
    } else {
        if !out_sample.is_null() { *out_sample = 0.0; }
        if !out_host.is_null() { *out_host = now_ticks; }
    }
    // sample 时间不回转，seed 保持恒定（HAL 用 seed 变化检测跳变）
    if !out_seed.is_null() { *out_seed = ZERO_SEED.load(Ordering::SeqCst); }
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
    if !out_do.is_null() {
        *out_do = match op {
            K_OP_READ_INPUT | K_OP_WRITE_OUTPUT => 1,
            _ => 0,
        };
    }
    if !out_in_place.is_null() { *out_in_place = 1; }
    0
}
unsafe extern "C" fn plugin_begin_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
    _op: u32,
    _frames: u32,
    _cycle: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus { 0 }
unsafe extern "C" fn plugin_do_io_operation(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _stream: AudioObjectID,
    _client: u32,
    op: u32,
    frames: u32,
    _cycle: *const AudioServerPlugInIOCycleInfo,
    main_buf: *mut c_void,
    _sec_buf: *mut c_void,
) -> OSStatus {
    if main_buf.is_null() { return 0; }
    let Some(idx) = dev_index(_id) else { return 0 };
    let n = frames as usize * CHANNELS; // 多声道交错
    let data = std::slice::from_raw_parts_mut(main_buf as *mut f32, n);
    // BlackHole 同款：用 IO cycle 的 sample time 定位 ring（不是 FIFO）
    let sample_time = if _cycle.is_null() {
        -1.0
    } else {
        let cycle = unsafe { &*_cycle };
        match op {
            K_OP_READ_INPUT => cycle.m_input_time.m_sample_time,
            K_OP_WRITE_OUTPUT => cycle.m_output_time.m_sample_time,
            _ => -1.0,
        }
    };
    match op {
        K_OP_WRITE_OUTPUT => {
            // DSP：EQ + 增益 + 软限幅（实时处理，再写入环）
            if let Ok(mut d) = dsp().lock() {
                d.process(data);
            }
            if sample_time >= 0.0 {
                ring_write_out(idx, data, sample_time, frames);
            } else {
                ring_write_out(idx, data, 0.0, frames);
            }
        }
        K_OP_READ_INPUT => {
            if sample_time >= 0.0 {
                ring_read_in(idx, data, sample_time, frames);
                let route = ROUTE.lock().unwrap_or_else(|e| e.into_inner());
                for src in 0..N_DEVICES {
                    if src != idx && route[src][idx] != 0.0 {
                        let g = route[src][idx];
                        unsafe {
                            MIX_BUF[..n].fill(0.0);
                            ring_peek(src, &mut MIX_BUF[..n], sample_time);
                            for i in 0..n {
                                data[i] += g * MIX_BUF[i];
                            }
                        }
                    }
                }
            } else {
                data.fill(0.0);
            }
        }
        _ => { data.fill(0.0); }
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
) -> OSStatus { 0 }

// ================= 属性 =================
include!("props.rs");

// ---------------- 供 props.rs 用 ----------------
pub(crate) const K_OP_READ_INPUT: u32 = 0x72656164; // 'read'
pub(crate) const K_OP_WRITE_OUTPUT: u32 = 0x72697465; // 'rite' kAudioServerPlugInIOOperationWriteMix
