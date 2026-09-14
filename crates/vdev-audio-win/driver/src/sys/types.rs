//! 最小 WDM 基础类型（手写，PortCls 驱动所需子集；ABI 稳定）
//!
//! 结构布局逐字段对照 WDK 头文件（ks.h / ksmedia.h / portcls.h / ntstatus.h，
//! 10.0.10240.0）推演；文末 `#[cfg(test)]` 提供 x64 布局断言兜底（宿主可跑）。

#![allow(non_camel_case_types, non_snake_case)]
// NTSTATUS 常量按 WDK 官方 hex 值直书后转 i32 位型（高位置 1 即"负"），回绕即本意
#![allow(clippy::cast_possible_wrap)]

pub use core::ffi::c_void;
pub type NTSTATUS = i32;

// ---- NTSTATUS / HRESULT 常量（B5：值逐条对照 WDK ntstatus.h / objbase.h，hex 直书，
// 不再用十进制换算——0xC0000001 的补码十进制极易抄错一位） ----
pub const STATUS_SUCCESS: NTSTATUS = 0x0000_0000;
pub const STATUS_UNSUCCESSFUL: NTSTATUS = 0xC000_0001u32 as i32;
pub const STATUS_INSUFFICIENT_RESOURCES: NTSTATUS = 0xC000_009Au32 as i32;
pub const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as i32;
pub const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023u32 as i32;
pub const STATUS_INVALID_DEVICE_REQUEST: NTSTATUS = 0xC000_0010u32 as i32;
/// warning 级（0x8000…），非 error 级
pub const STATUS_DEVICE_BUSY: NTSTATUS = 0x8000_0011u32 as i32;
pub const STATUS_NOT_SUPPORTED: NTSTATUS = 0xC000_00BBu32 as i32;
pub const STATUS_NO_MATCH: NTSTATUS = 0xC000_0272u32 as i32;
/// COM HRESULT（不是 NTSTATUS）：QueryInterface 失败必须返回它而非 STATUS_*
pub const E_NOINTERFACE: i32 = 0x8000_4002u32 as i32;

// 编译期钉死：错误/警告级 NTSTATUS 置位最高两位，转 i32 后必为负——
// 驱动各处 `st < 0` 的失败判据依赖这一不变式
const _: () = {
    assert!(STATUS_DEVICE_BUSY < 0);
    assert!(STATUS_NOT_SUPPORTED < 0);
    assert!(STATUS_INSUFFICIENT_RESOURCES < 0);
    assert!(STATUS_SUCCESS == 0);
};

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

/// 驱动对象。
///
/// 注意（透传约定）：本结构**仅供透传**——驱动只把它原样交给
/// `PcInitializeAdapterDriver`/`PcAddAdapterDevice`，字段并非完整 WDK 布局，
/// 任何代码都不得解引用读写其字段。
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

