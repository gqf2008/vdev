//! vdev-hid-win：Windows 虚拟 HID（SendInput 注入）CLI。

mod kernel;
mod keycodes;

use anyhow::{Context as _, Result, bail};
use clap::{Parser, Subcommand};
use vdev_hid_win::{
    KeyAction, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MouseAction, MouseButton, mouse_button,
    mouse_move_absolute, mouse_move_relative, mouse_wheel, send_hotkey, send_key, send_text,
};

#[derive(Parser)]
#[command(
    name = "vdev-hid-win",
    about = "vdev 虚拟 HID（Windows）：SendInput 键盘/鼠标事件注入"
)]
struct Args {
    /// JSON 输出
    #[arg(short, long)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 发送按键（默认 tap）
    Key {
        /// 键名（a-z/0-9/F1-F24/enter/tab/space/esc/arrows/ctrl/alt/shift/win…）
        key: String,
        /// down / up / tap
        #[arg(long)]
        action: Option<String>,
    },
    /// 发送快捷键，如 ctrl+shift+s、alt+F4
    Hotkey {
        /// 组合键，如 "ctrl+shift+s"
        combo: String,
    },
    /// 输入文本（支持中文等任意 Unicode）
    Type {
        /// 要输入的文本
        text: String,
    },
    /// 鼠标操作
    Mouse {
        #[command(subcommand)]
        cmd: MouseCmd,
    },
    /// 内核虚拟 HID（路线 B）
    Kernel {
        #[command(subcommand)]
        cmd: KernelCmd,
    },
}

#[derive(Subcommand)]
enum MouseCmd {
    /// 相对移动
    Move { dx: i32, dy: i32 },
    /// 绝对移动（0..=65535 归一化坐标）
    MoveTo { x: u16, y: u16 },
    /// 点击（默认左键）
    Click {
        #[arg(default_value = "left")]
        button: String,
    },
    /// 按下（默认左键）
    Down {
        #[arg(default_value = "left")]
        button: String,
    },
    /// 抬起（默认左键）
    Up {
        #[arg(default_value = "left")]
        button: String,
    },
    /// 滚轮（120 的倍数）
    Wheel { delta: i32 },
}

#[derive(Subcommand)]
enum KernelCmd {
    /// 安装内核虚拟键盘驱动（需管理员；vdev-hid.inf 与 vdev_hid.sys 在 --inf-dir）
    Install {
        /// 驱动文件目录（默认：本 exe 所在目录）
        #[arg(long)]
        inf_dir: Option<std::path::PathBuf>,
    },
    /// 卸载内核虚拟键盘驱动（需管理员）
    Uninstall,
    /// 查看内核虚拟键盘安装状态
    Status,
    /// 注入按键（down/up/tap；经内核 HID 驱动报告）
    Key {
        /// 键名（a-z/0-9/F1-F24/enter/tab/space/esc/arrows/ctrl/alt/shift/win…）
        key: String,
        /// down / up / tap
        #[arg(long)]
        action: Option<String>,
    },
}

fn parse_button(s: &str) -> Result<MouseButton> {
    match s.to_ascii_lowercase().as_str() {
        "left" => Ok(MouseButton::Left),
        "right" => Ok(MouseButton::Right),
        "middle" => Ok(MouseButton::Middle),
        _ => bail!("未知鼠标按键：{s}（left/right/middle）"),
    }
}

fn parse_mods(combo: &str) -> Result<(Vec<u16>, String)> {
    let parts: Vec<&str> = combo.split('+').collect();
    let (mods, key) = parts.split_at(parts.len() - 1);
    let mut vks = Vec::new();
    for m in mods {
        let vk = match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => MOD_CONTROL,
            "alt" => MOD_ALT,
            "shift" => MOD_SHIFT,
            "win" | "lwin" => 0x5B,
            _ => bail!("未知修饰键：{m}（ctrl/alt/shift/win）"),
        };
        vks.push(vk);
    }
    Ok((vks, key[0].to_string()))
}

