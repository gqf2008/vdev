//! VDCamera (Windows) — vdev 虚拟摄像头 GUI 宿主（Rust + Slint + slint-pixel）。
//!
//! 复用 macOS 版 vdev-app 的像素风 UI 与交互模式（状态面板 / 安装 / 卸载 /
//! 推流 / 日志），后端全部调用 `vdev-camera-win` 的安全封装（注册 / 注销 /
//! 推流 / 设备枚举）。本文件是纯业务层，不含 unsafe。

use std::path::PathBuf;
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
    ui.global::<AppState>()
        .set_log_text(v.join("\n").into());
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
                        vdev_camera_win::dshow::streaming::render_pattern(&mut buf, &format, &mut t);
                        if let Err(e) = server.push_frame(PUSH_WIDTH, PUSH_HEIGHT, &buf) {
                            if let Some(ui) = weak.upgrade() {
                                append_log(&ui, &logs2, format!("推流失败: {e:#}"));
                            }
                            break;
                        }
                        count += 1;
                        let next = start + interval * (count as u32);
                        if let Some(remain) = next.checked_duration_since(std::time::Instant::now()) {
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
}
