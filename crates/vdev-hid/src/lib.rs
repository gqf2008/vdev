//! vdev-hid — 虚拟 HID：在 macOS 上用 `CGEventPost` 合成键盘/鼠标事件。
//!
//! 不需要 kext / DriverKit，也不需要「辅助功能」权限（合成事件系统默认放行）。

pub mod keycodes;

use anyhow::{anyhow, Result};
use cgevents::{
    CGEventType, EventTap, KeyEvent, ModifierFlags, MouseEvent, Point, ScrollEvent, TapAction,
    TapLocation,
};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

pub use cgevents::{Keycode, MouseButton};

/// 事件注入位置：HID 会话层，全局生效。
const LOCATION: TapLocation = TapLocation::Hid;

/// 按键间隔，让接收方稳定识别 down/up。
const GAP: Duration = Duration::from_millis(12);

fn err(e: &cgevents::CGError) -> anyhow::Error {
    anyhow!("cgevents error: {e:?}")
}

/// 按住或松开一个键。
pub fn key(keycode: u16, pressed: bool) -> Result<()> {
    let ev = if pressed {
        KeyEvent::down(keycode)
    } else {
        KeyEvent::up(keycode)
    };
    ev.post(LOCATION).map_err(|e| err(&e))?;
    Ok(())
}

/// 点按一个键（按下 + 松开），可带修饰键。
pub fn tap_key(keycode: u16, modifiers: ModifierFlags) -> Result<()> {
    KeyEvent::down(keycode)
        .with_modifiers(modifiers)
        .post(LOCATION)
        .map_err(|e| err(&e))?;
    thread::sleep(GAP);
    KeyEvent::up(keycode)
        .with_modifiers(modifiers)
        .post(LOCATION)
        .map_err(|e| err(&e))?;
    Ok(())
}

/// 输入一段文本（Unicode 走 unicode string 通道，可输入中文等）。
///
/// 逐字符 paced 发射：`cgevents::type_string` 把每个字符的 down/up 背靠背
/// 连发（都用 keycode 0），macOS 26 实测会被 `WindowServer` 合并/丢弃——同一
/// 探针窗下 `"q1q1"` 只到达 0–2 个字符，而单键 `tap_key`（down/up 间有
/// `GAP`）稳定得多。这里与 `tap_key` 对齐：down 后 `GAP`、up 后 `GAP`
/// 再发下一个字符。
pub fn type_text(text: &str) -> Result<()> {
    // 整串共用一个 private EventSource（与原 cgevents::type_string 的资源口径一致），
    // 但按上面的节拍逐字符发射；每字符约 2*GAP，长文本按字符数线性耗时。
    let source = cgevents::EventSource::private().map_err(|e| err(&e))?;
    for ch in text.chars() {
        let chunk = ch.to_string();
        KeyEvent::down(0)
            .with_unicode(&chunk)
            .build(&source)
            .map_err(|e| err(&e))?
            .post(LOCATION);
        thread::sleep(GAP);
        KeyEvent::up(0)
            .with_unicode(&chunk)
            .build(&source)
            .map_err(|e| err(&e))?
            .post(LOCATION);
        thread::sleep(GAP);
    }
    Ok(())
}

/// 移动鼠标到绝对坐标（点坐标，原点左上）。
pub fn mouse_move(x: f64, y: f64) -> Result<()> {
    MouseEvent::move_to(Point::new(x, y))
        .post(LOCATION)
        .map_err(|e| err(&e))
}

/// 点击：先移动到目标再按下/松开。
pub fn mouse_click(x: f64, y: f64, button: MouseButton) -> Result<()> {
    mouse_move(x, y)?;
    MouseEvent::button_down(Point::new(x, y), button)
        .post(LOCATION)
        .map_err(|e| err(&e))?;
    thread::sleep(GAP);
    MouseEvent::button_up(Point::new(x, y), button)
        .post(LOCATION)
        .map_err(|e| err(&e))?;
    Ok(())
}

