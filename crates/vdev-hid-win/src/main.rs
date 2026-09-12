//! vdev-hid-win：Windows 虚拟 HID（SendInput 注入）CLI。
//! 非 Windows 宿主仅编译纯逻辑模块并运行其单测（cfg 门控对称）。

#[cfg(windows)]
mod kernel;
mod keycodes;
mod report;

use anyhow::Result;
use clap::{Parser, Subcommand};

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
    /// 注入鼠标（经内核 HID 驱动报告）
    Mouse {
        #[command(subcommand)]
        cmd: KernelMouseCmd,
    },
}

#[derive(Subcommand)]
enum KernelMouseCmd {
    /// 相对移动（±127）
    Move { dx: i32, dy: i32 },
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

/// Windows 命令行参数转义：含空白/引号/为空的参数用双引号包裹，
/// 内部引号按 MSVCRT argv 规则转义（引号前反斜杠翻倍再补一个）。
/// 修复记录：原实现 `args.join(" ")`，路径含空格即碎成多个参数。
#[cfg_attr(not(windows), allow(dead_code))]
fn quote_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty() || arg.chars().any(|c| matches!(c, ' ' | '\t' | '"'));
    if !needs_quotes {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 8);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                // 引号本身按 \" 转义，其前连续反斜杠翻倍（2n+1 规则）
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
    }
    // 收尾引号前的连续反斜杠翻倍（2n 规则）
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

fn main() -> Result<()> {
    #[cfg(windows)]
    return run(Args::parse());

    #[cfg(not(windows))]
    anyhow::bail!("vdev-hid-win 仅支持 Windows 执行（宿主平台只运行纯逻辑单测）")
}

#[cfg(windows)]
fn run(args: Args) -> Result<()> {
    use anyhow::{Context as _, bail};
    use vdev_hid_win::{
        KeyAction, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MouseAction, MouseButton, mouse_button,
        mouse_move_absolute, mouse_move_relative, mouse_wheel, send_hotkey, send_key, send_text,
    };

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
                        kernel::write_report(kernel::PID_KBD, &kernel::make_report(mods, usage))?;
                        println!("已注入按键按下（{key}）");
                    }
                    "up" => {
                        kernel::write_report(kernel::PID_KBD, &kernel::make_report(0, None))?;
                        println!("已注入按键抬起（{key}）");
                    }
                    "tap" => {
                        kernel::write_report(kernel::PID_KBD, &kernel::make_report(mods, usage))?;
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        kernel::write_report(kernel::PID_KBD, &kernel::make_report(0, None))?;
                        println!("已注入按键（{key}）");
                    }
                    other => bail!("未知动作：{other}（down/up/tap）"),
                }
            }
            KernelCmd::Mouse { cmd } => match cmd {
                KernelMouseCmd::Move { dx, dy } => {
                    kernel::mouse_move(dx, dy)?;
                    println!("已注入鼠标相对移动（{dx},{dy}）");
                }
                KernelMouseCmd::Click { button } => {
                    kernel::mouse_button(&button, "click")?;
                    println!("已注入鼠标点击（{button}）");
                }
                KernelMouseCmd::Down { button } => {
                    kernel::mouse_button(&button, "down")?;
                    println!("已注入鼠标按下（{button}）");
                }
                KernelMouseCmd::Up { button } => {
                    kernel::mouse_button(&button, "up")?;
                    println!("已注入鼠标抬起（{button}）");
                }
                KernelMouseCmd::Wheel { delta } => {
                    kernel::mouse_wheel(delta)?;
                    println!("已注入鼠标滚轮（{delta}）");
                }
            },
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

