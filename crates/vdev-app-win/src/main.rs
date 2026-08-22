//! VDCamera (Windows) — vdev 虚拟摄像头 GUI 宿主（Rust + Slint + slint-pixel）。
//!
//! 复用 macOS 版 vdev-app 的像素风 UI 与交互模式（状态面板 / 安装 / 卸载 /
//! 推流 / 日志），后端全部调用 `vdev-camera-win` 的安全封装（注册 / 注销 /
//! 推流 / 设备枚举）。本文件是纯业务层，不含 unsafe。

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

slint::include_modules!();
slint_pixel::impl_title_bar_ui!(MainWindow);

type Logs = Arc<Mutex<Vec<String>>>;

/// 推流状态：是否正在推流 + 推流线程句柄。
static PUSH_RUNNING: AtomicBool = AtomicBool::new(false);
static PUSH_THREAD: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

const PUSH_WIDTH: u32 = 640;
const PUSH_HEIGHT: u32 = 360;
const PUSH_FPS: u32 = 30;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // panic 落盘：GUI 无 stderr，崩溃后可读 %TEMP%\vdev-app-win-panic.log 定位。
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("[win] {info}\n");
        let path = std::env::var("TEMP")
            .or_else(|_| std::env::var("TMP"))
            .map(|t| format!("{t}\\vdev-app-win-panic.log"))
            .unwrap_or_else(|_| "vdev-app-win-panic.log".to_string());
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
        eprintln!("{info}");
    }));

    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let ui = MainWindow::new()?;
    slint_pixel::install_title_bar_controls(&ui);
    let logs: Logs = Arc::new(Mutex::new(Vec::new()));
    wire_ui(&ui, &logs);
    refresh_status(&ui, &logs);
    refresh_display_status(&ui, &logs);
    refresh_audio_status(&ui, &logs);
    refresh_hid_status(&ui, &logs);

    ui.run()?;
    // 退出时确保推流线程停止。
    PUSH_RUNNING.store(false, Ordering::SeqCst);
    if let Some(h) = PUSH_THREAD.lock().unwrap().take() {
        let _ = h.join();
    }
    Ok(())
}

fn append_log(ui: &MainWindow, logs: &Logs, s: impl AsRef<str>) {
    let s = s.as_ref();
    let mut v = logs.lock().unwrap();
    v.push(s.to_string());
    if v.len() > 300 {
        v.remove(0);
    }
    ui.global::<AppState>().set_log_text(v.join("\n").into());
}

fn set_status(ui: &MainWindow, glyph: &str, title: &str, detail: &str) {
    let g = ui.global::<AppState>();
    g.set_status_glyph(glyph.into());
    g.set_status_title(title.into());
    g.set_status_detail(detail.into());
}

fn set_enabled(ui: &MainWindow, can_install: bool, can_uninstall: bool, can_push: bool) {
    let g = ui.global::<AppState>();
    g.set_can_install(can_install);
    g.set_can_uninstall(can_uninstall);
    g.set_can_push(can_push);
}

/// 检测 vdev-camera 是否已注册可见（与 ffmpeg 同路径：ICreateDevEnum 枚举）。
fn camera_visible() -> bool {
    match vdev_camera_win::com::ComInit::new() {
        Ok(_com) => vdev_camera_win::dshow::device::list_video_capture_devices()
            .map(|names| names.iter().any(|n| n == "vdev-camera"))
            .unwrap_or(false),
        Err(_) => false,
    }
}

fn refresh_status(ui: &MainWindow, logs: &Logs) {
    append_log(ui, logs, "刷新状态…");
    if camera_visible() {
        set_status(
            ui,
            "✓",
            "已安装，摄像头可用",
            "系统已能看到 vdev-camera。任意 App 摄像头列表选 vdev-camera，或 ffmpeg -f dshow -i video=vdev-camera。",
        );
        set_enabled(ui, false, true, true);
        append_log(ui, logs, "✅ 检测到 vdev-camera");
    } else {
        set_status(
            ui,
            "○",
            "虚拟摄像头未安装",
            "点击「安装虚拟摄像头」，然后刷新状态确认。",
        );
        set_enabled(ui, true, false, false);
        append_log(ui, logs, "未检测到 vdev-camera（先安装）");
    }
}

