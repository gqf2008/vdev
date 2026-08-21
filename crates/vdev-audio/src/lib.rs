//! vdev-audio：100% Rust 虚拟声卡（CoreAudio HAL AudioServerPlugIn）。
//! 一个设备含输出流 + 输入流，输出环回输入（像 BlackHole/Soundflower）。
//! 系统只认 HAL 插件（/Library/Audio/Plug-Ins/HAL/*.driver），语言无关。

use std::ffi::c_void;

mod vtable;
use vtable::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;


// ---------------- 对象 ID 固定分配 ----------------
const OBJ_PLUGIN: AudioObjectID = 1;
const OBJ_BOX: AudioObjectID = 2;
const OBJ_DEVICE: AudioObjectID = 3;
const OBJ_STREAM_OUTPUT: AudioObjectID = 4;
const OBJ_STREAM_INPUT: AudioObjectID = 5;
const OBJ_VOLUME: AudioObjectID = 6;
const OBJ_MUTE: AudioObjectID = 7;

// ---------------- 全局状态 ----------------
static HOST: Mutex<Option<usize>> = Mutex::new(None);
static SAMPLE_RATE: AtomicU64 = AtomicU64::new(48_000);
static IO_RUNNING: AtomicBool = AtomicBool::new(false);
static ZERO_SEED: AtomicU64 = AtomicU64::new(1);
// GetZeroTimeStamp —— BlackHole 同款：锚定 + 环缓冲量化 + 追赶推进。
// sample = N * ZTS_PERIOD；host = anchor + N * ticks_per_period。
// 只在“计划下一拍已到”时推进一拍（Float64 累计 ticks），IO 停止期间不推进，
// 恢复后从断点继续，coreaudiod 看到的时钟永远连续（切换输入设备不再失声）。
const ZTS_PERIOD_FRAMES: u64 = 16384; // kAudioDevicePropertyZeroTimeStampPeriod（≥10923）
static ZTS_ANCHOR_TICKS: AtomicU64 = AtomicU64::new(0);
static ZTS_COUNT: AtomicU64 = AtomicU64::new(0);
static ZTS_PREV_TICKS_BITS: AtomicU64 = AtomicU64::new(0); // f64 位模式（Float64 累计 ticks）
static IO_CLIENTS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
// ---- BlackHole 式环形缓冲：按 sample time 定位（不是 FIFO head/tail）----
// ring 容量 65536 帧 × 2ch；输出（WriteMix）写入 mOutputTime 对应位置，
// 输入（ReadInput）读取 mInputTime 对应位置；输出未跟上时输出静音并清空。
const RING_FRAMES: usize = 65536;
const CHANNELS: usize = 2;
#[allow(static_mut_refs)]
static mut RING_BUF: [f32; RING_FRAMES * CHANNELS] = [0.0; RING_FRAMES * CHANNELS];
// 上次输出写入的“结束 sample time”（f64 位模式）+ 缓冲是否干净
static RING_LAST_OUTPUT_BITS: AtomicU64 = AtomicU64::new(0);
static RING_IS_CLEAR: AtomicBool = AtomicBool::new(true);
// 输入/输出时间戳不同步（切换设备后 input 落后 output）→ 清空缓冲并静音直到追平
static RING_RESYNC: AtomicBool = AtomicBool::new(false);

#[allow(static_mut_refs)]
fn ring_clear() {
    unsafe { RING_BUF.fill(0.0); }
    RING_IS_CLEAR.store(true, Ordering::SeqCst);
}

// 输出（WriteMix）：把混合数据写入 output sample time 对应的 ring 位置
fn ring_write_out(data: &[f32], out_sample_time: f64, frames: u32) {
    let start = ((out_sample_time as i64).rem_euclid(RING_FRAMES as i64)) as usize * CHANNELS;
    let n = data.len();
    unsafe {
        if start + n <= RING_FRAMES * CHANNELS {
            RING_BUF[start..start + n].copy_from_slice(data);
        } else {
            let first = RING_FRAMES * CHANNELS - start;
            RING_BUF[start..].copy_from_slice(&data[..first]);
            RING_BUF[..n - first].copy_from_slice(&data[first..]);
        }
    }
    // 记录输出写入的结束 sample time
    let end = out_sample_time + frames as f64;
    RING_LAST_OUTPUT_BITS.store(end.to_bits(), Ordering::SeqCst);
    RING_IS_CLEAR.store(false, Ordering::SeqCst);
}

