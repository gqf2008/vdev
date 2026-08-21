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
// GetZeroTimeStamp 增量推进：只在被调用时按真实流逝推进 sample，
// 设备 IO 停止期间不累计，恢复后从断点继续（避免墙钟跳变导致 coreaudiod 判定时钟异常）
static ZTS_LAST_TICKS: AtomicU64 = AtomicU64::new(0);
static ZTS_SAMPLE_BITS: AtomicU64 = AtomicU64::new(0); // f64 位模式
static RING: Mutex<Ring> = Mutex::new(Ring::new());

// 环回缓冲：Float32 交错立体声
const RING_LEN: usize = 65536 * 2; // 帧数（立体声 sample 数 = 帧*2）
struct Ring {
    buf: Vec<f32>,
    head: usize, // 写（输出流入）
    tail: usize, // 读（输入流出）
    count: usize,
}
impl Ring {
    const fn new() -> Self {
        Self { buf: Vec::new(), head: 0, tail: 0, count: 0 }
    }
    fn ensure(&mut self) {
        if self.buf.is_empty() {
            self.buf = vec![0.0; RING_LEN];
        }
    }
    fn write(&mut self, data: &[f32]) {
        self.ensure();
        for &v in data {
            self.buf[self.head] = v;
            self.head = (self.head + 1) % RING_LEN;
            if self.count < RING_LEN {
                self.count += 1;
            } else {
                self.tail = (self.tail + 1) % RING_LEN; // 满则丢最旧
            }
        }
    }
    fn read(&mut self, out: &mut [f32]) {
        self.ensure();
        for o in out.iter_mut() {
            if self.count > 0 {
                *o = self.buf[self.tail];
                self.tail = (self.tail + 1) % RING_LEN;
                self.count -= 1;
            } else {
                *o = 0.0; // 静音
            }
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
fn mach_ticks_to_ns(ticks: u64) -> f64 {
    unsafe extern "C" { fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32; }
    let mut info = MachTimebaseInfo { numer: 1, denom: 1 };
    unsafe { mach_timebase_info(&mut info) };
    ticks as f64 * info.numer as f64 / info.denom as f64
}
fn mach_now_ns() -> u64 {
    mach_ticks_to_ns(mach_now_ticks()) as u64
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
    ZTS_LAST_TICKS.store(mach_now_ticks(), Ordering::SeqCst);
    ZTS_SAMPLE_BITS.store(0.0f64.to_bits(), Ordering::SeqCst);
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
    IO_RUNNING.store(true, Ordering::SeqCst);
    0
}
unsafe extern "C" fn plugin_stop_io(
    _driver: AudioServerPlugInDriverRef,
    _id: AudioObjectID,
    _client: u32,
) -> OSStatus {
    IO_RUNNING.store(false, Ordering::SeqCst);
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
    // host time 必须用 mach_absolute_time() 的 ticks（CoreAudio 的标准）；
    // sample 时间按“上次调用以来的真实流逝”增量推进：IO 停止期间不调用就不累计，
    // 恢复后从断点继续，coreaudiod 看到的时钟永远连续。
    let now_ticks = mach_now_ticks();
    let last_ticks = ZTS_LAST_TICKS.swap(now_ticks, Ordering::SeqCst);
    let mut sample = f64::from_bits(ZTS_SAMPLE_BITS.load(Ordering::SeqCst));
    if last_ticks != 0 && now_ticks >= last_ticks {
        let delta_ns = mach_ticks_to_ns(now_ticks - last_ticks);
        let rate = SAMPLE_RATE.load(Ordering::SeqCst) as f64;
        sample += delta_ns / 1e9 * rate;
        ZTS_SAMPLE_BITS.store(sample.to_bits(), Ordering::SeqCst);
    }
    if !out_sample.is_null() { *out_sample = sample; }
    if !out_host.is_null() { *out_host = now_ticks; }
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
    let n = frames as usize * 2; // 立体声交错
    let data = std::slice::from_raw_parts_mut(main_buf as *mut f32, n);
    match op {
        K_OP_WRITE_OUTPUT => {
            // App 写音频到我们的输出流 → 写入环
            if let Ok(mut r) = RING.lock() {
                r.write(data);
            }
                }
        K_OP_READ_INPUT => {
            // 其它 App 读我们的输入流（麦克风）→ 从环读
            if let Ok(mut r) = RING.lock() {
                r.read(data);
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