/// 定位 vdev-camera-win CLI：环境变量 VDEV_CAMERA_WIN_EXE 优先，其次
/// 同目录 / 开发布局（worktree 下 ../vdev-camera-win/target/release）。
fn camera_cli() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(p) = std::env::var("VDEV_CAMERA_WIN_EXE") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            cands.push(dir.join("vdev-camera-win.exe"));
            cands.push(
                dir.join("..")
                    .join("..")
                    .join("..")
                    .join("vdev-camera-win")
                    .join("target")
                    .join("release")
                    .join("vdev-camera-win.exe"),
            );
        }
    }
    cands.into_iter().find(|p| p.exists())
}

/// 委托 vdev-camera-win CLI 执行安装/卸载：CLI 用自身目录的正确 DLL 注册
/// （GUI 自带的 vdev_camera_win.dll 是依赖快照，可能不是最新，不能用来注册）。
fn run_camera_cli(args: &[&str], ui: &MainWindow, logs: &Logs) {
    match camera_cli() {
        Some(exe) => match Command::new(&exe).args(args).output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if out.status.success() {
                    append_log(ui, logs, format!("✅ {}", stdout.trim()));
                } else {
                    append_log(ui, logs, format!("❌ CLI 失败: {}", stderr.trim()));
                }
            }
            Err(e) => append_log(ui, logs, format!("❌ 调用 CLI 失败: {e}")),
        },
        None => append_log(
            ui,
            logs,
            "❌ 未找到 vdev-camera-win.exe（设置 VDEV_CAMERA_WIN_EXE 或放到 GUI 同目录）",
        ),
    }
}

/// vdev-audio-win CLI 输出 JSON 的解析结构
#[derive(serde::Deserialize)]
struct AudioStatusOut {
    present: bool,
}

/// vdev-hid-win CLI 输出 JSON 的解析结构
#[derive(serde::Deserialize)]
struct HidStatusOut {
    present: bool,
}

/// vdev-display-win CLI 输出 JSON 的解析结构
#[derive(serde::Deserialize)]
struct DisplayStatusOut {
    device: DisplayDeviceStatus,
    monitors: Vec<DisplayMonitor>,
}
#[derive(serde::Deserialize)]
struct DisplayDeviceStatus {
    present: bool,
}
#[derive(serde::Deserialize)]
struct DisplayMonitor {
    id: u32,
    name: Option<String>,
    enabled: bool,
    modes: Vec<DisplayMode>,
}
#[derive(serde::Deserialize)]
struct DisplayMode {
    width: u32,
    height: u32,
    refresh_rates: Vec<u32>,
}

/// 定位 vdev-display-win CLI：环境变量 VDEV_DISPLAY_WIN_EXE 优先，其次
/// 同目录 / 开发布局（../vdev-display-win/target/x86_64-pc-windows-msvc/release）。
fn display_cli() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(p) = std::env::var("VDEV_DISPLAY_WIN_EXE") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            cands.push(dir.join("vdev-display-win.exe"));
            cands.push(
                dir.join("..")
                    .join("..")
                    .join("..")
                    .join("vdev-display-win")
                    .join("target")
                    .join("x86_64-pc-windows-msvc")
                    .join("release")
                    .join("vdev-display-win.exe"),
            );
        }
    }
    cands.into_iter().find(|p| p.exists())
}

/// 定位 vdev-audio-win CLI：环境变量 VDEV_AUDIO_WIN_EXE 优先，其次
/// 同目录 / 开发布局（../vdev-audio-win/target/x86_64-pc-windows-msvc/release）。
fn audio_cli() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(p) = std::env::var("VDEV_AUDIO_WIN_EXE") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            cands.push(dir.join("vdev-audio-win.exe"));
            cands.push(
                dir.join("..")
                    .join("..")
                    .join("..")
                    .join("vdev-audio-win")
                    .join("target")
                    .join("x86_64-pc-windows-msvc")
                    .join("release")
                    .join("vdev-audio-win.exe"),
            );
        }
    }
    cands.into_iter().find(|p| p.exists())
}

