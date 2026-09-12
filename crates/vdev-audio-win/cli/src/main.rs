//! vdev-audio-win：vdev 虚拟声卡（PortCls 内核驱动）安装与控制 CLI。
//! 非 Windows 宿主仅编译纯逻辑并运行其单测（cfg 门控对称，与 vdev-hid-win 同款）。

#[cfg(windows)]
mod install;

use std::path::PathBuf;

#[cfg(windows)]
use anyhow::Context as _;
use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "vdev-audio-win",
    about = "vdev 虚拟声卡（PortCls 内核驱动）安装与控制"
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
    /// 安装虚拟声卡驱动（需管理员；vdev-audio.inf 与 vdev_audio.sys 在 --inf-dir）
    Install {
        /// 驱动文件目录（默认：本 exe 所在目录）
        #[arg(long)]
        inf_dir: Option<PathBuf>,
    },
    /// 卸载虚拟声卡驱动（需管理员）
    Uninstall,
    /// 查看驱动安装状态
    Status,
}

fn main() -> Result<()> {
    #[cfg(windows)]
    return run(Args::parse());

    #[cfg(not(windows))]
    anyhow::bail!("vdev-audio-win 仅支持 Windows 执行（宿主平台只运行纯逻辑单测）")
}

#[cfg(windows)]
fn run(args: Args) -> Result<()> {
    match args.command {
        Command::Install { inf_dir } => {
            ensure_elevated()?;
            let dir = inf_dir.unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|p| p.parent().map(PathBuf::from))
                    .unwrap_or_else(|| PathBuf::from("."))
            });
            install::install(&dir)?;
        }
        Command::Uninstall => {
            ensure_elevated()?;
            install::uninstall()?;
        }
        Command::Status => {
            let st = install::status()?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&st)?);
            } else if st.present {
                println!("虚拟声卡设备：已安装");
                if let Some(name) = &st.friendly_name {
                    println!("  名称：{name}");
                }
                if let Some(drv) = &st.driver {
                    println!("  驱动：{drv}");
                }
            } else {
                println!(
                    "虚拟声卡设备：未安装（先运行 vdev-audio-win install；内核驱动需测试签名）"
                );
            }
        }
    }
    Ok(())
}

/// 非管理员时以 UAC 重新启动自身执行同一命令，等待完成后退出。
#[cfg(windows)]
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
    // 逐参数按 MSVCRT argv 规则引用后再拼接（修复记录：原实现 args.join(" ")，
    // 路径含空格即碎成多个参数）
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
        unsafe { windows::Win32::System::Threading::WaitForSingleObject(sei.hProcess, u32::MAX) };
    }
    std::process::exit(0);
}

/// Windows 命令行参数转义：含空白/引号/为空的参数用双引号包裹，
/// 内部引号按 MSVCRT argv 规则转义（引号前反斜杠翻倍再补一个）。
/// 与 vdev-hid-win/src/main.rs 同款实现（对照其修复）。
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：UAC 重启参数转义（修复前 args.join(" ") 使空格路径碎参）
    #[test]
    fn quote_arg_escapes_spaces_quotes_and_backslashes() {
        assert_eq!(quote_arg("install"), "install");
        assert_eq!(quote_arg("--inf-dir"), "--inf-dir");
        assert_eq!(
            quote_arg(r"C:\Program Files\vdev\vdev-audio-win.exe"),
            "\"C:\\Program Files\\vdev\\vdev-audio-win.exe\""
        );
        // 空参数也须包裹
        assert_eq!(quote_arg(""), "\"\"");
        // 含制表符视为需要包裹
        assert_eq!(quote_arg("a\tb"), "\"a\tb\"");
        // 内部引号按 MSVCRT 规则转义（2n+1）
        assert_eq!(quote_arg("a\"b"), "\"a\\\"b\"");
        // 收尾反斜杠翻倍（2n）
        assert_eq!(
            quote_arg(r"ends with backslash\"),
            "\"ends with backslash\\\\\""
        );
        // 无需包裹时反斜杠保持原样
        assert_eq!(quote_arg(r"a\b"), r"a\b");
        // 引号与收尾反斜杠组合：2n+1 与 2n 规则同时生效
        assert_eq!(quote_arg(r#"say "hi"\"#), "\"say \\\"hi\\\"\\\\\"");
    }
}
