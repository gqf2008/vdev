//! HID minidriver 契约中的纯逻辑部分：CTL_CODE/IOCTL 常量与报告描述符字节。
//!
//! 本模块不依赖 wdk-sys（windows-free），由内核驱动 `hid.rs` 与用户态 crate
//! （`src/report.rs` 经 `#[path]` 引入）共享同一份定义；`#[cfg(test)]` 单测在
//! macOS 宿主 `cargo test` 直接运行（cfg 门控两侧对称）。
//!
//! 出处：`hidport.h`（minidriver 内部 IOCTL 与 HID_DESCRIPTOR 布局）、
//! `hidclass.h`（HID_IN/OUT_CTL_CODE 与 feature/报告 IOCTL）、`winioctl.h`
//! （CTL_CODE 宏）、USB HID Usage Tables（Keyboard/Keypad Page usage 值）。

#![allow(dead_code)] // 常量以内核驱动消费为主；用户态 crate 仅单测引用

/// CTL_CODE(DeviceType, Function, Method, Access) —— winioctl.h：
/// `((DeviceType << 16) | (Access << 14) | (Function << 2) | Method)`
pub const fn ctl_code(device_type: u32, function: u32, method: u32, access: u32) -> u32 {
    (device_type << 16) | (access << 14) | (function << 2) | method
}

/// winioctl.h
pub const FILE_DEVICE_KEYBOARD: u32 = 0x0000_000B;
/// winioctl.h
pub const FILE_ANY_ACCESS: u32 = 0;
/// wdm.h：CTL_CODE 的 Method 值
pub const METHOD_BUFFERED: u32 = 0;
/// wdm.h：输入缓冲区直访（SystemBuffer 进、MDL 出）
pub const METHOD_IN_DIRECT: u32 = 1;
/// wdm.h：输出缓冲区直访
pub const METHOD_OUT_DIRECT: u32 = 2;
/// wdm.h：Type3InputBuffer / UserBuffer
pub const METHOD_NEITHER: u32 = 3;

/// hidport.h：`HID_CTL_CODE(id) = CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_NEITHER, FILE_ANY_ACCESS)`
const fn hid_ctl_code(id: u32) -> u32 {
    ctl_code(FILE_DEVICE_KEYBOARD, id, METHOD_NEITHER, FILE_ANY_ACCESS)
}

/// hidclass.h：`HID_IN_CTL_CODE(id) = CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_IN_DIRECT, FILE_ANY_ACCESS)`
const fn hid_in_ctl_code(id: u32) -> u32 {
    ctl_code(FILE_DEVICE_KEYBOARD, id, METHOD_IN_DIRECT, FILE_ANY_ACCESS)
}

/// hidclass.h：`HID_OUT_CTL_CODE(id) = CTL_CODE(FILE_DEVICE_KEYBOARD, id, METHOD_OUT_DIRECT, FILE_ANY_ACCESS)`
const fn hid_out_ctl_code(id: u32) -> u32 {
    ctl_code(FILE_DEVICE_KEYBOARD, id, METHOD_OUT_DIRECT, FILE_ANY_ACCESS)
}

// hidport.h "Internal IOCTLs for the class/mini driver interface"
//（Server 2003 DDK 与现行 WDK（wdkmetadata hidport.h）编号一致：
//  0/1/2/3/4/7=ACTIVATE/8=DEACTIVATE/9=ATTRIBUTES/10=IDLE，5/6 保留）
/// 取设备描述符（HID_DESCRIPTOR，METHOD_NEITHER）
pub const IOCTL_HID_GET_DEVICE_DESCRIPTOR: u32 = hid_ctl_code(0);
/// 取报告描述符（METHOD_NEITHER）
pub const IOCTL_HID_GET_REPORT_DESCRIPTOR: u32 = hid_ctl_code(1);
/// 读输入报告（METHOD_NEITHER）
pub const IOCTL_HID_READ_REPORT: u32 = hid_ctl_code(2);
/// 写报告（METHOD_NEITHER）
pub const IOCTL_HID_WRITE_REPORT: u32 = hid_ctl_code(3);
/// 取 HID 字符串（METHOD_NEITHER）
pub const IOCTL_HID_GET_STRING: u32 = hid_ctl_code(4);
/// 取设备属性（HID_DEVICE_ATTRIBUTES，METHOD_NEITHER；hidport.h 功能码 9）
pub const IOCTL_HID_GET_DEVICE_ATTRIBUTES: u32 = hid_ctl_code(9);

