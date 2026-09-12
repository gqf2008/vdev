//! windows-free 纯逻辑：键名 → HID usage/修饰位、报告组装、HWID 匹配。
//! 本模块不依赖 `windows`/`wdk-sys`（cfg 门控对称：宿主 macOS 与 Windows 目标
//! 同样编译），单测随宿主 `cargo test` 直接运行。
//! 报告描述符字节与 IOCTL 常量经 `#[path]` 引入内核驱动的 windows-free 契约模块，
//! 两侧共享同一份定义（单一事实来源）。

// 非 Windows 目标：本模块仅被单测消费（消费方 kernel.rs/run 均为 windows 门控）
#![cfg_attr(not(windows), allow(dead_code))]

use anyhow::{Result, bail};

// 内核驱动侧契约（纯逻辑，无 wdk-sys 依赖）：IOCTL 常量 + 报告描述符字节
#[path = "../kernel/driver/src/contract.rs"]
pub mod hid_contract;

/// 8 字节键盘报告：1 修饰键 + 1 保留 + 6 按键
pub const KEYBOARD_REPORT_SIZE: usize = 8;
/// 鼠标报告长度：1 键位 + X + Y + 滚轮
pub const MOUSE_REPORT_SIZE: usize = 4;

/// 键盘 HID 设备 PID（"HI"）
pub const PID_KBD: u16 = 0x4849;
/// 鼠标 HID 设备 PID（"HM"）
pub const PID_MOUSE: u16 = 0x484D;

/// 键名 → （修饰键位, HID 用法码）
///
/// HID usage 值依 USB HID Usage Tables（Keyboard/Keypad Page）：
/// '1'-'9' = 0x1E..0x26、'0' = 0x27、字母 = 0x04..、F13-F24 = 0x68..0x73。
pub fn key_to_hid(name: &str) -> Result<(u8, Option<u8>)> {
    let n = name.to_ascii_lowercase();
    // 修饰键：报告第 0 字节位（直接返回，不占按键槽）
    match n.as_str() {
        "ctrl" | "control" => return Ok((0x01, None)),
        "shift" => return Ok((0x02, None)),
        "alt" => return Ok((0x04, None)),
        "win" | "lwin" => return Ok((0x08, None)),
        _ => {}
    }
    // 单字母 a-z -> 0x04..
    if n.len() == 1
        && let Some(c) = n.chars().next()
    {
        if c.is_ascii_lowercase() {
            return Ok((0, Some(0x04 + (c as u8 - b'a'))));
        }
        if c.is_ascii_digit() {
            // 数字行：'1'..'9' → 0x1E..0x26；'0' 特判为 0x27
            //（修复前统一 0x1E+(c-'0')，'0' 错映射 0x1E 且 '1'-'9' 整体偏移 +1）
            let usage = if c == '0' {
                0x27
            } else {
                0x1E + (c as u8 - b'1')
            };
            return Ok((0, Some(usage)));
        }
    }
    // F1-F24
    if let Some(f) = n.strip_prefix('f')
        && let Ok(num) = f.parse::<u16>()
        && (1..=24).contains(&num)
    {
        let usage = if num <= 12 {
            0x3A + num - 1
        } else {
            0x68 + num - 13
        };
        return Ok((0, Some(usage as u8)));
    }
    let map: &[(&str, u8)] = &[
        ("enter", 0x28),
        ("return", 0x28),
        ("esc", 0x29),
        ("escape", 0x29),
        ("backspace", 0x2A),
        ("tab", 0x2B),
        ("space", 0x2C),
        ("minus", 0x2D),
        ("equal", 0x2E),
        ("lbrace", 0x2F),
        ("rbrace", 0x30),
        ("backslash", 0x31),
        ("semicolon", 0x33),
        ("quote", 0x34),
        ("grave", 0x35),
        ("comma", 0x36),
        ("period", 0x37),
        ("slash", 0x38),
        ("capslock", 0x39),
        ("caps", 0x39),
        ("f1", 0x3A),
        ("f2", 0x3B),
        ("f3", 0x3C),
        ("f4", 0x3D),
        ("f5", 0x3E),
        ("f6", 0x3F),
        ("f7", 0x40),
        ("f8", 0x41),
        ("f9", 0x42),
        ("f10", 0x43),
        ("f11", 0x44),
        ("f12", 0x45),
        ("printscreen", 0x46),
        ("scrolllock", 0x47),
        ("pause", 0x48),
        ("insert", 0x49),
        ("ins", 0x49),
        ("home", 0x4A),
        ("pageup", 0x4B),
        ("delete", 0x4C),
        ("del", 0x4C),
        ("end", 0x4D),
        ("pagedown", 0x4E),
        ("right", 0x4F),
        ("left", 0x50),
        ("down", 0x51),
        ("up", 0x52),
        ("numlock", 0x53),
        ("numpad0", 0x62),
        ("numpad1", 0x59),
        ("numpad2", 0x5A),
        ("numpad3", 0x5B),
        ("numpad4", 0x5C),
        ("numpad5", 0x5D),
        ("numpad6", 0x5E),
        ("numpad7", 0x5F),
        ("numpad8", 0x60),
        ("numpad9", 0x61),
    ];
    if let Some((_, usage)) = map.iter().find(|(k, _)| *k == n.as_str()) {
        return Ok((0, Some(*usage)));
    }
    bail!(
        "未知键名：{name}（支持 a-z/0-9/F1-F24/enter/tab/space/esc/backspace/arrows/ctrl/alt/shift/win 等）"
    )
}

