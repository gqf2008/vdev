//! VDCamera (Windows) — vdev 虚拟摄像头 GUI 宿主（Rust + Slint + slint-pixel）。
//!
//! 复用 macOS 版 vdev-app 的像素风 UI 与交互模式（状态面板 / 安装 / 卸载 /
//! 推流 / 日志），后端全部调用 `vdev-camera-win` 的安全封装（注册 / 注销 /
//! 推流 / 设备枚举）。本文件是纯业务层，不含 unsafe。

// release 构建隐藏控制台黑窗（GUI 程序）；debug 保留控制台便于看日志/panic。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use slint::Model as _;
use vdev_app_win::dsp_ui::{self, DspParams};
use vdev_app_win::{next_push_action, PushAction};

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
    init_dsp_ui(&ui);

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

/// `vdev-audio-win capture --json` 的输出（只取电平字段用于 GUI 显示）
#[derive(Debug, serde::Deserialize)]
struct AudioLevelOut {
    rms_dbfs: f64,
    peak_dbfs: f64,
    frames: u64,
    seconds: f64,
}

/// 从 GUI 的字符串输入里取数值，非法时回退默认值（GUI 不因手输错字就卡住）
fn num_or<T: std::str::FromStr + Copy>(s: &str, default: T) -> T {
    s.trim().parse::<T>().unwrap_or(default)
}