fn main() -> Result<()> {
    let args = Args::parse();
    match args.command {
        Command::Kernel { cmd } => match cmd {
            KernelCmd::Install { inf_dir } => {
                ensure_elevated()?;
                let dir = inf_dir.unwrap_or_else(|| {
                    std::env::current_exe()
                        .ok()
                        .and_then(|p| p.parent().map(std::path::PathBuf::from))
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                });
                kernel::install(&dir)?;
            }
            KernelCmd::Uninstall => {
                ensure_elevated()?;
                kernel::uninstall()?;
            }
            KernelCmd::Status => {
                let st = kernel::status()?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&st)?);
                } else if st.present {
                    println!("虚拟键盘（内核 HID）：已安装");
                    if let Some(name) = &st.friendly_name {
                        println!("  名称：{name}");
                    }
                    if let Some(drv) = &st.driver {
                        println!("  驱动：{drv}");
                    }
                } else {
                    println!("虚拟键盘（内核 HID）：未安装（先运行 vdev-hid-win kernel install）");
                }
            }
            KernelCmd::Key { key, action } => {
                let (mods, usage) = kernel::key_to_hid(&key)?;
                let action = action.as_deref().unwrap_or("tap");
                match action {
                    "down" => {
                        kernel::write_report(&kernel::make_report(mods, usage))?;
                        println!("已注入按键按下（{key}）");
                    }
                    "up" => {
                        kernel::write_report(&kernel::make_report(0, None))?;
                        println!("已注入按键抬起（{key}）");
                    }
                    "tap" => {
                        kernel::write_report(&kernel::make_report(mods, usage))?;
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        kernel::write_report(&kernel::make_report(0, None))?;
                        println!("已注入按键（{key}）");
                    }
                    other => bail!("未知动作：{other}（down/up/tap）"),
                }
            }
        },
        Command::Key { key, action } => {
            let vk = keycodes::key_to_vk(&key)
                .with_context(|| format!("未知键名：{key}（可用 a-z/0-9/F1-F24/enter/tab/space/esc/arrows/ctrl/alt/shift/win 等）"))?;
            let action = match action.as_deref().unwrap_or("tap") {
                "down" => KeyAction::Down,
                "up" => KeyAction::Up,
                "tap" => KeyAction::Tap,
                other => bail!("未知动作：{other}（down/up/tap）"),
            };
            let n = send_key(vk, action)?;
            println!("已发送 {n} 个键盘事件（{key} {action:?}）");
        }
        Command::Hotkey { combo } => {
            let (mods, key) = parse_mods(&combo)?;
            let vk = keycodes::key_to_vk(&key).with_context(|| format!("未知键名：{key}"))?;
            let n = send_hotkey(&mods, vk)?;
            println!("已发送 {n} 个键盘事件（{combo}）");
        }
        Command::Type { text } => {
            let n = send_text(&text)?;
            println!("已输入 {n} 个键盘事件（{text}）");
        }
        Command::Mouse { cmd } => match cmd {
            MouseCmd::Move { dx, dy } => {
                mouse_move_relative(dx, dy)?;
                println!("已移动鼠标（{dx},{dy}）");
            }
            MouseCmd::MoveTo { x, y } => {
                mouse_move_absolute(x, y)?;
                println!("已绝对移动鼠标（{x},{y}）");
            }
            MouseCmd::Click { button } => {
                let b = parse_button(&button)?;
                mouse_button(b, MouseAction::Click)?;
                println!("已点击 {button}");
            }
            MouseCmd::Down { button } => {
                let b = parse_button(&button)?;
                mouse_button(b, MouseAction::Down)?;
                println!("已按下 {button}");
            }
            MouseCmd::Up { button } => {
                let b = parse_button(&button)?;
                mouse_button(b, MouseAction::Up)?;
                println!("已抬起 {button}");
            }
            MouseCmd::Wheel { delta } => {
                mouse_wheel(delta)?;
                println!("已滚动滚轮 {delta}");
            }
        },
    }
    Ok(())
}
/// 非管理员时以 UAC 重新启动自身执行同一命令，等待完成后退出。
fn ensure_elevated() -> Result<()> {
    // SAFETY: IsUserAnAdmin 只读当前令牌
    if unsafe { windows::Win32::UI::Shell::IsUserAnAdmin() }.as_bool() {
        return Ok(());
    }
    let exe = std::env::current_exe().context("无法定位自身路径")?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let exe_wide: Vec<u16> = exe
        .to_string_lossy()
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let args_wide: Vec<u16> = args
        .join(" ")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut sei = windows::Win32::UI::Shell::SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<windows::Win32::UI::Shell::SHELLEXECUTEINFOW>() as u32,
        fMask: windows::Win32::UI::Shell::SEE_MASK_NOCLOSEPROCESS,
        lpVerb: windows::core::w!("runas"),
        lpFile: windows::core::PCWSTR(exe_wide.as_ptr()),
        lpParameters: windows::core::PCWSTR(args_wide.as_ptr()),
        nShow: windows::Win32::UI::WindowsAndMessaging::SW_HIDE.0,
        ..Default::default()
    };
    unsafe { windows::Win32::UI::Shell::ShellExecuteExW(&mut sei) }
        .context("请求管理员权限失败（请以管理员身份重试）")?;
    if !sei.hProcess.is_invalid() {
        unsafe { windows::Win32::System::Threading::WaitForSingleObject(sei.hProcess, u32::MAX) };
    }
    std::process::exit(0);
}
