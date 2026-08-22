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
pub const IID_IPort: GUID = GUID {
    data1: 0xd4fc_dd00,
    data2: 0x6b9d,
    data3: 0x11d0,
    data4: [0xab, 0x08, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
};
pub const IID_IPortWaveRT: GUID = GUID {
    data1: 0x339f_f909,
    data2: 0x68a9,
    data3: 0x4310,
    data4: [0xb0, 0x9b, 0x27, 0x4e, 0x96, 0xee, 0x4c, 0xbd],
};
pub const IID_IMiniport: GUID = GUID {
    data1: 0x9434_c220,
    data2: 0x6b9e,
    data3: 0x11d0,
    data4: [0xa9, 0x9d, 0x00, 0xa0, 0xc9, 0x22, 0x31, 0x96],
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

pub const IID_IUnknown: GUID = GUID {
    data1: 0x0000_0000,
    data2: 0x0000,
    data3: 0x0000,
    data4: [0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46],
};

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
    /// # Safety
    /// 参数必须来自内核，DriverObject/PhysicalDeviceObject 为有效指针。
    pub fn PcAddAdapterDevice(
        DriverObject: PDRIVER_OBJECT,
        PhysicalDeviceObject: PDEVICE_OBJECT,
        StartDevice: Option<
            unsafe extern "system" fn(PDEVICE_OBJECT, PIRP, PRESOURCELIST) -> NTSTATUS,
        >,
        AddDeviceContextSize: ULONG,
        Context: PVOID,
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
    /// 注册设备接口（音频端点可见性）
    ///
    /// # Safety
    /// 参数必须为有效指针，SymbolicLinkName 输出缓冲区由调用方管理。
    pub fn IoRegisterDeviceInterface(
        PhysicalDeviceObject: PDEVICE_OBJECT,
        InterfaceClassGuid: *const GUID,
        ReferenceString: PUNICODE_STRING,
        SymbolicLinkName: PUNICODE_STRING,
    ) -> NTSTATUS;
}