/// 构造键盘报告：mods 为修饰位，usage 为按键（None 表示纯修饰键）
pub fn make_report(mods: u8, usage: Option<u8>) -> [u8; KEYBOARD_REPORT_SIZE] {
    let mut r = [0u8; KEYBOARD_REPORT_SIZE];
    r[0] = mods;
    if let Some(u) = usage {
        r[2] = u;
    }
    r
}

/// 构造 4 字节鼠标报告（键位 + 相对 X/Y + 滚轮，带符号）
pub fn mouse_report(buttons: u8, dx: i8, dy: i8, wheel: i8) -> [u8; MOUSE_REPORT_SIZE] {
    [buttons, dx as u8, dy as u8, wheel as u8]
}

/// 鼠标按键位（HID：bit0 左 / bit1 右 / bit2 中）
pub fn mouse_button_bit(button: &str) -> Result<u8> {
    match button.to_ascii_lowercase().as_str() {
        "left" => Ok(0x01),
        "right" => Ok(0x02),
        "middle" => Ok(0x04),
        _ => bail!("未知鼠标按键：{button}（left/right/middle）"),
    }
}

/// u16 宽字符的 ASCII 小写化（HWID 为 ASCII；u16 无 to_ascii_lowercase）
fn ascii_lower_u16(c: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&c) {
        c + 0x20
    } else {
        c
    }
}

