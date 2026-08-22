//! `PortCls` 音频端口类驱动函数（`portcls.lib` 导出）

#![allow(non_snake_case)]

use crate::sys::types::{
    NTSTATUS, PCWSTR, PDEVICE_OBJECT, PDRIVER_OBJECT, PIRP, PUNICODE_STRING, PVOID, ULONG,
};

unsafe extern "system" {
    /// 初始化适配器驱动，注册 `AddDevice` 回调
    ///
    /// # Safety
    /// 参数必须来自内核，`DriverObject`/`RegistryPath` 为有效指针。
    pub fn PcInitializeAdapterDriver(
        DriverObject: PDRIVER_OBJECT,
        RegistryPath: PUNICODE_STRING,
        AddDevice: Option<unsafe extern "system" fn(PDRIVER_OBJECT, PDEVICE_OBJECT) -> NTSTATUS>,
    ) -> NTSTATUS;
    /// 为 `PnP` 设备创建功能设备对象并绑定 `PortCls`
    ///
    /// # Safety
    /// 参数必须来自内核，`DriverObject`/`PhysicalDeviceObject` 为有效指针。
    pub fn PcAddAdapterDevice(
        DriverObject: PDRIVER_OBJECT,
        PhysicalDeviceObject: PDEVICE_OBJECT,
        StartDevice: Option<unsafe extern "system" fn(PVOID, PIRP) -> NTSTATUS>,
        AddDeviceContextSize: ULONG,
        Context: PVOID,
    ) -> NTSTATUS;
    /// 注册子设备（音频端点）
    ///
    /// # Safety
    /// `DeviceObject` 为有效设备对象，`Name` 为有效宽字符串。
    pub fn PcRegisterSubdevice(
        DeviceObject: PDEVICE_OBJECT,
        Name: PCWSTR,
        Miniport: PVOID,
    ) -> NTSTATUS;
}
