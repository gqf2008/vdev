//! PortCls 音频端口类驱动函数与接口 GUID / vtable（portcls.lib 导出）

#![allow(non_snake_case)]

use crate::sys::types::*;

// ---- GUID 常量 ----
pub const CLSID_PortWaveRT: GUID = GUID {
    data1: 0xcc9b_e57a,
    data2: 0xeb9e,
    data3: 0x42b4,
    data4: [0x94, 0xfc, 0x0c, 0xad, 0x3d, 0xbc, 0xe7, 0xfa],
};
pub const CLSID_PortTopology: GUID = GUID {
    data1: 0xb4c9_0a32,
    data2: 0x5791,
    data3: 0x11d0,
    data4: [0x86, 0xf9, 0x00, 0xa0, 0xc9, 0x11, 0xb5, 0x44],
};
// 权威值：WDK portcls.h（本机拷贝 /tmp/wdk10/portcls.h 第 69-72 行）
// DEFINE_GUID(IID_IPort, 0xb4c90a25, 0x5791, 0x11d0, ...);
pub const IID_IPort: GUID = GUID {
    data1: 0xb4c9_0a25,
    data2: 0x5791,
    data3: 0x11d0,
    data4: [0x86, 0xf9, 0x00, 0xa0, 0xc9, 0x11, 0xb5, 0x44],
};
pub const IID_IPortWaveRT: GUID = GUID {
    data1: 0x339f_f909,
    data2: 0x68a9,
    data3: 0x4310,
    data4: [0xb0, 0x9b, 0x27, 0x4e, 0x96, 0xee, 0x4c, 0xbd],
};
// 权威值：WDK portcls.h（本机拷贝 /tmp/wdk10/portcls.h 第 69-72 行）
// DEFINE_GUID(IID_IMiniport, 0xb4c90a24, 0x5791, 0x11d0, ...);
pub const IID_IMiniport: GUID = GUID {
    data1: 0xb4c9_0a24,
    data2: 0x5791,
    data3: 0x11d0,
    data4: [0x86, 0xf9, 0x00, 0xa0, 0xc9, 0x11, 0xb5, 0x44],
};
pub const IID_IMiniportWaveRT: GUID = GUID {
    data1: 0x0f9f_c4d6,
    data2: 0x6061,
    data3: 0x4f3c,
    data4: [0xb1, 0xfc, 0x07, 0x5e, 0x35, 0xf7, 0x96, 0x0a],
};
pub const IID_IMiniportWaveRTStream: GUID = GUID {
    data1: 0x00_0ac9ab,
    data2: 0xfaab,
    data3: 0x4f3d,
    data4: [0x94, 0x55, 0x6f, 0xf8, 0x30, 0x6a, 0x74, 0xa0],
};
pub const IID_IAdapterCommon: GUID = GUID {
    data1: 0x7eda_2950,
    data2: 0xbf9f,
    data3: 0x11d0,
    data4: [0x87, 0x1f, 0x00, 0xa0, 0xc9, 0x11, 0xb5, 0x44],
};
pub const KSCATEGORY_AUDIO: GUID = GUID {
    data1: 0x6994_ad04,
    data2: 0x93ef,
    data3: 0x11d0,
    data4: [0xa3, 0xcc, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
/// ks.h：`KSCATEGORY_RENDER {65E8773E-8F56-11D0-A3B9-00A0C9223196}`
///
/// 音频端点构建器（AudioEndpointBuilder）按该类别下的设备接口枚举播放端点，
/// wave 滤波器必须把它列进 `PCFILTER_DESCRIPTOR.Categories`。
pub const KSCATEGORY_RENDER: GUID = GUID {
    data1: 0x65e8_773e,
    data2: 0x8f56,
    data3: 0x11d0,
    data4: [0xa3, 0xb9, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
/// ks.h：`KSCATEGORY_CAPTURE {65E8773D-8F56-11D0-A3B9-00A0C9223196}`
pub const KSCATEGORY_CAPTURE: GUID = GUID {
    data1: 0x65e8_773d,
    data2: 0x8f56,
    data3: 0x11d0,
    data4: [0xa3, 0xb9, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
/// ksmedia.h：`KSCATEGORY_REALTIME {EB115FFC-10C8-4964-831D-6DCB02E6F23F}`
///
/// WaveRT 低延迟通路（audio engine）用的类别；对照本机可用的 ToDesk 虚拟声卡
/// 与 VB-Audio 虚拟声卡 INF，wave 子设备同样登记该类别。
pub const KSCATEGORY_REALTIME: GUID = GUID {
    data1: 0xeb11_5ffc,
    data2: 0x10c8,
    data3: 0x4964,
    data4: [0x83, 0x1d, 0x6d, 0xcb, 0x02, 0xe6, 0xf2, 0x3f],
};
/// ksmedia.h：`KSCATEGORY_TOPOLOGY {DDA54A40-1E4C-11D1-A050-405705C10000}`
pub const KSCATEGORY_TOPOLOGY: GUID = GUID {
    data1: 0xdda5_4a40,
    data2: 0x1e4c,
    data3: 0x11d1,
    data4: [0xa0, 0x50, 0x40, 0x57, 0x05, 0xc1, 0x00, 0x00],
};
/// portcls.h：注销子设备经 port 对象的 IUnregisterSubdevice 接口
/// （不存在平面导出 PcUnregisterSubdevice）
pub const IID_IUnregisterSubdevice: GUID = GUID {
    data1: 0x1673_8177,
    data2: 0xe199,
    data3: 0x41f9,
    data4: [0x9a, 0x87, 0xab, 0xb2, 0xa5, 0x43, 0x2f, 0x21],
};

/// IID_IUnregisterPhysicalConnection（portcls.h:212-214）
pub const IID_IUnregisterPhysicalConnection: GUID = GUID {
    data1: 0x6c38_e231,
    data2: 0x2a0d,
    data3: 0x428d,
    data4: [0x81, 0xf8, 0x07, 0xcc, 0x42, 0x8b, 0xb9, 0xa4],
};

pub const IID_IUnknown: GUID = GUID {
    data1: 0x0000_0000,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

/// IUnknown vtable（仅用于解引用取 QI 方法；com.rs 只定义了方法签名类型）
#[repr(C)]
pub struct IUnknownVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
}

/// IUnregisterSubdevice vtable（portcls.h：IUnknown + UnregisterSubdevice）
#[repr(C)]
pub struct IUnregisterSubdeviceVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub unregister_subdevice: unsafe extern "system" fn(PVOID, PDEVICE_OBJECT, PVOID) -> NTSTATUS,
}

/// IUnregisterPhysicalConnection vtable
/// （portcls.h:3254-3265：IUnknown + UnregisterPhysicalConnection(6 参)）
#[repr(C)]
pub struct IUnregisterPhysicalConnectionVtbl {
    pub query_interface: crate::com::PFN_QUERYINTERFACE,
    pub add_ref: crate::com::PFN_ADDREF,
    pub release: crate::com::PFN_RELEASE,
    pub unregister_physical_connection:
        unsafe extern "system" fn(PVOID, PDEVICE_OBJECT, PVOID, ULONG, PVOID, ULONG) -> NTSTATUS,
}

unsafe extern "system" {
    /// 初始化适配器驱动，注册 AddDevice 回调
    ///
    /// # Safety
    /// 参数必须来自内核，DriverObject/RegistryPath 为有效指针。
    pub fn PcInitializeAdapterDriver(
        DriverObject: PDRIVER_OBJECT,
        RegistryPath: PUNICODE_STRING,
        AddDevice: Option<unsafe extern "system" fn(PDRIVER_OBJECT, PDEVICE_OBJECT) -> NTSTATUS>,
    ) -> NTSTATUS;
    /// 为 PnP 设备创建功能设备对象并绑定 PortCls
    ///
    /// 签名对照 portcls.h（5 参）：`PcAddAdapterDevice(DriverObject,
    /// PhysicalDeviceObject, StartDevice, MaxObjects, DeviceExtensionSize)`。
    ///
    /// # Safety
    /// 参数必须来自内核，DriverObject/PhysicalDeviceObject 为有效指针。
    pub fn PcAddAdapterDevice(
        DriverObject: PDRIVER_OBJECT,
        PhysicalDeviceObject: PDEVICE_OBJECT,
        StartDevice: Option<
            unsafe extern "system" fn(PDEVICE_OBJECT, PIRP, PRESOURCELIST) -> NTSTATUS,
        >,
        MaxObjects: ULONG,
        DeviceExtensionSize: ULONG,
    ) -> NTSTATUS;
    /// 注册子设备（音频端点）
    ///
    /// # Safety
    /// DeviceObject 为有效设备对象，Name 为有效宽字符串。
    pub fn PcRegisterSubdevice(
        DeviceObject: PDEVICE_OBJECT,
        Name: PCWSTR,
        Unknown: *mut c_void,
    ) -> NTSTATUS;
    /// 注册 filter 间物理连接（wave↔topology 桥接；portcls.h:3888 五参：
    /// `PcRegisterPhysicalConnection(DeviceObject, FromUnknown, FromPin,
    /// ToUnknown, ToPin)`——参数顺序对照 sysvad common.cpp ConnectTopologies）
    ///
    /// # Safety
    /// DeviceObject 为有效设备对象，From/ToUnknown 为已注册子设备的有效 PUNKNOWN。
    pub fn PcRegisterPhysicalConnection(
        DeviceObject: PDEVICE_OBJECT,
        FromUnknown: *mut c_void,
        FromPin: ULONG,
        ToUnknown: *mut c_void,
        ToPin: ULONG,
    ) -> NTSTATUS;
    /// 创建端口对象（CLSID_PortWaveRT 等）
    ///
    /// # Safety
    /// OutPort 必须为有效输出指针。
    pub fn PcNewPort(OutPort: *mut *mut c_void, ClassId: *const GUID) -> NTSTATUS;
    /// 创建小端口对象
    ///
    /// # Safety
    /// OutMiniport 必须为有效输出指针。
    pub fn PcNewMiniport(OutMiniport: *mut *mut c_void, ClassId: *const GUID) -> NTSTATUS;
    /// 获取物理设备对象
    ///
    /// # Safety
    /// DeviceObject 为有效指针。
    pub fn PcGetPhysicalDeviceObject(
        DeviceObject: PDEVICE_OBJECT,
        PhysicalDeviceObject: *mut PDEVICE_OBJECT,
    ) -> NTSTATUS;
}

/// 经 IUnregisterSubdevice 接口注销子设备（安装失败路径 teardown，M8）
///
/// portcls.h 没有 `PcUnregisterSubdevice` 平面导出——注销只能对 port 对象
/// QI `IID_IUnregisterSubdevice` 后调用 `UnregisterSubdevice(DeviceObject, port)`。
/// 成功后 PortCls 释放其对 port 的引用（连同 port->Init 时对 miniport 的引用）。
///
/// # Safety
/// device_object 与 port（已注册的子设备 PUNKNOWN）必须为有效 COM 对象。
pub unsafe fn pc_unregister_subdevice(device_object: PDEVICE_OBJECT, port: PVOID) -> NTSTATUS {
    // SAFETY: port 为有效 IUnknown，vtable 指针在对象头部（双重间接）
    let qi: crate::com::PFN_QUERYINTERFACE = unsafe {
        let vtbl = *(port as *const *const IUnknownVtbl);
        (*vtbl).query_interface
    };
    let mut unk: *mut c_void = core::ptr::null_mut();
    // SAFETY: IID_IUnregisterSubdevice 为静态 GUID
    let st = unsafe {
        qi(
            port,
            &IID_IUnregisterSubdevice,
            core::ptr::addr_of_mut!(unk),
        )
    };
    if st < 0 {
        return st;
    }
    // SAFETY: unk 为 QI 取得的有效 IUnregisterSubdevice，用后 Release
    let st = unsafe {
        let vtbl = *(unk as *const *const IUnregisterSubdeviceVtbl);
        ((*vtbl).unregister_subdevice)(unk, device_object, port)
    };
    // SAFETY: 释放 QI 取得的引用
    unsafe { crate::com::release_unknown(unk) };
    st
}

/// 经 IUnregisterPhysicalConnection 接口注销物理连接（安装失败路径 teardown）
///
/// portcls.h 没有 `PcUnregisterPhysicalConnection` 平面导出——注销只能对 port
/// 对象 QI `IID_IUnregisterPhysicalConnection` 后调用
/// `UnregisterPhysicalConnection(DeviceObject, FromUnknown, FromPin,
/// ToUnknown, ToPin)`，后四参须与 PcRegisterPhysicalConnection 完全一致。
///
/// # Safety
/// device_object 与 from_unknown/to_unknown（已注册连接两端的 PUNKNOWN）
/// 必须为有效 COM 对象。
pub unsafe fn pc_unregister_physical_connection(
    device_object: PDEVICE_OBJECT,
    from_unknown: PVOID,
    from_pin: ULONG,
    to_unknown: PVOID,
    to_pin: ULONG,
) -> NTSTATUS {
    // SAFETY: from_unknown 为有效 IUnknown，vtable 指针在对象头部（双重间接）
    let qi: crate::com::PFN_QUERYINTERFACE = unsafe {
        let vtbl = *(from_unknown as *const *const IUnknownVtbl);
        (*vtbl).query_interface
    };
    let mut unk: *mut c_void = core::ptr::null_mut();
    // SAFETY: IID_IUnregisterPhysicalConnection 为静态 GUID
    let st = unsafe {
        qi(
            from_unknown,
            &IID_IUnregisterPhysicalConnection,
            core::ptr::addr_of_mut!(unk),
        )
    };
    if st < 0 {
        return st;
    }
    // SAFETY: unk 为 QI 取得的有效 IUnregisterPhysicalConnection，用后 Release
    let st = unsafe {
        let vtbl = *(unk as *const *const IUnregisterPhysicalConnectionVtbl);
        ((*vtbl).unregister_physical_connection)(
            unk,
            device_object,
            from_unknown,
            from_pin,
            to_unknown,
            to_pin,
        )
    };
    // SAFETY: 释放 QI 取得的引用
    unsafe { crate::com::release_unknown(unk) };
    st
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 权威 GUID 的 16 字节序列（标准二进制布局：u32 LE + u16 LE + u16 LE + 8×u8），
    /// 抄自 WDK portcls.h（本机拷贝 /tmp/wdk10/portcls.h 第 69-72 行）：
    /// DEFINE_GUID(IID_IMiniport, 0xb4c90a24, 0x5791, 0x11d0, 0x86,0xf9,0x00,0xa0,0xc9,0x11,0xb5,0x44);
    /// DEFINE_GUID(IID_IPort,     0xb4c90a25, 0x5791, 0x11d0, 0x86,0xf9,0x00,0xa0,0xc9,0x11,0xb5,0x44);
    const IID_IMINIPORT_WDK_BYTES: [u8; 16] = [
        0x24, 0x0a, 0xc9, 0xb4, 0x91, 0x57, 0xd0, 0x11, 0x86, 0xf9, 0x00, 0xa0, 0xc9, 0x11, 0xb5,
        0x44,
    ];
    const IID_IPORT_WDK_BYTES: [u8; 16] = [
        0x25, 0x0a, 0xc9, 0xb4, 0x91, 0x57, 0xd0, 0x11, 0x86, 0xf9, 0x00, 0xa0, 0xc9, 0x11, 0xb5,
        0x44,
    ];

    fn guid_bytes(g: &GUID) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[..4].copy_from_slice(&g.data1.to_le_bytes());
        b[4..6].copy_from_slice(&g.data2.to_le_bytes());
        b[6..8].copy_from_slice(&g.data3.to_le_bytes());
        b[8..].copy_from_slice(&g.data4);
        b
    }

    /// 回归：IID_IMiniport / IID_IPort 必须与 WDK portcls.h 权威值逐字节一致
    ///（此前错写成 {9434C220-…}/{D4FCDD00-…}，PortCls 以真 IID 查询会拿到
    /// E_NOINTERFACE）。
    #[test]
    fn iid_iminiport_and_iid_iport_match_wdk_bytes() {
        assert_eq!(guid_bytes(&IID_IMiniport), IID_IMINIPORT_WDK_BYTES);
        assert_eq!(guid_bytes(&IID_IPort), IID_IPORT_WDK_BYTES);
    }
}