fn run_audio_cli(args: &[&str], ui: &MainWindow, logs: &Logs) {
    match audio_cli() {
        Some(exe) => match Command::new(&exe).args(args).output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if out.status.success() {
                    append_log(ui, logs, format!("✅ {}", stdout.trim()));
                } else {
                    append_log(ui, logs, format!("❌ CLI 失败: {}", stderr.trim()));
                }
            }
            Err(e) => append_log(ui, logs, format!("❌ 调用 CLI 失败: {e}")),
        },
        None => append_log(
            ui,
            logs,
            "❌ 未找到 vdev-audio-win.exe（设置 VDEV_AUDIO_WIN_EXE 或放到 GUI 同目录）",
        ),
    }
}

fn set_audio_status(ui: &MainWindow, glyph: &str, title: &str, detail: &str) {
    let g = ui.global::<AppState>();
    g.set_audio_glyph(glyph.into());
    g.set_audio_title(title.into());
    g.set_audio_detail(detail.into());
}

/// 刷新声卡状态：跑 vdev-audio-win --json status 并解析
fn refresh_audio_status(ui: &MainWindow, logs: &Logs) {
    append_log(ui, logs, "刷新声卡状态…");
    let Some(exe) = audio_cli() else {
        set_audio_status(
            ui,
            "!",
            "未找到 vdev-audio-win",
            "请把 vdev-audio-win.exe 放到 GUI 同目录，或设置 VDEV_AUDIO_WIN_EXE。",
        );
        return;
    };
    let out = Command::new(&exe).args(["--json", "status"]).output();
    let Ok(out) = out else {
        set_audio_status(
            ui,
            "!",
            "状态查询失败",
            "无法运行 vdev-audio-win --json status。",
        );
        return;
    };
    let Ok(text) = std::str::from_utf8(&out.stdout) else {
        set_audio_status(ui, "!", "状态解析失败", "CLI 输出非 UTF-8。");
        return;
    };
    let Ok(st) = serde_json::from_str::<AudioStatusOut>(text) else {
        set_audio_status(ui, "!", "状态解析失败", "无法解析 vdev-audio-win 输出。");
        return;
    };

    let g = ui.global::<AppState>();
    if st.present {
        set_audio_status(
            ui,
            "✓",
            "已安装，虚拟声卡可用",
            "控制面板应出现「vdev 扬声器」与「vdev 麦克风」；播放到扬声器的声音会被麦克风录制。",
        );
        g.set_audio_can_install(false);
        g.set_audio_can_uninstall(true);
        append_log(ui, logs, "✅ 检测到 vdev 虚拟声卡设备");
    } else {
        set_audio_status(
            ui,
            "○",
            "虚拟声卡未安装",
            "点击「安装虚拟声卡」安装 PortCls 内核驱动（需管理员与测试签名，会请求管理员权限）。",
        );
        g.set_audio_can_install(true);
        g.set_audio_can_uninstall(false);
        append_log(ui, logs, "未检测到 vdev 虚拟声卡（先安装）");
    }
}

/// 找到包含 vdev-display.inf 的目录（安装时 --inf-dir）：开发布局 target/dist 或 CLI 同目录。
/// 找到 vdev-hid-win.exe（与声卡 CLI 同目录或开发布局）
fn hid_cli() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(p) = std::env::var("VDEV_HID_WIN_EXE") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(cur) = std::env::current_exe() {
        if let Some(dir) = cur.parent() {
            cands.push(dir.join("vdev-hid-win.exe"));
            cands.push(
                dir.join("..")
                    .join("..")
                    .join("..")
                    .join("vdev-hid-win")
                    .join("target")
                    .join("x86_64-pc-windows-msvc")
                    .join("release")
                    .join("vdev-hid-win.exe"),
            );
        }
    }
    cands.into_iter().find(|p| p.exists())
}

fn run_hid_cli(args: &[&str], ui: &MainWindow, logs: &Logs) {
    match hid_cli() {
        Some(exe) => match Command::new(&exe).args(args).output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if out.status.success() {
                    append_log(ui, logs, format!("✅ {}", stdout.trim()));
                } else {
                    append_log(ui, logs, format!("❌ CLI 失败: {}", stderr.trim()));
                }
            }
            Err(e) => append_log(ui, logs, format!("❌ 调用 CLI 失败: {e}")),
        },
        None => append_log(
            ui,
            logs,
            "❌ 未找到 vdev-hid-win.exe（设置 VDEV_HID_WIN_EXE 或放到 GUI 同目录）",
        ),
    }
}