/// 非管理员时以 UAC 重新启动自身执行同一命令，等待完成后以其退出码退出。
///
/// 修复记录：原实现 `args.join(" ")`（空格路径碎参）且无条件 `exit(0)`（吞掉
/// 提权子进程的真实退出码）；现逐参数转义并回传 GetExitCodeProcess 结果。
#[cfg(windows)]
fn ensure_elevated() -> Result<()> {
    use anyhow::Context as _;
    use windows::Win32::Foundation::CloseHandle;

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
    // 每个参数独立转义后拼接，空格/引号路径不会碎参
    let args_wide: Vec<u16> = args
        .iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
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
        // SAFETY: SEE_MASK_NOCLOSEPROCESS 使 ShellExecuteExW 返回子进程句柄
        unsafe { windows::Win32::System::Threading::WaitForSingleObject(sei.hProcess, u32::MAX) };
        let mut code: u32 = 0;
        // SAFETY: 句柄有效且已等待其退出
        let got = unsafe {
            windows::Win32::System::Threading::GetExitCodeProcess(sei.hProcess, &mut code)
        };
        // SAFETY: 关闭自有句柄，避免泄漏
        unsafe { CloseHandle(sei.hProcess) }.ok();
        std::process::exit(elevated_exit_code(true, got.is_ok(), code));
    }
    // ShellExecuteExW 成功却未见子进程句柄（SEE_MASK_NOCLOSEPROCESS 下属意外）：
    // 无法等待/回传提权子进程结果，打印诊断并按失败退出。
    // 修复记录：原实现此处 exit(0)，静默吞掉提权子进程结果。
    eprintln!("警告：ShellExecuteExW 已成功但未返回提权子进程句柄，无法回传其退出码");
    std::process::exit(elevated_exit_code(false, false, 0));
}

/// 提权子进程结果 → 本进程退出码（纯函数，宿主可单测）：
/// 句柄有效且 GetExitCodeProcess 成功 → 原样回传子进程退出码；
/// 未拿到句柄或查询失败 → 1（结果无法回传即按失败，禁止静默当成功）。
#[cfg_attr(not(windows), allow(dead_code))]
fn elevated_exit_code(handle_valid: bool, query_ok: bool, child_code: u32) -> i32 {
    if !handle_valid || !query_ok {
        return 1;
    }
    child_code as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M9 回归：参数转义（修复前 join(" ") 使空格路径碎参）
    #[test]
    fn quote_arg_escapes_spaces_and_quotes() {
        assert_eq!(quote_arg("install"), "install");
        assert_eq!(quote_arg("kernel"), "kernel");
        assert_eq!(
            quote_arg(r"C:\Program Files\vdev\vdev-hid-win.exe"),
            "\"C:\\Program Files\\vdev\\vdev-hid-win.exe\""
        );
        // 空 参数也须包裹
        assert_eq!(quote_arg(""), "\"\"");
        // 内部引号按 MSVCRT 规则转义；引号前反斜杠翻倍
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
        assert_eq!(
            quote_arg(r"ends with backslash\"),
            "\"ends with backslash\\\\\""
        );
    }

    /// M8：键名解析在 bin 目标同样可用（宿主单测）
    #[test]
    fn keycodes_reachable_from_bin() {
        assert_eq!(keycodes::key_to_vk("A"), Some(0x41));
        assert_eq!(keycodes::key_to_vk("f13"), Some(0x7C));
    }

    /// 回归：提权子进程结果无法回传时退出码必须非 0。修复前 ShellExecuteExW
    /// 成功但 hProcess 意外无效 → exit(0)，静默吞掉提权子进程结果。
    #[test]
    fn elevated_exit_code_is_nonzero_when_child_result_unavailable() {
        // 子进程成功 → 原样回传 0；非零（如驱动安装 3010 REBOOT_REQUIRED）原样回传
        assert_eq!(elevated_exit_code(true, true, 0), 0);
        assert_eq!(elevated_exit_code(true, true, 3010), 3010);
        // 查询子进程退出码失败 → 按失败
        assert_eq!(elevated_exit_code(true, false, 0), 1);
        // 句柄意外无效 → 按失败（修复前此处等价返回 0）
        assert_eq!(elevated_exit_code(false, false, 0), 1);
    }
}
