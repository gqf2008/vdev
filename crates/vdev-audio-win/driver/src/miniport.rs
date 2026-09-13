//! WaveRT 小端口（虚拟扬声器/麦克风）+ 环回流
#![allow(non_snake_case, non_camel_case_types)]
#![allow(clippy::missing_errors_doc)]

use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::com::{interlocked_decrement, interlocked_increment};
use crate::position::{bytes_for_interval, mod_position, split_ring_span};
use crate::ringbuffer::RingBuffer;
use crate::sys::portcls::*;
use crate::sys::types::*;

pub const TAG: u32 = u32::from_le_bytes(*b"vdev");

// SAFETY: 内核 API；KeQueryPerformanceCounter 任意 IRQL 可调用（DISPATCH_LEVEL 安全），
// PLARGE_INTEGER 出参与 *mut u64 在 x64 上布局等价
unsafe extern "system" {
    fn KeQueryPerformanceCounter(Frequency: *mut u64) -> u64;
}

// ============ IPortWaveRTStream（小端口调用端口侧） ============

pub type PFN_AllocatePagesForMdl = unsafe extern "system" fn(PVOID, u64, usize) -> *mut c_void;
/// AllocateContiguousPagesForMdl(Highest, Lowest, TotalBytes)——4 参，与
/// AllocatePagesForMdl(Highest, TotalBytes) 3 参不同（portcls.h 逐一核对）
pub type PFN_AllocateContiguousPagesForMdl =
    unsafe extern "system" fn(PVOID, u64, u64, usize) -> *mut c_void;
pub type PFN_MapAllocatedPages = unsafe extern "system" fn(PVOID, *mut c_void, u32) -> *mut c_void;
pub type PFN_FreePagesFromMdl = unsafe extern "system" fn(PVOID, *mut c_void);

/// IPortWaveRTStream vtable（仅声明小端口需要的方法；槽序对照 portcls.h）
#[repr(C)]
pub struct IPortWaveRTStreamVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub allocate_pages_for_mdl: PFN_AllocatePagesForMdl,
    pub allocate_contiguous_pages_for_mdl: PFN_AllocateContiguousPagesForMdl,
    pub map_allocated_pages: PFN_MapAllocatedPages,
    pub unmap_allocated_pages: unsafe extern "system" fn(PVOID, *mut c_void, *mut c_void),
    pub free_pages_from_mdl: PFN_FreePagesFromMdl,
    pub get_physical_pages_count: unsafe extern "system" fn(PVOID, *mut c_void) -> u32,
    pub get_physical_page_address: unsafe extern "system" fn(PVOID, *mut c_void, u32) -> u64,
}

// ============ IMiniportWaveRTStream ============

#[repr(C)]
pub struct IMiniportWaveRTStreamVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub set_format: unsafe extern "system" fn(PVOID, PKSDATAFORMAT) -> NTSTATUS,
    pub set_state: unsafe extern "system" fn(PVOID, KSSTATE) -> NTSTATUS,
    pub get_position: unsafe extern "system" fn(PVOID, *mut KSAUDIO_POSITION) -> NTSTATUS,
    pub allocate_audio_buffer: unsafe extern "system" fn(
        PVOID,
        u32,
        *mut *mut c_void,
        *mut u32,
        *mut u32,
        *mut u32,
    ) -> NTSTATUS,
    pub free_audio_buffer: unsafe extern "system" fn(PVOID, *mut c_void, u32),
    pub get_hw_latency: unsafe extern "system" fn(PVOID, *mut KSRTAUDIO_HWLATENCY),
    pub get_position_register:
        unsafe extern "system" fn(PVOID, *mut KSRTAUDIO_HWREGISTER) -> NTSTATUS,
    pub get_clock_register: unsafe extern "system" fn(PVOID, *mut KSRTAUDIO_HWREGISTER) -> NTSTATUS,
}

/// WaveRT 流对象
#[repr(C)]
pub struct WaveRTStream {
    pub vtable: &'static IMiniportWaveRTStreamVtbl,
    pub refcount: u32,
    pub capture: bool,
    /// 流状态（u32 = KSSTATE；AtomicU32 使 GetPosition 在其他线程/DPC 读取可见）
    pub state: AtomicU32,
    pub port_stream: PVOID,
    pub miniport: *mut MiniportWaveRT,
    pub dma_buffer: *mut u8,
    pub dma_size: u32,
    pub buffer_mdl: *mut c_void,
    pub block_align: u16,
    /// 累计流位置（字节，RUN 起单调；输出时对 dma_size 取模）
    pub position: u64,
    /// B4：已搬运到/自 DMA 窗口处理到的位置（每次只处理 [last_processed, target)）
    pub last_processed: u64,
    /// B4：进入 RUN 时的 QPC 时间锚点（tick）
    pub qpc_anchor: u64,
    /// B4：进入 RUN 时的位置锚点
    pub anchor_position: u64,
    /// 字节速率（nSamplesPerSec × nBlockAlign；0 = 未设定格式）
    pub bytes_per_sec: u32,
}

impl WaveRTStream {
    /// 创建流对象
    ///
    /// # Safety
    /// miniport/port_stream 必须有效。
    unsafe fn new(
        miniport: *mut MiniportWaveRT,
        port_stream: PVOID,
        capture: bool,
    ) -> *mut WaveRTStream {
        let ptr = crate::sys::mem::ExAllocatePool2_np(0x40, size_of::<WaveRTStream>() as u64, TAG);
        if ptr.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: 刚分配的池内存
        unsafe {
            core::ptr::write(
                ptr as *mut WaveRTStream,
                WaveRTStream {
                    vtable: &WAVERT_STREAM_VTABLE,
                    refcount: 1,
                    capture,
                    state: AtomicU32::new(KSSTATE_STOP_U32),
                    port_stream,
                    miniport,
                    dma_buffer: core::ptr::null_mut(),
                    dma_size: 0,
                    buffer_mdl: core::ptr::null_mut(),
                    block_align: 4,
                    position: 0,
                    last_processed: 0,
                    qpc_anchor: 0,
                    anchor_position: 0,
                    bytes_per_sec: 0,
                },
            );
        }
        ptr as *mut WaveRTStream
    }
}

