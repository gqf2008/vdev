//! Topology 小端口（IMiniportTopology）——Windows 音频端点（播放/录音设备）建成所必需。
//!
//! 现状只有 Wave 子设备时控制面板不出设备；PortCls 适配器必须为每个端点注册
//! 一个 topology filter（`IPortTopology` + `IMiniportTopology`），端点拓扑把
//! Wave 桥接 pin 连到音量/静音终端节点，音频栈才能枚举出端点。
//!
//! 本文件逐字段对照（不凭记忆）：
//! - vtable 槽序：WDK `/tmp/wdk10/portcls.h:1776-1791`（IUnknown 3 槽 +
//!   `IMiniport` 的 GetDescription/DataRangeIntersection 2 槽 + `Init`，共 6 槽；
//!   `DEFINE_ABSTRACT_MINIPORT` 槽序见 portcls.h:326-340）
//! - 描述符表结构：sysvad `speakertoptable.h`（SPEAKER 播放风格）、
//!   `micarray1toptable.h`（MIC 采集风格）；连接表条目数值照抄 sysvad
//! - GUID 值：`/tmp/wdk10/ksmedia.h`、`/tmp/wdk10/ks.h`（各常量旁注明行号）
//! - PCPROPERTY_ITEM / PCAUTOMATION_TABLE / PCPROPERTY_REQUEST /
//!   KSPROPERTY_DESCRIPTION 布局：portcls.h:1231-1315、1361-1382，ks.h:231-237
//!
//! `sys/types.rs` 的 `PCAUTOMATION_TABLE` 是不透明占位（驱动此前只传空表），
//! 本模块需要真实静态自动化表，故在本地定义真实布局并用 #[test] 断言 x64 偏移。
#![allow(non_snake_case, non_camel_case_types)]
// NTSTATUS 常量 0xC000_xxxxu32 as i32 的惯用写法（与 sys/types.rs 一致）
#![allow(clippy::cast_possible_wrap)]

#[cfg(test)]
use core::mem::offset_of;
use core::mem::size_of;
use core::sync::atomic::{AtomicI32, Ordering};

use crate::com::{interlocked_decrement, interlocked_increment};
use crate::sys::portcls::{IID_IMiniport, IID_IUnknown, KSCATEGORY_AUDIO};
use crate::sys::types::{
    E_NOINTERFACE, GUID, KSDATARANGE, KSPIN_COMMUNICATION_NONE, KSPIN_DATAFLOW_IN,
    KSPIN_DATAFLOW_OUT, KSPIN_DESCRIPTOR, KSPIN_DESCRIPTOR_TAIL, NTSTATUS, PCCONNECTION_DESCRIPTOR,
    PCFILTER_DESCRIPTOR, PCNODE_DESCRIPTOR, PCPIN_DESCRIPTOR, PPCFILTER_DESCRIPTOR, PVOID,
    STATUS_SUCCESS, ULONG, c_void, is_equal_guid,
};

pub const TAG: u32 = u32::from_le_bytes(*b"vdev");

// ---- 本地 NTSTATUS（types.rs 未有的；值取自 WDK ntstatus.h） ----
/// ntstatus.h:1927
const STATUS_NOT_IMPLEMENTED: NTSTATUS = 0xC000_0002u32 as i32;
/// ntstatus.h:2027
const STATUS_INVALID_PARAMETER: NTSTATUS = 0xC000_000Du32 as i32;
/// ntstatus.h:2244
const STATUS_BUFFER_TOO_SMALL: NTSTATUS = 0xC000_0023u32 as i32;

/// portcls.h:1509 `PCFILTER_NODE` = ks.h:922 `KSFILTER_NODE` = `(ULONG)-1`
const PCFILTER_NODE: ULONG = u32::MAX;

// ---- KS 属性动词（ks.h:135-138） ----
const KSPROPERTY_TYPE_GET: ULONG = 0x0000_0001;
const KSPROPERTY_TYPE_SET: ULONG = 0x0000_0002;
const KSPROPERTY_TYPE_BASICSUPPORT: ULONG = 0x0000_0200;

/// ksmedia.h:1571-1587 枚举：LATENCY=1, COPY_PROTECTION=2, CHANNEL_CONFIG=3,
/// VOLUMELEVEL=4, …, MUX_SOURCE=12, MUTE=13
const KSPROPERTY_AUDIO_VOLUMELEVEL: ULONG = 4;
const KSPROPERTY_AUDIO_MUTE: ULONG = 13;

/// ks.h:179 `VT_I4 = 3`（basicsupport PropTypeSet.Id）
const VT_I4: ULONG = 3;

// ============ GUID（值逐一从 WDK 头文件抄录，行号见各注释） ============

/// IID_IMiniportTopology（portcls.h:108-109）
pub const IID_IMiniportTopology: GUID = GUID {
    data1: 0xb4c9_0a31,
    data2: 0x5791,
    data3: 0x11d0,
    data4: [0x86, 0xf9, 0x00, 0xa0, 0xc9, 0x11, 0xb5, 0x44],
};