// 输入（ReadInput）：从 input sample time 对应的 ring 位置读；输出未跟上则静音+清空
fn ring_read_in(out: &mut [f32], in_sample_time: f64, frames: u32) {
    let last_output = f64::from_bits(RING_LAST_OUTPUT_BITS.load(Ordering::SeqCst));
    let rate = SAMPLE_RATE.load(Ordering::SeqCst) as f64;
    // 切换设备后 input 时间戳会落后 output（读到的是切走前/切走期间的旧音频）。
    // 检测到落后 > 1s：清空缓冲一次，并在追平前输出静音，丢弃残留旧数据。
    if last_output - in_sample_time > rate {
        if !RING_RESYNC.swap(true, Ordering::SeqCst) {
            ring_clear();
        }
        out.fill(0.0);
        return;
    }
    if RING_RESYNC.load(Ordering::SeqCst) && (last_output - in_sample_time) <= rate {
        RING_RESYNC.store(false, Ordering::SeqCst);
    }
    // BlackHole 静音条件：输出最后写入的结束时间还不到当前输入帧（输出没跟上）
    if last_output - (frames as f64) < in_sample_time {
        out.fill(0.0);
        if !RING_IS_CLEAR.load(Ordering::SeqCst) {
            ring_clear();
        }
        return;
    }
    let start = ((in_sample_time as i64).rem_euclid(RING_FRAMES as i64)) as usize * CHANNELS;
    let n = out.len();
    unsafe {
        if start + n <= RING_FRAMES * CHANNELS {
            out.copy_from_slice(&RING_BUF[start..start + n]);
        } else {
            let first = RING_FRAMES * CHANNELS - start;
            out[..first].copy_from_slice(&RING_BUF[start..]);
            out[first..].copy_from_slice(&RING_BUF[..n - first]);
        }
    }
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
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
    ZTS_ANCHOR_TICKS.store(mach_now_ticks(), Ordering::SeqCst);
    ZTS_COUNT.store(0, Ordering::SeqCst);
    ZTS_PREV_TICKS_BITS.store(0.0f64.to_bits(), Ordering::SeqCst);
    0
}

unsafe extern "C" fn plugin_create_device(
    _driver: AudioServerPlugInDriverRef,
    _desc: *const c_void,
    _client: *const AudioServerPlugInClientInfo,
    out_id: *mut AudioObjectID,
) -> OSStatus {
    if out_id.is_null() { return -1; }
    *out_id = OBJ_DEVICE;
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
    if IO_CLIENTS.fetch_add(1, Ordering::SeqCst) == 0 {
        // 设备从空闲→活跃：重置时间锚点 + 清空 ring（BlackHole 同款）
        ZTS_ANCHOR_TICKS.store(mach_now_ticks(), Ordering::SeqCst);
        ZTS_COUNT.store(0, Ordering::SeqCst);
        ZTS_PREV_TICKS_BITS.store(0.0f64.to_bits(), Ordering::SeqCst);
        ring_clear();
    }
    IO_RUNNING.store(true, Ordering::SeqCst);
    0
}
unsafe extern "C" fn plugin_stop_io(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
) -> OSStatus {
    if IO_CLIENTS.fetch_sub(1, Ordering::SeqCst) <= 1 {
        IO_RUNNING.store(false, Ordering::SeqCst);
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
    // BlackHole 同款：host 用 mach ticks；sample 按 ZTS_PERIOD 量化；
    // 只在计划下一拍已到（anchor + prevTicks + periodTicks <= now）时推进一拍。
    let now_ticks = mach_now_ticks();
    let anchor = ZTS_ANCHOR_TICKS.load(Ordering::SeqCst);
    if anchor != 0 {
        let rate = SAMPLE_RATE.load(Ordering::SeqCst) as f64;
        let ns_per_tick = mach_ns_per_tick();
        let ticks_per_frame = 1e9 / ns_per_tick / rate; // Float64
        let period_ticks = ticks_per_frame * ZTS_PERIOD_FRAMES as f64;
        let mut prev_ticks = f64::from_bits(ZTS_PREV_TICKS_BITS.load(Ordering::SeqCst));
        if anchor + prev_ticks as u64 + period_ticks as u64 <= now_ticks {
            ZTS_COUNT.fetch_add(1, Ordering::SeqCst);
            prev_ticks += period_ticks;
            ZTS_PREV_TICKS_BITS.store(prev_ticks.to_bits(), Ordering::SeqCst);
        }
        let count = ZTS_COUNT.load(Ordering::SeqCst);
        if !out_sample.is_null() { *out_sample = (count * ZTS_PERIOD_FRAMES) as f64; }
        if !out_host.is_null() { *out_host = anchor + prev_ticks as u64; }
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
    let n = frames as usize * CHANNELS; // 立体声交错
    let data = std::slice::from_raw_parts_mut(main_buf as *mut f32, n);
    // BlackHole 同款：用 IO cycle 的 sample time 定位 ring（不是 FIFO）
    let sample_time = if _cycle.is_null() {
        -1.0 // 防御：cycle 缺失时退化为覆盖写/静音
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
            if sample_time >= 0.0 {
                ring_write_out(data, sample_time, frames);
            } else {
                // 无 sample time：写入 ring 开头（极少发生）
                ring_write_out(data, 0.0, frames);
            }
        }
        K_OP_READ_INPUT => {
            if sample_time >= 0.0 {
                ring_read_in(data, sample_time, frames);
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
