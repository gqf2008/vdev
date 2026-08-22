//! vdev 虚拟 HID（Windows）：SendInput 事件注入（路线 A，用户态）。
//! 等价 macOS vdev-hid 的 CGEventPost 语义：向系统注入键盘/鼠标事件。

#![allow(clippy::missing_errors_doc)]

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP,
    KEYEVENTF_UNICODE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_LEFTDOWN,
    MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, SendInput,
    VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
};

/// 按键事件（down/up/tap）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    Down,
    Up,
    Tap,
}

/// 鼠标按键
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}

/// 鼠标动作
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Down,
    Up,
    Click,
}

fn keyboard_input(vk: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    let mut ki = KEYBDINPUT {
        wVk: VIRTUAL_KEY(vk),
        wScan: 0,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    // 非 Unicode 键用扫描码更可靠
    if !flags.contains(KEYEVENTF_UNICODE) {
        ki.wScan = 0;
    }
    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: unsafe { std::mem::zeroed() },
    };
    input.Anonymous.ki = ki;
    input
}

fn unicode_input(ch: u16) -> INPUT {
    let ki = KEYBDINPUT {
        wVk: VIRTUAL_KEY(0),
        wScan: ch,
        dwFlags: KEYEVENTF_UNICODE,
        time: 0,
        dwExtraInfo: 0,
    };
    let mut input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: unsafe { std::mem::zeroed() },
    };
    input.Anonymous.ki = ki;
    input
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: u32) -> INPUT {
    let mi = MOUSEINPUT {
        dx,
        dy,
        mouseData: data,
        dwFlags: flags,
        time: 0,
        dwExtraInfo: 0,
    };
    let mut input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: unsafe { std::mem::zeroed() },
    };
    input.Anonymous.mi = mi;
    input
}

fn dispatch(inputs: &[INPUT]) -> anyhow::Result<u32> {
    // SAFETY: inputs 均为有效初始化的 INPUT
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent == 0 {
        anyhow::bail!(
            "SendInput 失败（错误 {}）",
            windows::core::Error::from_win32()
        );
    }
    Ok(sent)
}

/// 发送虚拟键事件（可带修饰键）
///
/// # Errors
/// SendInput 失败时返回错误。
pub fn send_key(vk: u16, action: KeyAction) -> anyhow::Result<u32> {
    let mut inputs = Vec::new();
    match action {
        KeyAction::Down => inputs.push(keyboard_input(vk, KEYBD_EVENT_FLAGS(0))),
        KeyAction::Up => inputs.push(keyboard_input(vk, KEYEVENTF_KEYUP)),
        KeyAction::Tap => {
            inputs.push(keyboard_input(vk, KEYBD_EVENT_FLAGS(0)));
            inputs.push(keyboard_input(vk, KEYEVENTF_KEYUP));
        }
    }
    dispatch(&inputs)
}

/// 发送带修饰键的快捷键（ctrl/alt/shift）
///
/// # Errors
/// SendInput 失败时返回错误。
pub fn send_hotkey(modifiers: &[u16], vk: u16) -> anyhow::Result<u32> {
    let mut inputs = Vec::new();
    for m in modifiers {
        inputs.push(keyboard_input(*m, KEYBD_EVENT_FLAGS(0)));
    }
    inputs.push(keyboard_input(vk, KEYBD_EVENT_FLAGS(0)));
    inputs.push(keyboard_input(vk, KEYEVENTF_KEYUP));
    for m in modifiers.iter().rev() {
        inputs.push(keyboard_input(*m, KEYEVENTF_KEYUP));
    }
    dispatch(&inputs)
}

pub const MOD_CONTROL: u16 = VK_CONTROL.0;
pub const MOD_ALT: u16 = VK_MENU.0;
pub const MOD_SHIFT: u16 = VK_SHIFT.0;

/// 输入文本（UTF-16，支持中文等任意 Unicode；经 KEYEVENTF_UNICODE）
///
/// # Errors
/// SendInput 失败时返回错误。
pub fn send_text(text: &str) -> anyhow::Result<u32> {
    let mut inputs = Vec::new();
    for u in text.encode_utf16() {
        inputs.push(unicode_input(u));
        inputs.push(unicode_input(u)); // 上行未置 KEYUP；需显式 keyup
    }
    // 修正：偶数下标为 down、奇数下标为 up
    for (i, input) in inputs.iter_mut().enumerate() {
        if i % 2 == 1 && matches!(input.r#type, INPUT_KEYBOARD) {
            // SAFETY: input 已初始化为键盘输入，union 字段有效
            unsafe { input.Anonymous.ki.dwFlags |= KEYEVENTF_KEYUP };
        }
    }
    dispatch(&inputs)
}

