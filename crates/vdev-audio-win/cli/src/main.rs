//! vdev-audio-win：vdev 虚拟声卡（PortCls 内核驱动）安装与控制 CLI。

mod install;

use std::path::PathBuf;

use anyhow::{Context as _, Result};
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
    let args = Args::parse();
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