fn set_hid_status(ui: &MainWindow, glyph: &str, title: &str, detail: &str) {
    let g = ui.global::<AppState>();
    g.set_hid_glyph(glyph.into());
    g.set_hid_title(title.into());
    g.set_hid_detail(detail.into());
}

/// 刷新 HID 键盘状态：跑 vdev-hid-win --json kernel status 并解析
fn refresh_hid_status(ui: &MainWindow, logs: &Logs) {
    append_log(ui, logs, "刷新 HID 键盘状态…");
    let Some(exe) = hid_cli() else {
        set_hid_status(
            ui,
            "!",
            "未找到 vdev-hid-win",
            "请把 vdev-hid-win.exe 放到 GUI 同目录，或设置 VDEV_HID_WIN_EXE。",
        );
        return;
    };
    let out = Command::new(&exe)
        .args(["--json", "kernel", "status"])
        .output();
    let Ok(out) = out else {
        set_hid_status(
            ui,
            "!",
            "状态查询失败",
            "无法运行 vdev-hid-win --json kernel status。",
        );
        return;
    };
    let Ok(text) = std::str::from_utf8(&out.stdout) else {
        set_hid_status(ui, "!", "状态解析失败", "CLI 输出非 UTF-8。");
        return;
    };
    let Ok(st) = serde_json::from_str::<HidStatusOut>(text) else {
        set_hid_status(ui, "!", "状态解析失败", "无法解析 vdev-hid-win 输出。");
        return;
    };

    let g = ui.global::<AppState>();
    if st.present {
        set_hid_status(
            ui,
            "✓",
            "已安装，虚拟键盘可用",
            "设备管理器 HID 类应出现「vdev 虚拟键盘」；可注入按键（经内核驱动报告）。",
        );
        g.set_hid_can_install(false);
        g.set_hid_can_uninstall(true);
        append_log(ui, logs, "✅ 检测到 vdev 虚拟键盘设备");
    } else {
        set_hid_status(
            ui,
            "○",
            "虚拟键盘未安装",
            "点击「安装虚拟键盘」安装 KMDF 内核 HID 驱动（需管理员与测试签名，会请求管理员权限）。",
        );
        g.set_hid_can_install(true);
        g.set_hid_can_uninstall(false);
        append_log(ui, logs, "未检测到 vdev 虚拟键盘（先安装）");
    }
}

fn display_inf_dir(cli: &Path) -> PathBuf {
    let mut cands = Vec::new();
    if let Some(dir) = cli.parent() {
        cands.push(dir.join("..").join("dist"));
        cands.push(dir.to_path_buf());
    }
    cands
        .into_iter()
        .find(|d| d.join("vdev-display.inf").exists())
        .unwrap_or_else(|| {
            cli.parent()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."))
        })
}

/// 委托 vdev-display-win CLI 执行安装/卸载/增删屏：CLI 内 self-elevate（UAC）。
fn run_display_cli(args: &[&str], ui: &MainWindow, logs: &Logs) {
    match display_cli() {
        Some(exe) => match Command::new(&exe).args(args).output() {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                if out.status.success() {
                    append_log(ui, logs, format!("✅ {}", stdout.trim()));
                } else {
                    append_log(ui, logs, format!("❌ CLI 失败: {}", stderr.trim()));
                }
            }
            Err(e) => append_log(ui, logs, format!("❌ 调用 CLI 失败: {e}")),
        },
        None => append_log(
            ui,
            logs,
            "❌ 未找到 vdev-display-win.exe（设置 VDEV_DISPLAY_WIN_EXE 或放到 GUI 同目录）",
        ),
    }
}

fn set_disp_status(ui: &MainWindow, glyph: &str, title: &str, detail: &str) {
    let g = ui.global::<AppState>();
    g.set_disp_glyph(glyph.into());
    g.set_disp_title(title.into());
    g.set_disp_detail(detail.into());
}

