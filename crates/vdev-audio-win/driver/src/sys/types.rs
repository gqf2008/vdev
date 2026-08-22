//! 最小 WDM 基础类型（手写，PortCls 驱动所需子集；ABI 稳定）

#![allow(non_camel_case_types, non_snake_case)]

pub use core::ffi::c_void;
pub type NTSTATUS = i32;
pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_UNSUCCESSFUL: NTSTATUS = -1_073_741_823; // 0xC0000001
pub const STATUS_INSUFFICIENT_RESOURCES: NTSTATUS = -1_073_741_803; // 0xC000009A
pub const STATUS_INVALID_PARAMETER: NTSTATUS = -1_073_741_815; // 0xC000000D
pub const STATUS_BUFFER_TOO_SMALL: NTSTATUS = -1_073_741_811; // 0xC0000023
pub const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = -1_073_741_810; // 0xC0000010
pub const STATUS_DEVICE_BUSY: NTSTATUS = -1_073_741_771; // 0xC0000011
pub type PRESOURCELIST = *mut c_void;

pub type PVOID = *mut core::ffi::c_void;
pub type PCHAR = *mut i8;
pub type ULONG = u32;
pub type USHORT = u16;
pub type UCHAR = u8;
pub type BOOLEAN = u8;

#[repr(C)]
pub struct UNICODE_STRING {
    pub Length: USHORT,
    pub MaximumLength: USHORT,
    pub Buffer: PWSTR,
}
pub type PUNICODE_STRING = *mut UNICODE_STRING;

pub type PWSTR = *mut u16;
pub type PCWSTR = *const u16;

/// 驱动对象（PortCls 只用到少量字段，其余保留占位）
#[repr(C)]
pub struct DRIVER_OBJECT {
    pub Type: i16,
    pub Size: i16,
    pub DeviceObject: PVOID,
    pub Flags: ULONG,
    pub DriverStart: PVOID,
    pub DriverSize: ULONG,
    pub DriverSection: PVOID,
    pub DriverExtension: PVOID,
    pub DriverUnload: Option<unsafe extern "system" fn(*mut DRIVER_OBJECT)>,
    pub MajorFunction: [Option<unsafe extern "system" fn(PVOID, PVOID) -> NTSTATUS>; 28],
}
pub type PDRIVER_OBJECT = *mut DRIVER_OBJECT;

/// 设备对象（占位，PortCls 内部使用）
#[repr(C)]
pub struct DEVICE_OBJECT {
    _private: [u8; 0],
}
pub type PDEVICE_OBJECT = *mut DEVICE_OBJECT;

/// IRP（占位）
#[repr(C)]
pub struct IRP {
    _private: [u8; 0],
}
pub type PIRP = *mut IRP;

// ---- GUID ----
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct GUID {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}
pub type REFGUID = *const GUID;

pub fn is_equal_guid(a: *const GUID, b: *const GUID) -> bool {
    unsafe { (*a) == (*b) }
}

// ---- KS 基础类型 ----
#[repr(C)]
pub struct KSDATARANGE {
    pub FormatSize: ULONG,
    pub Flags: ULONG,
    pub SampleSize: ULONG,
    pub Reserved: ULONG,
    pub MajorFormat: GUID,
    pub SubFormat: GUID,
    pub Specifier: GUID,
}

#[repr(C)]
pub struct KSDATAFORMAT {
    pub FormatSize: ULONG,
    pub Flags: ULONG,
    pub SampleSize: ULONG,
    pub Reserved: ULONG,
    pub MajorFormat: GUID,
    pub SubFormat: GUID,
    pub Specifier: GUID,
}
pub type PKSDATAFORMAT = *mut KSDATAFORMAT;

#[repr(C)]
pub struct KSDATAFORMAT_WAVEFORMATEX {
    pub DataFormat: KSDATAFORMAT,
    pub WaveFormatEx: *mut WAVEFORMATEX,
}

