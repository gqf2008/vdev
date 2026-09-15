//! vdev-audio-win：vdev 虚拟声卡（PortCls 内核驱动）安装与控制 CLI。
//! 非 Windows 宿主仅编译纯逻辑并运行其单测（cfg 门控对称，与 vdev-hid-win 同款）。

#[cfg(windows)]
mod install;
#[cfg(windows)]
mod wasapi;
mod wav;

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
    #[arg(short, long, global = true)]
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
    /// 向「vdev 扬声器」（render 端点）注入音频——宿主推流语义
    Inject {
        /// 端点名过滤（不区分大小写）
        #[arg(long, default_value = "vdev")]
        endpoint: String,
        /// 播放 16bit PCM / 32bit float WAV 文件（优先于 --tone）
        #[arg(long)]
        wav: Option<PathBuf>,
        /// 正弦频率 Hz（--wav 未给出时使用）
        #[arg(long, default_value_t = 1000.0)]
        tone: f32,
        /// 正弦幅度 0.0..1.0
        #[arg(long, default_value_t = 0.5)]
        amplitude: f32,
        /// 注入时长（秒）
        #[arg(long, default_value_t = 3.0)]
        duration: f64,
    },
    /// 从「vdev 麦克风」（capture 端点）采集音频——环回/录音语义
    Capture {
        /// 端点名过滤（不区分大小写）
        #[arg(long, default_value = "vdev")]
        endpoint: String,
        /// 采集时长（秒）
        #[arg(long, default_value_t = 3.0)]
        duration: f64,
        /// 跳过开头这么多秒再统计（避开驱动环回积压，默认 0）
        #[arg(long, default_value_t = 0.0)]
        skip: f64,
        /// 存为 16bit PCM WAV
        #[arg(long)]
        wav: Option<PathBuf>,
    },
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
        Command::Inject {
            endpoint,
            wav,
            tone,
            amplitude,
            duration,
        } => {
            let source = match &wav {
                Some(path) => {
                    let bytes = std::fs::read(path)
                        .with_context(|| format!("读 WAV 失败：{}", path.display()))?;
                    Some(wav::parse_wav(&bytes)?)
                }
                None => None,
            };
            let rep = wasapi::inject(&endpoint, source.as_ref(), tone, amplitude, duration)?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                println!("注入端点：{}", rep.endpoint);
                println!("  端点 ID：{}", rep.endpoint_id);
                println!(
                    "  格式：{} Hz / {} ch（注入 {} 帧 ≈ {:.2}s）",
                    rep.sample_rate, rep.channels, rep.frames, rep.seconds
                );
                println!(
                    "注入完成：Play 到该端点的声音即被「vdev 麦克风」听到（可用 capture 验证）"
                );
            }
        }
        Command::Capture {
            endpoint,
            duration,
            skip,
            wav,
        } => {
            let rep = wasapi::capture(&endpoint, duration, skip, wav.as_deref())?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                println!("采集端点：{}", rep.endpoint);
                println!("  端点 ID：{}", rep.endpoint_id);
                println!(
                    "  格式：{} Hz / {} ch（采集 {} 帧 ≈ {:.2}s）",
                    rep.sample_rate, rep.channels, rep.frames, rep.seconds
                );
                println!(
                    "  电平：RMS {:.1} dBFS / 峰值 {:.1} dBFS",
                    rep.rms_dbfs, rep.peak_dbfs
                );
                if let Some(p) = &rep.wav {
                    println!("  已保存：{p}");
                }
                // 与 wasapi-loop 验收脚本同一判据：静音约 −100 dBFS，有声音显著抬升
                if rep.rms_dbfs > -60.0 {
                    println!("判定：采到有效音频（RMS > −60 dBFS）");
                } else {
                    println!(
                        "判定：接近静音（RMS ≤ −60 dBFS）——若刚注入过，可用 --skip 避开环回积压"
                    );
                }
            }
        }
    }
    Ok(())
}

/// 非管理员时以 UAC 重新启动自身执行同一命令，等待完成后以其退出码退出。
///
/// 修复记录（审查 M-d，对照 PR #21 display 侧与 hid 侧同款修复）：原实现
/// 等待提权子进程后无条件 `exit(0)`，UAC 被取消或子进程失败都静默报成功。
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
        // SAFETY: SEE_MASK_NOCLOSEPROCESS 使 ShellExecuteExW 返回子进程句柄
        unsafe { windows::Win32::System::Threading::WaitForSingleObject(sei.hProcess, u32::MAX) };
        let mut code: u32 = 0;
        // SAFETY: 句柄有效且已等待其退出
        let got = unsafe {
            windows::Win32::System::Threading::GetExitCodeProcess(sei.hProcess, &mut code)
        };
        // SAFETY: 关闭自有句柄，避免泄漏
        unsafe { windows::Win32::Foundation::CloseHandle(sei.hProcess) }.ok();
        std::process::exit(elevated_exit_code(true, got.is_ok(), code));
    }
    // ShellExecuteExW 成功却未见子进程句柄（SEE_MASK_NOCLOSEPROCESS 下属意外）：
    // 无法等待/回传提权子进程结果，按失败退出（禁止静默当成功）。
    eprintln!("警告：ShellExecuteExW 已成功但未返回提权子进程句柄，无法回传其退出码");
    std::process::exit(elevated_exit_code(false, false, 0));
}

/// 提权子进程结果 → 本进程退出码（纯函数，宿主可单测；与 hid 侧同款）：
/// 句柄有效且 GetExitCodeProcess 成功 → 原样回传子进程退出码；
/// 未拿到句柄或查询失败 → 1（结果无法回传即按失败，禁止静默当成功）。
#[cfg_attr(not(windows), allow(dead_code))]
fn elevated_exit_code(handle_valid: bool, query_ok: bool, child_code: u32) -> i32 {
    if !handle_valid || !query_ok {
        return 1;
    }
    child_code as i32
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

    /// M-d 回归：提权子进程结果无法回传时退出码必须非 0。修复前等待后
    /// 无条件 exit(0)，UAC 取消/子进程失败都静默报成功。
    #[test]
    fn elevated_exit_code_is_nonzero_when_child_result_unavailable() {
        // 子进程成功 → 原样回传 0；非零（如 3010 REBOOT_REQUIRED）原样回传
        assert_eq!(elevated_exit_code(true, true, 0), 0);
        assert_eq!(elevated_exit_code(true, true, 3010), 3010);
        // 查询子进程退出码失败 → 按失败
        assert_eq!(elevated_exit_code(true, false, 0), 1);
        // 句柄意外无效 → 按失败（修复前此处等价返回 0）
        assert_eq!(elevated_exit_code(false, false, 0), 1);
    }
}