/// 刷新显示器状态：跑 vdev-display-win --json status 并解析。
fn refresh_display_status(ui: &MainWindow, logs: &Logs) {
    append_log(ui, logs, "刷新显示器状态…");
    let Some(exe) = display_cli() else {
        set_disp_status(
            ui,
            "!",
            "未找到 vdev-display-win",
            "请把 vdev-display-win.exe 放到 GUI 同目录，或设置 VDEV_DISPLAY_WIN_EXE。",
        );
        return;
    };
    let out = Command::new(&exe).args(["--json", "status"]).output();
    let Ok(out) = out else {
        set_disp_status(
            ui,
            "!",
            "状态查询失败",
            "无法运行 vdev-display-win --json status。",
        );
        return;
    };
    let Ok(text) = std::str::from_utf8(&out.stdout) else {
        set_disp_status(ui, "!", "状态解析失败", "CLI 输出非 UTF-8。");
        return;
    };
    let Ok(st) = serde_json::from_str::<DisplayStatusOut>(text) else {
        set_disp_status(ui, "!", "状态解析失败", "无法解析 vdev-display-win 输出。");
        return;
    };

    let g = ui.global::<AppState>();
    if st.device.present {
        set_disp_status(
            ui,
            "✓",
            "已安装，虚拟显示器可用",
            "系统已识别 vdev 虚拟显示器；点「添加 1920x1080」增加虚拟屏，然后在系统设置里扩展桌面。",
        );
        g.set_disp_can_install(false);
        g.set_disp_can_uninstall(true);
        g.set_disp_can_add(true);
        append_log(ui, logs, "✅ 检测到 vdev 虚拟显示器设备");
    } else {
        set_disp_status(
            ui,
            "○",
            "虚拟显示器未安装",
            "点击「安装虚拟显示器」安装 IddCx 驱动（会请求管理员权限）。",
        );
        g.set_disp_can_install(true);
        g.set_disp_can_uninstall(false);
        g.set_disp_can_add(false);
        append_log(ui, logs, "未检测到 vdev 虚拟显示器（先安装）");
    }

    // 显示器列表
    if st.monitors.is_empty() {
        g.set_disp_monitors_text("暂无虚拟显示器".into());
    } else {
        let mut lines = Vec::new();
        for m in &st.monitors {
            let state = if m.enabled { "启用" } else { "禁用" };
            let name = m.name.as_deref().unwrap_or("");
            lines.push(format!("显示器 {} {name} [{state}]", m.id));
            for mode in &m.modes {
                let rates = mode
                    .refresh_rates
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("/");
                lines.push(format!("  {}x{}@{}", mode.width, mode.height, rates));
            }
        }
        g.set_disp_monitors_text(lines.join("\n").into());
    }
}