/// KSDATAFORMAT_WAVEFORMATEX（ksmedia.h）：WAVEFORMATEX **按值内嵌**（B6 补修，
/// 旧实现写成指针会把布局差出一个 8 字节指针位，PortCls 解析格式必错位）。
#[repr(C)]
pub struct KSDATAFORMAT_WAVEFORMATEX {
    pub DataFormat: KSDATAFORMAT,
    pub WaveFormatEx: WAVEFORMATEX,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct WAVEFORMATEX {
    pub wFormatTag: u16,
    pub nChannels: u16,
    pub nSamplesPerSec: u32,
    pub nAvgBytesPerSec: u32,
    pub nBlockAlign: u16,
    pub wBitsPerSample: u16,
    pub cbSize: u16,
}

/// KSSTATE（ks.h）：MSVC C 枚举 4 字节；`repr(u32)` 锁定 ABI（跨 FFI 按值传参必须）。
/// 另提供 u32 常量供 AtomicU32 存储比较。
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum KSSTATE {
    KSSTATE_STOP = 0,
    KSSTATE_ACQUIRE = 1,
    KSSTATE_PAUSE = 2,
    KSSTATE_RUN = 3,
}
pub type PKSSTATE = *mut KSSTATE;

pub const KSSTATE_STOP_U32: u32 = KSSTATE::KSSTATE_STOP as u32;
pub const KSSTATE_ACQUIRE_U32: u32 = KSSTATE::KSSTATE_ACQUIRE as u32;
pub const KSSTATE_PAUSE_U32: u32 = KSSTATE::KSSTATE_PAUSE as u32;
pub const KSSTATE_RUN_U32: u32 = KSSTATE::KSSTATE_RUN as u32;

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

// ---- PortCls 描述符（B6：布局/字段逐项对照 portcls.h 与 ks.h） ----

/// GetDescription 出参类型：指向"描述符指针槽"（C: `PPCFILTER_DESCRIPTOR *`）
pub type PPCFILTER_DESCRIPTOR = *mut PCFILTER_DESCRIPTOR;

/// 自动化表：PortCls 内部布局，驱动只传空表指针，故保持不透明
#[repr(C)]
pub struct PCAUTOMATION_TABLE {
    _private: [u8; 0],
}
pub type PPCAUTOMATION_TABLE = *mut PCAUTOMATION_TABLE;

/// KSPIN_DESCRIPTOR 尾部匿名联合（ks.h）：
/// `union { LONGLONG Reserved; struct { ULONG ConstrainedDataRangesCount;
/// PKSDATARANGE* ConstrainedDataRanges; }; }`
///
/// 第二变体把联合撑到 16 字节——PortCls 校验 `PinSize >= sizeof(PCPIN_DESCRIPTOR)`
/// （已含该联合），省略它描述符整体短 16 字节会被 GetDescription 拒收。
#[repr(C)]
#[derive(Clone, Copy)]
pub union KSPIN_DESCRIPTOR_TAIL {
    pub Reserved: i64,
    pub Constrained: KSPIN_CONSTRAINED_DATA_RANGES,
}

/// 联合第二变体（仅用于撑起 16 字节 ABI 尺寸）
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KSPIN_CONSTRAINED_DATA_RANGES {
    pub ConstrainedDataRangesCount: ULONG,
    pub ConstrainedDataRanges: *mut *mut KSDATARANGE,
}

/// KSPIN_DATAFLOW（ks.h 枚举，MSVC C 枚举 4 字节，u32 等价表示）
pub type KSPIN_DATAFLOW = u32;
pub const KSPIN_DATAFLOW_IN: KSPIN_DATAFLOW = 1;
pub const KSPIN_DATAFLOW_OUT: KSPIN_DATAFLOW = 2;

/// KSPIN_COMMUNICATION（ks.h 枚举，u32 等价表示；常量见文末）
pub type KSPIN_COMMUNICATION = u32;

/// KSPIN_INTERFACE（ks.h：GUID + Id + Flags，x64 24 字节）
#[repr(C)]
pub struct KSPIN_INTERFACE {
    pub Set: GUID,
    pub Id: ULONG,
    pub Flags: ULONG,
}

/// KSPIN_MEDIUM（ks.h：布局与 KSPIN_INTERFACE 相同）
#[repr(C)]
pub struct KSPIN_MEDIUM {
    pub Set: GUID,
    pub Id: ULONG,
    pub Flags: ULONG,
}

/// KSPIN_DESCRIPTOR（ks.h `_NTDDK_` 变体；x64 布局 88 字节）。
/// 字段顺序固定：Counts/指针成对在前，DataFlow/Communication/Category/Name 在后，
/// 尾部联合收尾——与 ks.h 逐字段一致，不得重排。
#[repr(C)]
pub struct KSPIN_DESCRIPTOR {
    pub InterfacesCount: ULONG,
    pub Interfaces: *const KSPIN_INTERFACE,
    pub MediumsCount: ULONG,
    pub Mediums: *const KSPIN_MEDIUM,
    pub DataRangesCount: ULONG,
    pub DataRanges: *const *const KSDATARANGE,
    pub DataFlow: KSPIN_DATAFLOW,
    pub Communication: KSPIN_COMMUNICATION,
    pub Category: *const GUID,
    pub Name: *const GUID,
    pub Reserved: KSPIN_DESCRIPTOR_TAIL,
}

/// PCPIN_DESCRIPTOR（portcls.h；x64 布局 112 字节）。
/// 三个实例计数（MaxGlobal/MaxFilter/MinFilter）+ 自动化表 + **按值内嵌**的
/// KSPIN_DESCRIPTOR（旧实现 KsPinDescriptor 写成指针，层次差一级，PortCls 必崩）。
#[repr(C)]
pub struct PCPIN_DESCRIPTOR {
    pub MaxGlobalInstanceCount: ULONG,
    pub MaxFilterInstanceCount: ULONG,
    pub MinFilterInstanceCount: ULONG,
    pub AutomationTable: *const PCAUTOMATION_TABLE,
    pub KsPinDescriptor: KSPIN_DESCRIPTOR,
}

/// PCFILTER_DESCRIPTOR（portcls.h；x64 布局 80 字节）。
/// 头文件里**没有** ConnectionSize，也没有尾部 Category/Name/ComponentId 等
/// GUID 字段——Category 是 `const GUID*` 数组指针（Categories/CategoryCount）。
#[repr(C)]
pub struct PCFILTER_DESCRIPTOR {
    pub Version: ULONG,
    pub AutomationTable: *const PCAUTOMATION_TABLE,
    pub PinSize: ULONG,
    pub PinCount: ULONG,
    pub Pins: *const PCPIN_DESCRIPTOR,
    pub NodeSize: ULONG,
    pub NodeCount: ULONG,
    pub Nodes: *const PCNODE_DESCRIPTOR,
    pub ConnectionCount: ULONG,
    pub Connections: *const PCCONNECTION_DESCRIPTOR,
    pub CategoryCount: ULONG,
    pub Categories: *const GUID,
}

/// PCNODE_DESCRIPTOR（portcls.h）：Flags 在首，Type/Name 为 `const GUID*` 指针
#[repr(C)]
pub struct PCNODE_DESCRIPTOR {
    pub Flags: ULONG,
    pub AutomationTable: *const PCAUTOMATION_TABLE,
    pub Type: *const GUID,
    pub Name: *const GUID,
}

/// PCCONNECTION_DESCRIPTOR（portcls.h = KSTOPOLOGY_CONNECTION，ks.h）：
/// 字段名以头文件为准（FromNodePin/ToNodePin，非 FromPin/ToPin）
#[repr(C)]
pub struct PCCONNECTION_DESCRIPTOR {
    pub FromNode: ULONG,
    pub FromNodePin: ULONG,
    pub ToNode: ULONG,
    pub ToNodePin: ULONG,
}

// ---- DMA 描述（wdm.h；权威对照本机拷贝 /tmp/wdm.h，mingw-w64 master
// ddk/include/ddk/wdm.h 第 5759-5801 行，单一来源）----

/// DEVICE_DESCRIPTION.Version（wdm.h 宏）
pub const DEVICE_DESCRIPTION_VERSION: u32 = 0x0000;
pub const DEVICE_DESCRIPTION_VERSION1: u32 = 0x0001;
pub const DEVICE_DESCRIPTION_VERSION2: u32 = 0x0002;

/// DMA_WIDTH（wdm.h 第 5759-5766 行；C 枚举按 int 布局，repr(u32) 锁宽）
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DMA_WIDTH {
    Width8Bits = 0,
    Width16Bits = 1,
    Width32Bits = 2,
    Width64Bits = 3,
    WidthNoWrap = 4,
    MaximumDmaWidth = 5,
}

/// DMA_SPEED（wdm.h 第 5768-5777 行）
#[repr(u32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DMA_SPEED {
    Compatible = 0,
    TypeA = 1,
    TypeB = 2,
    TypeC = 3,
    TypeF = 4,
    MaximumDmaSpeed = 5,
}

