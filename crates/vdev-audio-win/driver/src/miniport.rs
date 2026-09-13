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
    let this = this as *mut WaveRTStream;
    // SAFETY: 调用方保证 data_format 指向完整 KSDATAFORMAT_WAVEFORMATEX
    let ksdm = unsafe { &*data_format.cast::<KSDATAFORMAT_WAVEFORMATEX>() };
    if ksdm.DataFormat.SubFormat != KSDATAFORMAT_SUBTYPE_PCM
        || ksdm.DataFormat.Specifier != KSDATAFORMAT_SPECIFIER_WAVEFORMATEX
        || ksdm.DataFormat.FormatSize < size_of::<KSDATAFORMAT_WAVEFORMATEX>() as u32
    {
        return STATUS_INVALID_PARAMETER;
    }
    let wf = &ksdm.WaveFormatEx;
    const WAVE_FORMAT_PCM: u16 = 0x0001;
    if wf.wFormatTag != WAVE_FORMAT_PCM
        || wf.nSamplesPerSec != 48_000
        || wf.wBitsPerSample != 16
        || wf.nChannels != 2
        || wf.nBlockAlign != 4
    {
        return STATUS_INVALID_PARAMETER;
    }
    // SAFETY: 单线程初始化（PortCls 串行调用）
    unsafe {
        (*this).block_align = wf.nBlockAlign;
        (*this).bytes_per_sec = wf.nSamplesPerSec * u32::from(wf.nBlockAlign);
    }
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
                },
            );
        }
        ptr as *mut MiniportWaveRT
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
    // SAFETY: 调用方保证 iid/obj 有效
    unsafe {
        if is_equal_guid(iid, &IID_IUnknown)
            || is_equal_guid(iid, &IID_IMiniport)
            || is_equal_guid(iid, &IID_IMiniportWaveRT)
        {
            *obj = this.cast();
            interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
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
    let _ = this;
    // _mine（本地数据范围）与 _pin 未用：交集仅比对客户端范围与 AUDIO_DATA_RANGE 常量
    // SAFETY: out_len_ret 为有效输出
    unsafe { *out_len_ret = 0 };
    // SAFETY: client 由 PortCls 传入且指向完整 KSDATARANGE
    let c = unsafe { &*client };
    if c.MajorFormat != KSDATAFORMAT_TYPE_AUDIO || c.SubFormat != KSDATAFORMAT_SUBTYPE_PCM {
        return STATUS_NO_MATCH;
    }
    let needed = size_of::<KSDATAFORMAT>() + size_of::<WAVEFORMATEX>();
    if out_len < needed as u32 || out.is_null() {
        // SAFETY: 输出指针有效
        unsafe { *out_len_ret = needed as u32 };
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
    STATUS_SUCCESS
}

unsafe extern "system" fn miniport_init(
    this: PVOID,
    unknown_adapter: PVOID,
    _resource_list: *mut c_void,
    port: PVOID,
) -> NTSTATUS {
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
static FILTER_CATEGORIES_RENDER: [GUID; 3] = crate::endpoint_names::WAVE_CATEGORIES_RENDER;
static FILTER_CATEGORIES_CAPTURE: [GUID; 3] = crate::endpoint_names::WAVE_CATEGORIES_CAPTURE;

/// render 端点 pin：DataFlow=OUT / Communication=SINK / Category=KSNODETYPE_SPEAKER
/// （B6：KSPIN_DESCRIPTOR 按 ks.h 布局按值内嵌于 PCPIN_DESCRIPTOR）
static PINS_RENDER: [PCPIN_DESCRIPTOR; 1] = [PCPIN_DESCRIPTOR {
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
        Category: &raw const KSNODETYPE_SPEAKER,
        Name: core::ptr::null(),
        Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
    },
}];

/// capture 端点 pin：DataFlow=IN / Communication=SOURCE / Category=KSNODETYPE_MICROPHONE
static PINS_CAPTURE: [PCPIN_DESCRIPTOR; 1] = [PCPIN_DESCRIPTOR {
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
        Communication: KSPIN_COMMUNICATION_SOURCE,
        Category: &raw const KSNODETYPE_MICROPHONE,
        Name: core::ptr::null(),
        Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
    },
}];

/// render 过滤器描述符（B6：字段集与 portcls.h 一致，Version=0，类别数组指针）
static FILTER_DESC_RENDER: PCFILTER_DESCRIPTOR = PCFILTER_DESCRIPTOR {
    Version: 0,
    AutomationTable: core::ptr::null(),
    PinSize: size_of::<PCPIN_DESCRIPTOR>() as u32,
    PinCount: 1,
    Pins: PINS_RENDER.as_ptr(),
    NodeSize: 0,
    NodeCount: 0,
    Nodes: core::ptr::null(),
    ConnectionCount: 0,
    Connections: core::ptr::null(),
    CategoryCount: FILTER_CATEGORIES_RENDER.len() as u32,
    Categories: FILTER_CATEGORIES_RENDER.as_ptr(),
};

/// capture 过滤器描述符
static FILTER_DESC_CAPTURE: PCFILTER_DESCRIPTOR = PCFILTER_DESCRIPTOR {
    Version: 0,
    AutomationTable: core::ptr::null(),
    PinSize: size_of::<PCPIN_DESCRIPTOR>() as u32,
    PinCount: 1,
    Pins: PINS_CAPTURE.as_ptr(),
    NodeSize: 0,
    NodeCount: 0,
    Nodes: core::ptr::null(),
    ConnectionCount: 0,
    Connections: core::ptr::null(),
    CategoryCount: FILTER_CATEGORIES_CAPTURE.len() as u32,
    Categories: FILTER_CATEGORIES_CAPTURE.as_ptr(),
};
