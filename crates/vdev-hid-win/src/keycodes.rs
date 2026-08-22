//! 键名 → 虚拟键码（VK_*）映射（大小写不敏感）
#![allow(clippy::missing_errors_doc)]

/// 解析键名到虚拟键码
pub fn key_to_vk(name: &str) -> Option<u16> {
    let n = name.to_ascii_lowercase();
    // 单字母 / 单数字
    if n.len() == 1
        && let Some(c) = n.chars().next()
    {
        if c.is_ascii_lowercase() {
            return Some((u32::from(c) - u32::from('a') + 0x41) as u16);
        }
        if c.is_ascii_digit() {
            return Some(c as u16); // '0'..'9' -> VK_0..VK_9 (0x30..0x39)
        }
    }
    // 功能键 F1-F24
    if let Some(f) = n.strip_prefix('f')
        && let Ok(num) = f.parse::<u16>()
        && (1..=24).contains(&num)
    {
        return Some(0x70 + num - 1);
    }
    let map: &[(&str, u16)] = &[
        ("enter", 0x0D),
        ("return", 0x0D),
        ("tab", 0x09),
        ("space", 0x20),
        ("esc", 0x1B),
        ("escape", 0x1B),
        ("backspace", 0x08),
        ("delete", 0x2E),
        ("del", 0x2E),
        ("home", 0x24),
        ("end", 0x23),
        ("pageup", 0x21),
        ("pagedown", 0x22),
        ("up", 0x26),
        ("down", 0x28),
        ("left", 0x25),
        ("right", 0x27),
        ("ctrl", 0x11),
        ("control", 0x11),
        ("alt", 0x12),
        ("shift", 0x10),
        ("win", 0x5B),
        ("lwin", 0x5B),
        ("capslock", 0x14),
        ("caps", 0x14),
        ("insert", 0x2D),
        ("ins", 0x2D),
        ("printscreen", 0x2C),
        ("scrolllock", 0x91),
        ("paus", 0x13),
        ("pause", 0x13),
        ("numlock", 0x90),
        ("semicolon", 0xBA),
        ("comma", 0xBC),
        ("period", 0xBE),
        ("minus", 0xBD),
        ("plus", 0xBB),
        ("slash", 0xBF),
        ("backslash", 0xDC),
        ("quote", 0xDE),
        ("bracketleft", 0xDB),
        ("bracketright", 0xDD),
        ("grave", 0xC0),
    ];
    map.iter().find(|(k, _)| *k == n).map(|(_, v)| *v)
}