/// PnP 硬件 ID 匹配：`haystack` 为 REG_MULTI_SZ 风格（NUL 分隔、双 NUL 结尾）宽字符串，
/// `needle` 为待匹配 HWID。逐段**精确**比较（大小写不敏感）。
///
/// 修复记录：原实现用子串包含匹配——"Root\vdev-hid" 是 "Root\vdev-hid-mouse"
/// 的前缀，会把鼠标节点误配成键盘；PnP 的 HWID 比较大小写不敏感，这里对齐。
pub fn hwid_matches(haystack: &[u16], needle: &str) -> bool {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    let mut start = 0usize;
    for (i, &ch) in haystack.iter().enumerate() {
        if ch == 0 {
            let part = &haystack[start..i];
            if part.len() == needle.len()
                && part
                    .iter()
                    .zip(needle.iter())
                    .all(|(a, b)| ascii_lower_u16(*a) == ascii_lower_u16(*b))
            {
                return true;
            }
            start = i + 1;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn multi(parts: &[&str]) -> Vec<u16> {
        let mut v: Vec<u16> = Vec::new();
        for p in parts {
            v.extend(p.encode_utf16());
            v.push(0);
        }
        v.push(0);
        v
    }

    /// M2 回归：数字行 HID usage（修复前 '0'→0x1E、'9'→0x27，整体错位）
    #[test]
    fn key_to_hid_digits() {
        assert_eq!(key_to_hid("1").unwrap(), (0, Some(0x1E)));
        assert_eq!(key_to_hid("5").unwrap(), (0, Some(0x22)));
        assert_eq!(key_to_hid("9").unwrap(), (0, Some(0x26)));
        assert_eq!(key_to_hid("0").unwrap(), (0, Some(0x27)));
    }

    /// M5 回归：F13-F24 注入为 0x68-0x73
    #[test]
    fn key_to_hid_f13_f24() {
        assert_eq!(key_to_hid("f13").unwrap(), (0, Some(0x68)));
        assert_eq!(key_to_hid("f24").unwrap(), (0, Some(0x73)));
        assert_eq!(key_to_hid("f1").unwrap(), (0, Some(0x3A)));
        assert_eq!(key_to_hid("f12").unwrap(), (0, Some(0x45)));
    }

    /// 2850f01 既有测试（随纯逻辑迁移自 src/kernel.rs）
    #[test]
    fn key_to_hid_letters_and_modifiers() {
        assert_eq!(key_to_hid("a").unwrap(), (0, Some(0x04)));
        assert_eq!(key_to_hid("z").unwrap(), (0, Some(0x1D)));
        assert_eq!(key_to_hid("A").unwrap(), (0, Some(0x04)));
        assert_eq!(key_to_hid("ctrl").unwrap(), (0x01, None));
        assert_eq!(key_to_hid("shift").unwrap(), (0x02, None));
        assert_eq!(key_to_hid("alt").unwrap(), (0x04, None));
        assert_eq!(key_to_hid("win").unwrap(), (0x08, None));
        assert_eq!(key_to_hid("enter").unwrap(), (0, Some(0x28)));
        assert_eq!(key_to_hid("space").unwrap(), (0, Some(0x2C)));
        assert!(key_to_hid("zzz").is_err());
    }

    /// M5 回归：注入侧全部 usage 不得超过键盘描述符 Usage Max（0x73）
    #[test]
    fn all_key_usages_within_descriptor_range() {
        let usage_max = hid_contract::KEYBOARD_REPORT_DESCRIPTOR
            .windows(2)
            .find(|w| w[0] == 0x29 && w[1] != 0xE7)
            .map(|w| w[1])
            .expect("描述符应有按键 Usage Maximum 项");
        assert_eq!(usage_max, 0x73);
        for key in [
            "a",
            "z",
            "0",
            "1",
            "9",
            "enter",
            "space",
            "esc",
            "tab",
            "up",
            "left",
            "numpad0",
            "numpad9",
            "capslock",
            "printscreen",
            "pause",
            "f1",
            "f12",
            "f13",
            "f24",
        ] {
            let (mods, usage) = key_to_hid(key).unwrap();
            assert_eq!(mods, 0);
            let usage = usage.expect("{key} 应有 usage");
            assert!(
                usage <= usage_max,
                "{key} usage {usage:#x} 超出描述符 Usage Max {usage_max:#x}"
            );
        }
    }

    /// 2850f01 既有测试：报告布局
    #[test]
    fn make_report_layout() {
        assert_eq!(
            make_report(0x03, Some(0x04)),
            [0x03, 0, 0x04, 0, 0, 0, 0, 0]
        );
        assert_eq!(make_report(0, None), [0; 8]);
    }

    /// 2850f01 既有测试：鼠标报告布局
    #[test]
    fn mouse_report_layout() {
        // 键位 + X(10) + Y(-5) + 滚轮(1)
        assert_eq!(mouse_report(0x01, 10, -5, 1), [0x01, 10, 251, 1]);
        assert_eq!(mouse_report(0, 0, 0, 0), [0; 4]);
    }

    /// 2850f01 既有测试：鼠标按键位
    #[test]
    fn mouse_button_bits() {
        assert_eq!(mouse_button_bit("left").unwrap(), 0x01);
        assert_eq!(mouse_button_bit("right").unwrap(), 0x02);
        assert_eq!(mouse_button_bit("middle").unwrap(), 0x04);
        assert!(mouse_button_bit("x1").is_err());
    }

    /// M10 回归：HWID 精确匹配（前缀不误配 + 大小写不敏感）
    #[test]
    fn hwid_exact_matching() {
        // "Root\vdev-hid" 不得匹配到鼠标节点 "Root\vdev-hid-mouse"（原子串匹配的缺陷）
        assert!(!hwid_matches(
            &multi(&["Root\\vdev-hid-mouse"]),
            "Root\\vdev-hid"
        ));
        assert!(hwid_matches(&multi(&["Root\\vdev-hid"]), "Root\\vdev-hid"));
        // 大小写不敏感（PnP 比较语义）
        assert!(hwid_matches(&multi(&["ROOT\\VDEV-HID"]), "root\\vdev-hid"));
        assert!(hwid_matches(
            &multi(&["Root\\vdev-hid", "Root\\vdev-hid-mouse"]),
            "root\\VDEV-HID-MOUSE"
        ));
        // 非 HWID 段不误配
        assert!(!hwid_matches(
            &multi(&["USB\\VID_1234&PID_5678"]),
            "Root\\vdev-hid"
        ));
    }
}
