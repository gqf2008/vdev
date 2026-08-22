//! 最小 WDM 基础类型（手写，PortCls 驱动所需子集；ABI 稳定）

#![allow(non_camel_case_types, non_snake_case)]

pub type NTSTATUS = i32;
pub const STATUS_SUCCESS: NTSTATUS = 0;
pub const STATUS_UNSUCCESSFUL: NTSTATUS = -1_073_741_823; // 0xC0000001

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