// feature / 报告 IOCTL 走 hidclass.h 的 HID_IN/OUT_CTL_CODE：功能码与用户态
// HidD_GetFeature(100)/HidD_SetFeature(100)/HidD_SetOutputReport(101)/
// HidD_GetInputReport(104) 同号——hidclass 原样转发给 minidriver
//（vhidmini2 vhidmini.c 的内核态分发 switch 同款：GET_FEATURE/GET_INPUT_REPORT
//  注释 METHOD_OUT_DIRECT，SET_FEATURE/SET_OUTPUT_REPORT 注释 METHOD_IN_DIRECT）。
/// HidD_GetFeature（METHOD_OUT_DIRECT）
pub const IOCTL_HID_GET_FEATURE: u32 = hid_out_ctl_code(100);
/// HidD_SetFeature（METHOD_IN_DIRECT）
pub const IOCTL_HID_SET_FEATURE: u32 = hid_in_ctl_code(100);
/// HidD_SetOutputReport（METHOD_IN_DIRECT）
pub const IOCTL_HID_SET_OUTPUT_REPORT: u32 = hid_in_ctl_code(101);
/// HidD_GetInputReport（METHOD_OUT_DIRECT）
pub const IOCTL_HID_GET_INPUT_REPORT: u32 = hid_out_ctl_code(104);

/// 键盘 HID 报告描述符：标准 Boot Keyboard 输入报告（8 字节）+ 厂商 8 字节输出管道。
/// 输出管道（Usage Undefined）专供用户态注入：WriteFile 的 8 字节报告经
/// IOCTL_HID_WRITE_REPORT 到达 minidriver，被当作键盘输入报告投递。
///
/// 按键码数组范围 0x00-0x73（115）：覆盖注入侧全部 usage——F13-F24 为 0x68-0x73
///（USB HID Usage Tables Keyboard/Keypad Page），其余键均 ≤0x65。
/// 修复记录：原 Usage/Logical Max 0x65 覆盖不了 F13+，注入 F13-F24 会被 hidclass 丢弃。
/// 注入管道（厂商自定义顶层集合）的字节序列，键盘 8 字节 / 鼠标 4 字节共用同一形状。
///
/// 为什么不把注入输出报告放进键盘/鼠标集合里：系统对**键盘/鼠标顶层集合（TLC）**
/// 做输出报告限制——HidD_SetOutputReport 与 WriteFile 一律返回
/// ERROR_INVALID_FUNCTION(1)，加不加管理员都一样（本机 Win10 19045 实测，8 字节与
/// 9 字节都试过）。把管道单独做成 0xFF00 厂商 TLC 后，用户态可正常读写、可写输出报告。
macro_rules! injection_pipe {
    ($count:expr) => {
        [
            0x06, 0x00, 0xFF, // Usage Page (Vendor-Defined 0xFF00)
            0x09, 0x01, // Usage (Vendor 1)
            0xA1, 0x01, // Collection (Application) —— 注入管道 TLC
            0x09, 0x01, //   Usage (Vendor 1)
            0x15, 0x00, //   Logical Minimum (0)
            0x26, 0xFF, 0x00, //   Logical Maximum (255)
            0x75, 0x08, //   Report Size (8)
            0x95, $count, //   Report Count
            0x91, 0x02, //   Output (Data, Variable, Absolute) —— 注入报告
            0xC0, // End Collection
        ]
    };
}

/// 键盘 HID 报告描述符：键盘 TLC（8 字节输入）+ 厂商 8 字节输出管道（注入）
pub static KEYBOARD_REPORT_DESCRIPTOR: [u8; 66] = [
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
    0x25, 0x73, //   Logical Maximum (115) —— 覆盖 F24
    0x05, 0x07, //   Usage Page (Key Codes)
    0x19, 0x00, //   Usage Minimum (0)
    0x29, 0x73, //   Usage Maximum (115) —— 覆盖 F13-F24
    0x81, 0x00, //   Input (Data, Array) —— 按键码
    0xC0, // End Collection
    0x06, 0x00, 0xFF, // 以下为注入管道（厂商 TLC），见 injection_pipe!
    0x09, 0x01, //
    0xA1, 0x01, //
    0x09, 0x01, //
    0x15, 0x00, //
    0x26, 0xFF, 0x00, //
    0x75, 0x08, //
    0x95, 0x08, //   Report Count (8) —— 键盘注入报告 8 字节
    0x91, 0x02, //
    0xC0, //
];