/// 相对移动鼠标
///
/// # Errors
/// SendInput 失败时返回错误。
pub fn mouse_move_relative(dx: i32, dy: i32) -> anyhow::Result<u32> {
    dispatch(&[mouse_input(MOUSEEVENTF_MOVE, dx, dy, 0)])
}

/// 绝对移动鼠标（0..=65535 归一化坐标）
///
/// # Errors
/// SendInput 失败时返回错误。
pub fn mouse_move_absolute(x: u16, y: u16) -> anyhow::Result<u32> {
    dispatch(&[mouse_input(
        MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE,
        i32::from(x),
        i32::from(y),
        0,
    )])
}

fn button_down_flag(btn: MouseButton) -> MOUSE_EVENT_FLAGS {
    match btn {
        MouseButton::Left => MOUSEEVENTF_LEFTDOWN,
        MouseButton::Right => MOUSEEVENTF_RIGHTDOWN,
        MouseButton::Middle => MOUSEEVENTF_MIDDLEDOWN,
    }
}

fn button_up_flag(btn: MouseButton) -> MOUSE_EVENT_FLAGS {
    match btn {
        MouseButton::Left => MOUSEEVENTF_LEFTUP,
        MouseButton::Right => MOUSEEVENTF_RIGHTUP,
        MouseButton::Middle => MOUSEEVENTF_MIDDLEUP,
    }
}

/// 鼠标按键动作
///
/// # Errors
/// SendInput 失败时返回错误。
pub fn mouse_button(btn: MouseButton, action: MouseAction) -> anyhow::Result<u32> {
    let mut inputs = Vec::new();
    match action {
        MouseAction::Down => inputs.push(mouse_input(button_down_flag(btn), 0, 0, 0)),
        MouseAction::Up => inputs.push(mouse_input(button_up_flag(btn), 0, 0, 0)),
        MouseAction::Click => {
            inputs.push(mouse_input(button_down_flag(btn), 0, 0, 0));
            inputs.push(mouse_input(button_up_flag(btn), 0, 0, 0));
        }
    }
    dispatch(&inputs)
}

/// 鼠标滚轮（delta 为 120 的倍数）
///
/// # Errors
/// SendInput 失败时返回错误。
pub fn mouse_wheel(delta: i32) -> anyhow::Result<u32> {
    dispatch(&[mouse_input(MOUSEEVENTF_WHEEL, 0, 0, delta as u32)])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_tap_builds_down_up() {
        let inputs = [
            keyboard_input(0x41, KEYBD_EVENT_FLAGS(0)), // A down
            keyboard_input(0x41, KEYEVENTF_KEYUP),      // A up
        ];
        assert_eq!(inputs.len(), 2);
        assert_eq!(inputs[0].r#type, INPUT_KEYBOARD);
        assert_eq!(unsafe { inputs[0].Anonymous.ki.wVk }, VIRTUAL_KEY(0x41));
        assert_eq!(unsafe { inputs[1].Anonymous.ki.dwFlags }, KEYEVENTF_KEYUP);
    }

    #[test]
    fn unicode_input_uses_scan() {
        let input = unicode_input(0x4F60); // '你'
        assert_eq!(input.r#type, INPUT_KEYBOARD);
        assert_eq!(unsafe { input.Anonymous.ki.wScan }, 0x4F60);
        assert_eq!(unsafe { input.Anonymous.ki.dwFlags }, KEYEVENTF_UNICODE);
    }

    #[test]
    fn text_alternates_down_up() {
        let text = "ab";
        let mut inputs = Vec::new();
        for u in text.encode_utf16() {
            inputs.push(unicode_input(u));
            inputs.push(unicode_input(u));
        }
        for (i, input) in inputs.iter_mut().enumerate() {
            if i % 2 == 1 {
                unsafe { input.Anonymous.ki.dwFlags |= KEYEVENTF_KEYUP };
            }
        }
        assert!(!unsafe { inputs[0].Anonymous.ki.dwFlags }.contains(KEYEVENTF_KEYUP));
        assert!(unsafe { inputs[1].Anonymous.ki.dwFlags }.contains(KEYEVENTF_KEYUP));
    }

    #[test]
    fn mouse_button_flags() {
        assert_eq!(button_down_flag(MouseButton::Left), MOUSEEVENTF_LEFTDOWN);
        assert_eq!(button_up_flag(MouseButton::Right), MOUSEEVENTF_RIGHTUP);
    }
}
