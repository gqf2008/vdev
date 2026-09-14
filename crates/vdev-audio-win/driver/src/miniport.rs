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
        DBG_LAST_STATUS.store(status_bits(STATUS_INVALID_PARAMETER), Ordering::Relaxed);
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
        DBG_LAST_STATUS.store(status_bits(STATUS_INVALID_PARAMETER), Ordering::Relaxed);
        return STATUS_INVALID_PARAMETER;
    }
    // EXTENSIBLE 时 SubFormat 必须是 PCM（前面的 ksdm.DataFormat.SubFormat 检查对应
    // KSDATAFORMAT 的 SubFormat；这里再核对 WAVEFORMATEXTENSIBLE.SubFormat 偏移）
    if wf.wFormatTag == WAVE_FORMAT_EXTENSIBLE {
        if ksdm.DataFormat.FormatSize < (size_of::<KSDATAFORMAT>() + 40) as u32 {
            DBG_LAST_STATUS.store(status_bits(STATUS_INVALID_PARAMETER), Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        }
        let base = (data_format as *const u8).add(size_of::<KSDATAFORMAT>() + 24);
        // SAFETY: FormatSize >= 104 已校验，偏移 64+24=88 处为 SubFormat GUID
        let sub = unsafe { core::ptr::read_unaligned(base.cast::<GUID>()) };
        if sub != KSDATAFORMAT_SUBTYPE_PCM {
            DBG_LAST_STATUS.store(status_bits(STATUS_INVALID_PARAMETER), Ordering::Relaxed);
            return STATUS_INVALID_PARAMETER;
        }
    }
    // SAFETY: 单线程初始化（PortCls 串行调用）
    unsafe {
        (*this).block_align = wf.nBlockAlign;
        (*this).bytes_per_sec = wf.nSamplesPerSec * u32::from(wf.nBlockAlign);
    }
    dbg_inc(&DBG_SET_FORMAT_OK);
    DBG_LAST_STATUS.store(status_bits(STATUS_SUCCESS), Ordering::Relaxed);
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
    /// 子对象：IMiniportAudioEngineNode（Windows 10 音频引擎）
    pub engine_node_obj: SubObject,
    /// 引擎通过 IMiniportAudioEngineNode::SetDeviceFormat 指定的设备格式（sysvad 的
    /// m_pDeviceFormat 同款）：GetDeviceFormat / GetMixFormat 必须**原样回读**，
    /// 否则引擎判定 AUDCLNT_E_UNSUPPORTED_FORMAT。
    pub device_format: [u8; size_of::<KsDataFormatWaveFormatExtensible>()],
    pub device_format_valid: bool,
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
/// 本驱动在 streaming pin 上暴露的信号处理模式（对照 sysvad speakertoptable.h 的
/// SpeakerHostPinSupportedDeviceModes：RAW/DEFAULT/MEDIA/MOVIE/COMMUNICATIONS/NOTIFICATION）。
/// 引擎会用 `IMiniportAudioSignalProcessing::GetModes` 取这个列表，并在
/// `KSPROPERTY_PIN_PROPOSEDATAFORMAT2` 请求里带上对应模式属性。
static SUPPORTED_MODES: [GUID; 6] = [
    AUDIO_SIGNALPROCESSINGMODE_DEFAULT,
    // RAW {9E90EA20-B493-4FD1-A1A8-7E1361A956CF}
    GUID {
        data1: 0x9e90_ea20,
        data2: 0xb493,
        data3: 0x4fd1,
        data4: [0xa1, 0xa8, 0x7e, 0x13, 0x61, 0xa9, 0x56, 0xcf],
    },
    // MEDIA {4780004E-7133-41D8-8C74-660DADD2C0EE}
    GUID {
        data1: 0x4780_004e,
        data2: 0x7133,
        data3: 0x41d8,
        data4: [0x8c, 0x74, 0x66, 0x0d, 0xad, 0xd2, 0xc0, 0xee],
    },
    // MOVIE {B26FEB0D-EC94-477C-9494-D1AB8E753F6E}
    GUID {
        data1: 0xb26f_eb0d,
        data2: 0xec94,
        data3: 0x477c,
        data4: [0x94, 0x94, 0xd1, 0xab, 0x8e, 0x75, 0x3f, 0x6e],
    },
    // COMMUNICATIONS {98951333-B9CD-48B1-A0A3-FF40682D73F7}
    GUID {
        data1: 0x9895_1333,
        data2: 0xb9cd,
        data3: 0x48b1,
        data4: [0xa0, 0xa3, 0xff, 0x40, 0x68, 0x2d, 0x73, 0xf7],
    },
    // NOTIFICATION {9CF2A70B-F377-403B-BD6B-360863E0355C}
    GUID {
        data1: 0x9cf2_a70b,
        data2: 0xf377,
        data3: 0x403b,
        data4: [0xbd, 0x6b, 0x36, 0x08, 0x63, 0xe0, 0x35, 0x5c],
    },
];

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
    pin: u32,
    modes: *mut GUID,
    num: *mut u32,
) -> NTSTATUS {
    dbg_inc(&DBG_MODES_CALLS);
    DBG_MODES_LAST_PIN.store(pin, Ordering::Relaxed);
    if num.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let cap = unsafe { *num };
    DBG_MODES_LAST_COUNT.store(cap, Ordering::Relaxed);
    if modes.is_null() || cap == 0 {
        unsafe { *num = SUPPORTED_MODES.len() as u32 };
        return STATUS_BUFFER_TOO_SMALL;
    }
    if (cap as usize) < SUPPORTED_MODES.len() {
        unsafe { *num = SUPPORTED_MODES.len() as u32 };
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 调用方保证 modes 至少可容纳 *num 个 GUID
    unsafe {
        core::ptr::copy_nonoverlapping(SUPPORTED_MODES.as_ptr(), modes, SUPPORTED_MODES.len());
        *num = SUPPORTED_MODES.len() as u32;
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

// ============ IMiniportAudioEngineNode（Windows 10 音频引擎，sysvad 同款） ============
//
// 引擎 QI 该接口成功后才走"音频引擎"路径；配套的 KSNODETYPE_AUDIO_ENGINE 节点已在
// 滤波器描述符里声明（PortCls 把 KSPROPSETID_AudioEngine 的属性请求转发到本接口）。
// 槽序与签名严格对照 portcls.h `DECLARE_INTERFACE_(IMiniportAudioEngineNode, IUnknown)`。

/// portcls.h `IID_IMiniportAudioEngineNode` = {2EBF536C-EF57-4C64-BEDC-25C1A6D668E6}
/// （实验 B 期间未暴露该接口；保留常量与实现以便随时切回"引擎模式"对比）
#[allow(dead_code)]
static IID_IMINIPORT_AUDIO_ENGINE_NODE: GUID = GUID {
    data1: 0x2ebf_536c,
    data2: 0xef57,
    data3: 0x4c64,
    data4: [0xbe, 0xdc, 0x25, 0xc1, 0xa6, 0xd6, 0x68, 0xe6],
};

/// ksmedia.h `KSAUDIOENGINE_DESCRIPTOR`
#[repr(C)]
pub struct KsAudioEngineDescriptor {
    host_pin_id: u32,
    offload: u32,
    loopback: u32,
}
/// ksmedia.h `KSAUDIOENGINE_BUFFER_SIZE_RANGE`
#[repr(C)]
pub struct KsAudioEngineBufferSizeRange {
    min_buffer_bytes: u32,
    max_buffer_bytes: u32,
}
/// ks.h `KSMULTIPLE_ITEM`
#[repr(C)]
pub struct KsMultipleItem {
    size: u32,
    count: u32,
}
/// ks.h `KSPROPERTY_STEPPING_LONG`
#[repr(C)]
pub struct KsPropertySteppingLong {
    stepping_delta: u32,
    reserved: u32,
    min: i32,
    max: i32,
}

#[allow(clippy::type_complexity)]
#[repr(C)]
pub struct IMiniportAudioEngineNodeVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub get_audio_engine_descriptor:
        unsafe extern "system" fn(PVOID, u32, *mut KsAudioEngineDescriptor) -> NTSTATUS,
    pub get_gfx_state: unsafe extern "system" fn(PVOID, u32, *mut i32) -> NTSTATUS,
    pub set_gfx_state: unsafe extern "system" fn(PVOID, u32, i32) -> NTSTATUS,
    pub get_engine_format_size: unsafe extern "system" fn(PVOID, u32, u32, *mut u32) -> NTSTATUS,
    pub get_mix_format: unsafe extern "system" fn(PVOID, u32, PVOID, u32) -> NTSTATUS,
    pub get_device_format: unsafe extern "system" fn(PVOID, u32, PVOID, u32) -> NTSTATUS,
    pub set_device_format: unsafe extern "system" fn(PVOID, u32, PVOID, u32) -> NTSTATUS,
    pub get_supported_device_formats: unsafe extern "system" fn(PVOID, u32, PVOID, u32) -> NTSTATUS,
    pub get_device_channel_count: unsafe extern "system" fn(PVOID, u32, u32, *mut u32) -> NTSTATUS,
    pub get_device_attribute_steppings:
        unsafe extern "system" fn(PVOID, u32, u32, *mut KsPropertySteppingLong, u32) -> NTSTATUS,
    pub get_device_channel_volume: unsafe extern "system" fn(PVOID, u32, u32, *mut i32) -> NTSTATUS,
    pub set_device_channel_volume: unsafe extern "system" fn(PVOID, u32, u32, i32) -> NTSTATUS,
    pub get_device_channel_mute: unsafe extern "system" fn(PVOID, u32, u32, *mut i32) -> NTSTATUS,
    pub set_device_channel_mute: unsafe extern "system" fn(PVOID, u32, u32, i32) -> NTSTATUS,
    pub get_device_channel_peak_meter:
        unsafe extern "system" fn(PVOID, u32, u32, *mut i32) -> NTSTATUS,
    pub get_buffer_size_range:
        unsafe extern "system" fn(PVOID, u32, PVOID, *mut KsAudioEngineBufferSizeRange) -> NTSTATUS,
}

/// 滤波器里只有这一个节点
const AUDIO_ENGINE_NODE_ID: u32 = 0;
/// 无 offload / loopback pin 的哨兵值
const PIN_ID_NONE: u32 = u32::MAX;
/// 缓冲区时长上下限（毫秒）——sysvad 用 10/500
const MIN_BUFFER_DURATION_MS: u32 = 10;
const MAX_BUFFER_DURATION_MS: u32 = 500;

fn host_pin_id(capture: bool) -> u32 {
    if capture {
        crate::endpoint_names::WAVE_CAPTURE_HOST_PIN
    } else {
        crate::endpoint_names::WAVE_RENDER_HOST_PIN
    }
}

unsafe extern "system" fn ae_get_descriptor(
    this: PVOID,
    _node: u32,
    desc: *mut KsAudioEngineDescriptor,
) -> NTSTATUS {
    if desc.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    let capture = unsafe { (*sub_owner(this)).capture };
    // SAFETY: desc 可写
    unsafe {
        core::ptr::write(
            desc,
            KsAudioEngineDescriptor {
                host_pin_id: host_pin_id(capture),
                offload: PIN_ID_NONE,
                loopback: PIN_ID_NONE,
            },
        );
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_gfx_state(_this: PVOID, _node: u32, enable: *mut i32) -> NTSTATUS {
    if enable.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // 不支持 GFX 卸载：恒为 FALSE
    unsafe { core::ptr::write(enable, 0) };
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_set_gfx_state(_this: PVOID, _node: u32, _enable: i32) -> NTSTATUS {
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_engine_format_size(
    _this: PVOID,
    node: u32,
    format_type: u32,
    size: *mut u32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || size.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    let need = if format_type == 2 {
        size_of::<KsMultipleItem>() + size_of::<KsDataFormatWaveFormatExtensible>()
    } else {
        size_of::<KsDataFormatWaveFormatExtensible>()
    };
    DBG_AE_FMT_TYPE.store(format_type, Ordering::Relaxed);
    DBG_AE_FMT_SIZE_OUT.store(need as u32, Ordering::Relaxed);
    // SAFETY: size 可写
    unsafe { core::ptr::write(size, need as u32) };
    STATUS_SUCCESS
}

/// 把当前设备格式（引擎设置过就用它，否则用默认 48k/16bit/2ch）拷给调用方。
/// sysvad 的 GetDeviceFormat / GetMixFormat 都是"回读 m_pDeviceFormat"，
/// 不回读会让引擎判定 AUDCLNT_E_UNSUPPORTED_FORMAT（实测 0x88890008）。
unsafe fn copy_device_format(this: PVOID, node: u32, out: *mut c_void, buf_size: u32) -> NTSTATUS {
    let need = size_of::<KsDataFormatWaveFormatExtensible>();
    if node != AUDIO_ENGINE_NODE_ID {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    if out.is_null() || (buf_size as usize) < need {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 目标缓冲 >= need 字节；源为静态或本对象的 device_format 缓冲
    unsafe {
        let owner = sub_owner(this);
        let src = if (*owner).device_format_valid {
            core::ptr::addr_of!((*owner).device_format).cast::<u8>()
        } else {
            (&raw const DEVICE_FORMAT).cast::<u8>()
        };
        core::ptr::copy_nonoverlapping(src, out.cast::<u8>(), need);
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_mix_format(
    this: PVOID,
    node: u32,
    out: *mut c_void,
    buf_size: u32,
) -> NTSTATUS {
    dbg_inc(&DBG_AE_GET_MIX_FORMAT);
    // 混音格式 = 引擎内部 float32（不是设备格式），见 MIX_FORMAT 注释
    let _ = this;
    let need = size_of::<KsDataFormatWaveFormatExtensible>();
    if node != AUDIO_ENGINE_NODE_ID {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    if out.is_null() || (buf_size as usize) < need {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 目标缓冲 >= need 字节；源为只读静态
    unsafe {
        core::ptr::copy_nonoverlapping(
            (&raw const MIX_FORMAT).cast::<u8>(),
            out.cast::<u8>(),
            need,
        );
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_device_format(
    this: PVOID,
    node: u32,
    out: *mut c_void,
    buf_size: u32,
) -> NTSTATUS {
    dbg_inc(&DBG_AE_GET_DEVICE_FORMAT);
    unsafe { copy_device_format(this, node, out, buf_size) }
}

unsafe extern "system" fn ae_set_device_format(
    this: PVOID,
    node: u32,
    fmt: *mut c_void,
    buf_size: u32,
) -> NTSTATUS {
    dbg_inc(&DBG_AE_SET_DEVICE_FORMAT);
    if node != AUDIO_ENGINE_NODE_ID || fmt.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // 契约对照官方 sysvad MiniportAudioEngineNode.cpp::SetDeviceFormat：
    // 缓冲 < sizeof(KSDATAFORMAT_WAVEFORMATEXTENSIBLE) 时回 STATUS_BUFFER_TOO_SMALL（不落盘）。
    // 回归：原来接受 84 字节并截断存储，回读给引擎的是残缺格式 → 引擎反复
    // SetDeviceFormat/GetDeviceFormat 并最终判 AUDCLNT_E_UNSUPPORTED_FORMAT(0x88890008)。
    let need = size_of::<KsDataFormatWaveFormatExtensible>();
    DBG_AE_SET_BUF_SIZE.store(buf_size, Ordering::Relaxed);
    if (buf_size as usize) < need {
        DBG_AE_SET_STATUS.store(status_bits(STATUS_BUFFER_TOO_SMALL), Ordering::Relaxed);
        return STATUS_BUFFER_TOO_SMALL;
    }
    // 记住引擎指定的设备格式（最多 104 字节），后续 GetDeviceFormat/GetMixFormat 原样回读
    // SAFETY: 源缓冲至少 KSDATAFORMAT_WAVEFORMATEX；目标为本对象内固定 104 字节缓冲
    unsafe {
        let owner = sub_owner(this);
        (*owner).device_format = [0u8; size_of::<KsDataFormatWaveFormatExtensible>()];
        core::ptr::copy_nonoverlapping(
            fmt.cast::<u8>(),
            core::ptr::addr_of_mut!((*owner).device_format).cast::<u8>(),
            need,
        );
        (*owner).device_format_valid = true;
        // 记录引擎设进来的格式（KSDATAFORMAT 之后是 WAVEFORMATEX）
        let wf = fmt.cast::<u8>().add(size_of::<KSDATAFORMAT>());
        DBG_AE_STORED_TAG.store(
            u32::from(core::ptr::read_unaligned(wf.cast::<u16>())),
            Ordering::Relaxed,
        );
        DBG_AE_STORED_RATE.store(
            core::ptr::read_unaligned(wf.add(4).cast::<u32>()),
            Ordering::Relaxed,
        );
        DBG_AE_STORED_BITS.store(
            u32::from(core::ptr::read_unaligned(wf.add(14).cast::<u16>())),
            Ordering::Relaxed,
        );
    }
    DBG_AE_SET_STATUS.store(status_bits(STATUS_SUCCESS), Ordering::Relaxed);
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_supported_device_formats(
    _this: PVOID,
    node: u32,
    out: *mut c_void,
    buf_size: u32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || out.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    let fmt_size = size_of::<KsDataFormatWaveFormatExtensible>();
    let need = size_of::<KsMultipleItem>() + fmt_size;
    // 自审修正：先判长度再写头。原来无论缓冲多大都先写 KSMULTIPLE_ITEM（8 字节），
    // 调用方若传 <8 字节的缓冲就会越界写。0 长度探测时也只回所需长度。
    if (buf_size as usize) < size_of::<KsMultipleItem>() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 缓冲 >= sizeof(KSMULTIPLE_ITEM)
    unsafe {
        core::ptr::write(
            out.cast::<KsMultipleItem>(),
            KsMultipleItem {
                size: need as u32,
                count: 1,
            },
        );
    }
    if (buf_size as usize) < need {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 缓冲区 >= need 字节
    unsafe {
        core::ptr::copy_nonoverlapping(
            (&raw const DEVICE_FORMAT).cast::<u8>(),
            out.cast::<u8>().add(size_of::<KsMultipleItem>()),
            fmt_size,
        );
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_device_channel_count(
    _this: PVOID,
    node: u32,
    target_type: u32,
    count: *mut u32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || count.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    if target_type > 2 {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // 设备格式是 2ch
    unsafe { core::ptr::write(count, 2) };
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_device_attribute_steppings(
    _this: PVOID,
    node: u32,
    _target_type: u32,
    stepping: *mut KsPropertySteppingLong,
    data_size: u32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || stepping.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    if (data_size as usize) < size_of::<KsPropertySteppingLong>() {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 缓冲足够
    unsafe {
        core::ptr::write(
            stepping,
            KsPropertySteppingLong {
                stepping_delta: 1,
                reserved: 0,
                min: i32::MIN,
                max: i32::MAX,
            },
        );
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_device_channel_volume(
    _this: PVOID,
    node: u32,
    _channel: u32,
    volume: *mut i32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || volume.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // 0 = 0 dB（1/100 dB，正值表示衰减）
    unsafe { core::ptr::write(volume, 0) };
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_set_device_channel_volume(
    _this: PVOID,
    node: u32,
    _channel: u32,
    _volume: i32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_device_channel_mute(
    _this: PVOID,
    node: u32,
    _channel: u32,
    mute: *mut i32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || mute.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    unsafe { core::ptr::write(mute, 0) };
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_set_device_channel_mute(
    _this: PVOID,
    node: u32,
    _channel: u32,
    _mute: i32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_device_channel_peak_meter(
    _this: PVOID,
    node: u32,
    _channel: u32,
    peak: *mut i32,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || peak.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // -100 dB（1/100 dB 单位）
    unsafe { core::ptr::write(peak, -10_000) };
    STATUS_SUCCESS
}

unsafe extern "system" fn ae_get_buffer_size_range(
    _this: PVOID,
    node: u32,
    format: PVOID,
    range: *mut KsAudioEngineBufferSizeRange,
) -> NTSTATUS {
    if node != AUDIO_ENGINE_NODE_ID || format.is_null() || range.is_null() {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // KSDATAFORMAT 之后是 WAVEFORMATEX，nAvgBytesPerSec 在 +8
    let base = format.cast::<u8>();
    // SAFETY: 调用方保证格式缓冲 >= KSDATAFORMAT_WAVEFORMATEX
    let avg_bytes_per_sec =
        unsafe { core::ptr::read_unaligned(base.add(size_of::<KSDATAFORMAT>() + 8).cast::<u32>()) };
    let avg = if avg_bytes_per_sec == 0 {
        192_000
    } else {
        avg_bytes_per_sec
    };
    // SAFETY: range 可写
    unsafe {
        core::ptr::write(
            range,
            KsAudioEngineBufferSizeRange {
                min_buffer_bytes: avg * MIN_BUFFER_DURATION_MS / 1000,
                max_buffer_bytes: avg * MAX_BUFFER_DURATION_MS / 1000,
            },
        );
    }
    STATUS_SUCCESS
}

static ENGINE_NODE_VTABLE: IMiniportAudioEngineNodeVtbl = IMiniportAudioEngineNodeVtbl {
    query_interface: sub_qi,
    add_ref: sub_addref,
    release: sub_release,
    get_audio_engine_descriptor: ae_tr_descriptor,
    get_gfx_state: ae_tr_gfx_get,
    set_gfx_state: ae_tr_gfx_set,
    get_engine_format_size: ae_tr_format_size,
    get_mix_format: ae_get_mix_format,
    get_device_format: ae_get_device_format,
    set_device_format: ae_set_device_format,
    get_supported_device_formats: ae_tr_supported,
    get_device_channel_count: ae_tr_channel_count,
    get_device_attribute_steppings: ae_tr_steppings,
    get_device_channel_volume: ae_tr_volume_get,
    set_device_channel_volume: ae_tr_volume_set,
    get_device_channel_mute: ae_tr_mute_get,
    set_device_channel_mute: ae_tr_mute_set,
    get_device_channel_peak_meter: ae_tr_peak,
    get_buffer_size_range: ae_tr_buffer_range,
};

// ---- 各方法的计数 trampoline（记录调用次数与最后失败点）----

unsafe extern "system" fn ae_tr_descriptor(
    this: PVOID,
    node: u32,
    desc: *mut KsAudioEngineDescriptor,
) -> NTSTATUS {
    let st = unsafe { ae_get_descriptor(this, node, desc) };
    ae_tick(0, st);
    st
}
unsafe extern "system" fn ae_tr_gfx_get(this: PVOID, node: u32, out: *mut i32) -> NTSTATUS {
    let st = unsafe { ae_get_gfx_state(this, node, out) };
    ae_tick(1, st);
    st
}
unsafe extern "system" fn ae_tr_gfx_set(this: PVOID, node: u32, en: i32) -> NTSTATUS {
    let st = unsafe { ae_set_gfx_state(this, node, en) };
    ae_tick(2, st);
    st
}
unsafe extern "system" fn ae_tr_format_size(
    this: PVOID,
    node: u32,
    ty: u32,
    size: *mut u32,
) -> NTSTATUS {
    let st = unsafe { ae_get_engine_format_size(this, node, ty, size) };
    ae_tick(3, st);
    st
}
unsafe extern "system" fn ae_tr_supported(
    this: PVOID,
    node: u32,
    out: *mut c_void,
    size: u32,
) -> NTSTATUS {
    let st = unsafe { ae_get_supported_device_formats(this, node, out, size) };
    ae_tick(7, st);
    st
}
unsafe extern "system" fn ae_tr_channel_count(
    this: PVOID,
    node: u32,
    ty: u32,
    count: *mut u32,
) -> NTSTATUS {
    let st = unsafe { ae_get_device_channel_count(this, node, ty, count) };
    ae_tick(8, st);
    st
}
unsafe extern "system" fn ae_tr_steppings(
    this: PVOID,
    node: u32,
    ty: u32,
    out: *mut KsPropertySteppingLong,
    size: u32,
) -> NTSTATUS {
    let st = unsafe { ae_get_device_attribute_steppings(this, node, ty, out, size) };
    ae_tick(9, st);
    st
}
unsafe extern "system" fn ae_tr_volume_get(
    this: PVOID,
    node: u32,
    ch: u32,
    out: *mut i32,
) -> NTSTATUS {
    let st = unsafe { ae_get_device_channel_volume(this, node, ch, out) };
    ae_tick(10, st);
    st
}
unsafe extern "system" fn ae_tr_volume_set(this: PVOID, node: u32, ch: u32, v: i32) -> NTSTATUS {
    let st = unsafe { ae_set_device_channel_volume(this, node, ch, v) };
    ae_tick(11, st);
    st
}
unsafe extern "system" fn ae_tr_mute_get(
    this: PVOID,
    node: u32,
    ch: u32,
    out: *mut i32,
) -> NTSTATUS {
    let st = unsafe { ae_get_device_channel_mute(this, node, ch, out) };
    ae_tick(12, st);
    st
}
unsafe extern "system" fn ae_tr_mute_set(this: PVOID, node: u32, ch: u32, v: i32) -> NTSTATUS {
    let st = unsafe { ae_set_device_channel_mute(this, node, ch, v) };
    ae_tick(13, st);
    st
}
unsafe extern "system" fn ae_tr_peak(this: PVOID, node: u32, ch: u32, out: *mut i32) -> NTSTATUS {
    let st = unsafe { ae_get_device_channel_peak_meter(this, node, ch, out) };
    ae_tick(14, st);
    st
}
unsafe extern "system" fn ae_tr_buffer_range(
    this: PVOID,
    node: u32,
    fmt: PVOID,
    out: *mut KsAudioEngineBufferSizeRange,
) -> NTSTATUS {
    let st = unsafe { ae_get_buffer_size_range(this, node, fmt, out) };
    ae_tick(15, st);
    st
}

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
                    engine_node_obj: SubObject {
                        vtbl: (&raw const ENGINE_NODE_VTABLE).cast::<c_void>(),
                        owner: core::ptr::null_mut(),
                    },
                    device_format: [0u8; size_of::<KsDataFormatWaveFormatExtensible>()],
                    device_format_valid: false,
                },
            );
            // 回填子对象的 owner（主对象地址此时已确定）
            (*ptr.cast::<MiniportWaveRT>()).pin_count_obj.owner = ptr.cast::<MiniportWaveRT>();
            (*ptr.cast::<MiniportWaveRT>()).signal_proc_obj.owner = ptr.cast::<MiniportWaveRT>();
            (*ptr.cast::<MiniportWaveRT>()).engine_node_obj.owner = ptr.cast::<MiniportWaveRT>();
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
        DBG_RANGE_LAST_STATUS.store(status_bits(STATUS_NO_MATCH), Ordering::Relaxed);
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
        DBG_RANGE_LAST_STATUS.store(status_bits(STATUS_BUFFER_OVERFLOW), Ordering::Relaxed);
        return STATUS_BUFFER_OVERFLOW;
    }
    if out_len < needed as u32 {
        // SAFETY: 输出指针有效
        unsafe { *out_len_ret = needed as u32 };
        DBG_RANGE_LAST_STATUS.store(status_bits(STATUS_BUFFER_TOO_SMALL), Ordering::Relaxed);
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

/// ks.h `KSDATARANGE_ATTRIBUTES`（=1<<KSDATARANGE_BIT_ATTRIBUTES）：
/// 置位表示"本范围后面在指针数组里紧跟一个属性列表（KSATTRIBUTE_LIST）"。
const KSDATARANGE_ATTRIBUTES: u32 = 0x0000_0002;
/// ks.h `KSDATAFORMAT_ATTRIBUTES`：值缓冲里"格式后面跟属性列表"的同一标志。
const KSDATAFORMAT_ATTRIBUTES: u32 = 0x0000_0002;

/// ks.h `KSATTRIBUTE`：{ ULONG Size; ULONG Flags; GUID Attribute; }
#[repr(C)]
pub struct KsAttribute {
    pub Size: u32,
    pub Flags: u32,
    pub Attribute: GUID,
}

/// ks.h `KSATTRIBUTE_LIST`：{ ULONG Count; PKSATTRIBUTE* Attributes; }
#[repr(C)]
pub struct KsAttributeList {
    pub Count: u32,
    pub Attributes: *const *const KsAttribute,
}
// SAFETY: 静态只读数据，仅被 PortCls / KS 读取，永不写入
unsafe impl Sync for KsAttributeList {}

/// 属性指针数组本身含裸指针 → !Sync，包一层只读描述符静态（同 SyncDataRanges 手法）
struct SyncAttributePtrs([*const KsAttribute; 1]);
// SAFETY: 静态只读数据，元素指向不可变静态，永不写入
unsafe impl Sync for SyncAttributePtrs {}

/// ksmedia.h `KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE`
/// = {E1F89EB5-5F46-419B-967B-FF6770B98401}
static KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE: GUID = GUID {
    data1: 0xe1f8_9eb5,
    data2: 0x5f46,
    data3: 0x419b,
    data4: [0x96, 0x7b, 0xff, 0x67, 0x70, 0xb9, 0x84, 0x01],
};

/// streaming pin 的数据范围属性（sysvad `endpoints.h::PinDataRangeSignalProcessingModeAttribute` /
/// VDA 同款）：一个"本范围带音频信号处理模式属性"的裸 KSATTRIBUTE（24 字节，不带具体模式值，
/// 表示该范围对默认模式有效）。ToDesk 虚拟声卡二进制里也是这一形状。
static STREAM_RANGE_MODE_ATTRIBUTE: KsAttribute = KsAttribute {
    Size: size_of::<KsAttribute>() as u32,
    Flags: 0,
    Attribute: KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE,
};
static STREAM_RANGE_MODE_ATTRIBUTE_PTRS: SyncAttributePtrs =
    SyncAttributePtrs([&raw const STREAM_RANGE_MODE_ATTRIBUTE]);
static STREAM_RANGE_MODE_ATTRIBUTE_LIST: KsAttributeList = KsAttributeList {
    Count: 1,
    Attributes: STREAM_RANGE_MODE_ATTRIBUTE_PTRS.0.as_ptr(),
};

/// PCM 16bit / 48kHz / 2ch 数据范围
///
/// Flags 必须置 `KSDATARANGE_ATTRIBUTES`：Win10 音频引擎（audiodg/EndpointBuilder）
/// 在给端点定"设备格式 / 混音格式"时会按**信号处理模式**匹配数据范围；范围上没有
/// 模式属性列表时匹配不到任何 (模式, 格式) 组合，客户端 `IAudioClient::GetMixFormat` /
/// `Initialize` 直接报 0x80070491（ERROR_NO_MATCH，「no match for the specified key in
/// the index」）/ 0x88890008。sysvad `speakerwavtable.h` 的每个 streaming 范围、
/// VDA、以及本机可用的 ToDesk 虚拟声卡（二进制实测 Flags=0x2）都带这个属性列表。
static AUDIO_DATA_RANGE: KSDATARANGE_AUDIO = KSDATARANGE_AUDIO {
    DataRange: KSDATARANGE {
        FormatSize: size_of::<KSDATARANGE_AUDIO>() as u32,
        Flags: KSDATARANGE_ATTRIBUTES,
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
struct SyncDataRanges<const N: usize>([*const KSDATARANGE; N]);
// SAFETY: 静态只读数据，永不写入
unsafe impl<const N: usize> Sync for SyncDataRanges<N> {}

/// streaming pin 的范围表：**2 项**——范围本身 + 属性列表指针。
/// （KS 约定：范围 Flags 带 KSDATARANGE_ATTRIBUTES 时，指针数组的下一项指向
/// KSATTRIBUTE_LIST；插槽类型仍是 PKSDATARANGE，故此处做指针类型转换。）
static DATA_RANGES: SyncDataRanges<2> = SyncDataRanges([
    &raw const AUDIO_DATA_RANGE.DataRange,
    (&raw const STREAM_RANGE_MODE_ATTRIBUTE_LIST).cast::<KSDATARANGE>(),
]);

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
static BRIDGE_DATA_RANGES: SyncDataRanges<1> = SyncDataRanges([&raw const AUDIO_BRIDGE_RANGE]);

/// 开流 pin 的接口表：**必须是 KSINTERFACE_STANDARD_LOOPED_STREAMING(1)**。
///
/// 回归（本轮真机定位）：原来写的是 KSINTERFACE_STANDARD_STREAMING(0)，而
/// audiodg/AudioEndpointBuilder 打开 WaveRT 端点时按 `KSPIN_INTERFACE
/// {KSINTERFACESETID_Standard, KSINTERFACE_STANDARD_LOOPED_STREAMING}` 选接口，
/// 找不到就返回 STATUS_NO_MATCH → 客户端拿到 0x80070491（ERROR_NO_MATCH）：
/// 实测 `IAudioClient::Activate` 成功、紧接着 `GetDevicePeriod` 就报 0x80070491
/// （ToDesk/Realtek 同一步 hr=0），之后 GetMixFormat/IsFormatSupported/Initialize
/// 全盘失败。ToDesk 端点同属性实测 id=1。
static PIN_INTERFACES: [KSPIN_INTERFACE; 1] = [KSPIN_INTERFACE {
    Set: KSINTERFACESETID_STANDARD,
    Id: KSINTERFACE_STANDARD_LOOPED_STREAMING,
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

/// ksmedia.h `KSDATAFORMAT_SUBTYPE_IEEE_FLOAT` = {00000003-0000-0010-8000-00AA00389B71}
static KSDATAFORMAT_SUBTYPE_IEEE_FLOAT: GUID = GUID {
    data1: 0x0000_0003,
    data2: 0x0000,
    data3: 0x0010,
    data4: [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
};

/// 音频引擎的**混音格式**（sysvad 的 m_pMixFormat 同款）：32bit IEEE float / 48k / 2ch。
///
/// 回归（实测 0x88890008 = AUDCLNT_E_UNSUPPORTED_FORMAT）：`IMiniportAudioEngineNode::GetMixFormat`
/// 必须返回引擎内部处理用的 float32 格式；返回设备格式（16bit PCM）会被判"格式不支持"。
/// 对照：本机 Realtek / ToDesk 端点的 GetMixFormat 均为 `0xFFFE / 48000|44100 / 32bit`。
/// （设备格式仍是 16bit PCM —— 引擎负责在混音格式与设备格式之间转换。）
static MIX_FORMAT: KsDataFormatWaveFormatExtensible = KsDataFormatWaveFormatExtensible {
    data_format: KSDATAFORMAT {
        FormatSize: size_of::<KsDataFormatWaveFormatExtensible>() as u32,
        Flags: 0,
        SampleSize: 0,
        Reserved: 0,
        MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
        SubFormat: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
        Specifier: KSDATAFORMAT_SPECIFIER_WAVEFORMATEX,
    },
    wave: WaveFormatExtensible {
        format_tag: 0xFFFE,
        channels: 2,
        samples_per_sec: 48_000,
        avg_bytes_per_sec: 384_000, // 48000 * 2ch * 4B
        block_align: 8,
        bits_per_sample: 32,
        cb_size: 22,
        valid_bits_per_sample: 32,
        channel_mask: 3,
        sub_format: KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    },
};

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
        return proposed_format_basicsupport(r);
    }
    if verb & KSPROPERTY_TYPE_GET != 0 {
        return proposed_format_get(r);
    }
    if verb & KSPROPERTY_TYPE_SET != 0 {
        return proposed_format_set(r);
    }

    STATUS_INVALID_PARAMETER
}

/// BASICSUPPORT：回本属性能用的 verb 位（PortCls 据此应答 KSPROPERTY_TYPE_BASICSUPPORT）。
unsafe fn proposed_format_basicsupport(r: &mut PCPROPERTY_REQUEST) -> NTSTATUS {
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
    STATUS_SUCCESS
}

/// ks.h `KSP_PIN` = KSPROPERTY(24) + PinId(4) + Reserved(4)
const KSP_PIN_SIZE: usize = 32;

/// pin 角色（sysvad::PropertyHandlerProposedFormat 的 IsSystemRenderPin/IsBridgePin 判定）
enum PinKind {
    Stream,
    Bridge,
    Invalid,
}

/// 判定 pin 是开流用的 host pin、物理连接的 bridge pin，还是不存在。
/// `MajorTarget` 对滤波器级属性就是本滤波器的 miniport（portcls.h 约定）。
unsafe fn stream_pin_kind(major: PVOID, pin: u32) -> PinKind {
    if major.is_null() {
        return PinKind::Invalid;
    }
    // SAFETY: MajorTarget 由 PortCls 保证是 MiniportWaveRT*
    let capture = unsafe { (*(major.cast::<MiniportWaveRT>())).capture };
    // render：pin0=host(pin1=bridge)；capture：pin0=bridge(pin1=host)
    let (stream_pin, bridge_pin) = if capture { (1u32, 0u32) } else { (0u32, 1u32) };
    if pin == stream_pin {
        PinKind::Stream
    } else if pin == bridge_pin {
        PinKind::Bridge
    } else {
        PinKind::Invalid
    }
}

/// 从实例（KSP_PIN 之后的 KSMULTIPLE_ITEM 属性列表）里取音频信号处理模式。
/// 与 sysvad `GetAttributesFromAttributeList` 同款：没有属性列表 → DEFAULT；
/// 属性 ID 不是 AUDIOSIGNALPROCESSING_MODE、或尺寸不符 → 调用方按 NOT_SUPPORTED 应答。
unsafe fn mode_from_instance(r: &PCPROPERTY_REQUEST) -> Result<GUID, NTSTATUS> {
    let mut mode = AUDIO_SIGNALPROCESSINGMODE_DEFAULT;
    let cb_items = (r.InstanceSize as usize).saturating_sub(KSP_PIN_SIZE);
    if cb_items == 0 || r.Instance.is_null() {
        return Ok(mode);
    }
    // KSMULTIPLE_ITEM{ Count, Size } 之后是 KSATTRIBUTE 链，每个按 8 字节对齐
    let mut off = size_of::<KsMultipleItem>();
    while off + size_of::<KsAttribute>() <= cb_items {
        // SAFETY: 已确认 off + sizeof(KSATTRIBUTE) <= cb_items
        let attr = unsafe {
            &*(r.Instance as *const u8)
                .add(KSP_PIN_SIZE + off)
                .cast::<KsAttribute>()
        };
        let cb_attr = ((attr.Size as usize) + 7) & !7;
        if attr.Attribute != KSATTRIBUTEID_AUDIOSIGNALPROCESSING_MODE {
            return Err(STATUS_NOT_SUPPORTED);
        }
        if (attr.Size as usize) != size_of::<KsAttributeMode>() || off + cb_attr > cb_items {
            return Err(STATUS_INVALID_PARAMETER);
        }
        // SAFETY: KSATTRIBUTE_AUDIOSIGNALPROCESSING_MODE 已按尺寸校验
        let full = unsafe {
            &*(r.Instance as *const u8)
                .add(KSP_PIN_SIZE + off)
                .cast::<KsAttributeMode>()
        };
        mode = full.SignalProcessingMode;
        off += cb_attr;
    }
    Ok(mode)
}

/// 引擎提出的格式是否就是本驱动的设备格式（sysvad `IsFormatSupported` 的等价判定）：
/// 2ch / 48kHz / 16bit / PCM（WAVEFORMATEX 或 WAVEFORMATEXTENSIBLE+PCM）。
unsafe fn proposed_format_matches_device(value: *const u8, size: u32) -> bool {
    let dsf = size_of::<KSDATAFORMAT>();
    let wfx = size_of::<WAVEFORMATEX>();
    if size as usize >= dsf + wfx {
        // SAFETY: 缓冲至少含一个 KSDATAFORMAT
        let df = unsafe { &*value.cast::<KSDATAFORMAT>() };
        if df.MajorFormat != KSDATAFORMAT_TYPE_AUDIO || df.SubFormat != KSDATAFORMAT_SUBTYPE_PCM {
            return false;
        }
        // SAFETY: 缓冲至少含 KSDATAFORMAT + WAVEFORMATEX
        let wf = unsafe { &*value.add(dsf).cast::<WAVEFORMATEX>() };
        if wf.nChannels != 2 || wf.nSamplesPerSec != 48_000 || wf.wBitsPerSample != 16 {
            return false;
        }
        return match wf.wFormatTag {
            1 => true, // WAVE_FORMAT_PCM
            0xFFFE => {
                if (size as usize) < dsf + size_of::<WaveFormatExtensible>() {
                    return false;
                }
                // SAFETY: 缓冲至少含完整 WAVEFORMATEXTENSIBLE；packed 结构只能按字段
                // 偏移做 read_unaligned（对 packed 结构取字段引用是 UB）
                let sub: GUID = unsafe {
                    core::ptr::read_unaligned(
                        value
                            .add(dsf + core::mem::offset_of!(WaveFormatExtensible, sub_format))
                            .cast::<GUID>(),
                    )
                };
                sub == KSDATAFORMAT_SUBTYPE_PCM
            }
            _ => false,
        };
    }
    // 裸 WAVEFORMATEX（sysvad 不支持，这里宽松接受同参数的 PCM）
    if (size as usize) < wfx {
        return false;
    }
    // SAFETY: 缓冲至少含一个 WAVEFORMATEX
    let wf = unsafe { &*value.cast::<WAVEFORMATEX>() };
    wf.wFormatTag == 1
        && wf.nChannels == 2
        && wf.nSamplesPerSec == 48_000
        && wf.wBitsPerSample == 16
}

/// ksmedia.h `KSATTRIBUTE_AUDIOSIGNALPROCESSING_MODE`：KSATTRIBUTE + GUID
#[repr(C)]
struct KsAttributeMode {
    pub header: KsAttribute,
    pub SignalProcessingMode: GUID,
}

/// GET（KSPROPERTY_PIN_PROPOSEDATAFORMAT2）——官方契约（sysvad
/// minwavert.cpp::PropertyHandlerProposedFormat2）：
///   cbMinSize = DefaultFormat->FormatSize 向上取 8 对齐，**再加上实例里的属性列表长度**
///   （KSATTRIBUTE 链，承载 AUDIO_SIGNALPROCESSINGMODE 等），value = [格式][属性列表]。
/// 0 长度缓冲 = "只问需要多大" → 回 STATUS_BUFFER_OVERFLOW + ValueSize；
/// 不足 → BUFFER_TOO_SMALL；足够 → 拷格式 + 原样回拷属性列表。
/// 回归：原实现只拷 104 字节格式、0 长度也回 BUFFER_TOO_SMALL，引擎会反复提案。
unsafe fn proposed_format_get(r: &mut PCPROPERTY_REQUEST) -> NTSTATUS {
    dbg_inc(&DBG_PROPOSED_GET);
    // 模式必须是本驱动 GetModes 报过的那些；未知模式 → NOT_SUPPORTED（sysvad 同款）
    let mode = match unsafe { mode_from_instance(r) } {
        Ok(m) => m,
        Err(status) => return status,
    };
    if !SUPPORTED_MODES.contains(&mode) {
        return STATUS_NOT_SUPPORTED;
    }
    let fmt_size = size_of::<KsDataFormatWaveFormatExtensible>();
    let attr_len = (r.InstanceSize as usize).saturating_sub(KSP_PIN_SIZE);
    let need_total = ((fmt_size + 7) & !7) + attr_len;
    if r.ValueSize == 0 {
        r.ValueSize = need_total as u32;
        DBG_PROPOSED_GET_SIZE.store(need_total as u32, Ordering::Relaxed);
        return STATUS_BUFFER_OVERFLOW;
    }
    if (r.ValueSize as usize) < need_total {
        r.ValueSize = need_total as u32;
        DBG_PROPOSED_GET_SIZE.store(need_total as u32, Ordering::Relaxed);
        return STATUS_BUFFER_TOO_SMALL;
    }
    // SAFETY: 目标缓冲区 >= need_total；源为只读静态（DEVICE_FORMAT 永不改写）
    unsafe {
        core::ptr::copy_nonoverlapping(
            (&raw const DEVICE_FORMAT).cast::<u8>(),
            r.Value.cast::<u8>(),
            fmt_size,
        );
        if attr_len > 0 && !r.Instance.is_null() {
            core::ptr::copy_nonoverlapping(
                (r.Instance as *const u8).add(KSP_PIN_SIZE),
                r.Value.cast::<u8>().add(fmt_size),
                attr_len,
            );
            // 值里带了属性列表 → 格式头必须置 KSDATAFORMAT_ATTRIBUTES
            // （sysvad::PropertyHandlerProposedFormat2 同款），否则引擎会认为
            // 属性列表无效、匹配不到模式。
            core::ptr::write(
                r.Value.cast::<u8>().add(4).cast::<u32>(),
                KSDATAFORMAT_ATTRIBUTES,
            );
        }
    }
    DBG_PROPOSED_GET_SIZE.store(need_total as u32, Ordering::Relaxed);
    DBG_PROPOSED_ATTR_LEN.store(attr_len as u32, Ordering::Relaxed);
    r.ValueSize = need_total as u32;
    STATUS_SUCCESS
}

/// SET（KSPROPERTY_PIN_PROPOSEDATAFORMAT）：按设备格式**校验**引擎提出的格式 ——
/// 支持回 SUCCESS，不支持回 NO_MATCH（sysvad `PropertyHandlerProposedFormat` 的
/// `IsFormatSupported` 语义）。
///
/// 回归：原实现"接受任何 tag∈{PCM,EXTENSIBLE} 的提案并把设备格式写回缓冲"，
/// 等于对引擎的每个候选格式都说"行"，真机实测引擎（AEB）据此反复提案 1600+ 次、
/// 最终仍报 0x80070491。参考实现只回状态、不回填缓冲。
unsafe fn proposed_format_set(r: &mut PCPROPERTY_REQUEST) -> NTSTATUS {
    let need = size_of::<KsDataFormatWaveFormatExtensible>() as u32;
    if (r.InstanceSize as usize) < KSP_PIN_SIZE || r.Instance.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: 实例至少含 KSP_PIN 的 PinId 字段
    let pin = unsafe { core::ptr::read_unaligned((r.Instance as *const u8).add(24).cast::<u32>()) };
    match unsafe { stream_pin_kind(r.MajorTarget, pin) } {
        PinKind::Stream => {}
        // 只对开流 pin 有效（sysvad：bridge pin 回 NOT_SUPPORTED、其它 pin 回 INVALID_PARAMETER）
        PinKind::Bridge => return STATUS_NOT_SUPPORTED,
        PinKind::Invalid => return STATUS_INVALID_PARAMETER,
    }
    if r.ValueSize == 0 {
        r.ValueSize = need;
        return STATUS_BUFFER_OVERFLOW;
    }
    if r.ValueSize < need {
        return STATUS_BUFFER_TOO_SMALL;
    }
    // 真机诊断：记录引擎最近提出的格式形状（tag / 采样率 / 位深 / 校验结果）
    // SAFETY: ValueSize >= 104，缓冲至少含 KSDATAFORMAT + WAVEFORMATEX
    let (tag, rate, bits) = unsafe {
        let df = r.Value.cast::<u8>();
        let wf = &*df.add(size_of::<KSDATAFORMAT>()).cast::<WAVEFORMATEX>();
        (
            u32::from(wf.wFormatTag),
            wf.nSamplesPerSec,
            u32::from(wf.wBitsPerSample),
        )
    };
    dbg_inc(&DBG_PROPOSED_SET);
    DBG_PROPOSED_LAST_TAG.store(tag, Ordering::Relaxed);
    DBG_PROPOSED_LAST_RATE.store(rate, Ordering::Relaxed);
    DBG_PROPOSED_LAST_BITS.store(bits, Ordering::Relaxed);
    let ok = unsafe { proposed_format_matches_device(r.Value.cast::<u8>(), r.ValueSize) };
    if ok {
        dbg_inc(&DBG_PROPOSED_SET_OK);
        STATUS_SUCCESS
    } else {
        dbg_inc(&DBG_PROPOSED_SET_REJECT);
        STATUS_NO_MATCH
    }
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
/// 引擎最后一次提案的形状 + 被拒次数（严格校验后用来判断"引擎到底想要什么"）
static DBG_PROPOSED_LAST_RATE: AtomicU32 = AtomicU32::new(0);
static DBG_PROPOSED_LAST_BITS: AtomicU32 = AtomicU32::new(0);
static DBG_PROPOSED_SET_REJECT: AtomicU32 = AtomicU32::new(0);
/// 音频引擎节点方法调用计数（看引擎走的是哪条路）
static DBG_AE_GET_MIX_FORMAT: AtomicU32 = AtomicU32::new(0);
static DBG_AE_GET_DEVICE_FORMAT: AtomicU32 = AtomicU32::new(0);
static DBG_AE_SET_DEVICE_FORMAT: AtomicU32 = AtomicU32::new(0);
static DBG_AE_SET_STATUS: AtomicU32 = AtomicU32::new(0);
/// 引擎 SetDeviceFormat 设进来的格式（tag/rate/bits，看它想要什么）
static DBG_AE_STORED_TAG: AtomicU32 = AtomicU32::new(0);
static DBG_AE_STORED_RATE: AtomicU32 = AtomicU32::new(0);
static DBG_AE_STORED_BITS: AtomicU32 = AtomicU32::new(0);
/// 更细的引擎调用现场（下一次真机验证用）：
/// - GetEngineFormatSize 的最后一次 formatType 与我们回的长度
/// - SetDeviceFormat 最后一次的缓冲长度（引擎是否带属性列表/是否 <104）
/// - GetModes 调用次数与最后一次的 pin / 返回的模式数
static DBG_AE_FMT_TYPE: AtomicU32 = AtomicU32::new(u32::MAX);
static DBG_AE_FMT_SIZE_OUT: AtomicU32 = AtomicU32::new(0);
static DBG_AE_SET_BUF_SIZE: AtomicU32 = AtomicU32::new(0);
static DBG_MODES_CALLS: AtomicU32 = AtomicU32::new(0);
static DBG_MODES_LAST_PIN: AtomicU32 = AtomicU32::new(u32::MAX);
static DBG_MODES_LAST_COUNT: AtomicU32 = AtomicU32::new(0);
/// PROPOSEDATAFORMAT2 GET 的返回长度与实例属性列表长度（看引擎带的模式属性）
static DBG_PROPOSED_GET_SIZE: AtomicU32 = AtomicU32::new(0);
static DBG_PROPOSED_ATTR_LEN: AtomicU32 = AtomicU32::new(0);
/// 引擎节点方法全量计数（16 个方法各一个槽，按 vtable 顺序）
static DBG_AE_CALLS: [AtomicU32; 16] = [
    AtomicU32::new(0), // 0 GetAudioEngineDescriptor
    AtomicU32::new(0), // 1 GetGfxState
    AtomicU32::new(0), // 2 SetGfxState
    AtomicU32::new(0), // 3 GetEngineFormatSize
    AtomicU32::new(0), // 4 GetMixFormat
    AtomicU32::new(0), // 5 GetDeviceFormat
    AtomicU32::new(0), // 6 SetDeviceFormat
    AtomicU32::new(0), // 7 GetSupportedDeviceFormats
    AtomicU32::new(0), // 8 GetDeviceChannelCount
    AtomicU32::new(0), // 9 GetDeviceAttributeSteppings
    AtomicU32::new(0), // 10 GetDeviceChannelVolume
    AtomicU32::new(0), // 11 SetDeviceChannelVolume
    AtomicU32::new(0), // 12 GetDeviceChannelMute
    AtomicU32::new(0), // 13 SetDeviceChannelMute
    AtomicU32::new(0), // 14 GetDeviceChannelPeakMeter
    AtomicU32::new(0), // 15 GetBufferSizeRange
];

/// 记录一次引擎节点方法调用（带 in-flight/失败标记方便定位）
fn ae_tick(idx: usize, status: NTSTATUS) {
    dbg_inc(&DBG_AE_CALLS[idx]);
    if status < 0 {
        DBG_AE_LAST_FAIL_IDX.store(idx as u32, Ordering::Relaxed);
        DBG_AE_LAST_FAIL_STATUS.store(status_bits(status), Ordering::Relaxed);
    }
}

static DBG_AE_LAST_FAIL_IDX: AtomicU32 = AtomicU32::new(u32::MAX);
static DBG_AE_LAST_FAIL_STATUS: AtomicU32 = AtomicU32::new(0);
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

/// NTSTATUS(i32) → u32 位模式（诊断计数器用；避免 clippy::cast_sign_loss）
const fn status_bits(s: NTSTATUS) -> u32 {
    u32::from_ne_bytes(s.to_ne_bytes())
}

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
/// 计数器快照长度：53 个 u32（前 53 个是标量计数，其后紧跟 16 个引擎节点方法计数）
const DBG_STATS_LEN: usize = 53 * 4;

unsafe extern "system" fn vdev_debug_property_handler(req: *mut PCPROPERTY_REQUEST) -> NTSTATUS {
    // SAFETY: PortCls 保证 req 有效
    let r = unsafe { &mut *req };
    if r.Value.is_null() {
        return STATUS_INVALID_PARAMETER;
    }
    if r.Verb & KSPROPERTY_TYPE_GET != 0 {
        if (r.ValueSize as usize) < DBG_STATS_LEN + 16 * 4 {
            r.ValueSize = (DBG_STATS_LEN + 16 * 4) as ULONG;
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
            DBG_AE_GET_MIX_FORMAT.load(Ordering::Relaxed),
            DBG_AE_GET_DEVICE_FORMAT.load(Ordering::Relaxed),
            DBG_AE_SET_DEVICE_FORMAT.load(Ordering::Relaxed),
            DBG_AE_SET_STATUS.load(Ordering::Relaxed),
            DBG_AE_STORED_TAG.load(Ordering::Relaxed),
            DBG_AE_STORED_RATE.load(Ordering::Relaxed),
            DBG_AE_STORED_BITS.load(Ordering::Relaxed),
            DBG_AE_LAST_FAIL_IDX.load(Ordering::Relaxed),
            DBG_AE_LAST_FAIL_STATUS.load(Ordering::Relaxed),
            DBG_PROPOSED_GET_SIZE.load(Ordering::Relaxed),
            DBG_PROPOSED_ATTR_LEN.load(Ordering::Relaxed),
            DBG_AE_FMT_TYPE.load(Ordering::Relaxed),
            DBG_AE_FMT_SIZE_OUT.load(Ordering::Relaxed),
            DBG_AE_SET_BUF_SIZE.load(Ordering::Relaxed),
            DBG_MODES_CALLS.load(Ordering::Relaxed),
            DBG_MODES_LAST_PIN.load(Ordering::Relaxed),
            DBG_MODES_LAST_COUNT.load(Ordering::Relaxed),
            DBG_PROPOSED_LAST_RATE.load(Ordering::Relaxed),
            DBG_PROPOSED_LAST_BITS.load(Ordering::Relaxed),
            DBG_PROPOSED_SET_REJECT.load(Ordering::Relaxed),
        ];
        // SAFETY: ValueSize >= DBG_STATS_LEN 已校验；逐元素 unaligned 写入
        unsafe {
            for (i, v) in vals.iter().enumerate() {
                core::ptr::write_unaligned(r.Value.cast::<u32>().add(i), *v);
            }
            // 16 个引擎节点方法计数接在结构体之后
            for (i, c) in DBG_AE_CALLS.iter().enumerate() {
                core::ptr::write_unaligned(
                    r.Value.cast::<u32>().add(vals.len() + i),
                    c.load(Ordering::Relaxed),
                );
            }
        }
        r.ValueSize = (DBG_STATS_LEN + 16 * 4) as ULONG;
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
const STATUS_BUFFER_OVERFLOW: NTSTATUS = i32::from_ne_bytes(0x8000_0005u32.to_ne_bytes());

/// wave 滤波器内部连接：host pin ↔ bridge pin（对照本机 ToDesk 虚拟声卡实测
/// `KSPROPERTY_TOPOLOGY_CONNECTIONS` 返回 1 条连接；sysvad 是经
/// KSNODETYPE_AUDIO_ENGINE 节点中转，本驱动不做音频引擎节点，直接相连）。
// 更新（Win10 音频引擎节点）：按 sysvad speakerwavtable.h 的形状经
// KSNODETYPE_AUDIO_ENGINE 节点中转 —— host pin → node 输入(pin 1)；node 输出(pin 0) → bridge pin。
// 音频引擎节点是 Win10 端点"应用可用"的前提（引擎会 QI IMiniportAudioEngineNode）。
/// 实验 B：无引擎节点 → 连接回到 host ↔ bridge 直连（ToDesk 实测就是 1 条连接）
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

/// ksmedia.h `KSNODETYPE_AUDIO_ENGINE` = {35CAF6E4-F3B3-4168-BB4B-55E77A461C7E}
/// （实验 B 不再声明引擎节点；保留常量以便切回）
#[allow(dead_code)]
static KSNODETYPE_AUDIO_ENGINE: GUID = GUID {
    data1: 0x35ca_f6e4,
    data2: 0xf3b3,
    data3: 0x4168,
    data4: [0xbb, 0x4b, 0x55, 0xe7, 0x7a, 0x46, 0x1c, 0x7e],
};
/// 实验 B（basic 模式）：**不声明音频引擎节点**。
///
/// 依据：本机可用的 ToDesk 虚拟声卡 wave 滤波器就是 `nodes=0`（无引擎节点），端点在
/// Win10 上走"basic 模式"由 audiodg 自行处理（实测其共享模式 AUTOCONVERTPCM 开流 hr=0）；
/// 而我们声明引擎节点后，引擎会要求完整的引擎能力（GetSupportedDeviceFormats /
/// GetBufferSizeRange / 建流前的校验），本驱动只实现了其中一部分，实测引擎方法全调通但
/// 从不建流、客户端报 AUDCLNT_E_UNSUPPORTED_FORMAT。先回到 basic 模式把端点跑通。
/// （引擎节点相关代码与接口仍保留在文件里，仅不在此声明/不在此暴露 QI。）
static WAVE_NODES: [PCNODE_DESCRIPTOR; 0] = [];

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
            DataRangesCount: DATA_RANGES.0.len() as u32,
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
            DataRangesCount: DATA_RANGES.0.len() as u32,
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
    NodeSize: size_of::<PCNODE_DESCRIPTOR>() as u32,
    NodeCount: WAVE_NODES.len() as u32,
    Nodes: WAVE_NODES.as_ptr(),
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
    NodeSize: size_of::<PCNODE_DESCRIPTOR>() as u32,
    NodeCount: WAVE_NODES.len() as u32,
    Nodes: WAVE_NODES.as_ptr(),
    ConnectionCount: WAVE_CAPTURE_CONNECTIONS.len() as u32,
    Connections: WAVE_CAPTURE_CONNECTIONS.as_ptr(),
    CategoryCount: FILTER_CATEGORIES.len() as u32,
    Categories: FILTER_CATEGORIES.as_ptr(),
};