/// 鼠标 HID 报告描述符：鼠标 TLC（4 字节输入）+ 厂商 4 字节输出管道（注入）
pub static MOUSE_REPORT_DESCRIPTOR: [u8; 73] = [
    0x05, 0x01, // Usage Page (Generic Desktop)
    0x09, 0x02, // Usage (Mouse)
    0xA1, 0x01, // Collection (Application)
    0x09, 0x01, //   Usage (Pointer)
    0xA1, 0x00, //   Collection (Physical)
    0x05, 0x09, //     Usage Page (Buttons)
    0x19, 0x01, //     Usage Minimum (1)
    0x29, 0x03, //     Usage Maximum (3)
    0x15, 0x00, //     Logical Minimum (0)
    0x25, 0x01, //     Logical Maximum (1)
    0x95, 0x03, //     Report Count (3)
    0x75, 0x01, //     Report Size (1)
    0x81, 0x02, //     Input (Data, Variable, Absolute) —— 键位
    0x95, 0x01, //     Report Count (1)
    0x75, 0x05, //     Report Size (5)
    0x81, 0x01, //     Input (Constant)
    0x05, 0x01, //     Usage Page (Generic Desktop)
    0x09, 0x30, //     Usage (X)
    0x09, 0x31, //     Usage (Y)
    0x09, 0x38, //     Usage (Wheel)
    0x15, 0x81, //     Logical Minimum (-127)
    0x25, 0x7F, //     Logical Maximum (127)
    0x75, 0x08, //     Report Size (8)
    0x95, 0x03, //     Report Count (3)
    0x81, 0x06, //     Input (Data, Variable, Relative) —— X/Y/滚轮
    0xC0, //   End Collection (Physical)
    0xC0, // End Collection (Application)
    0x06, 0x00, 0xFF, // 以下为注入管道（厂商 TLC），见 injection_pipe!
    0x09, 0x01, //
    0xA1, 0x01, //
    0x09, 0x01, //
    0x15, 0x00, //
    0x26, 0xFF, 0x00, //
    0x75, 0x08, //
    0x95, 0x04, //   Report Count (4) —— 鼠标注入报告 4 字节
    0x91, 0x02, //
    0xC0, //
];

// 注入管道段的编译期形状自检：两条描述符的尾部必须与 injection_pipe! 一致
const _: () = {
    let kbd = &KEYBOARD_REPORT_DESCRIPTOR;
    let pipe = injection_pipe!(0x08);
    assert!(kbd.len() >= pipe.len());
    let mut i = 0;
    while i < pipe.len() {
        assert!(kbd[kbd.len() - pipe.len() + i] == pipe[i]);
        i += 1;
    }
    let mouse = &MOUSE_REPORT_DESCRIPTOR;
    let pipe4 = injection_pipe!(0x04);
    assert!(mouse.len() >= pipe4.len());
    let mut j = 0;
    while j < pipe4.len() {
        assert!(mouse[mouse.len() - pipe4.len() + j] == pipe4[j]);
        j += 1;
    }
};

#[cfg(test)]
mod tests {
    use super::*;

    /// M3 回归：IOCTL 常量须解码为 hidport.h/hidclass.h 规定的 CTL_CODE 等式。
    /// 修复前 GET_STRING/GET_DEVICE_ATTRIBUTES 功能码互换、feature/报告 IOCTL
    /// 用了不存在的功能码 6-9 且 Method 位为 0（METHOD_BUFFERED）。
    #[test]
    fn ioctl_decodes_to_ctl_code_equations() {
        // hidport.h：内部 IOCTL 均为 METHOD_NEITHER
        assert_eq!(
            IOCTL_HID_GET_DEVICE_DESCRIPTOR,
            ctl_code(FILE_DEVICE_KEYBOARD, 0, METHOD_NEITHER, FILE_ANY_ACCESS)
        );
        assert_eq!(IOCTL_HID_GET_DEVICE_DESCRIPTOR, 0x000B_0003);
        assert_eq!(IOCTL_HID_GET_REPORT_DESCRIPTOR, 0x000B_0007);
        assert_eq!(IOCTL_HID_READ_REPORT, 0x000B_000B);
        assert_eq!(IOCTL_HID_WRITE_REPORT, 0x000B_000F);
        assert_eq!(IOCTL_HID_GET_STRING, 0x000B_0013);
        assert_eq!(
            IOCTL_HID_GET_DEVICE_ATTRIBUTES,
            ctl_code(FILE_DEVICE_KEYBOARD, 9, METHOD_NEITHER, FILE_ANY_ACCESS)
        );
        assert_eq!(IOCTL_HID_GET_DEVICE_ATTRIBUTES, 0x000B_0027);
        // hidclass.h：METHOD_IN_DIRECT=1 / METHOD_OUT_DIRECT=2，功能码与用户态同号
        assert_eq!(
            IOCTL_HID_GET_FEATURE,
            ctl_code(
                FILE_DEVICE_KEYBOARD,
                100,
                METHOD_OUT_DIRECT,
                FILE_ANY_ACCESS
            )
        );
        assert_eq!(IOCTL_HID_GET_FEATURE, 0x000B_0192);
        assert_eq!(
            IOCTL_HID_SET_FEATURE,
            ctl_code(FILE_DEVICE_KEYBOARD, 100, METHOD_IN_DIRECT, FILE_ANY_ACCESS)
        );
        assert_eq!(IOCTL_HID_SET_FEATURE, 0x000B_0191);
        assert_eq!(IOCTL_HID_SET_OUTPUT_REPORT, 0x000B_0195);
        assert_eq!(IOCTL_HID_GET_INPUT_REPORT, 0x000B_01A2);
    }