unsafe extern "system" fn stream_qi(
    this: PVOID,
    iid: *const GUID,
    obj: *mut *mut c_void,
) -> NTSTATUS {
    let this = this as *mut WaveRTStream;
    // SAFETY: 调用方保证 iid/obj 有效
    unsafe {
        if is_equal_guid(iid, &IID_IUnknown) || is_equal_guid(iid, &IID_IMiniportWaveRTStream) {
            *obj = this.cast();
            interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
            STATUS_SUCCESS
        } else {
            // 未知 IID：COM 约定返回 E_NOINTERFACE 且 *obj = NULL
            *obj = core::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn stream_addref(this: PVOID) -> u32 {
    // SAFETY: this 指向流对象
    unsafe {
        interlocked_increment(core::ptr::addr_of_mut!(
            (*(this as *mut WaveRTStream)).refcount
        ))
    }
}

unsafe extern "system" fn stream_release(this: PVOID) -> u32 {
    let this = this as *mut WaveRTStream;
    // SAFETY: 引用计数保护
    let rc = unsafe { interlocked_decrement(core::ptr::addr_of_mut!((*this).refcount)) };
    if rc == 0 {
        // SAFETY: 释放对象内存；MDL/缓冲由 stream_free_audio_buffer 先释放
        unsafe {
            crate::sys::mem::ExFreePoolWithTag_np(this.cast(), TAG);
        }
    }
    rc
}

static WAVERT_STREAM_VTABLE: IMiniportWaveRTStreamVtbl = IMiniportWaveRTStreamVtbl {
    query_interface: stream_qi,
    add_ref: stream_addref,
    release: stream_release,
    set_format: stream_set_format,
    set_state: stream_set_state,
    get_position: stream_get_position,
    allocate_audio_buffer: stream_allocate_audio_buffer,
    free_audio_buffer: stream_free_audio_buffer,
    get_hw_latency: stream_get_hw_latency,
    get_position_register: stream_get_position_register,
    get_clock_register: stream_get_clock_register,
};

// ---- 流方法实现 ----

/// minor b：校验 Specifier 与 WAVEFORMATEX 字段（仅支持 48kHz / 16bit / 2ch PCM，
/// 与数据范围 AUDIO_DATA_RANGE 一致）
unsafe extern "system" fn stream_set_format(this: PVOID, data_format: PKSDATAFORMAT) -> NTSTATUS {
    dbg_inc(&DBG_SET_FORMAT_CALLS);
    let this = this as *mut WaveRTStream;
    // SAFETY: 调用方保证 data_format 指向完整 KSDATAFORMAT_WAVEFORMATEX
    let ksdm = unsafe { &*data_format.cast::<KSDATAFORMAT_WAVEFORMATEX>() };
    // 诊断：记录最后一次协商到的格式（用户态经自定义属性集读回）
    DBG_LAST_FORMAT_SIZE.store(ksdm.DataFormat.FormatSize, Ordering::Relaxed);
    DBG_LAST_TAG.store(u32::from(ksdm.WaveFormatEx.wFormatTag), Ordering::Relaxed);
    DBG_LAST_CHANNELS.store(u32::from(ksdm.WaveFormatEx.nChannels), Ordering::Relaxed);
    DBG_LAST_RATE.store(ksdm.WaveFormatEx.nSamplesPerSec, Ordering::Relaxed);
    DBG_LAST_BITS.store(
        u32::from(ksdm.WaveFormatEx.wBitsPerSample),
        Ordering::Relaxed,
    );
    if ksdm.DataFormat.SubFormat != KSDATAFORMAT_SUBTYPE_PCM
        || ksdm.DataFormat.Specifier != KSDATAFORMAT_SPECIFIER_WAVEFORMATEX
        || ksdm.DataFormat.FormatSize < size_of::<KSDATAFORMAT_WAVEFORMATEX>() as u32
    {
        DBG_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    let wf = &ksdm.WaveFormatEx;
    // ksmedia.h：引擎按设备格式开流时可能用 WAVE_FORMAT_PCM(0x0001) 或
    // WAVE_FORMAT_EXTENSIBLE(0xFFFE)（后者 cbSize=22，SubFormat 仍是 PCM）。
    // 只认 PCM 会让"设备格式是 EXTENSIBLE"的端点开流被拒——实测表现为
    // IAudioClient::GetMixFormat 报 0x80070491、端点虽在但应用不可用。
    const WAVE_FORMAT_PCM: u16 = 0x0001;
    const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
    if (wf.wFormatTag != WAVE_FORMAT_PCM && wf.wFormatTag != WAVE_FORMAT_EXTENSIBLE)
        || wf.nSamplesPerSec != 48_000
        || wf.wBitsPerSample != 16
        || wf.nChannels != 2
        || wf.nBlockAlign != 4
    {
        DBG_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // EXTENSIBLE 时 SubFormat 必须是 PCM（前面的 ksdm.DataFormat.SubFormat 检查对应
    // KSDATAFORMAT 的 SubFormat；这里再核对 WAVEFORMATEXTENSIBLE.SubFormat 偏移）
    if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
        if ksdm.DataFormat.FormatSize < (size_of::<KSDATAFORMAT>() + 40) as u32 {
            DBG_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        }
        let base = (data_format as *const u8).add(size_of::<KSDATAFORMAT>() + 24);
        // SAFETY: FormatSize >= 104 已校验，偏移 64+24=88 处为 SubFormat GUID
        let sub = unsafe { core::ptr::read_unaligned(base.cast::<GUID>()) };
        if sub != KSDATAFORMAT_SUBTYPE_PCM {
            DBG_LAST_STATUS.store(STATUS_INVALID_PARAMETER as u32, Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        }
    }
    // SAFETY: 单线程初始化（PortCls 串行调用）
    unsafe {
        (*this).block_align = wf.nBlockAlign;
        (*this).bytes_per_sec = wf.nSamplesPerSec * u32::from(wf.nBlockAlign);
    }
    dbg_inc(&DBG_SET_FORMAT_OK);
    DBG_LAST_STATUS.store(STATUS_SUCCESS as u32, Ordering::Relaxed);
    STATUS_SUCCESS
}

/// 合法状态迁移表（minor d）：KS 状态只能逐级切换（STOP↔ACQUIRE↔PAUSE↔RUN），
/// PortCls 串行下发 SetState，同状态为幂等成功，越级返回 STATUS_INVALID_DEVICE_REQUEST。
const STREAM_STATE_TRANSITIONS: [(u32, u32); 6] = [
    (KSSTATE_STOP_U32, KSSTATE_ACQUIRE_U32),
    (KSSTATE_ACQUIRE_U32, KSSTATE_STOP_U32),
    (KSSTATE_ACQUIRE_U32, KSSTATE_PAUSE_U32),
    (KSSTATE_PAUSE_U32, KSSTATE_ACQUIRE_U32),
    (KSSTATE_PAUSE_U32, KSSTATE_RUN_U32),
    (KSSTATE_RUN_U32, KSSTATE_PAUSE_U32),
];

unsafe extern "system" fn stream_set_state(this: PVOID, state: KSSTATE) -> NTSTATUS {
    let this = this as *mut WaveRTStream;
    let next = state as u32;
    // SAFETY: 原子换状态（非法时回滚）；PortCls 串行调用本方法
    let prev = unsafe { (*this).state.swap(next, Ordering::SeqCst) };
    if next != prev && !STREAM_STATE_TRANSITIONS.contains(&(prev, next)) {
        // 非法越级迁移：回滚并拒绝（PortCls 之后按合法路径重试）
        // SAFETY: 回滚状态
        unsafe { (*this).state.store(prev, Ordering::SeqCst) };
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    match next {
        KSSTATE_STOP_U32 | KSSTATE_ACQUIRE_U32 => {
            // 停止/释放资源：复位位置锚点（WaveRT 从 STOP 重新运行需从头开始）
            // SAFETY: PortCls 串行调用
            unsafe {
                (*this).position = 0;
                (*this).last_processed = 0;
                (*this).anchor_position = 0;
                (*this).qpc_anchor = 0;
            }
        }
        KSSTATE_RUN_U32 => {
            // B4：进入 RUN 建立 QPC 时间锚点
            let mut freq: u64 = 0;
            // SAFETY: 任意 IRQL 可调用
            let now = unsafe { KeQueryPerformanceCounter(core::ptr::addr_of_mut!(freq)) };
            // SAFETY: PortCls 串行调用
            unsafe {
                (*this).qpc_anchor = now;
                (*this).anchor_position = (*this).last_processed;
            }
        }
        _ => {} // PAUSE：保持位置，待 RUN 重新锚定
    }
    STATUS_SUCCESS
}

/// M3：处理一个 DMA 窗口切片（capture 环→DMA 欠载补零 / render DMA→环 满载丢旧）
///
/// # Safety
/// ring 必须非空且有效。
#[inline]
unsafe fn process_dma_slice(ring: *mut RingBuffer, capture: bool, window: &mut [u8]) {
    // SAFETY: 调用方保证 ring 有效
    unsafe {
        if capture {
            (*ring).read_zero_fill(window);
        } else {
            (*ring).write_drop_oldest(window);
        }
    }
}

/// B4：GetPosition 以 QPC 时间锚点把流逝时间换算为应推进的字节数，仅处理
/// [last_processed, target) 这一 DMA 窗口（sysvad 风格），不再整缓冲搬运；
/// PlayOffset/WriteOffset 为环内偏移（mod dma_size）。
///
/// Windows 运行时行为（QPC 读取、DMA 窗口推进）无法在宿主单测，
/// 纯数学部分在 position.rs 有 host 测试兜底。
unsafe extern "system" fn stream_get_position(
    this: PVOID,
    position: *mut KSAUDIO_POSITION,
) -> NTSTATUS {
    let this = this as *mut WaveRTStream;
    // SAFETY: this 为已初始化流对象
    let s = unsafe { &mut *this };
    let ring = (*s.miniport).ring;
    let dma = s.dma_buffer;
    let dma_size = s.dma_size as usize;
    let running = s.state.load(Ordering::SeqCst) == KSSTATE_RUN_U32;
    if running && !dma.is_null() && !ring.is_null() && dma_size > 0 && s.bytes_per_sec > 0 {
        let mut freq: u64 = 0;
        // SAFETY: 任意 IRQL 可调用
        let now = unsafe { KeQueryPerformanceCounter(core::ptr::addr_of_mut!(freq)) };
        let elapsed = now.saturating_sub(s.qpc_anchor);
        let target = s.anchor_position.saturating_add(bytes_for_interval(
            elapsed,
            freq,
            u64::from(s.bytes_per_sec),
        ));
        // 单次最多推进一个 DMA 周期（引擎久未查询时不越过缓冲窗口）
        let target = target.min(s.last_processed + dma_size as u64);
        let n = target.saturating_sub(s.last_processed) as usize;
        if n > 0 {
            // SAFETY: dma 有效且 dma_size 字节；split_ring_span 保证 off+first ≤ dma_size
            let (off, first) = split_ring_span(s.last_processed as usize, n, dma_size);
            // SAFETY: 首段 [off, off+first) 在窗口内
            let head = unsafe { core::slice::from_raw_parts_mut(dma.add(off), first) };
            process_dma_slice(ring, s.capture, head);
            if first < n {
                // SAFETY: 尾段 [0, n-first) 在窗口内
                let tail = unsafe { core::slice::from_raw_parts_mut(dma, n - first) };
                process_dma_slice(ring, s.capture, tail);
            }
            // 位置按引擎消费量（时间）推进，即使欠载补零/满载丢弃也不回退
            s.last_processed = s.last_processed.wrapping_add(n as u64);
        }
        s.position = s.last_processed;
    }
    // SAFETY: position 为有效输出
    unsafe {
        (*position).PlayOffset = mod_position(s.position, s.dma_size);
        (*position).WriteOffset = mod_position(s.position, s.dma_size);
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn stream_allocate_audio_buffer(
    this: PVOID,
    requested_size: u32,
    mdl: *mut *mut c_void,
    actual_size: *mut u32,
    offset: *mut u32,
    cache_type: *mut u32,
) -> NTSTATUS {
    let this = this as *mut WaveRTStream;
    if requested_size == 0 {
        return STATUS_UNSUCCESSFUL;
    }
    // SAFETY: 调用方保证输出指针有效；port_stream 为 IPortWaveRTStream
    let ps: &WaveRTStream = unsafe { &*(this as *const WaveRTStream) };
    // B3：COM 对象头部是 vtable 指针——先一级解引用取 vtable 指针，再解引用取
    // vtable 本体；旧实现把对象体当 vtable 用的双重间接必崩
    let vtbl: &IPortWaveRTStreamVtbl =
        unsafe { &*(*(ps.port_stream as *const *const IPortWaveRTStreamVtbl)) };
    // SAFETY: vtbl 来自有效 COM 对象；参数按 portcls.h 原型传递
    let md =
        unsafe { (vtbl.allocate_pages_for_mdl)(ps.port_stream, u64::MAX, requested_size as usize) };
    if md.is_null() {
        return STATUS_UNSUCCESSFUL;
    }
    // SAFETY: 同上；MmCached = 2
    let base = unsafe { (vtbl.map_allocated_pages)(ps.port_stream, md, 2) };
    if base.is_null() {
        // SAFETY: 释放已分配页
        unsafe { (vtbl.free_pages_from_mdl)(ps.port_stream, md) };
        return STATUS_UNSUCCESSFUL;
    }
    // SAFETY: 输出指针有效
    unsafe {
        *mdl = md;
        *actual_size = requested_size;
        *offset = 0;
        *cache_type = 2;
        (*(this as *mut WaveRTStream)).dma_buffer = base.cast();
        (*(this as *mut WaveRTStream)).dma_size = requested_size;
        (*(this as *mut WaveRTStream)).buffer_mdl = md;
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn stream_free_audio_buffer(this: PVOID, mdl: *mut c_void, _size: u32) {
    let this = this as *mut WaveRTStream;
    let ps: &WaveRTStream = unsafe { &*(this as *const WaveRTStream) };
    // SAFETY: port_stream 为有效 IPortWaveRTStream；vtbl 双重间接见 B3 注释
    let vtbl: &IPortWaveRTStreamVtbl =
        unsafe { &*(*(ps.port_stream as *const *const IPortWaveRTStreamVtbl)) };
    if !ps.dma_buffer.is_null() {
        // SAFETY: 解除映射
        unsafe { (vtbl.unmap_allocated_pages)(ps.port_stream, ps.dma_buffer.cast(), mdl) };
    }
    if !mdl.is_null() {
        // SAFETY: 释放页
        unsafe { (vtbl.free_pages_from_mdl)(ps.port_stream, mdl) };
    }
    // minor f：复位缓冲状态（dma_size=0 使 GetPosition 的窗口推进失效）
    // SAFETY: 单一流对象字段复位
    unsafe {
        let s = &mut *this;
        s.dma_buffer = core::ptr::null_mut();
        s.dma_size = 0;
        s.buffer_mdl = core::ptr::null_mut();
    }
}

unsafe extern "system" fn stream_get_hw_latency(this: PVOID, latency: *mut KSRTAUDIO_HWLATENCY) {
    // SAFETY: latency 为有效输出
    unsafe {
        (*latency).FifoSize = 0;
        (*latency).ChipsetDelay = 0;
        (*latency).CodecDelay = 0;
    }
    let _ = this;
}

/// B7：虚拟设备无硬件位置寄存器——返回 STATUS_NOT_SUPPORTED 并清零出参；
/// PortCls 收到此状态后退回轮询 GetPosition（与 B4 时间锚点语义配套）。
/// Windows 运行时行为无法宿主单测（注释说明）。
unsafe extern "system" fn stream_get_position_register(
    this: PVOID,
    reg: *mut KSRTAUDIO_HWREGISTER,
) -> NTSTATUS {
    // SAFETY: reg 为有效输出
    unsafe {
        (*reg).Register = core::ptr::null_mut();
        (*reg).Width = 0;
        (*reg).Numerator = 0;
        (*reg).Denominator = 0;
        (*reg).Accuracy = 0;
    }
    let _ = this;
    STATUS_NOT_SUPPORTED
}

unsafe extern "system" fn stream_get_clock_register(
    this: PVOID,
    reg: *mut KSRTAUDIO_HWREGISTER,
) -> NTSTATUS {
    stream_get_position_register(this, reg)
}

// ============ IMiniportWaveRT ============

#[repr(C)]
pub struct IMiniportWaveRTVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub get_description: unsafe extern "system" fn(PVOID, *mut PPCFILTER_DESCRIPTOR) -> NTSTATUS,
    pub data_range_intersection: unsafe extern "system" fn(
        PVOID,
        u32,
        *mut KSDATARANGE,
        *mut KSDATARANGE,
        u32,
        PVOID,
        *mut u32,
    ) -> NTSTATUS,
    pub init: unsafe extern "system" fn(PVOID, PVOID, *mut c_void, PVOID) -> NTSTATUS,
    pub new_stream: unsafe extern "system" fn(
        PVOID,
        *mut *mut c_void,
        PVOID,
        u32,
        bool,
        PKSDATAFORMAT,
    ) -> NTSTATUS,
    pub get_device_description:
        unsafe extern "system" fn(PVOID, *mut DEVICE_DESCRIPTION) -> NTSTATUS,
}

/// WaveRT 小端口对象
#[repr(C)]
pub struct MiniportWaveRT {
    pub vtable: &'static IMiniportWaveRTVtbl,
    pub refcount: u32,
    pub paired: *mut MiniportWaveRT,
    pub ring: *mut RingBuffer,
    pub adapter: PVOID,
    pub device_object: PDEVICE_OBJECT,
    pub capture: bool,
    /// 子对象（COM 聚合）：IPinCount / IMiniportAudioSignalProcessing
    /// 引擎（audiodg）会 QI 这两个接口，返回 E_NOINTERFACE 时端点初始化会中途停下
    /// （真机 QI 探针实测：IMiniportWaveRT → IMiniportAudioSignalProcessing →
    ///  IMiniportAudioEngineNode → IPinCount，各两轮后停止，new_stream 恒 0）。
    pub pin_count_obj: SubObject,
    pub signal_proc_obj: SubObject,
}

/// COM 聚合子对象：首字段为该接口的 vtable 指针（引擎按各自的接口布局调用），
/// 第二字段回指主 miniport 对象以便转发 IUnknown。
#[repr(C)]
pub struct SubObject {
    pub vtbl: *const c_void,
    pub owner: *mut MiniportWaveRT,
}

/// portcls.h `IID_IPinCount` = {5DADB7DC-A2CB-4540-A4A8-425EE4AE9051}
static IID_IPINCOUNT: GUID = GUID {
    data1: 0x5dad_b7dc,
    data2: 0xa2cb,
    data3: 0x4540,
    data4: [0xa4, 0xa8, 0x42, 0x5e, 0xe4, 0xae, 0x90, 0x51],
};
/// portcls.h `IID_IMiniportAudioSignalProcessing` = {B532678C-BE50-472D-9973-8A6F16594989}
static IID_IMINIPORT_AUDIO_SIGNAL_PROCESSING: GUID = GUID {
    data1: 0xb532_678c,
    data2: 0xbe50,
    data3: 0x472d,
    data4: [0x99, 0x73, 0x8a, 0x6f, 0x16, 0x59, 0x49, 0x89],
};
/// ksmedia.h `AUDIO_SIGNALPROCESSINGMODE_DEFAULT` = {C18E2F7E-933D-4965-B7D1-1EEF228D2AF3}
static AUDIO_SIGNALPROCESSINGMODE_DEFAULT: GUID = GUID {
    data1: 0xc18e_2f7e,
    data2: 0x933d,
    data3: 0x4965,
    data4: [0xb7, 0xd1, 0x1e, 0xef, 0x22, 0x8d, 0x2a, 0xf3],
};

/// portcls.h `IPinCount`：PinCount(PinId, Necessary, Current, Possible, GlobalCurrent, GlobalPossible)
#[repr(C)]
pub struct IPinCountVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub pin_count:
        unsafe extern "system" fn(PVOID, u32, *mut u32, *mut u32, *mut u32, *mut u32, *mut u32),
}

/// portcls.h `IMiniportAudioSignalProcessing`：GetModes(Pin, GUID* Modes, ULONG* NumModes)
#[repr(C)]
pub struct IMiniportAudioSignalProcessingVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub get_modes: unsafe extern "system" fn(PVOID, u32, *mut GUID, *mut u32) -> NTSTATUS,
}

unsafe extern "system" fn sub_owner(this: PVOID) -> *mut MiniportWaveRT {
    // SAFETY: 子对象首字段是 vtable 指针，第二字段是 owner
    unsafe { (*(this as *const SubObject)).owner }
}

unsafe extern "system" fn sub_qi(this: PVOID, iid: *const GUID, obj: *mut *mut c_void) -> NTSTATUS {
    let owner = unsafe { sub_owner(this) };
    // SAFETY: owner 为主 miniport 对象
    unsafe { miniport_qi(owner.cast(), iid, obj) }
}

unsafe extern "system" fn sub_addref(this: PVOID) -> u32 {
    let owner = unsafe { sub_owner(this) };
    // SAFETY: owner 为主 miniport 对象
    unsafe { miniport_addref(owner.cast()) }
}

unsafe extern "system" fn sub_release(this: PVOID) -> u32 {
    let owner = unsafe { sub_owner(this) };
    // SAFETY: owner 为主 miniport 对象
    unsafe { miniport_release(owner.cast()) }
}

/// IPinCount::PinCount —— 不改动任何计数（沿用过滤器描述符里声明的实例数：
/// host pin 1/1、bridge pin 0/0）。方法返回 void。
#[allow(clippy::too_many_arguments)]
unsafe extern "system" fn pin_count_impl(
    _this: PVOID,
    _pin: u32,
    _necessary: *mut u32,
    _current: *mut u32,
    _possible: *mut u32,
    _global_current: *mut u32,
    _global_possible: *mut u32,
) {
}

/// IMiniportAudioSignalProcessing::GetModes —— 只支持一种模式：DEFAULT。
/// 约定：`*num` 进入时为缓冲可容纳的 GUID 数；不足则回所需数量 + BUFFER_TOO_SMALL。
unsafe extern "system" fn signal_proc_get_modes(
    _this: PVOID,
    _pin: u32,
    modes: *mut GUID,
    num: *mut u32,
) -> NTSTATUS {
    if num.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let cap = unsafe { *num };
    if modes.is_null() || cap == 0 {
        unsafe { *num = 1 };
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 调用方保证 modes 至少可容纳 *num 个 GUID
    unsafe {
        core::ptr::write(modes, AUDIO_SIGNALPROCESSINGMODE_DEFAULT);
        *num = 1;
    }
    STATUS_SUCCESS
}

static PIN_COUNT_VTABLE: IPinCountVtbl = IPinCountVtbl {
    query_interface: sub_qi,
    add_ref: sub_addref,
    release: sub_release,
    pin_count: pin_count_impl,
};
static SIGNAL_PROC_VTABLE: IMiniportAudioSignalProcessingVtbl =
    IMiniportAudioSignalProcessingVtbl {
        query_interface: sub_qi,
        add_ref: sub_addref,
        release: sub_release,
        get_modes: signal_proc_get_modes,
    };

impl MiniportWaveRT {
    /// 创建小端口对象
    ///
    /// 所有权：返回 refcount=1 的引用，归适配器持有；port->Init 成功后 PortCls
    /// 的 port 对象另持一份（内部 AddRef→2）。驱动侧引用由 adapter_cleanup 释放。
    ///
    /// # Safety
    /// adapter/device_object 必须为有效指针。
    pub unsafe fn create(
        adapter: PVOID,
        device_object: PDEVICE_OBJECT,
        capture: bool,
    ) -> *mut MiniportWaveRT {
        let ptr =
            crate::sys::mem::ExAllocatePool2_np(0x40, size_of::<MiniportWaveRT>() as u64, TAG);
        if ptr.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: 刚分配的池内存
        unsafe {
            core::ptr::write(
                ptr as *mut MiniportWaveRT,
                MiniportWaveRT {
                    vtable: &WAVERT_VTABLE,
                    refcount: 1,
                    paired: core::ptr::null_mut(),
                    ring: core::ptr::null_mut(),
                    adapter,
                    device_object,
                    capture,
                    // owner 先填空，分配完成后回填自身地址（子对象需要回指主对象）
                    pin_count_obj: SubObject {
                        vtbl: (&raw const PIN_COUNT_VTABLE).cast::<c_void>(),
                        owner: core::ptr::null_mut(),
                    },
                    signal_proc_obj: SubObject {
                        vtbl: (&raw const SIGNAL_PROC_VTABLE).cast::<c_void>(),
                        owner: core::ptr::null_mut(),
                    },
                },
            );
            // 回填子对象的 owner（主对象地址此时已确定）
            (*ptr.cast::<MiniportWaveRT>()).pin_count_obj.owner = ptr.cast::<MiniportWaveRT>();
            (*ptr.cast::<MiniportWaveRT>()).signal_proc_obj.owner = ptr.cast::<MiniportWaveRT>();
        }
        ptr.cast::<MiniportWaveRT>()
    }

    /// 设置配对小端口（环回）
    pub fn set_paired(&mut self, paired: *mut MiniportWaveRT) {
        self.paired = paired;
    }

    /// 设置共享环形缓冲
    pub fn set_ring(&mut self, ring: *mut RingBuffer) {
        self.ring = ring;
    }
}

unsafe extern "system" fn miniport_qi(
    this: PVOID,
    iid: *const GUID,
    obj: *mut *mut c_void,
) -> NTSTATUS {
    let this = this as *mut MiniportWaveRT;
    // 诊断：记录引擎问过的接口（最近 8 个 GUID 的 data1）
    let idx = DBG_QI_COUNT.fetch_add(1, Ordering::Relaxed);
    let d1 = unsafe { (*iid).data1 };
    match idx % 8 {
        0 => DBG_QI_0.store(d1, Ordering::Relaxed),
        1 => DBG_QI_1.store(d1, Ordering::Relaxed),
        2 => DBG_QI_2.store(d1, Ordering::Relaxed),
        3 => DBG_QI_3.store(d1, Ordering::Relaxed),
        4 => DBG_QI_4.store(d1, Ordering::Relaxed),
        5 => DBG_QI_5.store(d1, Ordering::Relaxed),
        6 => DBG_QI_6.store(d1, Ordering::Relaxed),
        _ => DBG_QI_7.store(d1, Ordering::Relaxed),
    }
    // SAFETY: 调用方保证 iid/obj 有效
    unsafe {
        if is_equal_guid(iid, &IID_IUnknown)
            || is_equal_guid(iid, &IID_IMiniport)
            || is_equal_guid(iid, &IID_IMiniportWaveRT)
        {
            *obj = this.cast();
            interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
            dbg_inc(&DBG_QI_OK);
            STATUS_SUCCESS
        } else if is_equal_guid(iid, &IID_IPINCOUNT) {
            // 子对象：IPinCount（引擎端点初始化会 QI）
            *obj = core::ptr::addr_of_mut!((*this).pin_count_obj).cast::<c_void>();
            interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
            dbg_inc(&DBG_QI_OK);
            STATUS_SUCCESS
        } else if is_equal_guid(iid, &IID_IMINIPORT_AUDIO_SIGNAL_PROCESSING) {
            // 子对象：IMiniportAudioSignalProcessing（信号处理模式枚举）
            *obj = core::ptr::addr_of_mut!((*this).signal_proc_obj).cast::<c_void>();
            interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
            dbg_inc(&DBG_QI_OK);
            STATUS_SUCCESS
        } else {
            // minor e：COM 约定 E_NOINTERFACE
            *obj = core::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

pub unsafe extern "system" fn miniport_addref(this: PVOID) -> u32 {
    // SAFETY: this 指向小端口
    unsafe {
        interlocked_increment(core::ptr::addr_of_mut!(
            (*(this as *mut MiniportWaveRT)).refcount
        ))
    }
}

pub unsafe extern "system" fn miniport_release(this: PVOID) -> u32 {
    let this = this as *mut MiniportWaveRT;
    // SAFETY: 引用计数保护
    let rc = unsafe { interlocked_decrement(core::ptr::addr_of_mut!((*this).refcount)) };
    if rc == 0 {
        // SAFETY: 释放对象内存
        unsafe {
            crate::sys::mem::ExFreePoolWithTag_np(this.cast(), TAG);
        }
    }
    rc
}

unsafe extern "system" fn miniport_get_description(
    this: PVOID,
    desc: *mut PPCFILTER_DESCRIPTOR,
) -> NTSTATUS {
    // SAFETY: 输出指针有效；返回静态描述符（capture=SOURCE / render=SINK）
    let this = this as *mut MiniportWaveRT;
    unsafe {
        if (*this).capture {
            *desc = core::ptr::addr_of!(FILTER_DESC_CAPTURE) as *mut PCFILTER_DESCRIPTOR;
        } else {
            *desc = core::ptr::addr_of!(FILTER_DESC_RENDER) as *mut PCFILTER_DESCRIPTOR;
        }
    }
    STATUS_SUCCESS
}

/// minor c：数据范围基本交集——客户端 MajorFormat/SubFormat 必须与本地范围一致
unsafe extern "system" fn miniport_data_range_intersection(
    this: PVOID,
    _pin: u32,
    client: *mut KSDATARANGE,
    _mine: *mut KSDATARANGE,
    out_len: u32,
    out: PVOID,
    out_len_ret: *mut u32,
) -> NTSTATUS {
    dbg_inc(&DBG_RANGE_CALLS);
    let _ = this;
    // _mine（本地数据范围）与 _pin 未用：交集仅比对客户端范围与 AUDIO_DATA_RANGE 常量
    // SAFETY: out_len_ret 为有效输出
    unsafe { *out_len_ret = 0 };
    // SAFETY: client 由 PortCls 传入且指向完整 KSDATARANGE
    let c = unsafe { &*client };
    DBG_RANGE_LAST_MAJOR.store(c.MajorFormat.data1, Ordering::Relaxed);
    DBG_RANGE_LAST_SUB.store(c.SubFormat.data1, Ordering::Relaxed);
    DBG_RANGE_LAST_SPEC.store(c.Specifier.data1, Ordering::Relaxed);
    DBG_RANGE_LAST_FSIZE.store(c.FormatSize, Ordering::Relaxed);
    DBG_RANGE_LAST_OUTLEN.store(out_len, Ordering::Relaxed);
    if c.MajorFormat != KSDATAFORMAT_TYPE_AUDIO || c.SubFormat != KSDATAFORMAT_SUBTYPE_PCM {
        DBG_RANGE_LAST_STATUS.store(STATUS_NO_MATCH as u32, Ordering::Relaxed);
        return STATUS_NO_MATCH;
    }
    let needed = size_of::<KSDATAFORMAT>() + size_of::<WAVEFORMATEX>();
    // 0 长度（或空缓冲）= 调用方"只问需要多大"：必须回 STATUS_BUFFER_OVERFLOW + 所需长度。
    // 回归（决定"端点打不开"的真凶）：原来这里统一回 STATUS_BUFFER_TOO_SMALL，调用方视为硬失败、
    // 不再带足缓冲重试 —— 真机计数器实测 range=314 / range_ok=0 / 全部 out_len=0 +
    // BUFFER_TOO_SMALL，引擎因此拿不到可用格式，new_stream 一直是 0，端点表现为
    // IAudioClient::GetMixFormat|Initialize 报 0x80070491。契约对照官方 sysvad
    // minwavert.cpp::DataRangeIntersection（"if (!OutputBufferLength) { *len = requiredSize;
    // return STATUS_BUFFER_OVERFLOW; } else if (OutputBufferLength < requiredSize) {
    // return STATUS_BUFFER_TOO_SMALL; }"）。
    if out_len == 0 || out.is_null() {
        // SAFETY: 输出指针有效
        unsafe { *out_len_ret = needed as u32 };
        DBG_RANGE_LAST_STATUS.store(STATUS_BUFFER_OVERFLOW as u32, Ordering::Relaxed);
        return STATUS_BUFFER_OVERFLOW;
    }
    if out_len < needed as u32 {
        // SAFETY: 输出指针有效
        unsafe { *out_len_ret = needed as u32 };
        DBG_RANGE_LAST_STATUS.store(STATUS_BUFFER_TOO_SMALL as u32, Ordering::Relaxed);
        return STATUS_BUFFER_TOO_SMALL;
    }
    let df = out as *mut KSDATAFORMAT;
    // SAFETY: 缓冲区足够
    unsafe {
        core::ptr::write(
            df,
            KSDATAFORMAT {
                FormatSize: needed as u32,
                Flags: 0,
                SampleSize: 0,
                Reserved: 0,
                MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
                SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
                Specifier: KSDATAFORMAT_SPECIFIER_WAVEFORMATEX,
            },
        );
        let wf = (df as *mut u8).add(size_of::<KSDATAFORMAT>()) as *mut WAVEFORMATEX;
        core::ptr::write(
            wf,
            WAVEFORMATEX {
                wFormatTag: 1, // WAVE_FORMAT_PCM
                nChannels: 2,
                nSamplesPerSec: 48_000,
                nAvgBytesPerSec: 48_000 * 2 * 2,
                nBlockAlign: 4,
                wBitsPerSample: 16,
                cbSize: 0,
            },
        );
        *out_len_ret = needed as u32;
    }
    dbg_inc(&DBG_RANGE_OK);
    DBG_RANGE_OK_SEEN.store(1, Ordering::Relaxed);
    STATUS_SUCCESS
}

unsafe extern "system" fn miniport_init(
    this: PVOID,
    unknown_adapter: PVOID,
    _resource_list: *mut c_void,
    port: PVOID,
) -> NTSTATUS {
    dbg_inc(&DBG_INIT_CALLS);
    let this = this as *mut MiniportWaveRT;
    // SAFETY: 单线程初始化
    unsafe {
        (*this).adapter = unknown_adapter;
    }
    let _ = port;
    STATUS_SUCCESS
}

unsafe extern "system" fn miniport_new_stream(
    this: PVOID,
    stream_out: *mut *mut c_void,
    port_stream: PVOID,
    _pin: u32,
    capture: bool,
    data_format: PKSDATAFORMAT,
) -> NTSTATUS {
    dbg_inc(&DBG_NEW_STREAM_CALLS);
    let this = this as *mut MiniportWaveRT;
    // 校验格式（详细字段校验在 SetFormat，PortCls 紧随其后调用）
    // SAFETY: 调用方保证 data_format 有效
    let df = unsafe { &*data_format };
    if df.SubFormat != KSDATAFORMAT_SUBTYPE_PCM {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // SAFETY: 创建流对象
    let stream = unsafe { WaveRTStream::new(this, port_stream, capture) };
    if stream.is_null() {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // SAFETY: 输出流指针
    unsafe { *stream_out = stream.cast() };
    STATUS_SUCCESS
}

unsafe extern "system" fn miniport_get_device_description(
    _this: PVOID,
    desc: *mut DEVICE_DESCRIPTION,
) -> NTSTATUS {
    // SAFETY: 输出指针有效
    unsafe {
        core::ptr::write(
            desc,
            DEVICE_DESCRIPTION {
                Version: DEVICE_DESCRIPTION_VERSION1,
                Master: 0,
                ScatterGather: 0,
                DemandMode: 0,
                AutoInitialize: 0,
                Dma32BitAddresses: 0,
                IgnoreCount: 0,
                Reserved1: 0,
                Dma64BitAddresses: 1,
                BusNumber: 0,
                DmaChannel: 0,
                // 虚拟设备无真实 DMA 总线/控制器：InterfaceType 取 0（Internal）、
                // DmaSpeed 取 0（Compatible），与既有全零填充等价（本地
                // /tmp/sysvad-ref 无 GetDeviceDescription 对照样本，保留批次验证值）
                InterfaceType: INTERFACE_TYPE::Internal,
                DmaWidth: DMA_WIDTH::Width32Bits,
                DmaSpeed: DMA_SPEED::Compatible,
                MaximumLength: 0xFFFF_FFFF,
                DmaPort: 0,
            },
        );
    }
    STATUS_SUCCESS
}

static WAVERT_VTABLE: IMiniportWaveRTVtbl = IMiniportWaveRTVtbl {
    query_interface: miniport_qi,
    add_ref: miniport_addref,
    release: miniport_release,
    get_description: miniport_get_description,
    data_range_intersection: miniport_data_range_intersection,
    init: miniport_init,
    new_stream: miniport_new_stream,
    get_device_description: miniport_get_device_description,
};

// ============ 过滤器描述符（每端点：render=SINK / capture=SOURCE） ============
// 布局与字段逐项对照 portcls.h / ksmedia.h（B6）；GUID 值对照 ksmedia.h。

/// ksmedia.h KSDATAFORMAT_TYPE_AUDIO = 73647561-0000-0010-8000-00aa00389b71
/// （旧值 0x0000_000f 为杜撰，音频主格式 GUID 错误导致格式协商必败）
static KSDATAFORMAT_TYPE_AUDIO: GUID = GUID {
    data1: 0x7364_7561,
    data2: 0x0000,
    data3: 0x0010,
    data4: [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
};
static KSDATAFORMAT_SUBTYPE_PCM: GUID = GUID {
    data1: 0x0000_0001,
    data2: 0x0000,
    data3: 0x0010,
    data4: [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
};
static KSDATAFORMAT_SPECIFIER_WAVEFORMATEX: GUID = GUID {
    data1: 0x0558_9f81,
    data2: 0xc356,
    data3: 0x11ce,
    data4: [0xbf, 0x01, 0x00, 0xaa, 0x00, 0x55, 0x59, 0x5a],
};
/// ksmedia.h KSDATAFORMAT_SUBTYPE_ANALOG = 6dba3190-67bd-11cf-a0f7-0020afd156e4
/// （bridge pin 的 subformat；对照 sysvad speakertoptable.h 与 ToDesk 虚拟声卡实测）
static KSDATAFORMAT_SUBTYPE_ANALOG: GUID = GUID {
    data1: 0x6dba_3190,
    data2: 0x67bd,
    data3: 0x11cf,
    data4: [0xa0, 0xf7, 0x00, 0x20, 0xaf, 0xd1, 0x56, 0xe4],
};
/// ks.h KSDATAFORMAT_SPECIFIER_NONE = 0f6417d6-c318-11d0-a43f-00a0c9223196
static KSDATAFORMAT_SPECIFIER_NONE: GUID = GUID {
    data1: 0x0f64_17d6,
    data2: 0xc318,
    data3: 0x11d0,
    data4: [0xa4, 0x3f, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
static KSINTERFACESETID_STANDARD: GUID = GUID {
    data1: 0x1a87_66a0,
    data2: 0x62ce,
    data3: 0x11cf,
    data4: [0xa5, 0xd6, 0x28, 0xdb, 0x04, 0xc1, 0x00, 0x00],
};
static KSMEDIUMSETID_STANDARD: GUID = GUID {
    data1: 0x4747_b320,
    data2: 0x62ce,
    data3: 0x11cf,
    data4: [0xa5, 0xd6, 0x28, 0xdb, 0x04, 0xc1, 0x00, 0x00],
};
/// ksmedia.h KSNODETYPE_SPEAKER = dff21ce1-f70f-11d0-b917-00a0c9223196
static KSNODETYPE_SPEAKER: GUID = GUID {
    data1: 0xdff2_1ce1,
    data2: 0xf70f,
    data3: 0x11d0,
    data4: [0xb9, 0x17, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
/// ksmedia.h KSNODETYPE_MICROPHONE = dff21be1-f70f-11d0-b917-00a0c9223196
static KSNODETYPE_MICROPHONE: GUID = GUID {
    data1: 0xdff2_1be1,
    data2: 0xf70f,
    data3: 0x11d0,
    data4: [0xb9, 0x17, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};

/// PCM 16bit / 48kHz / 2ch 数据范围
static AUDIO_DATA_RANGE: KSDATARANGE_AUDIO = KSDATARANGE_AUDIO {
    DataRange: KSDATARANGE {
        FormatSize: size_of::<KSDATARANGE_AUDIO>() as u32,
        Flags: 0,
        SampleSize: 0,
        Reserved: 0,
        MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
        Specifier: KSDATAFORMAT_SPECIFIER_WAVEFORMATEX,
    },
    MaximumChannels: 2,
    MinimumBitsPerSample: 16,
    MaximumBitsPerSample: 16,
    MinimumSampleFrequency: 48_000,
    MaximumSampleFrequency: 48_000,
};

/// 数据范围指针数组——静态共享要求 Sync，本工具链对含裸指针的类型不自动满足，
/// 故保留显式 Sync 包装（原实现同款）
struct SyncDataRanges([*const KSDATARANGE; 1]);
// SAFETY: 静态只读数据，永不写入
unsafe impl Sync for SyncDataRanges {}

static DATA_RANGES: SyncDataRanges = SyncDataRanges([&raw const AUDIO_DATA_RANGE.DataRange]);

/// bridge pin 的数据范围：TYPE_AUDIO / SUBTYPE_ANALOG / SPECIFIER_NONE
/// （对照 sysvad speakertoptable.h 与 ToDesk 虚拟声卡 wave 滤波器 pin1 实测值）
static AUDIO_BRIDGE_RANGE: KSDATARANGE = KSDATARANGE {
    FormatSize: size_of::<KSDATARANGE>() as u32,
    Flags: 0,
    SampleSize: 0,
    Reserved: 0,
    MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
    SubFormat: KSDATAFORMAT_SUBTYPE_ANALOG,
    Specifier: KSDATAFORMAT_SPECIFIER_NONE,
};
static BRIDGE_DATA_RANGES: SyncDataRanges = SyncDataRanges([&raw const AUDIO_BRIDGE_RANGE]);

static PIN_INTERFACES: [KSPIN_INTERFACE; 1] = [KSPIN_INTERFACE {
    Set: KSINTERFACESETID_STANDARD,
    Id: KSINTERFACE_STANDARD_STREAMING,
    Flags: 0,
}];

static PIN_MEDIUMS: [KSPIN_MEDIUM; 1] = [KSPIN_MEDIUM {
    Set: KSMEDIUMSETID_STANDARD,
    Id: KSMEDIUM_TYPE_ANYINSTANCE,
    Flags: 0,
}];

/// 过滤器注册类别（替代已删除的 IoRegisterDeviceInterface 块，minor a）
/// 回归：类别集合必须含 KSCATEGORY_RENDER/CAPTURE（不能只登记 AUDIO），
/// 否则 PortCls 不会为播放/录音类别创建子设备符号链接，音频端点（控制面板
/// 「vdev 扬声器/麦克风」）就不会出现。常量与宿主单测见 endpoint_names.rs。
static FILTER_CATEGORIES: [GUID; 4] = crate::endpoint_names::WAVE_CATEGORIES;

// ============ 滤波器级属性：预设格式（KSPROPERTY_PIN_PROPOSEDATAFORMAT/2） ============
//
// 音频引擎（audiodg/EndpointBuilder）在端点初始化时要用这两个属性**向驱动定设备格式**；
// 驱动不实现时端点能建出来、但拿不到设备格式（实测：MMDevices 里缺
// PKEY_AudioEngine_DeviceFormat，应用侧 IAudioClient::GetMixFormat 报 0x80070491，
// 端点不可用）。同机 ToDesk 虚拟声卡实现了 PROPOSEDATAFORMAT2（探针返回 87/50，
// 表示属性存在但请求不完整），vdev 原实现返回 1168「属性不存在」。sysvad 的
// speakertoptable.h / minwavert.cpp 同款：PROPOSEDATAFORMAT(SET) + PROPOSEDATAFORMAT2(GET)。

use crate::topology::{PCAUTOMATION_TABLE, PCPROPERTY_ITEM, PCPROPERTY_REQUEST};

/// ks.h KSPROPSETID_Pin = 8C134960-51AD-11CF-878A-94F801C10000
static KSPROPSETID_PIN: GUID = GUID {
    data1: 0x8c13_4960,
    data2: 0x51ad,
    data3: 0x11cf,
    data4: [0x87, 0x8a, 0x94, 0xf8, 0x01, 0xc1, 0x00, 0x00],
};
const KSPROPERTY_PIN_PROPOSEDATAFORMAT: ULONG = 14;
const KSPROPERTY_PIN_PROPOSEDATAFORMAT2: ULONG = 15;
const KSPROPERTY_TYPE_GET: ULONG = 0x0000_0001;
const KSPROPERTY_TYPE_SET: ULONG = 0x0000_0002;
const KSPROPERTY_TYPE_BASICSUPPORT: ULONG = 0x0000_0200;

/// mmreg.h WAVEFORMATEXTENSIBLE（pack(2)：GUID 落 offset 24，整块 40 字节）
#[repr(C, packed(2))]
#[derive(Clone, Copy)]
struct WaveFormatExtensible {
    format_tag: u16,
    channels: u16,
    samples_per_sec: u32,
    avg_bytes_per_sec: u32,
    block_align: u16,
    bits_per_sample: u16,
    cb_size: u16,
    valid_bits_per_sample: u16,
    channel_mask: u32,
    sub_format: GUID,
}

/// ksmedia.h KSDATAFORMAT_WAVEFORMATEXTENSIBLE（64 + 40 = 104 字节）
#[repr(C)]
struct KsDataFormatWaveFormatExtensible {
    data_format: KSDATAFORMAT,
    wave: WaveFormatExtensible,
}

// 布局回归（编译期）：设备格式必须是 ksmedia.h 的 104 字节布局，
// 引擎按该布局解析；WAVEFORMATEXTENSIBLE 若忘了 pack(2) 会变成 44 字节（108 total）。
const _: () = assert!(size_of::<WaveFormatExtensible>() == 40);
const _: () = assert!(size_of::<KsDataFormatWaveFormatExtensible>() == 104);

/// 本驱动唯一支持的设备格式：48 kHz / 16 bit / 2ch PCM（与 AUDIO_DATA_RANGE 一致）
static DEVICE_FORMAT: KsDataFormatWaveFormatExtensible = KsDataFormatWaveFormatExtensible {
    data_format: KSDATAFORMAT {
        FormatSize: size_of::<KsDataFormatWaveFormatExtensible>() as u32,
        Flags: 0,
        SampleSize: 0,
        Reserved: 0,
        MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
        SubFormat: KSDATAFORMAT_SUBTYPE_PCM,
        Specifier: KSDATAFORMAT_SPECIFIER_WAVEFORMATEX,
    },
    wave: WaveFormatExtensible {
        format_tag: 0xFFFE, // WAVE_FORMAT_EXTENSIBLE
        channels: 2,
        samples_per_sec: 48_000,
        avg_bytes_per_sec: 192_000,
        block_align: 4,
        bits_per_sample: 16,
        cb_size: 22,
        valid_bits_per_sample: 16,
        channel_mask: 3,
        sub_format: KSDATAFORMAT_SUBTYPE_PCM,
    },
};

/// 预设格式属性处理器：GET 回设备格式；SET 只接受本驱动支持的 PCM 形状；
/// BASICSUPPORT 回四种 verb 位（PortCls 会用它应答 KSPROPERTY_TYPE_BASICSUPPORT）。
unsafe extern "system" fn wave_format_property_handler(req: *mut PCPROPERTY_REQUEST) -> NTSTATUS {
    // SAFETY: PortCls 保证 req 及其字段有效
    let r = unsafe { &mut *req };
    if r.PropertyItem.is_null() || r.Value.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let verb = r.Verb;

    if verb & KSPROPERTY_TYPE_BASICSUPPORT != 0 {
        if r.ValueSize < 4 {
            return STATUS_BUFFER_TOO_SMALL;
        }
        // SAFETY: ValueSize >= 4
        unsafe {
            core::ptr::write(
                r.Value.cast::<u32>(),
                KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT,
            );
        }
        r.ValueSize = 4;
        return STATUS_SUCCESS;
    }

    let need = size_of::<KsDataFormatWaveFormatExtensible>() as u32;
    if verb & KSPROPERTY_TYPE_GET != 0 {
        dbg_inc(&DBG_PROPOSED_GET);
        if r.ValueSize < need {
            return STATUS_BUFFER_TOO_SMALL;
        }
        // SAFETY: 目标缓冲区 >= need 字节；源为只读静态（DEVICE_FORMAT 永不改写）
        unsafe {
            core::ptr::copy_nonoverlapping(
                (&raw const DEVICE_FORMAT).cast::<u8>(),
                r.Value.cast::<u8>(),
                need as usize,
            );
        }
        r.ValueSize = need;
        return STATUS_SUCCESS;
    }

    if verb & KSPROPERTY_TYPE_SET != 0 {
        // 接受引擎提出的任何 PCM 形状（WAVEFORMATEX 或 WAVEFORMATEXTENSIBLE）：
        // 设备格式由本驱动的数据范围/INF OEMFormat 决定，这里拒绝只会让引擎判定
        // "设备不可用"（实测：严格只收 16bit/48k 时端点仍拿不到设备格式）。
        // 真正的播放/录音格式校验在下游 stream_set_format / data_range_intersection。
        if r.ValueSize < 16 {
            return STATUS_INVALID_PARAMETER;
        }
        // SAFETY: ValueSize >= 16，逐字段 read_unaligned 不要求对齐
        let p = r.Value.cast::<u8>();
        // 值可能是 KSDATAFORMAT_WAVEFORMATEX(TENSIBLE)（offset 0 是 FormatSize，
        // wFormatTag 在 KSDATAFORMAT 之后 = offset 64），也可能是裸 WAVEFORMATEX（tag 在 0）。
        // 回归（真机计数器：prop_set=1883、prop_last_tag=104=FormatSize）：原来一律按裸
        // WAVEFORMATEX 在 offset 0 取 tag，于是把 104 当 tag、每次回
        // STATUS_INVALID_PARAMETER —— 引擎反复提案却永远拿不到确认，端点因此打不开。
        let tag_off = if r.ValueSize >= (size_of::<KSDATAFORMAT>() + 16) as u32 {
            size_of::<KSDATAFORMAT>()
        } else {
            0
        };
        let tag = unsafe { core::ptr::read_unaligned(p.add(tag_off).cast::<u16>()) };
        dbg_inc(&DBG_PROPOSED_SET);
        DBG_PROPOSED_LAST_TAG.store(u32::from(tag), Ordering::Relaxed);
        if tag != 1 && tag != 0xFFFE {
            return STATUS_INVALID_PARAMETER;
        }
        // KSPROPERTY_PIN_PROPOSEDATAFORMAT / 2 的 Value 是**输入输出**缓冲：驱动必须把
        // "实际会用的格式"写回去（引擎据此定设备格式）。只回 success 不回填时，引擎读到
        // 的还是它自己提的形状、于是反复重试（真机计数器实测 prop_set=2070 且循环不止）。
        let base = if tag_off == 0 {
            // 裸 WAVEFORMATEX：就地升级为 KSDATAFORMAT_WAVEFORMATEXTENSIBLE（若缓冲够）
            if r.ValueSize < need {
                return STATUS_SUCCESS; // 缓冲太小：接受但不回填（引擎会另取设备格式）
            }
            0usize
        } else {
            tag_off - size_of::<KSDATAFORMAT>()
        };
        if (r.ValueSize as usize) >= base + need as usize {
            // SAFETY: 目标缓冲 >= base + 104 字节
            unsafe {
                core::ptr::copy_nonoverlapping(
                    (&raw const DEVICE_FORMAT).cast::<u8>(),
                    p.add(base),
                    need as usize,
                );
            }
            r.ValueSize = (base as u32) + need;
        }
        dbg_inc(&DBG_PROPOSED_SET_OK);
        return STATUS_SUCCESS;
    }

    STATUS_INVALID_PARAMETER
}

static WAVE_PROPERTY_ITEMS: [PCPROPERTY_ITEM; 3] = [
    PCPROPERTY_ITEM {
        Set: &KSPROPSETID_PIN,
        Id: KSPROPERTY_PIN_PROPOSEDATAFORMAT,
        Flags: KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT,
        Handler: wave_format_property_handler,
    },
    PCPROPERTY_ITEM {
        Set: &KSPROPSETID_PIN,
        Id: KSPROPERTY_PIN_PROPOSEDATAFORMAT2,
        Flags: KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT,
        Handler: wave_format_property_handler,
    },
    // 诊断用：自定义只读属性集（用户态 IOCTL_KS_PROPERTY 读计数器）
    PCPROPERTY_ITEM {
        Set: &KSPROPSETID_VDEV_DEBUG,
        Id: KSPROPERTY_VDEV_DEBUG_STATS,
        Flags: KSPROPERTY_TYPE_GET,
        Handler: vdev_debug_property_handler,
    },
];
static WAVE_AUTOMATION: PCAUTOMATION_TABLE = PCAUTOMATION_TABLE {
    PropertyItemSize: size_of::<PCPROPERTY_ITEM>() as ULONG,
    PropertyCount: WAVE_PROPERTY_ITEMS.len() as ULONG,
    Properties: WAVE_PROPERTY_ITEMS.as_ptr(),
    MethodItemSize: 0,
    MethodCount: 0,
    Methods: core::ptr::null(),
    EventItemSize: 0,
    EventCount: 0,
    Events: core::ptr::null(),
    Reserved: 0,
};

// ============ 真机诊断计数器（自定义只读属性集） ============
//
// 目的：端点"看得见但打不开"时，无法从用户态判断引擎究竟走到驱动哪一步。
// 这组计数器用自定义 KSPROPSETID 暴露，用户态用 IOCTL_KS_PROPERTY 读取即可
// （无需改 CLI、无需 DbgPrint/DebugView）。生产构建里也可留着——只读、无副作用。
static DBG_INIT_CALLS: AtomicU32 = AtomicU32::new(0);
static DBG_NEW_STREAM_CALLS: AtomicU32 = AtomicU32::new(0);
static DBG_SET_FORMAT_CALLS: AtomicU32 = AtomicU32::new(0);
static DBG_SET_FORMAT_OK: AtomicU32 = AtomicU32::new(0);
static DBG_LAST_TAG: AtomicU32 = AtomicU32::new(0);
static DBG_LAST_CHANNELS: AtomicU32 = AtomicU32::new(0);
static DBG_LAST_RATE: AtomicU32 = AtomicU32::new(0);
static DBG_LAST_BITS: AtomicU32 = AtomicU32::new(0);
static DBG_LAST_FORMAT_SIZE: AtomicU32 = AtomicU32::new(0);
static DBG_LAST_STATUS: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_CALLS: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_OK: AtomicU32 = AtomicU32::new(0);
static DBG_PROPOSED_SET: AtomicU32 = AtomicU32::new(0);
static DBG_PROPOSED_GET: AtomicU32 = AtomicU32::new(0);
static DBG_PROPOSED_LAST_TAG: AtomicU32 = AtomicU32::new(0);
static DBG_PROPOSED_SET_OK: AtomicU32 = AtomicU32::new(0);
// 数据范围求交失败的现场（引擎一直在问，但 314 次全 NO_MATCH/TOO_SMALL）
static DBG_RANGE_LAST_MAJOR: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_LAST_SUB: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_LAST_SPEC: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_LAST_FSIZE: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_LAST_OUTLEN: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_LAST_STATUS: AtomicU32 = AtomicU32::new(0);
/// QueryInterface 追踪：引擎会 QI 各种接口（IMiniportWaveRT / IMiniportAudioEngineNode …），
/// 记录总次数与最近 8 个被请求 GUID 的 data1，用于判断它是否在某个 QI 上停下。
static DBG_QI_COUNT: AtomicU32 = AtomicU32::new(0);
static DBG_QI_0: AtomicU32 = AtomicU32::new(0);
static DBG_QI_1: AtomicU32 = AtomicU32::new(0);
static DBG_QI_2: AtomicU32 = AtomicU32::new(0);
static DBG_QI_3: AtomicU32 = AtomicU32::new(0);
static DBG_QI_4: AtomicU32 = AtomicU32::new(0);
static DBG_QI_5: AtomicU32 = AtomicU32::new(0);
static DBG_QI_6: AtomicU32 = AtomicU32::new(0);
static DBG_QI_7: AtomicU32 = AtomicU32::new(0);
static DBG_QI_OK: AtomicU32 = AtomicU32::new(0);
static DBG_RANGE_OK_SEEN: AtomicU32 = AtomicU32::new(0);

fn dbg_inc(counter: &AtomicU32) {
    counter.fetch_add(1, Ordering::Relaxed);
}

/// 自定义诊断属性集 GUID（仅本驱动使用，随机生成不复用系统 GUID）
static KSPROPSETID_VDEV_DEBUG: GUID = GUID {
    data1: 0x7f4e_2a11,
    data2: 0x9c3b,
    data3: 0x4b6e,
    data4: [0x8f, 0x2a, 0x1d, 0x2c, 0x3b, 0x4a, 0x5e, 0x60],
};
const KSPROPERTY_VDEV_DEBUG_STATS: ULONG = 0;
/// 计数器快照长度：32 个 u32
const DBG_STATS_LEN: usize = 33 * 4;

unsafe extern "system" fn vdev_debug_property_handler(req: *mut PCPROPERTY_REQUEST) -> NTSTATUS {
    // SAFETY: PortCls 保证 req 有效
    let r = unsafe { &mut *req };
    if r.Value.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if r.Verb & KSPROPERTY_TYPE_GET != 0 {
        if (r.ValueSize as usize) < DBG_STATS_LEN {
            r.ValueSize = DBG_STATS_LEN as ULONG;
            return STATUS_BUFFER_TOO_SMALL;
        }
        let vals = [
            DBG_INIT_CALLS.load(Ordering::Relaxed),
            DBG_NEW_STREAM_CALLS.load(Ordering::Relaxed),
            DBG_SET_FORMAT_CALLS.load(Ordering::Relaxed),
            DBG_SET_FORMAT_OK.load(Ordering::Relaxed),
            DBG_LAST_TAG.load(Ordering::Relaxed),
            DBG_LAST_CHANNELS.load(Ordering::Relaxed),
            DBG_LAST_RATE.load(Ordering::Relaxed),
            DBG_LAST_BITS.load(Ordering::Relaxed),
            DBG_LAST_FORMAT_SIZE.load(Ordering::Relaxed),
            DBG_LAST_STATUS.load(Ordering::Relaxed),
            DBG_RANGE_CALLS.load(Ordering::Relaxed),
            DBG_RANGE_OK.load(Ordering::Relaxed),
            DBG_PROPOSED_SET.load(Ordering::Relaxed),
            DBG_PROPOSED_GET.load(Ordering::Relaxed),
            DBG_PROPOSED_LAST_TAG.load(Ordering::Relaxed),
            DBG_RANGE_LAST_MAJOR.load(Ordering::Relaxed),
            DBG_RANGE_LAST_SUB.load(Ordering::Relaxed),
            DBG_RANGE_LAST_SPEC.load(Ordering::Relaxed),
            DBG_RANGE_LAST_FSIZE.load(Ordering::Relaxed),
            DBG_RANGE_LAST_OUTLEN.load(Ordering::Relaxed),
            DBG_RANGE_LAST_STATUS.load(Ordering::Relaxed),
            DBG_QI_COUNT.load(Ordering::Relaxed),
            DBG_QI_OK.load(Ordering::Relaxed),
            DBG_QI_0.load(Ordering::Relaxed),
            DBG_QI_1.load(Ordering::Relaxed),
            DBG_QI_2.load(Ordering::Relaxed),
            DBG_QI_3.load(Ordering::Relaxed),
            DBG_QI_4.load(Ordering::Relaxed),
            DBG_QI_5.load(Ordering::Relaxed),
            DBG_QI_6.load(Ordering::Relaxed),
            DBG_QI_7.load(Ordering::Relaxed),
            DBG_RANGE_OK_SEEN.load(Ordering::Relaxed),
            DBG_PROPOSED_SET_OK.load(Ordering::Relaxed),
        ];
        // SAFETY: ValueSize >= DBG_STATS_LEN 已校验；逐元素 unaligned 写入
        unsafe {
            for (i, v) in vals.iter().enumerate() {
                core::ptr::write_unaligned(r.Value.cast::<u32>().add(i), *v);
            }
        }
        r.ValueSize = DBG_STATS_LEN as ULONG;
        return STATUS_SUCCESS;
    }
    STATUS_INVALID_PARAMETER
}

/// portcls.h:1509 `PCFILTER_NODE` = ks.h:922 `KSFILTER_NODE` = `(ULONG)-1`
/// （同 topology.rs 的本地常量；滤波器内部连接的端点之一）
const PCFILTER_NODE: u32 = u32::MAX;

/// ntstatus.h `STATUS_BUFFER_OVERFLOW` = 0x80000005（**warning 级**成功码）：
/// KSPROPERTY_PIN_DATAINTERSECTION 以 0 长度缓冲询问"需要多大"时，PortCls/官方样例
/// （sysvad minwavert.cpp）即用它 + `*ResultantFormatLength = requiredSize` 应答；
/// 若这里回 STATUS_BUFFER_TOO_SMALL，调用方按硬失败处理、不会带足缓冲重试。
const STATUS_BUFFER_OVERFLOW: NTSTATUS = 0x8000_0005u32 as i32;

/// wave 滤波器内部连接：host pin ↔ bridge pin（对照本机 ToDesk 虚拟声卡实测
/// `KSPROPERTY_TOPOLOGY_CONNECTIONS` 返回 1 条连接；sysvad 是经
/// KSNODETYPE_AUDIO_ENGINE 节点中转，本驱动不做音频引擎节点，直接相连）。
static WAVE_RENDER_CONNECTIONS: [PCCONNECTION_DESCRIPTOR; 1] = [PCCONNECTION_DESCRIPTOR {
    FromNode: PCFILTER_NODE,
    FromNodePin: crate::endpoint_names::WAVE_RENDER_HOST_PIN,
    ToNode: PCFILTER_NODE,
    ToNodePin: crate::endpoint_names::WAVE_RENDER_BRIDGE_PIN,
}];
static WAVE_CAPTURE_CONNECTIONS: [PCCONNECTION_DESCRIPTOR; 1] = [PCCONNECTION_DESCRIPTOR {
    FromNode: PCFILTER_NODE,
    FromNodePin: crate::endpoint_names::WAVE_CAPTURE_BRIDGE_PIN,
    ToNode: PCFILTER_NODE,
    ToNodePin: crate::endpoint_names::WAVE_CAPTURE_HOST_PIN,
}];

/// render 小端口 pin 表（2 pin，对照 ToDesk 虚拟声卡逐 pin 实测值 / sysvad speakerwavtable.h）
///
/// - pin0 = host：DataFlow=**IN**（数据流入滤波器）/ Communication=SINK / Category=KSNODETYPE_SPEAKER
///   / 实例数 1 —— 客户端与 audio engine 开流用的那一端。
/// - pin1 = bridge：DataFlow=OUT / Communication=NONE / Category=KSNODETYPE_SPEAKER
///   / 桥接数据范围 —— **物理连接（wave→topology）挂在这里**。
///
/// 回归：原实现只有 1 个 pin（DataFlow=OUT + SINK + 物理连接挂其上），实测端点不出现。
static PINS_RENDER: [PCPIN_DESCRIPTOR; 2] = [
    PCPIN_DESCRIPTOR {
        MaxGlobalInstanceCount: 1,
        MaxFilterInstanceCount: 1,
        MinFilterInstanceCount: 0,
        AutomationTable: core::ptr::null(),
        KsPinDescriptor: KSPIN_DESCRIPTOR {
            InterfacesCount: 1,
            Interfaces: PIN_INTERFACES.as_ptr(),
            MediumsCount: 1,
            Mediums: PIN_MEDIUMS.as_ptr(),
            DataRangesCount: 1,
            DataRanges: DATA_RANGES.0.as_ptr(),
            DataFlow: KSPIN_DATAFLOW_IN,
            Communication: KSPIN_COMMUNICATION_SINK,
            Category: &raw const KSNODETYPE_SPEAKER,
            Name: core::ptr::null(),
            Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
        },
    },
    PCPIN_DESCRIPTOR {
        MaxGlobalInstanceCount: 0,
        MaxFilterInstanceCount: 0,
        MinFilterInstanceCount: 0,
        AutomationTable: core::ptr::null(),
        KsPinDescriptor: KSPIN_DESCRIPTOR {
            InterfacesCount: 0,
            Interfaces: core::ptr::null(),
            MediumsCount: 0,
            Mediums: core::ptr::null(),
            DataRangesCount: 1,
            DataRanges: BRIDGE_DATA_RANGES.0.as_ptr(),
            DataFlow: KSPIN_DATAFLOW_OUT,
            Communication: KSPIN_COMMUNICATION_NONE,
            Category: &raw const KSNODETYPE_SPEAKER,
            Name: core::ptr::null(),
            Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
        },
    },
];

/// capture 小端口 pin 表（2 pin，对照 ToDesk 虚拟声卡逐 pin 实测值 / sysvad micarraywavtable.h）
///
/// - pin0 = bridge：DataFlow=IN / Communication=NONE / Category=KSNODETYPE_MICROPHONE
///   / 桥接数据范围 —— **物理连接（topology→wave）挂在这里**。
/// - pin1 = host：DataFlow=**OUT**（数据流出滤波器）/ Communication=SINK / Category=KSNODETYPE_MICROPHONE
///   / 实例数 1 —— 开流用的那一端。
///
/// 回归：原实现只有 1 个 pin（DataFlow=IN + Communication=SOURCE），实测端点不出现。
static PINS_CAPTURE: [PCPIN_DESCRIPTOR; 2] = [
    PCPIN_DESCRIPTOR {
        MaxGlobalInstanceCount: 0,
        MaxFilterInstanceCount: 0,
        MinFilterInstanceCount: 0,
        AutomationTable: core::ptr::null(),
        KsPinDescriptor: KSPIN_DESCRIPTOR {
            InterfacesCount: 0,
            Interfaces: core::ptr::null(),
            MediumsCount: 0,
            Mediums: core::ptr::null(),
            DataRangesCount: 1,
            DataRanges: BRIDGE_DATA_RANGES.0.as_ptr(),
            DataFlow: KSPIN_DATAFLOW_IN,
            Communication: KSPIN_COMMUNICATION_NONE,
            Category: &raw const KSNODETYPE_MICROPHONE,
            Name: core::ptr::null(),
            Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
        },
    },
    PCPIN_DESCRIPTOR {
        MaxGlobalInstanceCount: 1,
        MaxFilterInstanceCount: 1,
        MinFilterInstanceCount: 0,
        AutomationTable: core::ptr::null(),
        KsPinDescriptor: KSPIN_DESCRIPTOR {
            InterfacesCount: 1,
            Interfaces: PIN_INTERFACES.as_ptr(),
            MediumsCount: 1,
            Mediums: PIN_MEDIUMS.as_ptr(),
            DataRangesCount: 1,
            DataRanges: DATA_RANGES.0.as_ptr(),
            DataFlow: KSPIN_DATAFLOW_OUT,
            Communication: KSPIN_COMMUNICATION_SINK,
            Category: &raw const KSNODETYPE_MICROPHONE,
            Name: core::ptr::null(),
            Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
        },
    },
];

/// render 过滤器描述符（B6：字段集与 portcls.h 一致，Version=0，类别数组指针）
static FILTER_DESC_RENDER: PCFILTER_DESCRIPTOR = PCFILTER_DESCRIPTOR {
    Version: 0,
    // 滤波器级属性表：KSPROPERTY_PIN_PROPOSEDATAFORMAT / 2（音频引擎定设备格式用）
    AutomationTable: &WAVE_AUTOMATION as *const PCAUTOMATION_TABLE
        as *const crate::sys::types::PCAUTOMATION_TABLE,
    PinSize: size_of::<PCPIN_DESCRIPTOR>() as u32,
    PinCount: PINS_RENDER.len() as u32,
    Pins: PINS_RENDER.as_ptr(),
    NodeSize: 0,
    NodeCount: 0,
    Nodes: core::ptr::null(),
    ConnectionCount: WAVE_RENDER_CONNECTIONS.len() as u32,
    Connections: WAVE_RENDER_CONNECTIONS.as_ptr(),
    CategoryCount: FILTER_CATEGORIES.len() as u32,
    Categories: FILTER_CATEGORIES.as_ptr(),
};

/// capture 过滤器描述符
static FILTER_DESC_CAPTURE: PCFILTER_DESCRIPTOR = PCFILTER_DESCRIPTOR {
    Version: 0,
    AutomationTable: &WAVE_AUTOMATION as *const PCAUTOMATION_TABLE
        as *const crate::sys::types::PCAUTOMATION_TABLE,
    PinSize: size_of::<PCPIN_DESCRIPTOR>() as u32,
    PinCount: PINS_CAPTURE.len() as u32,
    Pins: PINS_CAPTURE.as_ptr(),
    NodeSize: 0,
    NodeCount: 0,
    Nodes: core::ptr::null(),
    ConnectionCount: WAVE_CAPTURE_CONNECTIONS.len() as u32,
    Connections: WAVE_CAPTURE_CONNECTIONS.as_ptr(),
    CategoryCount: FILTER_CATEGORIES.len() as u32,
    Categories: FILTER_CATEGORIES.as_ptr(),
};