fn wire_ui(ui: &MainWindow, logs: &Logs) {
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_install(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, "安装虚拟摄像头…（委托 vdev-camera-win CLI）");
            run_camera_cli(&["install"], &ui, &logs);
            refresh_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_uninstall(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, "卸载虚拟摄像头…（委托 vdev-camera-win CLI）");
            run_camera_cli(&["uninstall"], &ui, &logs);
            refresh_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_refresh(move || {
            let Some(ui) = weak.upgrade() else { return };
            refresh_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_push(move || {
            let Some(ui) = weak.upgrade() else { return };
            toggle_push(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_display_install(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(
                &ui,
                &logs,
                "安装虚拟显示器…（委托 vdev-display-win CLI，将请求管理员权限）",
            );
            let cli = display_cli();
            let inf_dir = cli.as_deref().map(display_inf_dir);
            match (cli, inf_dir) {
                (Some(_exe), Some(dir)) => {
                    run_display_cli(
                        &["install", "--inf-dir", dir.to_string_lossy().as_ref()],
                        &ui,
                        &logs,
                    );
                }
                _ => append_log(&ui, &logs, "❌ 未找到 vdev-display-win.exe"),
            }
            refresh_display_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_display_uninstall(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, "卸载虚拟显示器…（将请求管理员权限）");
            run_display_cli(&["uninstall"], &ui, &logs);
            refresh_display_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_display_refresh(move || {
            let Some(ui) = weak.upgrade() else { return };
            refresh_display_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_display_add(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, "添加 1920x1080 虚拟屏…");
            run_display_cli(&["add", "1920x1080"], &ui, &logs);
            refresh_display_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_display_remove_all(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, "移除全部虚拟屏…");
            run_display_cli(&["remove-all"], &ui, &logs);
            refresh_display_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_audio_install(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(
                &ui,
                &logs,
                "安装虚拟声卡…（委托 vdev-audio-win CLI，将请求管理员权限）",
            );
            run_audio_cli(&["install"], &ui, &logs);
            refresh_audio_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_audio_uninstall(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, "卸载虚拟声卡…（将请求管理员权限）");
            run_audio_cli(&["uninstall"], &ui, &logs);
            refresh_audio_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_audio_refresh(move || {
            let Some(ui) = weak.upgrade() else { return };
            refresh_audio_status(&ui, &logs);
        });
    }
}

/// 推流开关：点击「开始推流 / 停止推流」切换。
fn toggle_push(ui: &MainWindow, logs: &Logs) {
    // 先尝试占用：已占用说明正在推流 → 停止。
    if PUSH_RUNNING.swap(true, Ordering::SeqCst) {
        // 主线程立即复位按钮（不依赖推流线程退出；线程可能卡在 SHM 写入）。
        ui.global::<AppState>().set_push_btn_text("开始推流".into());
        PUSH_RUNNING.store(false, Ordering::SeqCst);
        if let Some(h) = PUSH_THREAD.lock().unwrap().take() {
            let _ = h.join();
        }
        append_log(ui, logs, "推流已停止");
        return;
    }
    match vdev_camera_win::CameraServer::open() {
        Ok(server) => {
            // 切换按钮为「停止推流」（再点一次即停止）。
            ui.global::<AppState>().set_push_btn_text("停止推流".into());
            append_log(
                ui,
                logs,
                format!("推流开始（棋盘格 {PUSH_WIDTH}x{PUSH_HEIGHT}@{PUSH_FPS}fps）…"),
            );
            let weak = ui.as_weak();
            let logs2 = logs.clone();
            let handle = std::thread::Builder::new()
                .name("vdev-app-win-push".into())
                .spawn(move || {
                    let format = vdev_camera_win::VideoFormat {
                        width: PUSH_WIDTH,
                        height: PUSH_HEIGHT,
                        fps: PUSH_FPS,
                    };
                    let mut buf = Vec::new();
                    let mut t = 0.0f64;
                    let interval = std::time::Duration::from_secs_f64(1.0 / PUSH_FPS as f64);
                    let start = std::time::Instant::now();
                    let mut count: u64 = 0;
                    while PUSH_RUNNING.load(Ordering::SeqCst) {
                        vdev_camera_win::dshow::streaming::render_pattern(
                            &mut buf, &format, &mut t,
                        );
                        if let Err(e) = server.push_frame(PUSH_WIDTH, PUSH_HEIGHT, &buf) {
                            if let Some(ui) = weak.upgrade() {
                                append_log(&ui, &logs2, format!("推流失败: {e:#}"));
                            }
                            break;
                        }
                        count += 1;
                        let next = start + interval * (count as u32);
                        if let Some(remain) = next.checked_duration_since(std::time::Instant::now())
                        {
                            std::thread::sleep(remain);
                        }
                    }
                    PUSH_RUNNING.store(false, Ordering::SeqCst);
                    // UI 更新必须回到主线程事件循环执行（Slint 跨线程 setter 不可靠）。
                    let weak2 = weak.clone();
                    let logs3 = logs2.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = weak2.upgrade() {
                            append_log(&ui, &logs3, format!("推流线程结束，共 {count} 帧"));
                            ui.global::<AppState>().set_push_btn_text("开始推流".into());
                        }
                    });
                })
                .expect("spawn push thread");
            *PUSH_THREAD.lock().unwrap() = Some(handle);
        }
        Err(e) => append_log(ui, logs, format!("打开推流通道失败: {e:#}")),
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_hid_install(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(
                &ui,
                &logs,
                "安装虚拟键盘…（委托 vdev-hid-win CLI，将请求管理员权限）",
            );
            run_hid_cli(&["kernel", "install"], &ui, &logs);
            refresh_hid_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_hid_uninstall(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, "卸载虚拟键盘…（将请求管理员权限）");
            run_hid_cli(&["kernel", "uninstall"], &ui, &logs);
            refresh_hid_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_hid_refresh(move || {
            let Some(ui) = weak.upgrade() else { return };
            refresh_hid_status(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_hid_key(move |key| {
            let Some(ui) = weak.upgrade() else { return };
            let key: String = key.trim().to_string();
            if key.is_empty() {
                append_log(&ui, &logs, "⚠️ 键名为空");
                return;
            }
            append_log(&ui, &logs, format!("注入按键：{key}"));
            run_hid_cli(&["kernel", "key", &key], &ui, &logs);
        });
    }
}