/// 注入 / 环回自测（在后台线程跑 CLI，避免阻塞 UI 事件循环）。
///
/// `loopback=true` 时并发做「capture ‖ inject」——顺序执行会读到环形缓冲里的历史数据
/// （驱动侧环回有 1 MB ≈ 1.36 s 积压），只有并发才能量出本次注入的电平。
fn spawn_audio_test(ui: &MainWindow, logs: &Logs, loopback: bool) {
    let Some(exe) = audio_cli() else {
        append_log(
            ui,
            logs,
            "❌ 未找到 vdev-audio-win.exe（设置 VDEV_AUDIO_WIN_EXE 或放到 GUI 同目录）",
        );
        return;
    };
    let g = ui.global::<AppState>();
    let tone = num_or(&g.get_audio_tone(), 1000.0f64);
    let duration = num_or(&g.get_audio_duration(), 3.0f64).clamp(0.2, 30.0);
    let amplitude = num_or(&g.get_audio_amplitude(), 0.5f64).clamp(0.0, 1.0);
    g.set_audio_busy(true);
    let busy_text = if loopback {
        "环回自测中：并发 注入 → 采集（约需时长 + 2 秒）…"
    } else {
        "注入中…"
    };
    g.set_audio_level_text(busy_text.into());
    append_log(
        ui,
        logs,
        if loopback {
            format!(
                "环回自测：{tone:.0} Hz / 幅度 {amplitude:.2} / {duration:.1}s（capture ‖ inject）"
            )
        } else {
            format!("注入音频：{tone:.0} Hz / 幅度 {amplitude:.2} / {duration:.1}s")
        },
    );

    let weak = ui.as_weak();
    let logs2 = logs.clone();
    let injected = format!("{tone}");
    let spawned = std::thread::Builder::new()
        .name("vdev-app-win-audio".into())
        .spawn(move || {
            let dur = format!("{duration}");
            let amp = format!("{amplitude}");
            // 环回：先起采集（skip 1s 避开起播瞬间），0.5s 后开始注入，再等采集收工
            let result: Result<Option<AudioLevelOut>, String> = if loopback {
                let dur_cap = format!("{}", duration + 2.0);
                let capture = std::process::Command::new(&exe)
                    .args(["capture", "--duration", &dur_cap, "--skip", "1", "--json"])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn();
                match capture {
                    Err(e) => Err(format!("启动 capture 失败：{e}")),
                    Ok(child) => {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let inj = std::process::Command::new(&exe)
                            .args([
                                "inject",
                                "--tone",
                                &injected,
                                "--amplitude",
                                &amp,
                                "--duration",
                                &dur,
                            ])
                            .output();
                        match inj {
                            Err(e) => Err(format!("运行 inject 失败：{e}")),
                            Ok(inj) if !inj.status.success() => Err(format!(
                                "inject 失败：{}",
                                String::from_utf8_lossy(&inj.stderr).trim()
                            )),
                            Ok(_) => match child.wait_with_output() {
                                Err(e) => Err(format!("等待 capture 失败：{e}")),
                                Ok(out) => {
                                    let text = String::from_utf8_lossy(&out.stdout).to_string();
                                    match serde_json::from_str::<AudioLevelOut>(text.trim()) {
                                        Ok(lv) => Ok(Some(lv)),
                                        Err(e) => Err(format!(
                                            "解析 capture 输出失败：{e}（原始：{}）",
                                            text.trim()
                                        )),
                                    }
                                }
                            },
                        }
                    }
                }
            } else {
                std::process::Command::new(&exe)
                    .args([
                        "inject",
                        "--tone",
                        &injected,
                        "--amplitude",
                        &amp,
                        "--duration",
                        &dur,
                    ])
                    .output()
                    .map(|_| None)
                    .map_err(|e| format!("运行 inject 失败：{e}"))
            };

            // 回到 UI 事件循环更新界面（Slint 跨线程 setter 不可靠）
            let weak2 = weak.clone();
            let logs3 = logs2.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak2.upgrade() else { return };
                let g = ui.global::<AppState>();
                g.set_audio_busy(false);
                match result {
                    Ok(Some(lv)) => {
                        let verdict = if lv.rms_dbfs > -60.0 {
                            "通过（采到有效音频）"
                        } else {
                            "未通过（接近静音：确认已安装驱动且端点未被独占）"
                        };
                        g.set_audio_level_text(
                            format!(
                                "环回自测 {verdict}：RMS {:.1} dBFS / 峰值 {:.1} dBFS（{} 帧 ≈ {:.2}s）",
                                lv.rms_dbfs, lv.peak_dbfs, lv.frames, lv.seconds
                            )
                            .into(),
                        );
                        append_log(
                            &ui,
                            &logs3,
                            format!(
                                "环回自测：RMS {:.1} dBFS / 峰值 {:.1} dBFS → {verdict}",
                                lv.rms_dbfs, lv.peak_dbfs
                            ),
                        );
                    }
                    Ok(None) => {
                        g.set_audio_level_text(
                            "注入完成（未做采集）。点「环回自测」可一键验证环回。".into(),
                        );
                        append_log(&ui, &logs3, "注入完成");
                    }
                    Err(e) => {
                        g.set_audio_level_text(format!("测试失败：{e}").into());
                        append_log(&ui, &logs3, format!("❌ {e}"));
                    }
                }
            });
        });
    if let Err(e) = spawned {
        ui.global::<AppState>().set_audio_busy(false);
        ui.global::<AppState>()
            .set_audio_level_text(format!("创建测试线程失败：{e}").into());
        append_log(ui, logs, format!("创建测试线程失败: {e}"));
    }
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

// ---------------------------------------------------------------- 音频增强（DSP）
//
// 面板上的每个控件都直接驱动 vdev-dsp：曲线用 GraphicEq::magnitude_response_db
// 的真实系数逐点求值，离线渲染走 GraphicEq → Leveling → Limiter 三级链路。

/// 曲线画布内层像素尺寸（与 ui/main.slint 里 Path 的 width/height 一致）。
const DSP_CURVE_W: f32 = 556.0;
const DSP_CURVE_H: f32 = 136.0;
/// 曲线纵轴范围（dB）。
const DSP_CURVE_DB_MIN: f64 = -12.0;
const DSP_CURVE_DB_MAX: f64 = 12.0;
/// 频响曲线预览用的采样率（正式离线渲染跟随被渲染文件的采样率）。
const DSP_PREVIEW_SAMPLE_RATE: f64 = 48_000.0;

/// 频点标签：31 / 62 / 125 / 1k / 2k …
fn band_label(hz: f64) -> String {
    if hz >= 1000.0 {
        let k = hz / 1000.0;
        if (k - k.round()).abs() < 0.05 {
            format!("{k:.0}k")
        } else {
            format!("{k:.1}k")
        }
    } else {
        format!("{hz:.0}")
    }
}

