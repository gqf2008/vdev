//! macOS 虚拟键码表（US QWERTY），用于 CLI 按名字查键码。
//! 参考：Apple 的 kVK_* 常量。

use cgevents::Keycode;

/// 常见键名 → 虚拟键码。
#[must_use]
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

/// 所有可解析的键名（含别名），供帮助信息展示；
/// 须与 `by_name` 支持的键集合一致（由下方单测保证）。
pub(crate) const NAMES: &[&str] = &[
    "a",
    "b",
    "c",
    "d",
    "e",
    "f",
    "g",
    "h",
    "i",
    "j",
    "k",
    "l",
    "m",
    "n",
    "o",
    "p",
    "q",
    "r",
    "s",
    "t",
    "u",
    "v",
    "w",
    "x",
    "y",
    "z",
    "0",
    "1",
    "2",
    "3",
    "4",
    "5",
    "6",
    "7",
    "8",
    "9",
    "-",
    "=",
    "[",
    "]",
    "\\",
    ";",
    "'",
    "`",
    ",",
    ".",
    "/",
    "f1",
    "f2",
    "f3",
    "f4",
    "f5",
    "f6",
    "f7",
    "f8",
    "f9",
    "f10",
    "f11",
    "f12",
    "return",
    "enter",
    "tab",
    "space",
    "spc",
    "delete",
    "backspace",
    "escape",
    "esc",
    "command",
    "cmd",
    "shift",
    "capslock",
    "caps",
    "option",
    "alt",
    "control",
    "ctrl",
    "up",
    "down",
    "left",
    "right",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：NAMES 是帮助信息展示的键名列表，每一项都必须能被 `by_name` 解析，
    /// 否则用户照 help 输入仍会得到 "unknown key"；且 `by_name` 的全部别名
    /// （enter/spc/backspace/esc/cmd/caps/alt/ctrl）必须收录，否则帮助信息缺失。
    /// （`by_name` 的 match 臂无法程序化枚举，反向全覆盖检查做不了，只能点名别名。）
    #[test]
    fn names_all_resolve_via_by_name() {
        for name in NAMES {
            assert!(
                by_name(name).is_some(),
                "NAMES 中的 {name:?} 无法被 by_name 解析"
            );
        }
        for alias in [
            "enter",
            "spc",
            "backspace",
            "esc",
            "cmd",
            "caps",
            "alt",
            "ctrl",
        ] {
            assert!(
                NAMES.contains(&alias),
                "by_name 支持的别名 {alias:?} 未收录进 NAMES"
            );
        }
    }
}