/// INTERFACE_TYPE（wdm.h 第 3060-3079 行；InterfaceTypeUndefined = -1，
/// MSVC C 枚举按 int 布局，故 repr(i32)；Vmcs/ACPIBus 为该头文件新增成员）
#[repr(i32)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum INTERFACE_TYPE {
    InterfaceTypeUndefined = -1,
    Internal = 0,
    Isa = 1,
    Eisa = 2,
    MicroChannel = 3,
    TurboChannel = 4,
    PCIBus = 5,
    VMEBus = 6,
    NuBus = 7,
    PCMCIABus = 8,
    CBus = 9,
    MPIBus = 10,
    MPSABus = 11,
    ProcessorInternal = 12,
    InternalPowerBus = 13,
    PNPISABus = 14,
    PNPBus = 15,
    Vmcs = 16,
    ACPIBus = 17,
    MaximumInterfaceType = 18,
}

/// DEVICE_DESCRIPTION（wdm.h 第 5783-5801 行，逐字段对照 /tmp/wdm.h）
/// x64 布局：size 40、align 4，见文末布局断言
#[repr(C)]
pub struct DEVICE_DESCRIPTION {
    pub Version: ULONG,
    pub Master: BOOLEAN,
    pub ScatterGather: BOOLEAN,
    pub DemandMode: BOOLEAN,
    pub AutoInitialize: BOOLEAN,
    pub Dma32BitAddresses: BOOLEAN,
    pub IgnoreCount: BOOLEAN,
    pub Reserved1: BOOLEAN,
    pub Dma64BitAddresses: BOOLEAN,
    pub BusNumber: ULONG,
    pub DmaChannel: ULONG,
    pub InterfaceType: INTERFACE_TYPE,
    pub DmaWidth: DMA_WIDTH,
    pub DmaSpeed: DMA_SPEED,
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

// KSPIN_COMMUNICATION（ks.h）
pub const KSPIN_COMMUNICATION_NONE: KSPIN_COMMUNICATION = 0;
pub const KSPIN_COMMUNICATION_SINK: KSPIN_COMMUNICATION = 1;
pub const KSPIN_COMMUNICATION_SOURCE: KSPIN_COMMUNICATION = 2;
pub const KSPIN_COMMUNICATION_BOTH: KSPIN_COMMUNICATION = 3;
pub const KSPIN_COMMUNICATION_BRIDGE: KSPIN_COMMUNICATION = 4;

// KSINTERFACE_STANDARD
pub const KSINTERFACE_STANDARD_STREAMING: u32 = 0;
/// ks.h `KSINTERFACE_STANDARD_LOOPED_STREAMING` = 1：WaveRT 播放/录音 pin 必须暴露的
/// 接口（环回缓冲流）。sysvad/VDA 的 wave pin 不显式声明 Interfaces（PortCls 默认给这套），
/// 本机可用的 ToDesk 虚拟声卡实测 `KSPROPERTY_PIN_INTERFACES` 返回的也是 id=1。
pub const KSINTERFACE_STANDARD_LOOPED_STREAMING: u32 = 1;
// KSMEDIUM 任意实例
pub const KSMEDIUM_TYPE_ANYINSTANCE: u32 = 0;

#[cfg(test)]
mod tests {
    use core::mem::{offset_of, size_of};