/// 从 UI 读回 10 段增益（dB）。
fn dsp_gains_from_ui(ui: &MainWindow) -> [f64; dsp_ui::EQ_BANDS] {
    let model = ui.global::<AppState>().get_eq_gains();
    let mut out = [0.0_f64; dsp_ui::EQ_BANDS];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = f64::from(model.row_data(i).unwrap_or(0.0));
    }
    out
}

/// 把面板上的全部参数收拢成 `DspParams`（越界/手输错的值统一夹到合法范围）。
fn dsp_params_from_ui(ui: &MainWindow) -> DspParams {
    let g = ui.global::<AppState>();
    DspParams {
        gains_db: dsp_gains_from_ui(ui),
        eq_bypass: g.get_eq_bypass(),
        leveling_enabled: g.get_leveling_on(),
        target_lufs: num_or(&g.get_target_lufs(), -14.0),
        limiter_enabled: g.get_limiter_on(),
        ceiling_db: num_or(&g.get_ceiling_db(), -1.0),
        sample_rate: DSP_PREVIEW_SAMPLE_RATE,
    }
    .clamped()
}

/// 用 vdev-dsp 的真实系数重算频响曲线并推给 UI。
///
/// 曲线不是画出来的、也不是写死的数组：每个频点都调
/// `GraphicEq::magnitude_response_db`（同一批 biquad 系数）求值。滑块一动就重算。
fn refresh_dsp_curve(ui: &MainWindow) {
    let p = dsp_params_from_ui(ui);
    let path = dsp_ui::curve_path(
        &p,
        DSP_CURVE_W,
        DSP_CURVE_H,
        DSP_CURVE_DB_MIN,
        DSP_CURVE_DB_MAX,
    );
    let peak = p.gains_db.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let trough = p.gains_db.iter().copied().fold(f64::INFINITY, f64::min);
    let note = if p.eq_bypass {
        "EQ 旁路：整链恒等，频响平直于 0 dB".to_string()
    } else if peak.abs() < 1.0e-9 && trough.abs() < 1.0e-9 {
        "全 0 增益：频响平直于 0 dB".to_string()
    } else {
        format!(
            "增益 {trough:+.1} .. {peak:+.1} dB · {} 点 · {:.0} Hz（vdev-dsp GraphicEq 系数）",
            dsp_ui::CURVE_POINTS,
            p.sample_rate
        )
    };
    let g = ui.global::<AppState>();
    g.set_dsp_curve_path(path.into());
    g.set_dsp_curve_note(note.into());
}

/// 把 10 段增益一次性写成同一个值（「全部归零」）。
fn set_all_gains(ui: &MainWindow, value: f32) {
    let model = ui.global::<AppState>().get_eq_gains();
    for i in 0..dsp_ui::EQ_BANDS {
        model.set_row_data(i, value);
    }
}

/// 启动时初始化 DSP 面板：频点标签直接取 vdev-dsp 的默认网格（UI 侧不另写频率表）。
fn init_dsp_ui(ui: &MainWindow) {
    let labels: Vec<slint::SharedString> = dsp_ui::eq_band_frequencies_hz(DSP_PREVIEW_SAMPLE_RATE)
        .iter()
        .map(|f| band_label(*f).into())
        .collect();
    let g = ui.global::<AppState>();
    g.set_eq_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
    g.set_eq_gains(slint::ModelRc::new(slint::VecModel::from(vec![
        0.0_f32;
        dsp_ui::EQ_BANDS
    ])));
    refresh_dsp_curve(ui);
}

/// 输出路径留空时的默认值：输入同目录下的 `<名字>_dsp.wav`。
fn default_output_path(input: &str) -> String {
    let p = Path::new(input);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "out".to_string());
    let dir = p
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    dir.join(format!("{stem}_dsp.wav"))
        .to_string_lossy()
        .into_owned()
}