#[repr(C)]
pub struct WAVEFORMATEX {
    pub wFormatTag: u16,
    pub nChannels: u16,
    pub nSamplesPerSec: u32,
    pub nAvgBytesPerSec: u32,
    pub nBlockAlign: u16,
    pub wBitsPerSample: u16,
    pub cbSize: u16,
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KSSTATE {
    KSSTATE_STOP,
    KSSTATE_ACQUIRE,
    KSSTATE_PAUSE,
    KSSTATE_RUN,
}
pub type PKSSTATE = *mut KSSTATE;

// ---- 音频专用 ----
#[repr(C)]
pub struct KSAUDIO_POSITION {
    pub PlayOffset: u64,
    pub WriteOffset: u64,
}

#[repr(C)]
pub struct KSRTAUDIO_HWLATENCY {
    pub FifoSize: u32,
    pub ChipsetDelay: u32,
    pub CodecDelay: u32,
}

#[repr(C)]
pub struct KSRTAUDIO_HWREGISTER {
    pub Register: *mut c_void,
    pub Width: ULONG,
    pub Numerator: u64,
    pub Denominator: u64,
    pub Accuracy: ULONG,
}

pub type MEMORY_CACHING_TYPE = u32;

pub type PPCFILTER_DESCRIPTOR = *mut *mut PCFILTER_DESCRIPTOR;

#[repr(C)]
pub struct PCFILTER_DESCRIPTOR {
    pub Version: ULONG,
    pub AutomationTable: *mut c_void,
    pub PinSize: ULONG,
    pub PinCount: ULONG,
    pub Pins: *mut PCPIN_DESCRIPTOR,
    pub NodeSize: ULONG,
    pub NodeCount: ULONG,
    pub Nodes: *mut PCNODE_DESCRIPTOR,
    pub ConnectionSize: ULONG,
    pub ConnectionCount: ULONG,
    pub Connections: *mut PCCONNECTION_DESCRIPTOR,
    pub Category: GUID,
    pub Name: GUID,
    pub ComponentId: GUID,
    pub Topology: GUID,
    pub CapsFlags: ULONG,
    pub DeviceInterfaceGuid: GUID,
}

#[repr(C)]
pub struct PCPIN_DESCRIPTOR {
    pub MaxInstances: ULONG,
    pub Interrupts: ULONG,
    pub AutomationTable: *mut c_void,
    pub KsPinDescriptor: *mut KSPIN_DESCRIPTOR,
}

#[repr(C)]
pub struct KSPIN_DESCRIPTOR {
    pub InterfacesCount: ULONG,
    pub Interfaces: *mut KSPIN_INTERFACE,
    pub MediumsCount: ULONG,
    pub Mediums: *mut KSPIN_MEDIUM,
    pub DataRangesCount: ULONG,
    pub DataRanges: *mut *mut KSDATARANGE,
    pub Category: GUID,
    pub Name: GUID,
    pub Communication: u32,
}

#[repr(C)]
pub struct KSPIN_INTERFACE {
    pub Set: GUID,
    pub Id: ULONG,
    pub Flags: ULONG,
}

#[repr(C)]
pub struct KSPIN_MEDIUM {
    pub Set: GUID,
    pub Id: ULONG,
    pub Flags: ULONG,
}

#[repr(C)]
pub struct PCNODE_DESCRIPTOR {
    pub AutomationTable: *mut c_void,
    pub Type: GUID,
    pub Name: GUID,
}

#[repr(C)]
pub struct PCCONNECTION_DESCRIPTOR {
    pub FromNode: ULONG,
    pub FromPin: ULONG,
    pub ToNode: ULONG,
    pub ToPin: ULONG,
}

#[repr(C)]
pub struct DEVICE_DESCRIPTION {
    pub Version: ULONG,
    pub Master: BOOLEAN,
    pub ScatterGather: BOOLEAN,
    pub DemandMode: BOOLEAN,
    pub AutoInitialize: BOOLEAN,
    pub Paging: BOOLEAN,
    pub Dma32BitAddresses: BOOLEAN,
    pub IgnoreCount: BOOLEAN,
    pub Reserved1: BOOLEAN,
    pub Dma64BitAddresses: BOOLEAN,
    pub BusNumber: ULONG,
    pub DmaWidth: ULONG,
    pub DmaTransferWidth: ULONG,
    pub MaximumLength: ULONG,
    pub DmaPort: ULONG,
}

// 静态描述符含裸指针，Rust 2024 要求 Sync；这些仅作静态只读数据
unsafe impl Sync for PCFILTER_DESCRIPTOR {}
unsafe impl Sync for PCPIN_DESCRIPTOR {}
unsafe impl Sync for KSDATARANGE {}
unsafe impl Sync for KSDATAFORMAT {}
unsafe impl Sync for KSPIN_DESCRIPTOR {}
unsafe impl Sync for PCNODE_DESCRIPTOR {}
unsafe impl Sync for PCCONNECTION_DESCRIPTOR {}
unsafe impl Sync for KSPIN_MEDIUM {}
unsafe impl Sync for KSPIN_INTERFACE {}

// ---- KS 音频数据范围与常量 ----
#[repr(C)]
pub struct KSDATARANGE_AUDIO {
    pub DataRange: KSDATARANGE,
    pub MaximumChannels: u32,
    pub MinimumBitsPerSample: u32,
    pub MaximumBitsPerSample: u32,
    pub MinimumSampleFrequency: u32,
    pub MaximumSampleFrequency: u32,
}

// KSPIN_COMMUNICATION
pub const KSPIN_COMMUNICATION_NONE: u32 = 0;
pub const KSPIN_COMMUNICATION_SINK: u32 = 1;
pub const KSPIN_COMMUNICATION_SOURCE: u32 = 2;
pub const KSPIN_COMMUNICATION_BOTH: u32 = 3;
pub const KSPIN_COMMUNICATION_BRIDGE: u32 = 4;

// KSINTERFACE_STANDARD
pub const KSINTERFACE_STANDARD_STREAMING: u32 = 0;
// KSMEDIUM 任意实例
pub const KSMEDIUM_TYPE_ANYINSTANCE: u32 = 0;
