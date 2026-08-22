//! WaveRT 小端口（虚拟扬声器/麦克风）+ 环回流
#![allow(non_snake_case, non_camel_case_types)]
#![allow(clippy::missing_errors_doc)]

use core::mem::size_of;

use crate::com::{interlocked_decrement, interlocked_increment};
use crate::ringbuffer::RingBuffer;
use crate::sys::portcls::*;
use crate::sys::types::*;

pub const TAG: u32 = u32::from_le_bytes(*b"vdev");

// ============ IPortWaveRTStream（小端口调用端口侧） ============

pub type PFN_AllocatePagesForMdl = unsafe extern "system" fn(PVOID, u64, usize) -> *mut c_void;
pub type PFN_MapAllocatedPages = unsafe extern "system" fn(PVOID, *mut c_void, u32) -> *mut c_void;
pub type PFN_FreePagesFromMdl = unsafe extern "system" fn(PVOID, *mut c_void);

/// IPortWaveRTStream vtable（仅声明小端口需要的方法）
#[repr(C)]
pub struct IPortWaveRTStreamVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub allocate_pages_for_mdl: PFN_AllocatePagesForMdl,
    pub allocate_contiguous_pages_for_mdl: PFN_AllocatePagesForMdl,
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
    pub state: KSSTATE,
    pub port_stream: PVOID,
    pub miniport: *mut MiniportWaveRT,
    pub dma_buffer: *mut u8,
    pub dma_size: u32,
    pub buffer_mdl: *mut c_void,
    pub block_align: u16,
    pub position: u64,
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
        core::ptr::write(
            ptr as *mut WaveRTStream,
            WaveRTStream {
                vtable: &WAVERT_STREAM_VTABLE,
                refcount: 1,
                capture,
                state: KSSTATE::KSSTATE_STOP,
                port_stream,
                miniport,
                dma_buffer: core::ptr::null_mut(),
                dma_size: 0,
                buffer_mdl: core::ptr::null_mut(),
                block_align: 4,
                position: 0,
            },
        );
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
    if is_equal_guid(iid, &IID_IUnknown) || is_equal_guid(iid, &IID_IMiniportWaveRTStream) {
        *obj = this.cast();
        interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
        STATUS_SUCCESS
    } else {
        *obj = core::ptr::null_mut();
        STATUS_INVALID_PARAMETER
    }
}

unsafe extern "system" fn stream_addref(this: PVOID) -> u32 {
    // SAFETY: this 指向流对象
    interlocked_increment(core::ptr::addr_of_mut!(
        (*(this as *mut WaveRTStream)).refcount
    ))
}