/// 滚轮：`delta_y` 为正向上滚（行单位）。
pub fn scroll(delta_y: i32) -> Result<()> {
    ScrollEvent::lines(delta_y)
        .post(LOCATION)
        .map_err(|e| err(&e))
}

/// 监听键盘/鼠标事件（需要「辅助功能」权限），`Some(seconds)` 秒后自动退出；
/// `None` 表示无超时，一直监听（Ctrl-C 结束）。
///
/// 注意：cgevents 的 Swift 桥在 `EventTap::new` 时把 run loop source 挂到
/// **当前线程**（创建线程）的 run loop 上，因此创建与 `run()` 必须放在同一个
/// 专用线程；主线程只做计时，到点调 `tap.stop()`（stop 停的正是创建线程的
/// run loop，线程安全）。事件收发依赖真实 `EventTap` + 系统权限，无法单测。
pub fn listen(seconds: Option<u64>) -> Result<()> {
    if !EventTap::preflight_listen_access() {
        let _ = EventTap::request_listen_access();
        return Err(anyhow!(
            "需要「辅助功能」权限：请在 系统设置 → 隐私与安全性 → 辅助功能 中勾选当前终端，然后重试。"
        ));
    }

    // 创建与运行同线程（见函数注释）；创建结果经 channel 交还主线程，
    // 成功后主线程持有 Arc 句柄用于到点 stop。
    let (tx, rx) = mpsc::channel::<Result<Arc<EventTap>>>();
    let handle = thread::spawn(move || {
        let tap = match EventTap::new(
            TapLocation::Session,
            cgevents::CG_EVENT_MASK_FOR_ALL_EVENTS,
            |ev| {
                let ty = ev.event_type_typed();
                match ty {
                    Some(CGEventType::KeyDown | CGEventType::KeyUp | CGEventType::FlagsChanged) => {
                        println!(
                            "[key] {ty:?} code=0x{:02x} flags={:?}",
                            ev.keycode(),
                            ev.flags()
                        );
                    }
                    Some(
                        CGEventType::MouseMoved
                        | CGEventType::LeftMouseDown
                        | CGEventType::LeftMouseUp
                        | CGEventType::RightMouseDown
                        | CGEventType::RightMouseUp
                        | CGEventType::LeftMouseDragged
                        | CGEventType::RightMouseDragged,
                    ) => {
                        let p = ev.location();
                        println!("[mouse] {ty:?} at=({:.0},{:.0})", p.x, p.y);
                    }
                    Some(CGEventType::ScrollWheel) => {
                        println!("[scroll]");
                    }
                    _ => {}
                }
                TapAction::Pass
            },
        ) {
            Ok(tap) => Arc::new(tap),
            Err(e) => {
                let _ = tx.send(Err(err(&e)));
                return;
            }
        };
        if tx.send(Ok(tap.clone())).is_ok() {
            tap.run(); // 阻塞于本线程 run loop，直到 stop() 或 Ctrl-C 终止进程
        }
    });

    let tap = match rx.recv() {
        Ok(Ok(tap)) => tap,
        Ok(Err(e)) => {
            let _ = handle.join();
            return Err(e);
        }
        Err(_) => {
            let _ = handle.join();
            return Err(anyhow!("监听线程在创建 EventTap 前意外退出"));
        }
    };

    match seconds {
        Some(s) => {
            println!("监听 HID 事件中（{s}s 后自动退出，Ctrl-C 可提前结束）…");
            thread::sleep(Duration::from_secs(s));
            println!("超时，退出");
            tap.stop();
        }
        None => println!("监听 HID 事件中（无超时，Ctrl-C 可结束）…"),
    }
    // 无超时时 join 即一直等待：run() 不会自行返回，进程由 Ctrl-C（SIGINT）终止。
    handle.join().map_err(|_| anyhow!("监听线程意外退出"))?;
    Ok(())
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

/// 所有可能的键名（含别名），供 CLI 帮助信息展示；与 `keycodes::NAMES` 一致。
#[must_use]
pub fn key_names() -> &'static [&'static str] {
    keycodes::NAMES
}