/// KSDATAFORMAT_TYPE_AUDIO（ksmedia.h:717-719）
static KSDATAFORMAT_TYPE_AUDIO: GUID = GUID {
    data1: 0x7364_7561,
    data2: 0x0000,
    data3: 0x0010,
    data4: [0x80, 0x00, 0x00, 0xaa, 0x00, 0x38, 0x9b, 0x71],
};
/// KSDATAFORMAT_SUBTYPE_ANALOG（ksmedia.h:829-831）
static KSDATAFORMAT_SUBTYPE_ANALOG: GUID = GUID {
    data1: 0x6dba_3190,
    data2: 0x67bd,
    data3: 0x11cf,
    data4: [0xa0, 0xf7, 0x00, 0x20, 0xaf, 0xd1, 0x56, 0xe4],
};
/// KSDATAFORMAT_SPECIFIER_NONE（ks.h:1475-1477）
static KSDATAFORMAT_SPECIFIER_NONE: GUID = GUID {
    data1: 0x0f64_17d6,
    data2: 0xc318,
    data3: 0x11d0,
    data4: [0xa4, 0x3f, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
/// KSNODETYPE_VOLUME（ksmedia.h:1944-1946）
pub static KSNODETYPE_VOLUME: GUID = GUID {
    data1: 0x3a5a_cc00,
    data2: 0xc557,
    data3: 0x11d0,
    data4: [0x8a, 0x2b, 0x00, 0xa0, 0xc9, 0x25, 0x5a, 0xc1],
};
/// KSNODETYPE_MUTE（ksmedia.h:1939-1941）
pub static KSNODETYPE_MUTE: GUID = GUID {
    data1: 0x02b2_23c0,
    data2: 0xc557,
    data3: 0x11d0,
    data4: [0x8a, 0x2b, 0x00, 0xa0, 0xc9, 0x25, 0x5a, 0xc1],
};
/// KSNODETYPE_SPEAKER（ksmedia.h:235-237）
pub static KSNODETYPE_SPEAKER: GUID = GUID {
    data1: 0xdff2_1ce1,
    data2: 0xf70f,
    data3: 0x11d0,
    data4: [0xb9, 0x17, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
/// KSNODETYPE_MICROPHONE（ksmedia.h:191-193）
pub static KSNODETYPE_MICROPHONE: GUID = GUID {
    data1: 0xdff2_1be1,
    data2: 0xf70f,
    data3: 0x11d0,
    data4: [0xb9, 0x17, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
/// KSAUDFNAME_MASTER_VOLUME（ksmedia.h:2104-2106）
pub static KSAUDFNAME_MASTER_VOLUME: GUID = GUID {
    data1: 0x185f_ede3,
    data2: 0x9905,
    data3: 0x11d1,
    data4: [0x95, 0xa9, 0x00, 0xc0, 0x4f, 0xb9, 0x25, 0xd3],
};
/// KSAUDFNAME_MASTER_MUTE（ksmedia.h:2109-2111）
pub static KSAUDFNAME_MASTER_MUTE: GUID = GUID {
    data1: 0x185f_ede4,
    data2: 0x9905,
    data3: 0x11d1,
    data4: [0x95, 0xa9, 0x00, 0xc0, 0x4f, 0xb9, 0x25, 0xd3],
};
/// KSAUDFNAME_MIC_VOLUME（ksmedia.h:2154-2156）
pub static KSAUDFNAME_MIC_VOLUME: GUID = GUID {
    data1: 0x185f_eded,
    data2: 0x9905,
    data3: 0x11d1,
    data4: [0x95, 0xa9, 0x00, 0xc0, 0x4f, 0xb9, 0x25, 0xd3],
};
/// KSAUDFNAME_MIC_MUTE（ksmedia.h:2159-2161）
pub static KSAUDFNAME_MIC_MUTE: GUID = GUID {
    data1: 0x185f_edee,
    data2: 0x9905,
    data3: 0x11d1,
    data4: [0x95, 0xa9, 0x00, 0xc0, 0x4f, 0xb9, 0x25, 0xd3],
};
/// KSPROPSETID_Audio（ksmedia.h:1567-1569）
static KSPROPSETID_AUDIO: GUID = GUID {
    data1: 0x45ff_aaa0,
    data2: 0x6e1b,
    data3: 0x11d0,
    data4: [0xbc, 0xf2, 0x44, 0x45, 0x53, 0x54, 0x00, 0x00],
};
/// KSPROPTYPESETID_General（ks.h:169-171）
static KSPROPTYPESETID_GENERAL: GUID = GUID {
    data1: 0x97e9_9ba0,
    data2: 0xbdea,
    data3: 0x11cf,
    data4: [0xa5, 0xd6, 0x28, 0xdb, 0x04, 0xc1, 0x00, 0x00],
};

// ============ 本地真实布局：PortCls 自动化表结构（portcls.h） ============

/// portcls.h:1231-1236（x64 24 字节：Set 0 / Id 8 / Flags 12 / Handler 16）
#[repr(C)]
pub struct PCPROPERTY_ITEM {
    pub Set: *const GUID,
    pub Id: ULONG,
    pub Flags: ULONG,
    pub Handler: unsafe extern "system" fn(*mut PCPROPERTY_REQUEST) -> NTSTATUS,
}
// SAFETY: 纯描述符数据（不可变），指针仅在 PortCls 侧被读取
unsafe impl Sync for PCPROPERTY_ITEM {}

/// portcls.h:1294-1315（x64 72 字节）
#[repr(C)]
pub struct PCPROPERTY_REQUEST {
    pub MajorTarget: PVOID,
    pub MinorTarget: PVOID,
    pub Node: ULONG,
    pub PropertyItem: *const PCPROPERTY_ITEM,
    pub Verb: ULONG,
    pub InstanceSize: ULONG,
    pub Instance: PVOID,
    pub ValueSize: ULONG,
    pub Value: PVOID,
    pub Irp: PVOID,
}

/// portcls.h:1361-1382（x64 56 字节；PropertyItemSize 须为 8 的倍数，
/// 见头文件 1372-1374 注释"Item sizes must be a multiple of 8"——24 满足）
#[repr(C)]
pub struct PCAUTOMATION_TABLE {
    pub PropertyItemSize: ULONG,
    pub PropertyCount: ULONG,
    pub Properties: *const PCPROPERTY_ITEM,
    pub MethodItemSize: ULONG,
    pub MethodCount: ULONG,
    pub Methods: *const c_void,
    pub EventItemSize: ULONG,
    pub EventCount: ULONG,
    pub Events: *const c_void,
    pub Reserved: ULONG,
}
// SAFETY: 纯描述符数据（不可变）
unsafe impl Sync for PCAUTOMATION_TABLE {}

/// ks.h:231-237（x64 40 字节；KSIDENTIFIER = GUID16+Id4+Flags4 内联展开）
#[repr(C)]
pub struct KSPROPERTY_DESCRIPTION {
    pub AccessFlags: ULONG,
    pub DescriptionSize: ULONG,
    pub PropTypeSetSet: GUID,
    pub PropTypeSetId: ULONG,
    pub PropTypeSetFlags: ULONG,
    pub MembersListCount: ULONG,
    pub Reserved: ULONG,
}

// ============ 自动化表（volume/mute 各一项；Handler 共用，按 Id 分派） ============

static TOPO_VOLUME_PROPERTY_ITEMS: [PCPROPERTY_ITEM; 1] = [PCPROPERTY_ITEM {
    Set: &KSPROPSETID_AUDIO,
    Id: KSPROPERTY_AUDIO_VOLUMELEVEL,
    Flags: KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT,
    Handler: topo_property_handler,
}];
static TOPO_MUTE_PROPERTY_ITEMS: [PCPROPERTY_ITEM; 1] = [PCPROPERTY_ITEM {
    Set: &KSPROPSETID_AUDIO,
    Id: KSPROPERTY_AUDIO_MUTE,
    Flags: KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT,
    Handler: topo_property_handler,
}];

static AUT_VOLUME: PCAUTOMATION_TABLE = PCAUTOMATION_TABLE {
    PropertyItemSize: size_of::<PCPROPERTY_ITEM>() as ULONG,
    PropertyCount: 1,
    Properties: &TOPO_VOLUME_PROPERTY_ITEMS[0],
    MethodItemSize: 0,
    MethodCount: 0,
    Methods: core::ptr::null(),
    EventItemSize: 0,
    EventCount: 0,
    Events: core::ptr::null(),
    Reserved: 0,
};
static AUT_MUTE: PCAUTOMATION_TABLE = PCAUTOMATION_TABLE {
    PropertyItemSize: size_of::<PCPROPERTY_ITEM>() as ULONG,
    PropertyCount: 1,
    Properties: &TOPO_MUTE_PROPERTY_ITEMS[0],
    MethodItemSize: 0,
    MethodCount: 0,
    Methods: core::ptr::null(),
    EventItemSize: 0,
    EventCount: 0,
    Events: core::ptr::null(),
    Reserved: 0,
};

// ============ 桥接 pin 数据范围（sysvad speakertoptable.h:19-37 同构） ============

static TOPO_PIN_DATARANGES: [KSDATARANGE; 1] = [KSDATARANGE {
    FormatSize: size_of::<KSDATARANGE>() as ULONG,
    Flags: 0,
    SampleSize: 0,
    Reserved: 0,
    MajorFormat: KSDATAFORMAT_TYPE_AUDIO,
    SubFormat: KSDATAFORMAT_SUBTYPE_ANALOG,
    Specifier: KSDATAFORMAT_SPECIFIER_NONE,
}];
/// 裸指针数组本身 !Sync，包一层只读描述符静态（同 miniport.rs SyncDataRanges 手法）
struct SyncDataRangePtrs([*const KSDATARANGE; 1]);
// SAFETY: 数组元素指向不可变静态 KSDATARANGE，仅被 PortCls 读取
unsafe impl Sync for SyncDataRangePtrs {}
static TOPO_PIN_DATARANGE_PTRS: SyncDataRangePtrs = SyncDataRangePtrs([&TOPO_PIN_DATARANGES[0]]);

// ============ 渲染（SPEAKER 风格，sysvad speakertoptable.h:41-86 同构） ============

/// pin0 = WAVEOUT_SOURCE（来自 Wave 滤波器，DataFlow IN，Category=KSCATEGORY_AUDIO）；
/// pin1 = LINEOUT_DEST（物理扬声器端点，DataFlow OUT，Category=KSNODETYPE_SPEAKER）
static RENDER_PINS: [PCPIN_DESCRIPTOR; 2] = [
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
            DataRanges: core::ptr::addr_of!(TOPO_PIN_DATARANGE_PTRS.0[0]),
            DataFlow: KSPIN_DATAFLOW_IN,
            Communication: KSPIN_COMMUNICATION_NONE,
            Category: &KSCATEGORY_AUDIO,
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
            DataRanges: core::ptr::addr_of!(TOPO_PIN_DATARANGE_PTRS.0[0]),
            DataFlow: KSPIN_DATAFLOW_OUT,
            Communication: KSPIN_COMMUNICATION_NONE,
            Category: &KSNODETYPE_SPEAKER,
            Name: core::ptr::null(),
            Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
        },
    },
];

/// node0 = VOLUME（KSNODETYPE_VOLUME，名 KSAUDFNAME_MASTER_VOLUME）；
/// node1 = MUTE（KSNODETYPE_MUTE，名 KSAUDFNAME_MASTER_MUTE）
static RENDER_NODES: [PCNODE_DESCRIPTOR; 2] = [
    PCNODE_DESCRIPTOR {
        Flags: 0,
        AutomationTable: &AUT_VOLUME as *const PCAUTOMATION_TABLE
            as *const crate::sys::types::PCAUTOMATION_TABLE,
        Type: &KSNODETYPE_VOLUME,
        Name: &KSAUDFNAME_MASTER_VOLUME,
    },
    PCNODE_DESCRIPTOR {
        Flags: 0,
        AutomationTable: &AUT_MUTE as *const PCAUTOMATION_TABLE
            as *const crate::sys::types::PCAUTOMATION_TABLE,
        Type: &KSNODETYPE_MUTE,
        Name: &KSAUDFNAME_MASTER_MUTE,
    },
];

/// sysvad speakertoptable.h:163-169 逐条照抄（pin0→volume→mute→pin1；
/// 节点 pin 约定：0=输出侧、1=输入侧，与 sysvad 表一致）
static RENDER_CONNECTIONS: [PCCONNECTION_DESCRIPTOR; 3] = [
    PCCONNECTION_DESCRIPTOR {
        FromNode: PCFILTER_NODE,
        FromNodePin: 0,
        ToNode: 0,
        ToNodePin: 1,
    },
    PCCONNECTION_DESCRIPTOR {
        FromNode: 0,
        FromNodePin: 0,
        ToNode: 1,
        ToNodePin: 1,
    },
    PCCONNECTION_DESCRIPTOR {
        FromNode: 1,
        FromNodePin: 0,
        ToNode: PCFILTER_NODE,
        ToNodePin: 1,
    },
];

pub static RENDER_FILTER_DESCRIPTOR: PCFILTER_DESCRIPTOR = PCFILTER_DESCRIPTOR {
    Version: 0,
    AutomationTable: core::ptr::null(), // 滤波器级无自动化（jack 描述最小可用省略）
    PinSize: size_of::<PCPIN_DESCRIPTOR>() as ULONG,
    PinCount: 2,
    Pins: &RENDER_PINS[0],
    NodeSize: size_of::<PCNODE_DESCRIPTOR>() as ULONG,
    NodeCount: 2,
    Nodes: &RENDER_NODES[0],
    ConnectionCount: 3,
    Connections: &RENDER_CONNECTIONS[0],
    CategoryCount: 0,
    Categories: core::ptr::null(),
};

// ============ 采集（MICIN 风格，sysvad micarray1toptable.h 同构） ============

/// pin0 = 麦克风物理端点（DataFlow IN，Category=KSNODETYPE_MICROPHONE）；
/// pin1 = 桥接 pin（去往 Wave 滤波器，DataFlow OUT，Category=KSCATEGORY_AUDIO）
static CAPTURE_PINS: [PCPIN_DESCRIPTOR; 2] = [
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
            DataRanges: core::ptr::addr_of!(TOPO_PIN_DATARANGE_PTRS.0[0]),
            DataFlow: KSPIN_DATAFLOW_IN,
            Communication: KSPIN_COMMUNICATION_NONE,
            Category: &KSNODETYPE_MICROPHONE,
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
            DataRanges: core::ptr::addr_of!(TOPO_PIN_DATARANGE_PTRS.0[0]),
            DataFlow: KSPIN_DATAFLOW_OUT,
            Communication: KSPIN_COMMUNICATION_NONE,
            Category: &KSCATEGORY_AUDIO,
            Name: core::ptr::null(),
            Reserved: KSPIN_DESCRIPTOR_TAIL { Reserved: 0 },
        },
    },
];

/// node0 = VOLUME（KSAUDFNAME_MIC_VOLUME）；node1 = MUTE（KSAUDFNAME_MIC_MUTE）
static CAPTURE_NODES: [PCNODE_DESCRIPTOR; 2] = [
    PCNODE_DESCRIPTOR {
        Flags: 0,
        AutomationTable: &AUT_VOLUME as *const PCAUTOMATION_TABLE
            as *const crate::sys::types::PCAUTOMATION_TABLE,
        Type: &KSNODETYPE_VOLUME,
        Name: &KSAUDFNAME_MIC_VOLUME,
    },
    PCNODE_DESCRIPTOR {
        Flags: 0,
        AutomationTable: &AUT_MUTE as *const PCAUTOMATION_TABLE
            as *const crate::sys::types::PCAUTOMATION_TABLE,
        Type: &KSNODETYPE_MUTE,
        Name: &KSAUDFNAME_MIC_MUTE,
    },
];

/// sysvad micarray1toptable.h:146-149 逐条照抄（pin0→volume→mute→pin1）
static CAPTURE_CONNECTIONS: [PCCONNECTION_DESCRIPTOR; 3] = [
    PCCONNECTION_DESCRIPTOR {
        FromNode: PCFILTER_NODE,
        FromNodePin: 0,
        ToNode: 0,
        ToNodePin: 1,
    },
    PCCONNECTION_DESCRIPTOR {
        FromNode: 0,
        FromNodePin: 0,
        ToNode: 1,
        ToNodePin: 1,
    },
    PCCONNECTION_DESCRIPTOR {
        FromNode: 1,
        FromNodePin: 0,
        ToNode: PCFILTER_NODE,
        ToNodePin: 1,
    },
];

pub static CAPTURE_FILTER_DESCRIPTOR: PCFILTER_DESCRIPTOR = PCFILTER_DESCRIPTOR {
    Version: 0,
    AutomationTable: core::ptr::null(),
    PinSize: size_of::<PCPIN_DESCRIPTOR>() as ULONG,
    PinCount: 2,
    Pins: &CAPTURE_PINS[0],
    NodeSize: size_of::<PCNODE_DESCRIPTOR>() as ULONG,
    NodeCount: 2,
    Nodes: &CAPTURE_NODES[0],
    ConnectionCount: 3,
    Connections: &CAPTURE_CONNECTIONS[0],
    CategoryCount: 0,
    Categories: core::ptr::null(),
};

// ============ IMiniportTopology COM 对象 ============

/// vtable 槽序：portcls.h:1776-1791 + DEFINE_ABSTRACT_MINIPORT（portcls.h:326-340）——
/// IUnknown 3 槽、GetDescription、DataRangeIntersection、Init（UnknownAdapter,
/// ResourceList, Port）
#[repr(C)]
pub struct IMiniportTopologyVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub get_description: unsafe extern "system" fn(PVOID, *mut PPCFILTER_DESCRIPTOR) -> NTSTATUS,
    pub data_range_intersection: unsafe extern "system" fn(
        PVOID,
        ULONG,
        *mut KSDATARANGE,
        *mut KSDATARANGE,
        ULONG,
        PVOID,
        *mut ULONG,
    ) -> NTSTATUS,
    pub init: unsafe extern "system" fn(PVOID, PVOID, PVOID, PVOID) -> NTSTATUS,
}

/// 节点数：node0=VOLUME、node1=MUTE（两张表一致）
pub const TOPO_NODE_COUNT: usize = 2;

/// Topology 小端口对象。首字段是 vtable 指针——对象指针可直接 cast 为
/// `IMiniportTopology*`（与 miniport.rs 的 WaveRT 小端口同一布局约定）。
#[repr(C)]
pub struct MiniportTopology {
    pub vtable: &'static IMiniportTopologyVtbl,
    pub refcount: u32,
    /// true = 采集（MICIN 表），false = 渲染（SPEAKER 表）
    pub capture: bool,
    /// IPortTopology 端口对象——弱引用（Init 存入，不 AddRef/不 Release；
    /// 端口生命周期由 PortCls/接线层保证长于本对象）
    pub port: PVOID,
    /// 音量值（KSPROPERTY_AUDIO_VOLUMELEVEL：LONG，0=0dB，负值衰减；按节点序号索引）
    pub volume: [AtomicI32; TOPO_NODE_COUNT],
    /// 静音值（KSPROPERTY_AUDIO_MUTE：LONG，0=不静音，1=静音；按节点序号索引）
    pub mute: [AtomicI32; TOPO_NODE_COUNT],
}

static TOPOLOGY_VTABLE: IMiniportTopologyVtbl = IMiniportTopologyVtbl {
    query_interface: topo_query_interface,
    add_ref: topo_add_ref,
    release: topo_release,
    get_description: topo_get_description,
    data_range_intersection: topo_data_range_intersection,
    init: topo_init,
};

// ---- 池分配/释放：仅 `kernel` feature 链接内核 API；宿主构建/测试用桩，
// create() 返回 null、free 为 no-op（COM 生命周期逻辑仍可在宿主全链路测试） ----
#[cfg(feature = "kernel")]
unsafe fn pool_alloc(size: usize) -> PVOID {
    // SAFETY: 内核 API；0x40=NonPagedPool(Nx) 标志位，与 miniport.rs 一致
    unsafe { crate::sys::mem::ExAllocatePool2_np(0x40, size as u64, TAG) }
}
#[cfg(feature = "kernel")]
unsafe fn pool_free(ptr: PVOID) {
    // SAFETY: ptr 必须来自 pool_alloc 且未被释放
    unsafe { crate::sys::mem::ExFreePoolWithTag_np(ptr, TAG) }
}
#[cfg(not(feature = "kernel"))]
unsafe fn pool_alloc(_size: usize) -> PVOID {
    // 宿主无内核池；宿主测试不走此路径（直接构造栈上对象）
    core::ptr::null_mut()
}
#[cfg(not(feature = "kernel"))]
unsafe fn pool_free(_ptr: PVOID) {}

impl MiniportTopology {
    /// 创建 topology 小端口（refcount=1，归调用方/接线层持有）
    ///
    /// # Safety
    /// 仅在 PASSIVE_LEVEL 调用（PortCls PcRegisterSubdevice 流程）。
    pub unsafe fn create(capture: bool) -> *mut MiniportTopology {
        let ptr = unsafe { pool_alloc(size_of::<MiniportTopology>()) } as *mut MiniportTopology;
        if ptr.is_null() {
            return core::ptr::null_mut();
        }
        // SAFETY: 刚分配的非分页池，大小恰为 MiniportTopology
        unsafe {
            core::ptr::write(
                ptr as *mut MiniportTopology,
                MiniportTopology {
                    vtable: &TOPOLOGY_VTABLE,
                    refcount: 1,
                    capture,
                    port: core::ptr::null_mut(),
                    volume: [AtomicI32::new(0), AtomicI32::new(0)],
                    mute: [AtomicI32::new(0), AtomicI32::new(0)],
                },
            );
        }
        ptr as *mut MiniportTopology
    }
}

unsafe extern "system" fn topo_query_interface(
    this: PVOID,
    iid: *const GUID,
    obj: *mut *mut c_void,
) -> NTSTATUS {
    let this = this as *mut MiniportTopology;
    // SAFETY: 调用方（PortCls）保证 iid/obj 有效
    unsafe {
        if is_equal_guid(iid, &IID_IUnknown)
            || is_equal_guid(iid, &IID_IMiniport)
            || is_equal_guid(iid, &IID_IMiniportTopology)
        {
            *obj = this.cast();
            interlocked_increment(core::ptr::addr_of_mut!((*this).refcount));
            STATUS_SUCCESS
        } else {
            // COM 约定：不认识的 IID 置空出参并返回 E_NOINTERFACE
            *obj = core::ptr::null_mut();
            E_NOINTERFACE
        }
    }
}

unsafe extern "system" fn topo_add_ref(this: PVOID) -> u32 {
    let this = this as *mut MiniportTopology;
    // SAFETY: PortCls 保证 this 有效且存活
    unsafe { interlocked_increment(core::ptr::addr_of_mut!((*this).refcount)) }
}

unsafe extern "system" fn topo_release(this: PVOID) -> u32 {
    let this = this as *mut MiniportTopology;
    // SAFETY: PortCls 保证 this 在本次 Release 期间有效
    let rc = unsafe { interlocked_decrement(core::ptr::addr_of_mut!((*this).refcount)) };
    if rc == 0 {
        // port 为弱引用，不释放；port 对象归 PortCls/接线层管理
        // SAFETY: refcount 归零 ⇒ 没有其他持有者
        unsafe { pool_free(this.cast()) };
    }
    rc
}

unsafe extern "system" fn topo_get_description(
    this: PVOID,
    description: *mut PPCFILTER_DESCRIPTOR,
) -> NTSTATUS {
    let this = this as *mut MiniportTopology;
    // SAFETY: PortCls 保证 this/description 有效
    unsafe {
        *description = if (*this).capture {
            &CAPTURE_FILTER_DESCRIPTOR
        } else {
            &RENDER_FILTER_DESCRIPTOR
        } as *const PCFILTER_DESCRIPTOR as *mut PCFILTER_DESCRIPTOR;
    }
    STATUS_SUCCESS
}

/// topology 滤波器不参与数据交叉（sysvad mintopo.cpp:135 起返回 STATUS_NOT_IMPLEMENTED）
unsafe extern "system" fn topo_data_range_intersection(
    _this: PVOID,
    _pin_id: ULONG,
    _client: *mut KSDATARANGE,
    _mine: *mut KSDATARANGE,
    _out_len: ULONG,
    _result: PVOID,
    _result_len: *mut ULONG,
) -> NTSTATUS {
    STATUS_NOT_IMPLEMENTED
}

/// IMiniportTopology::Init（portcls.h:1782-1790：UnknownAdapter, ResourceList, Port）
///
/// # Safety
/// PortCls 在 PASSIVE_LEVEL 调用；port 必须为有效 IPortTopology 指针。
unsafe extern "system" fn topo_init(
    this: PVOID,
    _unknown_adapter: PVOID,
    _resource_list: PVOID,
    port: PVOID,
) -> NTSTATUS {
    let this = this as *mut MiniportTopology;
    // SAFETY: PortCls 保证 this 有效
    unsafe {
        (*this).port = port;
    }
    STATUS_SUCCESS
}

/// volume/mute 属性处理器（Get/Set 值存内存；BasicSupport 两级应答）
///
/// IRQL：属性值只经原子访问，DISPATCH_LEVEL 下调用同样安全；
/// MajorTarget 是本小端口对象（portcls.h:1304-1307 注释：miniport 时
/// MajorTarget 即 miniport 指针），据此取回状态。
unsafe extern "system" fn topo_property_handler(req: *mut PCPROPERTY_REQUEST) -> NTSTATUS {
    // SAFETY: PortCls 保证 req 及其字段有效
    let (node, verb, id, mp) = unsafe {
        let r = &mut *req;
        let id = if r.PropertyItem.is_null() {
            return STATUS_INVALID_PARAMETER;
        } else {
            (*r.PropertyItem).Id
        };
        (r.Node, r.Verb, id, r.MajorTarget as *mut MiniportTopology)
    };
    if node as usize >= TOPO_NODE_COUNT {
        // 节点定向请求只可能是 node0/node1（PCFILTER_NODE 会打滤波器级空表，不会进来）
        return STATUS_INVALID_PARAMETER;
    }
    let slot = unsafe {
        match id {
            KSPROPERTY_AUDIO_VOLUMELEVEL => &(*mp).volume[node as usize],
            KSPROPERTY_AUDIO_MUTE => &(*mp).mute[node as usize],
            _ => return STATUS_INVALID_PARAMETER,
        }
    };
    let value = unsafe { &mut *req };

    if verb & KSPROPERTY_TYPE_BASICSUPPORT != 0 {
        if value.ValueSize as usize >= size_of::<KSPROPERTY_DESCRIPTION>() {
            let desc = value.Value as *mut KSPROPERTY_DESCRIPTION;
            // SAFETY: ValueSize 足容 40 字节
            unsafe {
                core::ptr::write(
                    desc,
                    KSPROPERTY_DESCRIPTION {
                        AccessFlags: KSPROPERTY_TYPE_GET
                            | KSPROPERTY_TYPE_SET
                            | KSPROPERTY_TYPE_BASICSUPPORT,
                        DescriptionSize: size_of::<KSPROPERTY_DESCRIPTION>() as ULONG,
                        PropTypeSetSet: KSPROPTYPESETID_GENERAL,
                        PropTypeSetId: VT_I4,
                        PropTypeSetFlags: 0,
                        MembersListCount: 0,
                        Reserved: 0,
                    },
                );
            }
            value.ValueSize = size_of::<KSPROPERTY_DESCRIPTION>() as ULONG;
        } else if value.ValueSize >= 4 {
            // SAFETY: ValueSize 足容 4 字节
            unsafe {
                core::ptr::write(
                    value.Value as *mut ULONG,
                    KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT,
                );
            }
            value.ValueSize = 4;
        } else {
            return STATUS_BUFFER_TOO_SMALL;
        }
        return STATUS_SUCCESS;
    }

    if verb & KSPROPERTY_TYPE_GET != 0 {
        if value.ValueSize < 4 {
            return STATUS_BUFFER_TOO_SMALL;
        }
        // SAFETY: ValueSize 足容 4 字节
        unsafe {
            core::ptr::write(value.Value as *mut i32, slot.load(Ordering::Relaxed));
        }
        value.ValueSize = 4;
        return STATUS_SUCCESS;
    }

    if verb & KSPROPERTY_TYPE_SET != 0 {
        if value.ValueSize < 4 {
            return STATUS_BUFFER_TOO_SMALL;
        }
        // SAFETY: ValueSize 足容 4 字节
        let new_value = unsafe { core::ptr::read(value.Value as *const i32) };
        slot.store(new_value, Ordering::Relaxed);
        return STATUS_SUCCESS;
    }

    STATUS_INVALID_PARAMETER
}

// ============ 宿主回归测试（纯逻辑/布局断言，macOS 可跑） ============
//
// Windows 运行时行为（池分配/释放、PortCls 对 GetDescription/属性请求的
// 实际调用、IRQL 语义）无法在 macOS 单测——涉及内核 API 链接与运行环境，
// 由 cargo check --target x86_64-pc-windows-msvc 交叉编译保证可构建，
// 运行时验证留在 Windows 侧安装流程。
#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::size_of_val;

    /// PCPROPERTY_ITEM（portcls.h:1231-1236，x64 24 字节）
    #[test]
    fn layout_pcproperty_item() {
        assert_eq!(size_of::<PCPROPERTY_ITEM>(), 24);
        assert_eq!(offset_of!(PCPROPERTY_ITEM, Set), 0);
        assert_eq!(offset_of!(PCPROPERTY_ITEM, Id), 8);
        assert_eq!(offset_of!(PCPROPERTY_ITEM, Flags), 12);
        assert_eq!(offset_of!(PCPROPERTY_ITEM, Handler), 16);
    }

    /// PCAUTOMATION_TABLE（portcls.h:1361-1382，x64 56 字节）
    #[test]
    fn layout_pcautomation_table() {
        assert_eq!(size_of::<PCAUTOMATION_TABLE>(), 56);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, PropertyItemSize), 0);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, PropertyCount), 4);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, Properties), 8);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, MethodItemSize), 16);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, MethodCount), 20);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, Methods), 24);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, EventItemSize), 32);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, EventCount), 36);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, Events), 40);
        assert_eq!(offset_of!(PCAUTOMATION_TABLE, Reserved), 48);
        // portcls.h:1372-1374：ItemSize 必须是 8 的倍数
        assert_eq!(size_of::<PCPROPERTY_ITEM>() % 8, 0);
    }

    /// PCPROPERTY_REQUEST（portcls.h:1294-1315，x64 72 字节）
    #[test]
    fn layout_pcproperty_request() {
        assert_eq!(size_of::<PCPROPERTY_REQUEST>(), 72);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, MajorTarget), 0);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, MinorTarget), 8);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, Node), 16);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, PropertyItem), 24);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, Verb), 32);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, InstanceSize), 36);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, Instance), 40);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, ValueSize), 48);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, Value), 56);
        assert_eq!(offset_of!(PCPROPERTY_REQUEST, Irp), 64);
    }

    /// KSPROPERTY_DESCRIPTION（ks.h:231-237，x64 40 字节）
    #[test]
    fn layout_ksproperty_description() {
        assert_eq!(size_of::<KSPROPERTY_DESCRIPTION>(), 40);
        assert_eq!(offset_of!(KSPROPERTY_DESCRIPTION, AccessFlags), 0);
        assert_eq!(offset_of!(KSPROPERTY_DESCRIPTION, DescriptionSize), 4);
        assert_eq!(offset_of!(KSPROPERTY_DESCRIPTION, PropTypeSetSet), 8);
        assert_eq!(offset_of!(KSPROPERTY_DESCRIPTION, PropTypeSetId), 24);
        assert_eq!(offset_of!(KSPROPERTY_DESCRIPTION, PropTypeSetFlags), 28);
        assert_eq!(offset_of!(KSPROPERTY_DESCRIPTION, MembersListCount), 32);
        assert_eq!(offset_of!(KSPROPERTY_DESCRIPTION, Reserved), 36);
    }

    /// 描述符一致性：计数/尺寸与实例化表一致，连接表引用的节点号/pin 号都在界内，
    /// 节点自动化表非空且 PropertyItemSize 合规
    #[test]
    fn descriptor_consistency() {
        for desc in [&RENDER_FILTER_DESCRIPTOR, &CAPTURE_FILTER_DESCRIPTOR] {
            assert_eq!(desc.PinSize as usize, size_of::<PCPIN_DESCRIPTOR>());
            assert_eq!(desc.NodeSize as usize, size_of::<PCNODE_DESCRIPTOR>());
            let pins = unsafe { core::slice::from_raw_parts(desc.Pins, desc.PinCount as usize) };
            let nodes = unsafe { core::slice::from_raw_parts(desc.Nodes, desc.NodeCount as usize) };
            let conns = unsafe {
                core::slice::from_raw_parts(desc.Connections, desc.ConnectionCount as usize)
            };
            assert_eq!(pins.len(), 2);
            assert_eq!(nodes.len(), TOPO_NODE_COUNT);
            assert_eq!(conns.len(), 3);
            for pin in pins {
                assert!(pin.KsPinDescriptor.DataRangesCount >= 1);
                assert!(!pin.KsPinDescriptor.DataRanges.is_null());
            }
            for (i, node) in nodes.iter().enumerate() {
                // AutomationTable 经 types.rs 的不透明指针类型传递，转回本地真实布局
                let aut = unsafe { &*(node.AutomationTable as *const PCAUTOMATION_TABLE) };
                assert_eq!(aut.PropertyItemSize as usize, size_of::<PCPROPERTY_ITEM>());
                assert_eq!(aut.PropertyCount, 1);
                assert!(!aut.Properties.is_null());
                let item = unsafe { &*aut.Properties };
                let expected = if i == 0 {
                    KSPROPERTY_AUDIO_VOLUMELEVEL
                } else {
                    KSPROPERTY_AUDIO_MUTE
                };
                assert_eq!(item.Id, expected);
                assert_eq!(
                    item.Flags,
                    KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT
                );
            }
            for c in conns {
                assert!(c.FromNode == PCFILTER_NODE || (c.FromNode as usize) < nodes.len());
                assert!(c.ToNode == PCFILTER_NODE || (c.ToNode as usize) < nodes.len());
                assert!((c.FromNodePin as usize) < pins.len());
                assert!((c.ToNodePin as usize) < pins.len());
            }
        }
    }

    /// 数据流方向与端点类别（任务 A.4）：
    /// 渲染 pin0=IN（来自 Wave）、pin1=OUT（扬声器端点）；采集反之；
    /// 桥接端 pin 的 Category 分别是 SPEAKER / MICROPHONE
    #[test]
    fn dataflow_and_categories() {
        let render_pins = unsafe { core::slice::from_raw_parts(RENDER_FILTER_DESCRIPTOR.Pins, 2) };
        let capture_pins =
            unsafe { core::slice::from_raw_parts(CAPTURE_FILTER_DESCRIPTOR.Pins, 2) };
        assert_eq!(render_pins[0].KsPinDescriptor.DataFlow, KSPIN_DATAFLOW_IN);
        assert_eq!(render_pins[1].KsPinDescriptor.DataFlow, KSPIN_DATAFLOW_OUT);
        assert_eq!(capture_pins[0].KsPinDescriptor.DataFlow, KSPIN_DATAFLOW_IN);
        assert_eq!(capture_pins[1].KsPinDescriptor.DataFlow, KSPIN_DATAFLOW_OUT);
        unsafe {
            // GUID 无 Debug，用字段比较（types.rs 的 GUID 不在本次改动范围）
            assert!(guid_eq(
                &*render_pins[0].KsPinDescriptor.Category,
                &KSCATEGORY_AUDIO
            ));
            assert!(guid_eq(
                &*render_pins[1].KsPinDescriptor.Category,
                &KSNODETYPE_SPEAKER
            ));
            assert!(guid_eq(
                &*capture_pins[0].KsPinDescriptor.Category,
                &KSNODETYPE_MICROPHONE
            ));
            assert!(guid_eq(
                &*capture_pins[1].KsPinDescriptor.Category,
                &KSCATEGORY_AUDIO
            ));
        }
        // 节点类型：volume/mute 顺序固定（node0/node1）
        let render_nodes =
            unsafe { core::slice::from_raw_parts(RENDER_FILTER_DESCRIPTOR.Nodes, 2) };
        let capture_nodes =
            unsafe { core::slice::from_raw_parts(CAPTURE_FILTER_DESCRIPTOR.Nodes, 2) };
        unsafe {
            assert!(guid_eq(&*render_nodes[0].Type, &KSNODETYPE_VOLUME));
            assert!(guid_eq(&*render_nodes[1].Type, &KSNODETYPE_MUTE));
            assert!(guid_eq(&*capture_nodes[0].Type, &KSNODETYPE_VOLUME));
            assert!(guid_eq(&*capture_nodes[1].Type, &KSNODETYPE_MUTE));
        }
    }

    /// GUID 逐字段比较（types.rs::GUID 无 Debug/PartialEq，不在改动范围）
    fn guid_eq(a: &GUID, b: &GUID) -> bool {
        a.data1 == b.data1 && a.data2 == b.data2 && a.data3 == b.data3 && a.data4 == b.data4
    }

    /// GUID 转录抽查：防止手抄数字错位（值对照 WDK 头文件注释）
    #[test]
    fn guid_transcription_spot_checks() {
        assert_eq!(KSNODETYPE_VOLUME.data1, 0x3a5a_cc00);
        assert_eq!(KSNODETYPE_VOLUME.data2, 0xc557);
        assert_eq!(
            KSNODETYPE_VOLUME.data4,
            [0x8a, 0x2b, 0, 0xa0, 0xc9, 0x25, 0x5a, 0xc1]
        );
        assert_eq!(KSNODETYPE_MUTE.data1, 0x02b2_23c0);
        assert_eq!(KSNODETYPE_SPEAKER.data1, 0xdff2_1ce1);
        assert_eq!(KSNODETYPE_MICROPHONE.data1, 0xdff2_1be1);
        assert_eq!(KSAUDFNAME_MASTER_VOLUME.data1, 0x185f_ede3);
        assert_eq!(KSAUDFNAME_MASTER_MUTE.data1, 0x185f_ede4);
        assert_eq!(KSAUDFNAME_MIC_VOLUME.data1, 0x185f_eded);
        assert_eq!(KSAUDFNAME_MIC_MUTE.data1, 0x185f_edee);
        assert_eq!(IID_IMiniportTopology.data1, 0xb4c9_0a31);
        assert_eq!(
            IID_IMiniportTopology.data4,
            [0x86, 0xf9, 0, 0xa0, 0xc9, 0x11, 0xb5, 0x44]
        );
    }

    /// 测试用构造（不走内核池分配）：等价于 create() 的字段初始化
    fn make_mp(capture: bool) -> MiniportTopology {
        MiniportTopology {
            vtable: &TOPOLOGY_VTABLE,
            refcount: 1,
            capture,
            port: core::ptr::null_mut(),
            volume: [AtomicI32::new(0), AtomicI32::new(0)],
            mute: [AtomicI32::new(0), AtomicI32::new(0)],
        }
    }

    /// COM 链路（宿主可测部分）：GetDescription 出参按 capture 方向取表，
    /// QI 认识 IUnknown/IMiniport/IMiniportTopology 且引用计数联动
    #[test]
    fn com_get_description_and_qi() {
        let mut render = make_mp(false);
        let mut capture = make_mp(true);
        let r = &mut render as *mut MiniportTopology as PVOID;
        let c = &mut capture as *mut MiniportTopology as PVOID;
        unsafe {
            let mut desc: PPCFILTER_DESCRIPTOR = core::ptr::null_mut();
            assert_eq!(
                ((render.vtable).get_description)(r, &mut desc),
                STATUS_SUCCESS
            );
            assert_eq!(desc as *const _, &RENDER_FILTER_DESCRIPTOR as *const _);
            let mut desc2: PPCFILTER_DESCRIPTOR = core::ptr::null_mut();
            assert_eq!(
                ((capture.vtable).get_description)(c, &mut desc2),
                STATUS_SUCCESS
            );
            assert_eq!(desc2 as *const _, &CAPTURE_FILTER_DESCRIPTOR as *const _);

            // QI：自己的接口 → 成功且 AddRef；陌生 IID → E_NOINTERFACE 且置空
            let iid_unknown = IID_IUnknown;
            let mut out: *mut c_void = core::ptr::null_mut();
            assert_eq!(
                ((render.vtable).query_interface)(r, &iid_unknown, &mut out),
                STATUS_SUCCESS
            );
            assert!(!out.is_null());
            assert_eq!(render.refcount, 2);
            assert_eq!(((render.vtable).release)(r), 1);
            let bogus = GUID {
                data1: 1,
                data2: 2,
                data3: 3,
                data4: [0; 8],
            };
            let mut out2: *mut c_void = core::ptr::null_mut();
            assert_eq!(
                ((render.vtable).query_interface)(r, &bogus, &mut out2),
                E_NOINTERFACE
            );
            assert!(out2.is_null());
        }
    }

    /// 属性处理器：GET/SET/BASICSUPPORT 三路 + 越界节点拒绝 + 陌生 Id 拒绝
    #[test]
    fn property_handler_get_set_basicsupport() {
        let mut mp = make_mp(false);
        let mp_ptr = &mut mp as *mut MiniportTopology as PVOID;
        let mut buf: [u8; 40] = [0; 40];
        let vol_in: i32 = -30 * 65536;
        let mute_in: i32 = 1;
        let mut req = PCPROPERTY_REQUEST {
            MajorTarget: mp_ptr,
            MinorTarget: core::ptr::null_mut(),
            Node: 0,
            PropertyItem: &TOPO_VOLUME_PROPERTY_ITEMS[0],
            Verb: KSPROPERTY_TYPE_SET,
            InstanceSize: 0,
            Instance: core::ptr::null_mut(),
            ValueSize: 4,
            Value: (&vol_in as *const i32).cast::<c_void>() as PVOID,
            Irp: core::ptr::null_mut(),
        };
        unsafe {
            // SET 音量 -30dB → GET 应回读同值
            assert_eq!(topo_property_handler(&mut req), STATUS_SUCCESS);
            let mut out: i32 = 0;
            req.Verb = KSPROPERTY_TYPE_GET;
            req.Value = (&mut out as *mut i32).cast::<c_void>() as PVOID;
            assert_eq!(topo_property_handler(&mut req), STATUS_SUCCESS);
            assert_eq!(req.ValueSize, 4);
            assert_eq!(out, -30 * 65536);

            // MUTE：SET 1 → GET 1（走 TOPO_MUTE_PROPERTY_ITEMS 的 Id 分派）
            req.PropertyItem = &TOPO_MUTE_PROPERTY_ITEMS[0];
            req.Verb = KSPROPERTY_TYPE_SET;
            req.Value = (&mute_in as *const i32).cast::<c_void>() as PVOID;
            assert_eq!(topo_property_handler(&mut req), STATUS_SUCCESS);
            let mut out2: i32 = 0;
            req.Verb = KSPROPERTY_TYPE_GET;
            req.Value = (&mut out2 as *mut i32).cast::<c_void>() as PVOID;
            assert_eq!(topo_property_handler(&mut req), STATUS_SUCCESS);
            assert_eq!(out2, 1);

            // BASICSUPPORT：40 字节出完整描述，4 字节出 AccessFlags，0 字节拒绝
            req.PropertyItem = &TOPO_VOLUME_PROPERTY_ITEMS[0];
            req.Verb = KSPROPERTY_TYPE_BASICSUPPORT;
            req.ValueSize = size_of::<KSPROPERTY_DESCRIPTION>() as ULONG;
            req.Value = buf.as_mut_ptr().cast::<c_void>();
            assert_eq!(topo_property_handler(&mut req), STATUS_SUCCESS);
            assert_eq!(req.ValueSize as usize, size_of::<KSPROPERTY_DESCRIPTION>());
            let desc = core::ptr::read(buf.as_ptr() as *const KSPROPERTY_DESCRIPTION);
            assert_eq!(
                desc.AccessFlags,
                KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT
            );
            assert_eq!(desc.PropTypeSetId, VT_I4);
            req.ValueSize = 4;
            assert_eq!(topo_property_handler(&mut req), STATUS_SUCCESS);
            assert_eq!(req.ValueSize, 4);
            assert_eq!(
                u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]),
                KSPROPERTY_TYPE_GET | KSPROPERTY_TYPE_SET | KSPROPERTY_TYPE_BASICSUPPORT
            );
            req.ValueSize = 0;
            assert_eq!(topo_property_handler(&mut req), STATUS_BUFFER_TOO_SMALL);

            // 越界节点 / 滤波器级节点号拒绝
            req.ValueSize = 4;
            req.Node = PCFILTER_NODE;
            assert_eq!(topo_property_handler(&mut req), STATUS_INVALID_PARAMETER);
            req.Node = 2;
            assert_eq!(topo_property_handler(&mut req), STATUS_INVALID_PARAMETER);
        }
    }

    /// 桥接数据范围条目：Type=Audio/SubType=Analog/Specifier=None（sysvad 同构）
    #[test]
    fn bridge_datarange_matches_sysvad() {
        let dr = &TOPO_PIN_DATARANGES[0];
        assert_eq!(size_of_val(dr), 64);
        assert_eq!(dr.FormatSize as usize, size_of::<KSDATARANGE>());
        assert_eq!(dr.MajorFormat.data1, 0x7364_7561); // 'aud ' 小端
        assert_eq!(dr.SubFormat.data1, 0x6dba_3190);
        assert_eq!(dr.Specifier.data1, 0x0f64_17d6);
    }
}