unsafe extern "system" fn stream_release(this: PVOID) -> u32 {
    let this = this as *mut WaveRTStream;
    // SAFETY: 引用计数保护
    let rc = interlocked_decrement(core::ptr::addr_of_mut!((*this).refcount));
    if rc == 0 {
        // SAFETY: 释放对象内存；MDL/缓冲由 stream_free_audio_buffer 先释放
        crate::sys::mem::ExFreePoolWithTag_np(this.cast(), TAG);
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

unsafe extern "system" fn stream_set_format(this: PVOID, data_format: PKSDATAFORMAT) -> NTSTATUS {
    let this = this as *mut WaveRTStream;
    // SAFETY: 调用方保证参数有效
    let df = &*data_format;
    if df.SubFormat != KSDATAFORMAT_SUBTYPE_PCM {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // 解析 WAVEFORMATEX（紧跟 KSDATAFORMAT）
    // SAFETY: 调用方保证格式有效
    let wf = &*((data_format as *mut u8).add(size_of::<KSDATAFORMAT>()) as *const WAVEFORMATEX);
    // SAFETY: 单线程初始化
    (*this).block_align = wf.nBlockAlign;
    STATUS_SUCCESS
}

unsafe extern "system" fn stream_set_state(this: PVOID, state: KSSTATE) -> NTSTATUS {
    let this = this as *mut WaveRTStream;
    // SAFETY: 原子写状态
    (*this).state = state;
    STATUS_SUCCESS
}

unsafe extern "system" fn stream_get_position(
    this: PVOID,
    position: *mut KSAUDIO_POSITION,
) -> NTSTATUS {
    let this = this as *mut WaveRTStream;
    // SAFETY: position 为有效输出
    (*position).PlayOffset = (*this).position;
    (*position).WriteOffset = (*this).position;
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
    let ps: &WaveRTStream = &*(this as *const WaveRTStream);
    let vtbl = &*(ps.port_stream as *const c_void as *const IPortWaveRTStreamVtbl);
    let md = (vtbl.allocate_pages_for_mdl)(ps.port_stream, u64::MAX, requested_size as usize);
    if md.is_null() {
        return STATUS_UNSUCCESSFUL;
    }
    let base = (vtbl.map_allocated_pages)(ps.port_stream, md, 2); // MmCached
    if base.is_null() {
        (vtbl.free_pages_from_mdl)(ps.port_stream, md);
        return STATUS_UNSUCCESSFUL;
    }
    *mdl = md;
    *actual_size = requested_size;
    *offset = 0;
    *cache_type = 2;
    (*(this as *mut WaveRTStream)).dma_buffer = base.cast();
    (*(this as *mut WaveRTStream)).dma_size = requested_size;
    (*(this as *mut WaveRTStream)).buffer_mdl = md;
    STATUS_SUCCESS
}

unsafe extern "system" fn stream_free_audio_buffer(this: PVOID, mdl: *mut c_void, _size: u32) {
    let this = this as *mut WaveRTStream;
    // SAFETY: port_stream 为有效 IPortWaveRTStream
    let ps: &WaveRTStream = &*(this as *const WaveRTStream);
    let vtbl = &*(ps.port_stream as *const c_void as *const IPortWaveRTStreamVtbl);
    if !ps.dma_buffer.is_null() {
        (vtbl.unmap_allocated_pages)(ps.port_stream, ps.dma_buffer.cast(), mdl);
    }
    if !mdl.is_null() {
        (vtbl.free_pages_from_mdl)(ps.port_stream, mdl);
    }
    (*(this as *mut WaveRTStream)).dma_buffer = core::ptr::null_mut();
    (*(this as *mut WaveRTStream)).buffer_mdl = core::ptr::null_mut();
}

unsafe extern "system" fn stream_get_hw_latency(this: PVOID, latency: *mut KSRTAUDIO_HWLATENCY) {
    // SAFETY: latency 为有效输出
    (*latency).FifoSize = 0;
    (*latency).ChipsetDelay = 0;
    (*latency).CodecDelay = 0;
    let _ = this;
}

unsafe extern "system" fn stream_get_position_register(
    this: PVOID,
    reg: *mut KSRTAUDIO_HWREGISTER,
) -> NTSTATUS {
    // SAFETY: reg 为有效输出
    (*reg).Register = core::ptr::null_mut();
    (*reg).Width = 0;
    (*reg).Numerator = 1;
    (*reg).Denominator = 1;
    (*reg).Accuracy = 0;
    let _ = this;
    STATUS_SUCCESS
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
    pub get_description: unsafe extern "system" fn(PVOID, PPCFILTER_DESCRIPTOR) -> NTSTATUS,
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
    if is_equal_guid(iid, &IID_IUnknown) || is_equal_guid(iid, &IID_IMiniport) {
        *obj = this.cast();
        interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
        STATUS_SUCCESS
    } else if is_equal_guid(iid, &IID_IMiniportWaveRT) {
        *obj = this.cast();
        interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
        STATUS_SUCCESS
    } else {
        *obj = core::ptr::null_mut();
        STATUS_INVALID_PARAMETER
    }
}

unsafe extern "system" fn miniport_addref(this: PVOID) -> u32 {
    // SAFETY: this 指向小端口
    interlocked_increment(core::ptr::addr_of_mut!(
        (*(this as *mut MiniportWaveRT)).refcount
    ))
}

pub unsafe extern "system" fn miniport_release(this: PVOID) -> u32 {
    let this = this as *mut MiniportWaveRT;
    // SAFETY: 引用计数保护
    let rc = interlocked_decrement(core::ptr::addr_of_mut!((*this).refcount));
    if rc == 0 {
        // SAFETY: 释放对象内存
        crate::sys::mem::ExFreePoolWithTag_np(this.cast(), TAG);
    }
    rc
}

unsafe extern "system" fn miniport_get_description(
    this: PVOID,
    desc: PPCFILTER_DESCRIPTOR,
) -> NTSTATUS {
    // SAFETY: 输出指针有效；返回静态描述符
    *desc = &FILTER_DESC as *const PCFILTER_DESCRIPTOR as *mut PCFILTER_DESCRIPTOR;
    let _ = this;
    STATUS_SUCCESS
}

unsafe extern "system" fn miniport_data_range_intersection(
    this: PVOID,
    _pin: u32,
    _client: *mut KSDATARANGE,
    _mine: *mut KSDATARANGE,
    out_len: u32,
    out: PVOID,
    out_len_ret: *mut u32,
) -> NTSTATUS {
    // SAFETY: 输出指针有效
    let _ = this;
    let needed = size_of::<KSDATAFORMAT>() + size_of::<WAVEFORMATEX>();
    if out_len < needed as u32 || out.is_null() {
        *out_len_ret = needed as u32;
        return STATUS_BUFFER_TOO_SMALL;
    }
    let df = out as *mut KSDATAFORMAT;
    // SAFETY: 缓冲区足够
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
    // SAFETY: 缓冲区足够
    core::ptr::write(
        wf,
        WAVEFORMATEX {
            wFormatTag: 1, // WAVE_FORMAT_PCM
            nChannels: 2,
            nSamplesPerSec: 48000,
            nAvgBytesPerSec: 48000 * 2 * 2,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        },
    );
    *out_len_ret = needed as u32;
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
    (*this).adapter = unknown_adapter;
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
    // 校验格式
    // SAFETY: 调用方保证参数有效
    let df = &*data_format;
    if df.SubFormat != KSDATAFORMAT_SUBTYPE_PCM {
        return STATUS_INVALID_DEVICE_REQUEST;
    }
    // SAFETY: 创建流对象
    let stream = WaveRTStream::new(this, port_stream, capture);
    if stream.is_null() {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // SAFETY: 输出流指针
    *stream_out = stream.cast();
    STATUS_SUCCESS
}

unsafe extern "system" fn miniport_get_device_description(
    _this: PVOID,
    desc: *mut DEVICE_DESCRIPTION,
) -> NTSTATUS {
    // SAFETY: 输出指针有效
    core::ptr::write(
        desc,
        DEVICE_DESCRIPTION {
            Version: 0x1,
            Master: 0,
            ScatterGather: 0,
            DemandMode: 0,
            AutoInitialize: 0,
            Paging: 0,
            Dma32BitAddresses: 0,
            IgnoreCount: 0,
            Reserved1: 0,
            Dma64BitAddresses: 1,
            BusNumber: 0,
            DmaWidth: 32,
            DmaTransferWidth: 32,
            MaximumLength: 0xFFFF_FFFF,
            DmaPort: 0,
        },
    );
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

// ============ 过滤器描述符（单 Pin） ============

static KSDATAFORMAT_TYPE_AUDIO: GUID = GUID {
    data1: 0x0000_000f,
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

static PINS: [PCPIN_DESCRIPTOR; 1] = [PCPIN_DESCRIPTOR {
    MaxInstances: 1,
    Interrupts: 0,
    AutomationTable: core::ptr::null_mut(),
    KsPinDescriptor: core::ptr::null_mut(),
}];

static FILTER_DESC: PCFILTER_DESCRIPTOR = PCFILTER_DESCRIPTOR {
    Version: 0x0100,
    AutomationTable: core::ptr::null_mut(),
    PinSize: size_of::<PCPIN_DESCRIPTOR>() as u32,
    PinCount: 1,
    Pins: PINS.as_ptr() as *mut PCPIN_DESCRIPTOR,
    NodeSize: 0,
    NodeCount: 0,
    Nodes: core::ptr::null_mut(),
    ConnectionSize: 0,
    ConnectionCount: 0,
    Connections: core::ptr::null_mut(),
    Category: GUID {
        data1: 0x17ea_fd10,
        data2: 0x2b3f,
        data3: 0x11d1,
        data4: [0x8f, 0x0e, 0x00, 0xc0, 0x4f, 0xb9, 0x80, 0xb9],
    },
    Name: GUID {
        data1: 0x17ea_fd10,
        data2: 0x2b3f,
        data3: 0x11d1,
        data4: [0x8f, 0x0e, 0x00, 0xc0, 0x4f, 0xb9, 0x80, 0xb9],
    },
    ComponentId: GUID {
        data1: 0x17ea_fd10,
        data2: 0x2b3f,
        data3: 0x11d1,
        data4: [0x8f, 0x0e, 0x00, 0xc0, 0x4f, 0xb9, 0x80, 0xb9],
    },
    Topology: GUID {
        data1: 0x17ea_fd10,
        data2: 0x2b3f,
        data3: 0x11d1,
        data4: [0x8f, 0x0e, 0x00, 0xc0, 0x4f, 0xb9, 0x80, 0xb9],
    },
    CapsFlags: 0,
    DeviceInterfaceGuid: GUID {
        data1: 0x17ea_fd10,
        data2: 0x2b3f,
        data3: 0x11d1,
        data4: [0x8f, 0x0e, 0x00, 0xc0, 0x4f, 0xb9, 0x80, 0xb9],
    },
};
