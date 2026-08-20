//! macOS 虚拟键码表（US QWERTY），用于 CLI 按名字查键码。
//! 参考：Apple 的 kVK_* 常量。

use cgevents::Keycode;

/// 常见键名 → 虚拟键码。
pub fn by_name(name: &str) -> Option<u16> {
    let n = name.to_ascii_lowercase();
    Some(match n.as_str() {
        // 字母
        "a" => Keycode::A,
        "b" => Keycode::B,
        "c" => Keycode::C,
        "d" => Keycode::D,
        "e" => Keycode::E,
        "f" => Keycode::F,
        "g" => Keycode::G,
        "h" => Keycode::H,
        "i" => Keycode::I,
        "j" => Keycode::J,
        "k" => Keycode::K,
        "l" => Keycode::L,
        "m" => Keycode::M,
        "n" => Keycode::N,
        "o" => Keycode::O,
        "p" => Keycode::P,
        "q" => Keycode::Q,
        "r" => Keycode::R,
        "s" => Keycode::S,
        "t" => Keycode::T,
        "u" => Keycode::U,
        "v" => Keycode::V,
        "w" => Keycode::W,
        "x" => Keycode::X,
        "y" => Keycode::Y,
        "z" => Keycode::Z,
        // 数字
        "0" => 0x1D,
        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "5" => 0x17,
        "6" => 0x16,
        "7" => 0x1A,
        "8" => 0x1C,
        "9" => 0x19,
        // 标点（US 布局）
        "-" => 0x1B,
        "=" => 0x18,
        "[" => 0x21,
        "]" => 0x1E,
        "\\" => 0x2A,
        ";" => 0x29,
        "'" => 0x27,
        "`" => 0x32,
        "," => 0x2B,
        "." => 0x2F,
        "/" => 0x2C,
        // 功能键
        "f1" => Keycode::F1,
        "f2" => Keycode::F2,
        "f3" => Keycode::F3,
        "f4" => Keycode::F4,
        "f5" => 0x60,
        "f6" => 0x61,
        "f7" => 0x62,
        "f8" => 0x64,
        "f9" => 0x65,
        "f10" => 0x6D,
        "f11" => 0x67,
        "f12" => 0x6F,
        // 控制键
        "return" | "enter" => Keycode::RETURN,
        "tab" => Keycode::TAB,
        "space" | "spc" => Keycode::SPACE,
        "delete" | "backspace" => Keycode::DELETE,
        "escape" | "esc" => Keycode::ESCAPE,
        "command" | "cmd" => Keycode::COMMAND,
        "shift" => Keycode::SHIFT,
        "capslock" | "caps" => Keycode::CAPS_LOCK,
        "option" | "alt" => Keycode::OPTION,
        "control" | "ctrl" => Keycode::CONTROL,
        // 方向键
        "up" => Keycode::ARROW_UP,
        "down" => Keycode::ARROW_DOWN,
        "left" => Keycode::ARROW_LEFT,
        "right" => Keycode::ARROW_RIGHT,
        _ => return None,
    })
}

/// 所有可解析的键名（用于帮助信息）。
pub const NAMES: &[&str] = &[
    "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r",
    "s", "t", "u", "v", "w", "x", "y", "z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9",
    "-", "=", "[", "]", "\\", ";", "'", "`", ",", ".", "/", "f1", "f2", "f3", "f4", "f5", "f6",
    "f7", "f8", "f9", "f10", "f11", "f12", "return", "tab", "space", "delete", "escape",
    "command", "shift", "capslock", "option", "control", "up", "down", "left", "right",
];
