//! HID minidriver 契约：类型（对照 hidport.h / hidclass.h）；
//! IOCTL 常量与报告描述符的纯逻辑在 `contract.rs`（windows-free，可宿主单测）。

#![allow(non_snake_case, non_camel_case_types)]

use core::mem::size_of;

use wdk_sys::{PVOID, UCHAR, ULONG};

#[path = "contract.rs"]
mod contract;

pub use contract::{
    IOCTL_HID_GET_DEVICE_ATTRIBUTES, IOCTL_HID_GET_DEVICE_DESCRIPTOR, IOCTL_HID_GET_FEATURE,
    IOCTL_HID_GET_INPUT_REPORT, IOCTL_HID_GET_REPORT_DESCRIPTOR, IOCTL_HID_GET_STRING,
    IOCTL_HID_READ_REPORT, IOCTL_HID_SET_FEATURE, IOCTL_HID_SET_OUTPUT_REPORT,
    IOCTL_HID_WRITE_REPORT, KEYBOARD_REPORT_DESCRIPTOR, MOUSE_REPORT_DESCRIPTOR,
};

/// HID 描述符（IOCTL_HID_GET_DEVICE_DESCRIPTOR 返回）
///
/// 布局出处：hidport.h `_HID_DESCRIPTOR`，整体被 `pshpack1.h`/`poppack.h` 包裹
/// （1 字节对齐，sizeof = 9）——Rust 侧必须 `packed` 与之对齐。
/// packed 结构体禁止 `&field`/跨字段引用：所有字段访问一律按值读写或整结构体
/// 字节拷贝（当前仅静态构造 + `copy_to_output` 整体拷出，无字段引用）。
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct HID_DESCRIPTOR {
    pub bLength: UCHAR,
    pub bDescriptorType: UCHAR,
    pub bcdHID: u16,
    pub bCountryCode: UCHAR,
    pub bNumDescriptors: UCHAR,
    pub DescriptorList: [HID_DESCRIPTOR_LIST_ENTRY; 1],
}

/// hidport.h `_HID_DESCRIPTOR_DESC_LIST`（同在 pshpack1.h 范围内：UCHAR+USHORT 紧排）
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct HID_DESCRIPTOR_LIST_ENTRY {
    pub bDescriptorType: UCHAR,
    pub wDescriptorLength: u16,
}

// 编译期布局断言：与 hidport.h pshpack1.h 的 9 字节一致。
// 验证范围（如实表述）：contract.rs（IOCTL 常量+描述符字节）经宿主 `cargo test`
// 编译并运行单测验证（src/report.rs 以 #[path] 共享同一份）；本 packed 结构体与
// 下方断言只能在 Windows 侧编译时验证——kernel workspace 因 wdk-build 拒绝非
// Windows 主机，本机 macOS 编不到（rustc 1.98 已手工验证 packed+derive+静态构造
// 可编译）。
const _: () = assert!(size_of::<HID_DESCRIPTOR>() == 9);

/// 设备属性（IOCTL_HID_GET_DEVICE_ATTRIBUTES 返回）
///
/// 布局出处：hidport.h `HID_DEVICE_ATTRIBUTES`（无 pack 包裹，自然对齐，sizeof = 8）
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HID_DEVICE_ATTRIBUTES {
    pub Size: u32,
    pub VendorID: u16,
    pub ProductID: u16,
    pub VersionNumber: u16,
}

/// hidclass 与 minidriver 之间传递报告的传输包
///
/// 布局出处：hidclass.h `HID_XFER_PACKET`（无 pack 包裹，自然对齐）
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HID_XFER_PACKET {
    pub reportBuffer: PVOID,
    pub reportBufferLen: ULONG,
    pub reportId: UCHAR,
}