    /// M3 回归：Method 位落在 CTL_CODE 的 bit[1:0]，Access 位（bit[15:14]）保持 0
    #[test]
    fn method_bits_do_not_leak_into_access_field() {
        for ioctl in [
            IOCTL_HID_GET_DEVICE_DESCRIPTOR,
            IOCTL_HID_GET_REPORT_DESCRIPTOR,
            IOCTL_HID_READ_REPORT,
            IOCTL_HID_WRITE_REPORT,
            IOCTL_HID_GET_STRING,
            IOCTL_HID_GET_DEVICE_ATTRIBUTES,
            IOCTL_HID_GET_FEATURE,
            IOCTL_HID_SET_FEATURE,
            IOCTL_HID_SET_OUTPUT_REPORT,
            IOCTL_HID_GET_INPUT_REPORT,
        ] {
            assert_eq!(
                ioctl >> 14 & 0x3,
                0,
                "Access 位须为 FILE_ANY_ACCESS: {ioctl:#x}"
            );
            assert_eq!(ioctl >> 16, FILE_DEVICE_KEYBOARD);
        }
    }

    /// M5 回归：键盘描述符 Usage/Logical Max 须覆盖注入侧最大 usage（F24=0x73）
    #[test]
    fn keyboard_descriptor_covers_f24() {
        assert_eq!(KEYBOARD_REPORT_DESCRIPTOR.len(), 66);
        // Usage Maximum (0x29 0x73) 与 Logical Maximum (0x25 0x73)
        assert!(
            KEYBOARD_REPORT_DESCRIPTOR
                .windows(2)
                .any(|w| w == [0x29, 0x73])
        );
        assert!(
            KEYBOARD_REPORT_DESCRIPTOR
                .windows(2)
                .any(|w| w == [0x25, 0x73])
        );
        // 修复前的 0x65 上限已不存在
        assert!(
            !KEYBOARD_REPORT_DESCRIPTOR
                .windows(2)
                .any(|w| w == [0x29, 0x65])
        );
        assert!(
            !KEYBOARD_REPORT_DESCRIPTOR
                .windows(2)
                .any(|w| w == [0x25, 0x65])
        );
    }

    /// 鼠标描述符长度稳定（驱动 statics 与 hidclass 依赖其字节数）
    #[test]
    fn mouse_descriptor_length() {
        assert_eq!(MOUSE_REPORT_DESCRIPTOR.len(), 73);
    }

    /// 注入管道必须是**独立厂商 TLC**：键盘/鼠标 TLC 里的输出报告会被系统拒绝
    /// （HidD_SetOutputReport/WriteFile 返回 ERROR_INVALID_FUNCTION，实测 Win10 19045）
    #[test]
    fn injection_pipe_is_separate_vendor_collection() {
        for (desc, count) in [
            (&KEYBOARD_REPORT_DESCRIPTOR[..], 0x08u8),
            (&MOUSE_REPORT_DESCRIPTOR[..], 0x04u8),
        ] {
            let pipe = injection_pipe!(count);
            assert_eq!(
                &desc[desc.len() - pipe.len()..],
                &pipe[..],
                "描述符尾部应为厂商注入管道"
            );
            // 管道以 Usage Page 0xFF00 开头（厂商自定义页）
            assert_eq!(&pipe[0..3], &[0x06, 0x00, 0xFF]);
        }
    }
}
