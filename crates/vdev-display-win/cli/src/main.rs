//! vdev-display-win：vdev 虚拟显示器（IddCx UMDF 驱动）安装与控制 CLI。
//!
//! 本地操作：install / uninstall / status（SetupAPI，需管理员）。
//! 驱动控制：list / add / remove / remove-all / set-mode / persist（经命名管道 IPC）。

mod install;
mod mode;

use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};
use clap::{Parser, Subcommand};
use driver_ipc::{sync::DriverClient, Id, Monitor};

#[derive(Parser)]
#[command(
    name = "vdev-display-win",
    about = "vdev 虚拟显示器（IddCx UMDF）安装与控制"
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
    /// 安装虚拟显示器驱动（需管理员；vdev-display.inf 与 vdev_display.dll 需在 --inf-dir）
    Install {
        /// 驱动文件目录（默认：本 exe 所在目录）
        #[arg(long)]
        inf_dir: Option<PathBuf>,
    },
    /// 卸载虚拟显示器驱动（需管理员）
    Uninstall,
    /// 查看驱动安装状态
    Status,
    /// 列出当前虚拟显示器
    List,
    /// 添加一个虚拟显示器
    Add {
        /// 分辨率/刷新率，如 1920x1080、3840x2160@60/120
        mode: Vec<mode::Mode>,
        /// 手动指定 ID
        #[arg(long)]
        id: Option<Id>,
        /// 显示器名称
        #[arg(long)]
        name: Option<String>,
        /// 创建后保持禁用
        #[arg(long)]
        disabled: bool,
    },
    /// 移除一个或多个虚拟显示器
    Remove {
        /// ID 或名称
        id: Vec<String>,
    },
    /// 移除全部虚拟显示器
    RemoveAll,
    /// 设置某显示器的分辨率/刷新率
    SetMode {
        /// ID 或名称
        id: String,
        /// 新的分辨率/刷新率
        mode: Vec<mode::Mode>,
    },
    /// 把当前显示器配置持久化到注册表（重启后恢复）
    Persist,
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
            let monitors = DriverClient::new()
                .map(|c| c.monitors().to_vec())
                .unwrap_or_default();
            if args.json {
                #[derive(serde::Serialize)]
                struct StatusOut {
                    device: install::DeviceStatus,
                    monitors: Vec<Monitor>,
                }
                println!(
                    "{}",
                    serde_json::to_string_pretty(&StatusOut {
                        device: st,
                        monitors,
                    })?
                );
            } else if st.present {
                println!("虚拟显示器设备：已安装");
                if let Some(name) = &st.friendly_name {
                    println!("  名称：{name}");
                }
                if let Some(drv) = &st.driver {
                    println!("  驱动：{drv}");
                }
                println!();
                print_monitors(&monitors, false);
            } else {
                println!("虚拟显示器设备：未安装（先运行 vdev-display-win install）");
            }
        }
        Command::List => {
            let client = connect()?;
            let monitors = client.monitors().to_vec();
            print_monitors(&monitors, args.json);
        }
        Command::Add {
            mode,
            id,
            name,
            disabled,
        } => {
            let mut client = connect()?;

            let modes: Vec<driver_ipc::Mode> =
                mode.into_iter().map(driver_ipc::Mode::from).collect();
            for m in &modes {
                mode::validate(m)?;
            }

            let existing = client.monitors().to_vec();
            let new_id = match id {
                Some(id) => {
                    if existing.iter().any(|m| m.id == id) {
                        bail!("ID {id} 已存在");
                    }
                    id
                }
                None => (0u32..)
                    .find(|i| !existing.iter().any(|m| m.id == *i))
                    .expect("不可能找不到空闲 ID"),
            };

            let monitor = Monitor {
                id: new_id,
                enabled: !disabled,
                name,
                modes,
            };
            client.add(monitor)?;
            client.notify()?;

            if args.json {
                println!("{}", serde_json::to_string_pretty(&new_id)?);
            } else {
                println!(
                    "已添加虚拟显示器 ID {new_id}（驱动生效中，稍后可在系统设置里看到新屏幕）"
                );
            }
        }
        Command::Remove { id } => {
            let mut client = connect()?;
            let ids = resolve_ids(&client, &id)?;
            if ids.is_empty() {
                bail!("未匹配到任何显示器");
            }
            client.remove(&ids);
            client.notify()?;
            println!("已移除虚拟显示器：{ids:?}");
        }
        Command::RemoveAll => {
            let mut client = connect()?;
            client.remove_all();
            client.notify()?;
            println!("已移除全部虚拟显示器");
        }
        Command::SetMode { id, mode } => {
            let mut client = connect()?;

            let mut modes: Vec<driver_ipc::Mode> =
                mode.into_iter().map(driver_ipc::Mode::from).collect();
            for m in &modes {
                mode::validate(m)?;
            }

            let found = client.find_monitor_mut_query(&id, |monitor| {
                std::mem::swap(&mut monitor.modes, &mut modes);
            });
            if found.is_none() {
                bail!("找不到显示器：{id}");
            }
            client.notify()?;
            println!("已更新显示器 {id} 的模式");
        }
        Command::Persist => {
            let client = connect()?;
            client.persist()?;
            println!("已持久化到注册表");
        }
    }

    Ok(())
}

fn connect() -> Result<DriverClient> {
    DriverClient::new()
        .context("无法连接 vdev 显示驱动（管道 \\\\.\\pipe\\vdev-display）；请先运行 vdev-display-win install")
}

/// 把 ID 或名称解析成 ID 列表
fn resolve_ids(client: &DriverClient, queries: &[String]) -> Result<Vec<Id>> {
    let mut ids = Vec::new();
    for q in queries {
        match q.parse::<Id>() {
            Ok(id) => ids.push(id),
            Err(_) => {
                let id = client
                    .find_id(q)
                    .with_context(|| format!("找不到显示器：{q}"))?;
                ids.push(id);
            }
        }
    }
    Ok(ids)
}

fn print_monitors(monitors: &[Monitor], json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(monitors).unwrap());
        return;
    }

    if monitors.is_empty() {
        println!("当前没有虚拟显示器（vdev-display-win add 1920x1080 添加）");
        return;
    }

    for m in monitors {
        let name = m.name.as_deref().unwrap_or("");
        let state = if m.enabled { "启用" } else { "禁用" };
        println!("显示器 {} {name} [{state}]", m.id);
        for mode in &m.modes {
            let rates = mode
                .refresh_rates
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("/");
            println!("  {}x{}@{}", mode.width, mode.height, rates);
        }
    }
}

/// 非管理员时以 UAC 重新启动自身执行同一命令，等待完成后退出（返回 true 表示已提权重跑）。
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

    // 等待提权后的实例完成，再退出本进程
    if !sei.hProcess.is_invalid() {
        unsafe {
            windows::Win32::System::Threading::WaitForSingleObject(sei.hProcess, u32::MAX);
        }
    }
    std::process::exit(0);
}
