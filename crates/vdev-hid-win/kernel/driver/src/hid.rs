//! HID minidriver 契约：类型、IOCTL 常量与键盘报告描述符（对照 hidport.h / vhidmini2）

#![allow(non_snake_case, non_camel_case_types)]

use wdk_sys::{PVOID, UCHAR, ULONG};

/// HID 描述符（IOCTL_HID_GET_DEVICE_DESCRIPTOR 返回）
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HID_DESCRIPTOR {
    pub bLength: UCHAR,
    pub bDescriptorType: UCHAR,
    pub bcdHID: u16,
    pub bCountryCode: UCHAR,
    pub bNumDescriptors: UCHAR,
    pub DescriptorList: [HID_DESCRIPTOR_LIST_ENTRY; 1],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HID_DESCRIPTOR_LIST_ENTRY {
    pub bDescriptorType: UCHAR,
    pub wDescriptorLength: u16,
}

/// 设备属性（IOCTL_HID_GET_DEVICE_ATTRIBUTES 返回）
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HID_DEVICE_ATTRIBUTES {
    pub Size: u32,
    pub VendorID: u16,
    pub ProductID: u16,
    pub VersionNumber: u16,
}

/// hidclass 与 minidriver 之间传递报告的传输包（hidport.h HID_XFER_PACKET）
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct HID_XFER_PACKET {
    pub reportBuffer: PVOID,
    pub reportBufferLen: ULONG,
    pub reportId: UCHAR,
}

/// CTL_CODE(FILE_DEVICE_KEYBOARD=0x0B, id, method, FILE_ANY_ACCESS=0)
const fn ctl_code(id: u32, method: u32) -> u32 {
    (0x0Bu32 << 16) | (id << 2) | method
}

/// METHOD_NEITHER = 3
pub const IOCTL_HID_GET_DEVICE_DESCRIPTOR: u32 = ctl_code(0x0000, 3);
pub const IOCTL_HID_GET_REPORT_DESCRIPTOR: u32 = ctl_code(0x0001, 3);
pub const IOCTL_HID_READ_REPORT: u32 = ctl_code(0x0002, 3);
pub const IOCTL_HID_WRITE_REPORT: u32 = ctl_code(0x0003, 3);
pub const IOCTL_HID_GET_DEVICE_ATTRIBUTES: u32 = ctl_code(0x0004, 3);
pub const IOCTL_HID_GET_STRING: u32 = ctl_code(0x0005, 3);
/// METHOD_OUT_DIRECT = 2
pub const IOCTL_HID_GET_FEATURE: u32 = ctl_code(0x0006, 2);
/// METHOD_IN_DIRECT = 0
pub const IOCTL_HID_SET_FEATURE: u32 = ctl_code(0x0007, 0);
pub const IOCTL_HID_GET_INPUT_REPORT: u32 = ctl_code(0x0008, 2);
pub const IOCTL_HID_SET_OUTPUT_REPORT: u32 = ctl_code(0x0009, 0);

/// 键盘 HID 报告描述符：标准 Boot Keyboard 输入报告（8 字节）+ 厂商 8 字节输出管道。
/// 输出管道（Usage Undefined）专供用户态注入：WriteFile 的 8 字节报告经
/// IOCTL_HID_WRITE_REPORT 到达 minidriver，被当作键盘输入报告投递。
pub static KEYBOARD_REPORT_DESCRIPTOR: [u8; 60] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x06, // Usage (Keyboard)
    0xA1, 0x01, // Collection (Application)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0xE0, //   Usage Minimum (224)
    0x29, 0xE7, //   Usage Maximum (231)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x01, //   Logical Maximum (1)
    0x75, 0x01, //   Report Size (1)
    0x95, 0x08, //   Report Count (8)
    0x81, 0x02, //   Input (Data, Variable, Absolute) —— 修饰键
    0x95, 0x01, //   Report Count (1)
    0x75, 0x08, //   Report Size (8)
    0x81, 0x01, //   Input (Constant) —— 保留
    0x95, 0x06, //   Report Count (6)
    0x75, 0x08, //   Report Size (8)
    0x15, 0x00, //   Logical Minimum (0)
    0x25, 0x65, //   Logical Maximum (101)
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x65, //   Usage Maximum (101)
    0x81, 0x00, //   Input (Data, Array) —— 按键码
    0x05, 0x01, //   Usage Page (Generic Desktop)
    0x09, 0x00, //   Usage (Undefined)
    0x15, 0x00, //   Logical Minimum (0)
    0x26, 0xFF, 0x00, //   Logical Maximum (255)
    0x75, 0x08, //   Report Size (8)
    0x95, 0x08, //   Report Count (8)
    0x91, 0x00, //   Output (Data, Array, Absolute) —— 注入管道
    0xC0, // End Collection
];
