//! vdev-hid — 虚拟 HID：在 macOS 上用 CGEventPost 合成键盘/鼠标事件。
//!
//! 不需要 kext / DriverKit，也不需要「辅助功能」权限（合成事件系统默认放行）。

pub mod keycodes;

use anyhow::{anyhow, Result};
use cgevents::{KeyEvent, ModifierFlags, MouseEvent, Point, ScrollEvent, TapLocation};
use std::thread;
use std::time::Duration;

pub use cgevents::{Keycode, MouseButton};

/// 事件注入位置：HID 会话层，全局生效。
const LOCATION: TapLocation = TapLocation::Hid;

/// 按键间隔，让接收方稳定识别 down/up。
const GAP: Duration = Duration::from_millis(12);

fn err(e: cgevents::CGError) -> anyhow::Error {
    anyhow!("cgevents error: {e:?}")
}

/// 按住或松开一个键。
pub fn key(keycode: u16, pressed: bool) -> Result<()> {
    let ev = if pressed {
        KeyEvent::down(keycode)
    } else {
        KeyEvent::up(keycode)
    };
    ev.post(LOCATION).map_err(err)?;
    Ok(())
}

/// 点按一个键（按下 + 松开），可带修饰键。
pub fn tap_key(keycode: u16, modifiers: ModifierFlags) -> Result<()> {
    KeyEvent::down(keycode)
        .with_modifiers(modifiers)
        .post(LOCATION)
        .map_err(err)?;
    thread::sleep(GAP);
    KeyEvent::up(keycode)
        .with_modifiers(modifiers)
        .post(LOCATION)
        .map_err(err)?;
    Ok(())
}

/// 输入一段文本（Unicode 走 unicode string 通道，可输入中文等）。
pub fn type_text(text: &str) -> Result<()> {
    cgevents::type_string(text, LOCATION).map_err(err)
}

/// 移动鼠标到绝对坐标（点坐标，原点左上）。
pub fn mouse_move(x: f64, y: f64) -> Result<()> {
    MouseEvent::move_to(Point::new(x, y))
        .post(LOCATION)
        .map_err(err)
}

/// 点击：先移动到目标再按下/松开。
pub fn mouse_click(x: f64, y: f64, button: MouseButton) -> Result<()> {
    mouse_move(x, y)?;
    MouseEvent::button_down(Point::new(x, y), button)
        .post(LOCATION)
        .map_err(err)?;
    thread::sleep(GAP);
    MouseEvent::button_up(Point::new(x, y), button)
        .post(LOCATION)
        .map_err(err)?;
    Ok(())
}

/// 滚轮：delta_y 为正向上滚（行单位）。
pub fn scroll(delta_y: i32) -> Result<()> {
    ScrollEvent::lines(delta_y).post(LOCATION).map_err(err)
}

/// 把修饰键名字符串列表解析成 `ModifierFlags`。
pub fn parse_modifiers(names: &[String]) -> Result<ModifierFlags> {
    let mut flags = ModifierFlags::empty();
    for name in names {
        match name.to_ascii_lowercase().as_str() {
            "shift" => flags |= ModifierFlags::SHIFT,
            "cmd" | "command" => flags |= ModifierFlags::COMMAND,
            "ctrl" | "control" => flags |= ModifierFlags::CONTROL,
            "alt" | "option" => flags |= ModifierFlags::ALTERNATE,
            other => {
                return Err(anyhow!(
                    "unknown modifier: {other} (expect shift/cmd/ctrl/alt)"
                ))
            }
        }
    }
    Ok(flags)
}