    use super::*;

    /// B5 回归：NTSTATUS 常量必须等于 WDK ntstatus.h 的官方值
    #[test]
    fn ntstatus_values_match_ntstatus_h() {
        assert_eq!(STATUS_SUCCESS, 0);
        assert_eq!(STATUS_UNSUCCESSFUL, 0xC000_0001u32 as i32);
        assert_eq!(STATUS_INSUFFICIENT_RESOURCES, 0xC000_009Au32 as i32);
        assert_eq!(STATUS_INVALID_PARAMETER, 0xC000_000Du32 as i32);
        assert_eq!(STATUS_BUFFER_TOO_SMALL, 0xC000_0023u32 as i32);
        assert_eq!(STATUS_INVALID_DEVICE_REQUEST, 0xC000_0010u32 as i32);
        assert_eq!(STATUS_DEVICE_BUSY, 0x8000_0011u32 as i32);
        assert_eq!(STATUS_NOT_SUPPORTED, 0xC000_00BBu32 as i32);
        assert_eq!(STATUS_NO_MATCH, 0xC000_0272u32 as i32);
        assert_eq!(E_NOINTERFACE, 0x8000_4002u32 as i32);
    }

    /// 基础宽度（x64 ABI；宿主为 64 位 macOS，布局断言可宿主运行）
    #[test]
    fn fundamental_widths_x64() {
        assert_eq!(size_of::<ULONG>(), 4);
        assert_eq!(size_of::<BOOLEAN>(), 1);
        assert_eq!(size_of::<*const c_void>(), 8);
        assert_eq!(size_of::<GUID>(), 16);
        assert_eq!(size_of::<KSSTATE>(), 4); // repr(u32) 锁定 C 枚举宽度
        assert_eq!(size_of::<KSPIN_INTERFACE>(), 24);
        assert_eq!(size_of::<KSPIN_MEDIUM>(), 24);
        assert_eq!(size_of::<KSDATAFORMAT>(), 64);
        // WAVEFORMATEX = 18 字节按 4 对齐 → 20；KSDATAFORMAT_WAVEFORMATEX = 64 + 20
        assert_eq!(size_of::<WAVEFORMATEX>(), 20);
        assert_eq!(size_of::<KSDATAFORMAT_WAVEFORMATEX>(), 84);
    }