/// 后台生成示例素材（零依赖合成：多音节目 + 轻节目 + 白噪声），供离线渲染对比。
fn spawn_make_demo_wav(ui: &MainWindow, logs: &Logs) {
    let path_s = std::env::temp_dir()
        .join("vdev-dsp-demo.wav")
        .to_string_lossy()
        .into_owned();
    append_log(
        ui,
        logs,
        format!("生成示例 wav：{path_s}（48 kHz / 立体声 / 6 s）"),
    );
    let weak = ui.as_weak();
    let logs2 = logs.clone();
    let spawned = std::thread::Builder::new()
        .name("vdev-app-win-dsp-demo".into())
        .spawn(move || {
            let res = dsp_ui::synth_demo_wav(&path_s);
            let weak2 = weak.clone();
            let logs3 = logs2.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak2.upgrade() else { return };
                match res {
                    Ok(data) => {
                        let g = ui.global::<AppState>();
                        g.set_render_input(path_s.clone().into());
                        if g.get_render_output().trim().is_empty() {
                            g.set_render_output(default_output_path(&path_s).into());
                        }
                        g.set_render_result(
                            format!(
                                "示例素材已生成：{} Hz / {} 声道 / {} 帧（{:.2} s）。点「渲染并测量」跑整链。",
                                data.sample_rate,
                                data.channels,
                                data.frames(),
                                data.frames() as f64 / f64::from(data.sample_rate)
                            )
                            .into(),
                        );
                        append_log(&ui, &logs3, "✅ 示例 wav 已生成");
                    }
                    Err(e) => append_log(&ui, &logs3, format!("❌ 生成示例 wav 失败：{e}")),
                }
            });
        });
    if spawned.is_err() {
        append_log(ui, logs, "❌ 创建示例素材线程失败");
    }
}

/// 系统「打开文件」对话框挑 wav（PowerShell + WinForms，零第三方依赖）。
fn spawn_pick_wav(ui: &MainWindow, logs: &Logs) {
    let weak = ui.as_weak();
    let logs2 = logs.clone();
    let spawned = std::thread::Builder::new()
        .name("vdev-app-win-dsp-pick".into())
        .spawn(move || {
            let script = "Add-Type -AssemblyName System.Windows.Forms;\
                $d = New-Object System.Windows.Forms.OpenFileDialog;\
                $d.Filter = 'WAV 音频 (*.wav)|*.wav|所有文件 (*.*)|*.*';\
                $d.Title = '选择要渲染的 wav';\
                if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { [Console]::Out.Write($d.FileName) }";
            let picked = std::process::Command::new("powershell")
                .args(["-NoProfile", "-STA", "-Command", script])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty());
            let weak2 = weak.clone();
            let logs3 = logs2.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak2.upgrade() else { return };
                match picked {
                    Some(p) => {
                        let g = ui.global::<AppState>();
                        g.set_render_input(p.clone().into());
                        if g.get_render_output().trim().is_empty() {
                            g.set_render_output(default_output_path(&p).into());
                        }
                        append_log(&ui, &logs3, format!("已选择 wav：{p}"));
                    }
                    None => append_log(
                        &ui,
                        &logs3,
                        "⚠️ 未选择文件（或系统对话框不可用：可直接手输路径或点「生成示例 wav」）",
                    ),
                }
            });
        });
    if spawned.is_err() {
        append_log(ui, logs, "❌ 创建文件选择线程失败");
    }
}

/// 离线渲染：读 wav → 整链（EQ → 响度归一化 → 限幅）→ 写 wav + 测量。
/// 跑在后台线程，结果回事件循环；面板上的参数就是本次渲染用的参数。
fn spawn_dsp_render(ui: &MainWindow, logs: &Logs) {
    let input = ui
        .global::<AppState>()
        .get_render_input()
        .trim()
        .to_string();
    if input.is_empty() {
        ui.global::<AppState>()
            .set_render_result("请先选择或输入一个 wav 路径（可点「生成示例 wav」）。".into());
        append_log(ui, logs, "⚠️ 未指定输入 wav");
        return;
    }
    let out_field = ui
        .global::<AppState>()
        .get_render_output()
        .trim()
        .to_string();
    let output = if out_field.is_empty() {
        default_output_path(&input)
    } else {
        out_field
    };
    let params = dsp_params_from_ui(ui);
    ui.global::<AppState>().set_render_busy(true);
    ui.global::<AppState>()
        .set_render_result(format!("渲染中…\n输入 {input}\n输出 {output}").into());
    append_log(ui, logs, format!("DSP 离线渲染：{input} → {output}"));

    let weak = ui.as_weak();
    let logs2 = logs.clone();
    let spawned = std::thread::Builder::new()
        .name("vdev-app-win-dsp-render".into())
        .spawn(move || {
            let result = dsp_ui::render_file(&input, &output, &params);
            let weak2 = weak.clone();
            let logs3 = logs2.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak2.upgrade() else { return };
                ui.global::<AppState>().set_render_busy(false);
                match result {
                    Ok(r) => {
                        ui.global::<AppState>().set_render_result(r.summary().into());
                        append_log(
                            &ui,
                            &logs3,
                            format!(
                                "✅ 渲染完成：输出 RMS {:.2} dBFS / 真峰值 {:.2} dBTP / 伺服 {:+.2} dB",
                                r.out_rms_dbfs, r.out_true_peak_dbtp, r.final_servo_gain_db
                            ),
                        );
                    }
                    Err(e) => {
                        ui.global::<AppState>()
                            .set_render_result(format!("渲染失败：{e}").into());
                        append_log(&ui, &logs3, format!("❌ DSP 渲染失败：{e}"));
                    }
                }
            });
        });
    if let Err(e) = spawned {
        ui.global::<AppState>().set_render_busy(false);
        ui.global::<AppState>()
            .set_render_result(format!("创建渲染线程失败：{e}").into());
        append_log(ui, logs, format!("创建渲染线程失败: {e}"));
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
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_audio_inject(move || {
            let Some(ui) = weak.upgrade() else { return };
            spawn_audio_test(&ui, &logs, false);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_audio_loopback(move || {
            let Some(ui) = weak.upgrade() else { return };
            spawn_audio_test(&ui, &logs, true);
        });
    }
    // M1：HID 回调注册必须在 wire_ui 一次性完成——此前写在 toggle_push 里，
    // 每点一次推流就重复注册一份（旧回调不注销 → 重复日志/重复刷新）；且
    // 鼠标回调原本嵌在 key 回调闭包内，首次注入按键前甚至未注册。这些回调
    // 均由 UI 事件循环线程触发，不涉及跨线程（推流线程才是 M2 的问题点）。
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
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_hid_mouse_click(move |button| {
            let Some(ui) = weak.upgrade() else { return };
            let button: String = button.trim().to_string();
            append_log(&ui, &logs, format!("注入鼠标点击：{button}"));
            run_hid_cli(&["kernel", "mouse", "click", &button], &ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_hid_mouse_move(move |dx, dy| {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, format!("注入鼠标相对移动（{dx},{dy}）"));
            run_hid_cli(
                &["kernel", "mouse", "move", &dx.to_string(), &dy.to_string()],
                &ui,
                &logs,
            );
        });
    }
    // ---- 音频增强（DSP）：控件改动即时重算频响曲线（真实 vdev-dsp 系数） ----
    {
        let weak = ui.as_weak();
        ui.on_dsp_eq_changed(move |idx, value| {
            let Some(ui) = weak.upgrade() else { return };
            let clamped = f64::from(value).clamp(dsp_ui::MIN_GAIN_DB, dsp_ui::MAX_GAIN_DB) as f32;
            let model = ui.global::<AppState>().get_eq_gains();
            model.set_row_data(idx.max(0) as usize, clamped);
            refresh_dsp_curve(&ui);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_dsp_bypass_toggled(move || {
            let Some(ui) = weak.upgrade() else { return };
            let g = ui.global::<AppState>();
            let on = !g.get_eq_bypass();
            g.set_eq_bypass(on);
            refresh_dsp_curve(&ui);
            append_log(
                &ui,
                &logs,
                if on {
                    "EQ 旁路：开（整链恒等）"
                } else {
                    "EQ 旁路：关"
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_dsp_zero_all(move || {
            let Some(ui) = weak.upgrade() else { return };
            set_all_gains(&ui, 0.0);
            refresh_dsp_curve(&ui);
            append_log(&ui, &logs, "EQ 全部归零");
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_dsp_leveling_toggled(move || {
            let Some(ui) = weak.upgrade() else { return };
            let g = ui.global::<AppState>();
            append_log(
                &ui,
                &logs,
                format!(
                    "响度归一化：{} · 目标 {} LUFS",
                    if g.get_leveling_on() { "开" } else { "关" },
                    g.get_target_lufs()
                ),
            );
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_dsp_limiter_toggled(move || {
            let Some(ui) = weak.upgrade() else { return };
            let g = ui.global::<AppState>();
            append_log(
                &ui,
                &logs,
                format!(
                    "真峰值限幅：{} · ceiling {} dBFS",
                    if g.get_limiter_on() { "开" } else { "关" },
                    g.get_ceiling_db()
                ),
            );
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_dsp_pick_wav(move || {
            let Some(ui) = weak.upgrade() else { return };
            spawn_pick_wav(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_dsp_make_demo(move || {
            let Some(ui) = weak.upgrade() else { return };
            spawn_make_demo_wav(&ui, &logs);
        });
    }
    {
        let weak = ui.as_weak();
        let logs = logs.clone();
        ui.on_dsp_render(move || {
            let Some(ui) = weak.upgrade() else { return };
            spawn_dsp_render(&ui, &logs);
        });
    }
}

/// 推流开关：点击「开始推流 / 停止推流」切换。
///
/// 本函数只做启停（M1：HID 等回调注册一律放在 wire_ui 一次性完成，
/// 此前写在这里导致每点一次推流就重复注册一份回调）。状态转移决策在
/// `next_push_action`（lib.rs，宿主可测）；所有错误路径必须复位
/// PUSH_RUNNING（M4），否则按钮卡在「停止推流」态且无法再次启动。
fn toggle_push(ui: &MainWindow, logs: &Logs) {
    // swap(true)：原子占用——原值 true 说明正在推流 → 停止；false → 开始。
    match next_push_action(PUSH_RUNNING.swap(true, Ordering::SeqCst)) {
        PushAction::Stop => {
            // 停止即复位标志（主线程立即复位按钮，不依赖推流线程退出；
            // 线程可能卡在 SHM 写入，join 放在复位之后）。
            PUSH_RUNNING.store(false, Ordering::SeqCst);
            ui.global::<AppState>().set_push_btn_text("开始推流".into());
            if let Some(h) = PUSH_THREAD.lock().unwrap().take() {
                let _ = h.join();
            }
            append_log(ui, logs, "推流已停止");
        }
        PushAction::Start => match vdev_camera_win::CameraServer::open() {
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
                let spawned = std::thread::Builder::new()
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
                                // M2：不得在推流线程直接 upgrade()/调 Slint setter——
                                // Weak::upgrade() 在非创建线程恒返回 None（日志静默
                                // 丢失），跨线程直接改 UI 状态本身也是 UB。统一投递回
                                // 事件循环执行；闭包只捕获 Send 的 Weak 与
                                // Arc<Mutex<_>>，upgrade 在 UI 线程内完成。
                                let err = format!("{e:#}");
                                let weak2 = weak.clone();
                                let logs3 = logs2.clone();
                                let _ = slint::invoke_from_event_loop(move || {
                                    if let Some(ui) = weak2.upgrade() {
                                        append_log(&ui, &logs3, format!("推流失败: {err}"));
                                    }
                                });
                                break;
                            }
                            count += 1;
                            let next = start + interval * (count as u32);
                            if let Some(remain) =
                                next.checked_duration_since(std::time::Instant::now())
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
                    });
                match spawned {
                    Ok(handle) => *PUSH_THREAD.lock().unwrap() = Some(handle),
                    Err(e) => {
                        // M4：线程创建失败也是启动失败——复位标志并还原按钮，
                        // 否则按钮卡在「停止推流」态且无法再次启动。
                        PUSH_RUNNING.store(false, Ordering::SeqCst);
                        ui.global::<AppState>().set_push_btn_text("开始推流".into());
                        append_log(ui, logs, format!("创建推流线程失败: {e}"));
                    }
                }
            }
            Err(e) => {
                // M4：启动失败必须复位标志，否则下次点击走停止分支，无法再次启动。
                PUSH_RUNNING.store(false, Ordering::SeqCst);
                append_log(ui, logs, format!("打开推流通道失败: {e:#}"));
            }
        },
    }
}