    /// 回归：DEVICE_DESCRIPTION 必须与 wdm.h `_DEVICE_DESCRIPTION` 逐字段一致
    ///（权威对照本机拷贝 /tmp/wdm.h 第 5783-5801 行；此前多出 Paging/
    /// DmaTransferWidth、缺 DmaChannel/InterfaceType/DmaSpeed，导致字段
    /// 错位）。x64：size 40、align 4；指针 8、ULONG 4、枚举 4。
    #[test]
    fn device_description_layout_x64() {
        assert_eq!(size_of::<ULONG>(), 4);
        assert_eq!(size_of::<INTERFACE_TYPE>(), 4); // repr(i32) 锁定 C 枚举宽度
        assert_eq!(size_of::<DMA_WIDTH>(), 4); // repr(u32)
        assert_eq!(size_of::<DMA_SPEED>(), 4); // repr(u32)
        assert_eq!(size_of::<DEVICE_DESCRIPTION>(), 40);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, Version), 0);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, Master), 4);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, ScatterGather), 5);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, DemandMode), 6);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, AutoInitialize), 7);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, Dma32BitAddresses), 8);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, IgnoreCount), 9);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, Reserved1), 10);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, Dma64BitAddresses), 11);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, BusNumber), 12);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, DmaChannel), 16);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, InterfaceType), 20);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, DmaWidth), 24);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, DmaSpeed), 28);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, MaximumLength), 32);
        assert_eq!(offset_of!(DEVICE_DESCRIPTION, DmaPort), 36);
        // 枚举权威数值抽查（wdm.h）
        assert_eq!(INTERFACE_TYPE::InterfaceTypeUndefined as i32, -1);
        assert_eq!(INTERFACE_TYPE::PCIBus as i32, 5);
        assert_eq!(DMA_WIDTH::Width32Bits as u32, 2);
        assert_eq!(DMA_SPEED::Compatible as u32, 0);
    }

    /// B6 回归：KSPIN_DESCRIPTOR 必须与 ks.h `_NTDDK_` 布局逐字段一致（x64 88 字节）
    #[test]
    fn ks_pin_descriptor_layout_x64() {
        assert_eq!(size_of::<KSPIN_DESCRIPTOR_TAIL>(), 16); // 联合被第二变体撑到 16
        assert_eq!(size_of::<KSPIN_DESCRIPTOR>(), 88);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, InterfacesCount), 0);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Interfaces), 8);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, MediumsCount), 16);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Mediums), 24);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, DataRangesCount), 32);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, DataRanges), 40);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, DataFlow), 48);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Communication), 52);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Category), 56);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Name), 64);
        assert_eq!(offset_of!(KSPIN_DESCRIPTOR, Reserved), 72);
    }

    /// B6 回归：PCPIN_DESCRIPTOR（portcls.h，x64 112 字节，KsPinDescriptor 按值内嵌）
    #[test]
    fn pc_pin_descriptor_layout_x64() {
        assert_eq!(size_of::<PCPIN_DESCRIPTOR>(), 112);
        assert_eq!(offset_of!(PCPIN_DESCRIPTOR, MaxGlobalInstanceCount), 0);
        assert_eq!(offset_of!(PCPIN_DESCRIPTOR, MaxFilterInstanceCount), 4);
        assert_eq!(offset_of!(PCPIN_DESCRIPTOR, MinFilterInstanceCount), 8);
        assert_eq!(offset_of!(PCPIN_DESCRIPTOR, AutomationTable), 16);
        assert_eq!(offset_of!(PCPIN_DESCRIPTOR, KsPinDescriptor), 24);
    }

    /// B6 回归：PCFILTER_DESCRIPTOR（portcls.h，x64 80 字节，无 ConnectionSize/尾部 GUID）
    #[test]
    fn pc_filter_descriptor_layout_x64() {
        assert_eq!(size_of::<PCFILTER_DESCRIPTOR>(), 80);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, Version), 0);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, AutomationTable), 8);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, PinSize), 16);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, PinCount), 20);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, Pins), 24);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, NodeSize), 32);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, NodeCount), 36);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, Nodes), 40);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, ConnectionCount), 48);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, Connections), 56);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, CategoryCount), 64);
        assert_eq!(offset_of!(PCFILTER_DESCRIPTOR, Categories), 72);
    }

    /// B6 回归：PCNODE_DESCRIPTOR（Flags 在首）与 PCCONNECTION_DESCRIPTOR（4×ULONG）
    #[test]
    fn pc_node_and_connection_layout_x64() {
        assert_eq!(size_of::<PCNODE_DESCRIPTOR>(), 32);
        assert_eq!(offset_of!(PCNODE_DESCRIPTOR, Flags), 0);
        assert_eq!(offset_of!(PCNODE_DESCRIPTOR, AutomationTable), 8);
        assert_eq!(offset_of!(PCNODE_DESCRIPTOR, Type), 16);
        assert_eq!(offset_of!(PCNODE_DESCRIPTOR, Name), 24);
        assert_eq!(size_of::<PCCONNECTION_DESCRIPTOR>(), 16);
        assert_eq!(offset_of!(PCCONNECTION_DESCRIPTOR, FromNodePin), 4);
        assert_eq!(offset_of!(PCCONNECTION_DESCRIPTOR, ToNodePin), 12);
    }
}
