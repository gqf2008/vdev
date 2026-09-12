//! `VDCamera` — vdev 摄像头宿主（Rust + Slint）
// 档位对齐 vdev-audio-win：CoreAudio/Cocoa FFI 镜像结构与宽度转换属惯例。
#![allow(clippy::cast_possible_truncation)] // FFI 结构字段宽度转换
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)] // 时间戳/进度换算的整数↔浮点
#![allow(clippy::struct_field_names)] // CoreAudio C 头结构体 m 前缀字段按原样镜像
mod audio;
mod camera;
mod frame;
mod screen;
mod sysext;
mod video;
mod vimage;
mod vscreen;

use std::sync::{Arc, Mutex};
use std::time::Duration;

slint::include_modules!();
slint_pixel::impl_title_bar_ui!(MainWindow);

const BUNDLE_ID: &str = "com.vdev.camera.host.extension";
const SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.ExtensionsPreferences";
const QUICKTIME_PATH: &str = "/System/Applications/QuickTime Player.app";

type Logs = Arc<Mutex<Vec<String>>>;

#[derive(Clone, Copy, PartialEq, Debug)]
enum PushMode {
    ScreenMain,
    ScreenVd,
    Video,
}

static PUSH_MODE: Mutex<Option<PushMode>> = Mutex::new(None);
static VIDEO_STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static VIDEO_CLIENT: Mutex<Option<frame::FrameClient>> = Mutex::new(None);

/// 生产/自测共用的发帧路径：客户端未建立则 `connect()`，发送失败则重置为 None，
/// 下一帧自动重连。返回本帧是否成功发出。
/// 之前生产路径 `let _ = send_frame` 吞错不重置——扩展重启后旧 TCP 连接永久发失败，
/// 画面冻结；只有自测路径会重连（与 README 承诺不符）。抽成单函数保证各路径同源。
/// 泛型 + 注入 connect 是为了单测：真实 connect 会连 127.0.0.1:27890 并重试约 30s。
fn send_or_reconnect<C: frame::FrameSender>(
    client: &mut Option<C>,
    connect: impl FnOnce() -> Option<C>,
    buf: &[u8],
    w: u32,
    h: u32,
    stride: u32,
    pts_ns: u64,
) -> bool {
    if client.is_none() {
        *client = connect();
    }
    match client.as_mut() {
        Some(c) => {
            if c.send_frame(buf, w, h, stride, pts_ns).is_ok() {
                true
            } else {
                *client = None; // 连接断了，下一帧自动重连
                false
            }
        }
        None => false,
    }
}

fn set_btn_texts(ui: &MainWindow) {
    let g = ui.global::<AppState>();
    let mode = *PUSH_MODE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    g.set_screen_btn_text(if mode == Some(PushMode::ScreenMain) {
        "停止屏幕推流".into()
    } else {
        "屏幕推流".into()
    });
    g.set_video_btn_text(if mode == Some(PushMode::Video) {
        "停止视频推流".into()
    } else {
        "视频推流".into()
    });
    g.set_vd_push_btn_text(if mode == Some(PushMode::ScreenVd) {
        "停止推虚拟屏".into()
    } else {
        "推虚拟屏幕".into()
    });
}

fn stop_current_push(ui: &MainWindow, logs: &Logs) {
    let mode = *PUSH_MODE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match mode {
        Some(PushMode::ScreenMain | PushMode::ScreenVd) => {
            screen::stop();
            append_log(ui, logs, "屏幕推流已停止");
        }
        Some(PushMode::Video) => {
            // 解码/音频线程的退出条件是 VIDEO_STOP 或文件播完（发送错误被 `let _ =`
            // 吞掉不会让线程退出），必须显式置位，否则切换推流时旧线程与新连接并发
            // 写扩展唯一的 INJECTED 槽导致画面互串。线程退出需真实解码+TCP，无法单测。
            VIDEO_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
            append_log(ui, logs, "视频推流已停止");
        }
        None => {}
    }
    *PUSH_MODE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    set_btn_texts(ui);
}

fn start_screen_push(ui: &MainWindow, logs: &Logs, display_id: u32, mode: PushMode) {
    stop_current_push(ui, logs);
    append_log(ui, logs, format!("屏幕推流开始（显示器 {display_id:#x}）"));
    *PUSH_MODE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(mode);
    set_btn_texts(ui);
    let client: Arc<Mutex<Option<frame::FrameClient>>> = Arc::new(Mutex::new(None));
    let client_cb = client.clone();
    if let Err(e) = screen::start(
        display_id,
        Box::new(move |buf, w, h, stride| {
            let mut guard = client_cb
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // 发送失败即重置，下一帧自动重连（与自测路径共用同一实现）
            send_or_reconnect(
                &mut guard,
                || frame::connect().ok(),
                &buf,
                w,
                h,
                stride,
                video::host_time_ns(),
            );
        }),
    ) {
        append_log(ui, logs, format!("屏幕推流失败: {e}"));
        *PUSH_MODE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        set_btn_texts(ui);
    }
}

/// 自动修复 launchd 竞态：停用 → 等待清理 → 重新启用 → 确认摄像头。
/// log 在后台线程回调。
fn recover_extension(log: &Arc<dyn Fn(String) + Send + Sync>) {
    log("自动修复：先停用再重新启用扩展…".to_string());
    let l2 = log.clone();
    let cb = move |ev: sysext::SysextEvent| {
        let lg = l2.clone();
        match ev {
            sysext::SysextEvent::Finished(n) => {
                if n == 0 {
                    lg("已停用，等待 6s 后重新启用…".to_string());
                } else {
                    // 非零结果说明停用未按预期确认：照常尝试重启用，但必须暴露在日志
                    lg(format!("停用返回非零结果 {n}，仍尝试重新启用…"));
                }
                let lg2 = lg.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_secs(6));
                    lg2("重新启用…".to_string());
                    let lg3 = lg2.clone();
                    let cb3 = move |ev3: sysext::SysextEvent| {
                        let lg4 = lg3.clone();
                        match ev3 {
                            sysext::SysextEvent::Finished(0) => {
                                lg4("启用完成，确认摄像头…".to_string());
                                let lg5 = lg4.clone();
                                std::thread::spawn(move || {
                                    let mut ok = false;
                                    for _ in 0..30 {
                                        if camera::find_vdev() {
                                            ok = true;
                                            break;
                                        }
                                        std::thread::sleep(Duration::from_millis(500));
                                    }
                                    lg5(if ok {
                                        "✅ 摄像头已恢复".to_string()
                                    } else {
                                        "❌ 仍未出现：请到 系统设置 → … → 相机扩展 关闭再打开，或重启电脑".to_string()
                                    });
                                });
                            }
                            sysext::SysextEvent::NeedsApproval => {
                                lg4("重新启用需要批准：系统设置 → … → 相机扩展".to_string());
                            }
                            sysext::SysextEvent::Failed(e) => {
                                lg4(format!("重新启用失败: {e}"));
                            }
                            sysext::SysextEvent::Finished(n) => {
                                // 之前非零结果落入空分支静默（并伴随 unreachable 误报）
                                lg4(format!(
                                    "重新启用返回非零结果 {n}，请到 系统设置 → … → 相机扩展 手动检查"
                                ));
                            }
                        }
                    };
                    let _ = sysext::submit(BUNDLE_ID, true, Box::new(cb3));
                });
            }
            sysext::SysextEvent::NeedsApproval => lg("停用需要系统确认".to_string()),
            sysext::SysextEvent::Failed(e) => lg(format!("停用失败: {e}")),
        }
    };
    let _ = sysext::submit(BUNDLE_ID, false, Box::new(cb));
}

fn set_status(ui: &MainWindow, glyph: &str, title: &str, detail: &str) {
    let g = ui.global::<AppState>();
    g.set_status_glyph(glyph.into());
    g.set_status_title(title.into());
    g.set_status_detail(detail.into());
}

fn set_audio_status(ui: &MainWindow, glyph: &str, title: &str, detail: &str) {
    let g = ui.global::<AppState>();
    g.set_audio_glyph(glyph.into());
    g.set_audio_title(title.into());
    g.set_audio_detail(detail.into());
}

#[allow(clippy::fn_params_excessive_bools)] // 四个按钮开关的直白映射
fn set_enabled(ui: &MainWindow, install: bool, uninstall: bool, push: bool, vd_push: bool) {
    let g = ui.global::<AppState>();
    g.set_can_install(install);
    g.set_can_uninstall(uninstall);
    g.set_can_push(push);
    g.set_can_vd_push(vd_push);
}

fn append_log(ui: &MainWindow, logs: &Logs, s: impl AsRef<str>) {
    let s = s.as_ref();
    let mut v = logs
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    v.push(s.to_string());
    if v.len() > 300 {
        v.remove(0);
    }
    ui.global::<AppState>().set_log_text(v.join("\n").into());
}

fn open_url(url: &str) {
    let Some(cls) = objc2::runtime::AnyClass::get(c"NSWorkspace") else {
        return;
    };
    // SAFETY：sharedWorkspace 为无参类方法，返回系统单例
    let ws: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![cls, sharedWorkspace] };
    if ws.is_null() {
        return;
    }
    let Some(url_cls) = objc2::runtime::AnyClass::get(c"NSURL") else {
        return;
    };
    // SAFETY：URLWithString 按字符串构造 NSURL，临时 NSString 在表达式内存活
    let ns_url: *mut objc2::runtime::AnyObject = unsafe {
        objc2::msg_send![url_cls, URLWithString: &*objc2_foundation::NSString::from_str(url)]
    };
    if !ns_url.is_null() {
        // SAFETY：ns_url 非空时指向 NSURL，ws 指向 NSWorkspace 单例
        let url_ref: &objc2::runtime::AnyObject = unsafe { &*ns_url };
        let ws_ref: &objc2::runtime::AnyObject = unsafe { &*ws };
        let _: bool = unsafe { objc2::msg_send![ws_ref, openURL: url_ref] };
    }
}

fn open_quicktime() {
    let url = format!("file://{}", QUICKTIME_PATH.replace(' ', "%20"));
    open_url(&url);
}

fn refresh_status(ui: &MainWindow, logs: &Logs) {
    append_log(ui, logs, "刷新状态…");
    let found = camera::find_vdev();
    if found {
        set_status(
            ui,
            "✓",
            "已安装，摄像头可用",
            "系统已能看到 vdev-camera。打开 QuickTime → 新建影片录制 → 选择 vdev-camera。",
        );
        set_enabled(ui, false, true, true, vscreen::display_id().is_some());
    } else {
        set_status(
            ui,
            "○",
            "虚拟摄像头未安装",
            "点击「安装虚拟摄像头」，然后在系统提示中允许扩展。",
        );
        set_enabled(ui, true, false, false, false);
    }
    // 虚拟声卡检测：vdev-audio（输出环回输入，音频推流目标）
    let audio_ok = audio::find_vdev_audio();
    if audio_ok {
        set_audio_status(
            ui,
            "✓",
            "虚拟声卡可用",
            "系统已能看到 vdev-audio。会议/录制软件把麦克风选成 vdev-audio 即可收到视频音轨。",
        );
        append_log(ui, logs, "检测到虚拟声卡：vdev-audio");
    } else {
        set_audio_status(
            ui,
            "○",
            "虚拟声卡未安装",
            "未检测到 vdev-audio。在 crates/vdev-audio 下执行 make install 安装。",
        );
        append_log(
            ui,
            logs,
            "未检测到虚拟声卡：vdev-audio（cd crates/vdev-audio && make install）",
        );
    }
}

// selftest 分支 + UI 声明周期线性排布（拆分需跨分支传递 ui/logs）；wire_ui 内嵌为其私有接线表
#[allow(clippy::too_many_lines, clippy::items_after_statements)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    // panic 落盘：GUI 无 stderr，崩溃后可读 $HOME/vdev-panic.log 定位
    // （沙盒 App 写不了 /tmp，无 HOME 时才退回 /tmp/vdev-panic.log）
    std::panic::set_hook(Box::new(|info| {
        let msg = format!(
            "[unix={}] {}\n",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            info
        );
        // 沙盒 App 写不了 /tmp，优先写 $HOME（容器目录）
        let path = std::env::var("HOME").map_or_else(
            |_| "/tmp/vdev-panic.log".to_string(),
            |h| format!("{h}/vdev-panic.log"),
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
        eprintln!("{info}");
    }));
    video::ensure_nsapp();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--selftest-audio") {
        // 虚拟声卡检测自测：枚举 CoreAudio 设备，找 vdev-audio
        println!("selftest-audio: vdev-audio={}", audio::find_vdev_audio());
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest-openpanel") {
        // 只验证 NSOpenPanel 可创建（不弹窗），沙盒权限自测用
        match video::openpanel_selftest() {
            Ok(()) => println!("selftest-openpanel: OK"),
            Err(e) => println!("selftest-openpanel: FAIL {e}"),
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest-screen") {
        let dur = args
            .iter()
            .position(|a| a == "--dur")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(8);
        let client: Arc<Mutex<Option<frame::FrameClient>>> = Arc::new(Mutex::new(None));
        let sent: Arc<std::sync::atomic::AtomicU64> =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let client_cb = client.clone();
        let sent_cb = sent.clone();
        println!("selftest: 屏幕推流 {dur}s …");
        screen::start(
            screen::main_display_id(),
            Box::new(move |buf, w, h, stride| {
                let mut guard = client_cb
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if send_or_reconnect(
                    &mut guard,
                    || frame::connect().ok(),
                    &buf,
                    w,
                    h,
                    stride,
                    video::host_time_ns(),
                ) {
                    let n = sent_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                    if n.is_multiple_of(60) {
                        println!("selftest: 已推 {n} 帧");
                    }
                }
            }),
        )?;
        std::thread::sleep(std::time::Duration::from_secs(dur));
        screen::stop();
        println!(
            "selftest: 共推 {} 帧",
            sent.load(std::sync::atomic::Ordering::Relaxed)
        );
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest-screen-vd") {
        // 诊断：推虚拟屏（静态画面），验证 idle-hold 长时间是否仍发帧
        let dur = args
            .iter()
            .position(|a| a == "--dur")
            .and_then(|i| args.get(i + 1))
            .and_then(|x| x.parse::<u64>().ok())
            .unwrap_or(20);
        vscreen::destroy();
        match vscreen::create_display() {
            Ok(id) => {
                println!("selftest-screen-vd: 虚拟屏 {id:#x} 推 {dur}s …");
                let sent: Arc<std::sync::atomic::AtomicU64> =
                    Arc::new(std::sync::atomic::AtomicU64::new(0));
                let sent_cb = sent.clone();
                screen::start(
                    id,
                    Box::new(move |buf, w, h, stride| {
                        let mut guard = VIDEO_CLIENT
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if send_or_reconnect(
                            &mut guard,
                            || frame::connect().ok(),
                            &buf,
                            w,
                            h,
                            stride,
                            video::host_time_ns(),
                        ) {
                            let n = sent_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            if n.is_multiple_of(60) {
                                println!("selftest-screen-vd: 已推 {n} 帧");
                            }
                        }
                    }),
                )?;
                std::thread::sleep(std::time::Duration::from_secs(dur));
                screen::stop();
                vscreen::destroy();
                println!(
                    "selftest-screen-vd: 共推 {} 帧",
                    sent.load(std::sync::atomic::Ordering::Relaxed)
                );
            }
            Err(e) => println!("selftest-screen-vd: 创建虚拟屏失败: {e}"),
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest-sysext") {
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_cb = events.clone();
        sysext::submit(
            BUNDLE_ID,
            true,
            Box::new(move |ev| {
                let s = match ev {
                    sysext::SysextEvent::NeedsApproval => "NeedsApproval".to_string(),
                    sysext::SysextEvent::Finished(n) => format!("Finished({n})"),
                    sysext::SysextEvent::Failed(e) => format!("Failed({e})"),
                };
                println!("selftest-sysext: {s}");
                ev_cb.lock().unwrap().push(s);
            }),
        )?;
        sysext::service_main_queue(10.0);
        println!("selftest-sysext: events={:?}", *events.lock().unwrap());
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest-video") {
        let path = args
            .iter()
            .position(|a| a == "--file")
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or_default();
        if path.is_empty() {
            println!("usage: vdev-app --selftest-video --file <mp4> [--dur N]");
            return Ok(());
        }
        let dur = args
            .iter()
            .position(|a| a == "--dur")
            .and_then(|i| args.get(i + 1))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(10);
        VIDEO_STOP.store(false, std::sync::atomic::Ordering::SeqCst);
        crate::audio::set_log_cb(None);
        // 隔离测试：只跑音频（正弦），不推视频，验证爆音是否来自 CPU 抢占
        if std::env::var("VDEV_TEST_SINE").is_ok() {
            println!("audio-only 测试（无视频推流）");
            crate::audio::start_audio_push(&path);
            std::thread::sleep(Duration::from_secs(dur));
            VIDEO_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(300));
            return Ok(());
        }
        let sent: Arc<std::sync::atomic::AtomicU64> =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sent_cb = sent.clone();
        // 诊断统计：帧间隔 + 发送耗时，暴露 >2s 停顿（会导致扩展回落彩条）
        let last_cb: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let max_gap: Arc<Mutex<u128>> = Arc::new(Mutex::new(0));
        let max_send: Arc<Mutex<u128>> = Arc::new(Mutex::new(0));
        let diag_last = last_cb.clone();
        let diag_gap = max_gap.clone();
        let diag_send = max_send.clone();
        println!("selftest-video: {path} 推流 {dur}s …");
        video::push_video(
            &path,
            video::FileAccess::none(),
            1920,
            1080,
            60,
            move |buf, w, h, stride| {
                let t0 = std::time::Instant::now();
                let mut lg = diag_last
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(prev) = *lg {
                    let gap = t0.duration_since(prev).as_millis();
                    let mut mg = diag_gap
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if gap > *mg {
                        *mg = gap;
                    }
                }
                *lg = Some(t0);
                drop(lg);
                let mut guard = VIDEO_CLIENT
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let s0 = std::time::Instant::now();
                // 失败即重置、下帧重连（与生产路径共用同一实现）
                let ok = send_or_reconnect(
                    &mut guard,
                    || frame::connect().ok(),
                    &buf,
                    w,
                    h,
                    stride,
                    video::host_time_ns(),
                );
                let ms = s0.elapsed().as_millis();
                {
                    let mut msx = diag_send
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if ms > *msx {
                        *msx = ms;
                    }
                }
                if !ok {
                    println!(
                        "selftest-video: send FAILED at #{}",
                        sent_cb.load(std::sync::atomic::Ordering::Relaxed)
                    );
                }
                let n = sent_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n.is_multiple_of(60) {
                    let g = *diag_gap
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let s = *diag_send
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    println!("selftest-video: 已推 {n} 帧 | 最大帧间隔 {g}ms | 最大发送耗时 {s}ms");
                    *diag_gap
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
                    *diag_send
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = 0;
                }
            },
            || {},
        )?;
        std::thread::sleep(std::time::Duration::from_secs(dur));
        VIDEO_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(500));
        println!(
            "selftest-video: 共推 {} 帧",
            sent.load(std::sync::atomic::Ordering::Relaxed)
        );
        return Ok(());
    }
    let ui = MainWindow::new()?;
    slint_pixel::install_title_bar_controls(&ui);
    let logs: Logs = Arc::new(Mutex::new(Vec::new()));
    append_log(&ui, &logs, "VDCamera 就绪（Rust + Slint + slint-pixel）");

    fn wire_ui(ui: &MainWindow, logs: &Logs) {
        let ui_weak = ui.as_weak();

        // 安装
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_install(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, format!("激活扩展: {BUNDLE_ID}"));
            set_status(&ui, "⏳", "正在安装…", "正在请求系统激活摄像头扩展。");
            set_enabled(&ui, false, false, false, false);

            let cb_logs = logs.clone();
            let wk_outer = weak.clone();
            let cb = move |ev: sysext::SysextEvent| {
                let logs = cb_logs.clone();
                let wk = wk_outer.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(ui) = wk.upgrade() else { return };
                    match ev {
                        sysext::SysextEvent::NeedsApproval => {
                            append_log(&ui, &logs, "需要用户在系统设置中批准，已打开系统设置");
                            set_status(&ui, "⚠️", "等待系统批准",
                                "请到 系统设置 → 通用 → 登录项与扩展 → 扩展 → 按类别 → 相机扩展，打开 vdev-camera。");
                            open_url(SETTINGS_URL);
                        }
                        sysext::SysextEvent::Finished(0) => {
                            append_log(&ui, &logs, "请求完成，正在确认摄像头…");
                            set_status(&ui, "⏳", "正在确认…", "最长约 15 秒。");
                            let ui2 = ui.as_weak();
                            let logs2 = logs.clone();
                            std::thread::spawn(move || {
                                let mut ok = false;
                                for _ in 0..30 {
                                    if camera::find_vdev() {
                                        ok = true;
                                        break;
                                    }
                                    std::thread::sleep(Duration::from_millis(500));
                                }
                                let _ = slint::invoke_from_event_loop(move || {
                                    let Some(ui) = ui2.upgrade() else { return };
                                    if ok {
                                        append_log(&ui, &logs2, "检测到 vdev-camera，安装成功");
                                        set_status(&ui, "✓", "已安装，摄像头可用",
                                            "打开 QuickTime → 新建影片录制 → 选择 vdev-camera。");
                                        set_enabled(&ui, false, true, true, vscreen::display_id().is_some());
                                    } else {
                                        append_log(&ui, &logs2, "15 秒内未检测到摄像头，触发自动修复");
                                        set_status(&ui, "⏳", "正在自动修复…",
                                            "停用→重新启用扩展，最长约 30 秒。");
                                        let log: Arc<dyn Fn(String) + Send + Sync> = {
                                            let wk = ui.as_weak();
                                            let lg = logs.clone();
                                            Arc::new(move |line: String| {
                                                if line.contains("批准") {
                                                    open_url(SETTINGS_URL);
                                                }
                                                let w = wk.clone();
                                                let l = lg.clone();
                                                let _ = slint::invoke_from_event_loop(move || {
                                                    if let Some(ui) = w.upgrade() {
                                                        append_log(&ui, &l, line);
                                                    }
                                                });
                                            })
                                        };
                                        recover_extension(&log);
                                    }
                                });
                            });
                        }
                        sysext::SysextEvent::Finished(n) => {
                            set_status(&ui, "✕", "操作失败", format!("系统返回未预期结果 {n}").as_str());
                        }
                        sysext::SysextEvent::Failed(msg) => {
                            set_status(&ui, "✕", "操作失败", msg.as_str());
                            set_enabled(&ui, true, false, false, false);
                        }
                    }
                });
            };
            if let Err(e) = sysext::submit(BUNDLE_ID, true, Box::new(cb)) {
                append_log(&ui, &logs, format!("提交失败: {e}"));
            }
        }));});
        }

        // 卸载
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_uninstall(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let Some(ui) = weak.upgrade() else { return };
                    append_log(&ui, &logs, format!("停用扩展: {BUNDLE_ID}"));
                    set_status(&ui, "⏳", "正在卸载…", "正在从系统移除摄像头扩展。");
                    set_enabled(&ui, false, false, false, false);
                    let cb_logs = logs.clone();
                    let wk_outer = weak.clone();
                    let cb = move |ev: sysext::SysextEvent| {
                        let logs = cb_logs.clone();
                        let wk = wk_outer.clone();
                        let _ = slint::invoke_from_event_loop(move || {
                            let Some(ui) = wk.upgrade() else { return };
                            match ev {
                                sysext::SysextEvent::Finished(_) => {
                                    append_log(&ui, &logs, "卸载完成");
                                    set_status(&ui, "○", "已卸载", "vdev-camera 已从系统移除。");
                                    set_enabled(&ui, true, false, false, false);
                                }
                                sysext::SysextEvent::Failed(msg) => {
                                    set_status(&ui, "✕", "操作失败", msg.as_str());
                                    set_enabled(&ui, true, false, false, false);
                                }
                                sysext::SysextEvent::NeedsApproval => {
                                    append_log(&ui, &logs, "卸载需要系统确认");
                                }
                            }
                        });
                    };
                    if let Err(e) = sysext::submit(BUNDLE_ID, false, Box::new(cb)) {
                        append_log(&ui, &logs, format!("提交失败: {e}"));
                    }
                }));
            });
        }

        // 自动修复（停用→重新启用，解决 launchd 竞态）
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_recover(move || {
                // 其余业务回调都有 catch_unwind，此处补齐防 panic 直接穿透 Slint 回调
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let Some(ui) = weak.upgrade() else { return };
                    append_log(&ui, &logs, "手动触发自动修复…");
                    set_status(
                        &ui,
                        "⏳",
                        "正在自动修复…",
                        "停用→重新启用扩展，最长约 30 秒。",
                    );
                    let log: Arc<dyn Fn(String) + Send + Sync> = {
                        let wk = ui.as_weak();
                        let lg = logs.clone();
                        Arc::new(move |line: String| {
                            if line.contains("批准") {
                                open_url(SETTINGS_URL);
                            }
                            let w = wk.clone();
                            let l = lg.clone();
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = w.upgrade() {
                                    append_log(&ui, &l, line);
                                }
                            });
                        })
                    };
                    recover_extension(&log);
                }));
            });
        }

        // 刷新状态
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_refresh(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let Some(ui) = weak.upgrade() else { return };
                    refresh_status(&ui, &logs);
                }));
            });
        }

        // 打开系统设置 / QuickTime
        {
            let weak = ui_weak.clone();
            ui.on_open_settings(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if weak.upgrade().is_some() {
                        open_url(SETTINGS_URL);
                    }
                }));
            });
            let weak = ui_weak.clone();
            ui.on_open_quicktime(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    if weak.upgrade().is_some() {
                        open_quicktime();
                    }
                }));
            });
        }

        // 创建/销毁虚拟屏幕
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_vd_create(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let Some(ui) = weak.upgrade() else { return };
                    if vscreen::display_id().is_some() {
                        vscreen::destroy();
                        append_log(&ui, &logs, "虚拟屏幕已销毁");
                        ui.global::<AppState>()
                            .set_vd_btn_text("创建虚拟屏幕".into());
                        set_enabled(&ui, false, true, true, false);
                    } else {
                        match vscreen::create_display() {
                            Ok(id) => {
                                append_log(&ui, &logs, format!("虚拟屏幕已创建 0x{id:x}"));
                                ui.global::<AppState>()
                                    .set_vd_btn_text("销毁虚拟屏幕".into());
                                set_enabled(&ui, false, true, true, true);
                            }
                            Err(e) => {
                                append_log(&ui, &logs, format!("创建虚拟屏幕失败: {e}"));
                                set_status(&ui, "✕", "操作失败", e.to_string().as_str());
                            }
                        }
                    }
                }));
            });
        }

        // 屏幕推流（主显示器）
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_screen_push(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let Some(ui) = weak.upgrade() else { return };
                    if *PUSH_MODE
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        == Some(PushMode::ScreenMain)
                    {
                        stop_current_push(&ui, &logs);
                    } else {
                        start_screen_push(
                            &ui,
                            &logs,
                            screen::main_display_id(),
                            PushMode::ScreenMain,
                        );
                    }
                }));
            });
        }
        // 视频推流
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_video_push(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let Some(ui) = weak.upgrade() else { return };
                    if *PUSH_MODE
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        == Some(PushMode::Video)
                    {
                        VIDEO_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
                        stop_current_push(&ui, &logs);
                        return;
                    }
                    // NSOpenPanel 必须在主线程：经 invoke_from_event_loop 派发到 UI 线程
                    append_log(&ui, &logs, "正在打开文件选择器…");
                    let ui2 = ui.as_weak();
                    let logs2 = logs.clone();
                    // 音频推流状态打进日志区
                    let audio_ui = ui.as_weak();
                    let audio_logs = logs.clone();
                    crate::audio::set_log_cb(Some(Arc::new(move |s: String| {
                        if let Some(ui) = audio_ui.upgrade() {
                            append_log(&ui, &audio_logs, s);
                        }
                    })));
                    let _ = slint::invoke_from_event_loop(move || {
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            let Some(ui) = ui2.upgrade() else { return };
                            let picked = match video::pick_video() {
                                Ok(Some(p)) => p,
                                Ok(None) => {
                                    append_log(&ui, &logs2, "未选择视频（已取消）");
                                    return;
                                }
                                Err(e) => {
                                    append_log(&ui, &logs2, format!("打开文件选择器失败: {e}"));
                                    return;
                                }
                            };
                            let path = picked.path;
                            append_log(&ui, &logs2, format!("已选择: {path}"));
                            if path.starts_with("/Volumes/") {
                                append_log(
                                    &ui,
                                    &logs2,
                                    "正在准备视频（外置/网络卷，首次会复制到本地缓存，请稍候）…",
                                );
                            }
                            stop_current_push(&ui, &logs2);
                            *PUSH_MODE
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(PushMode::Video);
                            VIDEO_STOP.store(false, std::sync::atomic::Ordering::SeqCst);
                            set_btn_texts(&ui);
                            append_log(&ui, &logs2, format!("视频推流开始: {path}"));
                            let done_ui = ui.as_weak();
                            let logs3 = logs2.clone();
                            if let Err(e) = video::push_video(
                                &path,
                                picked.access,
                                1920,
                                1080,
                                60,
                                move |buf, w, h, stride| {
                                    let mut guard = VIDEO_CLIENT
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    // 发送失败即重置，下一帧自动重连（与屏幕/自测路径共用）
                                    send_or_reconnect(
                                        &mut guard,
                                        || frame::connect().ok(),
                                        &buf,
                                        w,
                                        h,
                                        stride,
                                        video::host_time_ns(),
                                    );
                                },
                                move || {
                                    let _ = slint::invoke_from_event_loop(move || {
                                        if let Some(ui) = done_ui.upgrade() {
                                            if *PUSH_MODE
                                                .lock()
                                                .unwrap_or_else(std::sync::PoisonError::into_inner)
                                                == Some(PushMode::Video)
                                            {
                                                *PUSH_MODE.lock().unwrap_or_else(
                                                    std::sync::PoisonError::into_inner,
                                                ) = None;
                                                set_btn_texts(&ui);
                                            }
                                        }
                                    });
                                },
                            ) {
                                append_log(&ui, &logs3, format!("视频推流启动失败: {e}"));
                                *PUSH_MODE
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                                set_btn_texts(&ui);
                            }
                        }));
                    });
                }));
            });
        }
        // 推虚拟屏幕
        {
            let weak = ui_weak.clone();
            let logs = logs.clone();
            ui.on_vd_push(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let Some(ui) = weak.upgrade() else { return };
                    if *PUSH_MODE
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        == Some(PushMode::ScreenVd)
                    {
                        stop_current_push(&ui, &logs);
                        return;
                    }
                    let Some(id) = vscreen::display_id() else {
                        append_log(&ui, &logs, "请先创建虚拟屏幕");
                        return;
                    };
                    start_screen_push(&ui, &logs, id, PushMode::ScreenVd);
                }));
            });
        }
    }

    wire_ui(&ui, &logs);

    if args.iter().any(|a| a == "--selftest-pick") {
        println!("selftest-pick: 打开文件选择器…");
        match video::pick_video() {
            Ok(Some(p)) => println!("selftest-pick: result={:?}", p.path),
            Ok(None) => println!("selftest-pick: cancelled"),
            Err(e) => println!("selftest-pick: ERROR {e}"),
        }
        return Ok(());
    }
    if args.iter().any(|a| a == "--install-extension") {
        // 提交扩展激活请求并保持进程 120s（等价于点「安装虚拟摄像头」，供自动化/CLI 用）
        let events: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let ev_cb = events.clone();
        sysext::submit(
            BUNDLE_ID,
            true,
            Box::new(move |ev| {
                let s = match ev {
                    sysext::SysextEvent::NeedsApproval => "NeedsApproval".to_string(),
                    sysext::SysextEvent::Finished(n) => format!("Finished({n})"),
                    sysext::SysextEvent::Failed(e) => format!("Failed({e})"),
                };
                println!("install-extension: {s}");
                ev_cb.lock().unwrap().push(s);
            }),
        )?;
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.ExtensionsPreferences")
            .spawn();
        sysext::service_main_queue(120.0);
        println!("install-extension: events={:?}", *events.lock().unwrap());
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest-recover") {
        let log: Arc<dyn Fn(String) + Send + Sync> =
            Arc::new(|line: String| println!("recover: {line}"));
        recover_extension(&log);
        std::thread::sleep(Duration::from_secs(40));
        println!("recover: done, camera={}", camera::find_vdev());
        return Ok(());
    }
    if args.iter().any(|a| a == "--ui-selftest") {
        // 程序化触发按钮回调，验证 UI 接线（Slint invoke_* == 点击按钮）
        let ui2 = MainWindow::new()?;
        slint_pixel::install_title_bar_controls(&ui2);
        let logs2: Logs = Arc::new(Mutex::new(Vec::new()));
        wire_ui(&ui2, &logs2);

        ui2.invoke_vd_create();
        std::thread::sleep(Duration::from_secs(2));
        println!(
            "ui-selftest: vd_create -> display_id={}",
            vscreen::display_id().is_some()
        );

        ui2.invoke_vd_push();
        std::thread::sleep(Duration::from_secs(4));
        println!(
            "ui-selftest: vd_push -> mode={:?}",
            *PUSH_MODE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );

        ui2.invoke_vd_push();
        std::thread::sleep(Duration::from_secs(2));
        println!(
            "ui-selftest: vd_push stop -> mode={:?}",
            *PUSH_MODE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );

        ui2.invoke_screen_push();
        std::thread::sleep(Duration::from_secs(3));
        println!(
            "ui-selftest: screen_push -> mode={:?}",
            *PUSH_MODE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );

        ui2.invoke_screen_push();
        std::thread::sleep(Duration::from_secs(2));
        println!(
            "ui-selftest: screen_push stop -> mode={:?}",
            *PUSH_MODE
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        );

        // 剩余回调：refresh（摄像头检测，只读）安全执行；
        // install 腿会向真实系统提交扩展激活请求（摄像头 15s 不出现的机器上会真实
        // 触发停用→重启用），默认跳过；显式设 VDEV_SELFTEST_INSTALL=1 才执行。
        ui2.invoke_refresh();
        if matches!(std::env::var("VDEV_SELFTEST_INSTALL").as_deref(), Ok("1")) {
            ui2.invoke_install();
            std::thread::sleep(Duration::from_secs(3));
            println!("ui-selftest: refresh/install ok");
        } else {
            println!(
                "ui-selftest: refresh ok；install 腿跳过（会真实提交扩展激活，设 VDEV_SELFTEST_INSTALL=1 执行）"
            );
        }

        vscreen::destroy();
        return Ok(());
    }

    refresh_status(&ui, &logs);

    if args.iter().any(|a| a == "--auto-pick-test") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let log_path = format!("{home}/pick-gui.log");
        let _ = std::fs::write(&log_path, "scheduled\n");
        let _ = slint::invoke_from_event_loop(move || {
            let _ = std::fs::write(&log_path, "calling pick\n");
            let r = video::pick_video();
            let _ = std::fs::write(
                &log_path,
                format!("result={:?}\n", r.map(|o| o.map(|p| p.path))),
            );
        });
    }

    ui.run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 可注入的假客户端：前 `fail_left` 次发送失败，之后成功。
    /// 模拟 `FrameClient` 断连（对端扩展重启导致旧 TCP 写失败）而不需要真实网络。
    struct FakeClient {
        fail_left: std::sync::atomic::AtomicUsize,
        sends: std::sync::atomic::AtomicUsize,
    }

    impl FakeClient {
        fn new(fail_left: usize) -> Self {
            Self {
                fail_left: std::sync::atomic::AtomicUsize::new(fail_left),
                sends: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    impl frame::FrameSender for FakeClient {
        fn send_frame(
            &mut self,
            _data: &[u8],
            _w: u32,
            _h: u32,
            _stride: u32,
            _pts_ns: u64,
        ) -> anyhow::Result<()> {
            self.sends
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let f = self.fail_left.load(std::sync::atomic::Ordering::Relaxed);
            if f > 0 {
                self.fail_left
                    .store(f - 1, std::sync::atomic::Ordering::Relaxed);
                return Err(anyhow::anyhow!("fake: 连接已断"));
            }
            Ok(())
        }
    }

    /// 回归（M2）：发送失败必须重置客户端（置 None），下一帧重连出新客户端。
    /// 之前生产路径 `let _ = send_frame` 吞错不重置，旧连接永久失败画面冻结。
    #[test]
    fn send_failure_resets_client_and_next_frame_reconnects() {
        let connects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mk_connect = |c: Arc<std::sync::atomic::AtomicUsize>| {
            move || {
                c.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Some(FakeClient::new(0)) // 重连出的新客户端健康
            }
        };
        let mut client: Option<FakeClient> = Some(FakeClient::new(1)); // 首发必失败的旧客户端

        // 第 1 帧：已有客户端，失败 → 返回 false 且重置，不触发重连
        assert!(!send_or_reconnect(
            &mut client,
            mk_connect(connects.clone()),
            &[0u8; 16],
            4,
            4,
            16,
            0
        ));
        assert!(client.is_none(), "发送失败后客户端应被重置为 None");
        assert_eq!(
            connects.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "已有客户端时本帧不应触发重连"
        );

        // 第 2 帧：重连出新客户端并发送成功
        assert!(send_or_reconnect(
            &mut client,
            mk_connect(connects.clone()),
            &[0u8; 16],
            4,
            4,
            16,
            0
        ));
        assert_eq!(
            connects.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "失败后下一帧应恰好重连一次"
        );

        // 第 3 帧：复用同一客户端，不再重连
        assert!(send_or_reconnect(
            &mut client,
            mk_connect(connects.clone()),
            &[0u8; 16],
            4,
            4,
            16,
            0
        ));
        assert_eq!(
            connects.load(std::sync::atomic::Ordering::Relaxed),
            1,
            "连接健康时不应重复重连"
        );
    }

    /// 回归（M2 补充）：连接失败（扩展未运行）时保持 None 且每帧重试连接，
    /// 不 panic、不缓存失败状态。
    #[test]
    fn connect_failure_keeps_client_none_and_retries_each_frame() {
        let connects = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut client: Option<FakeClient> = None;
        for i in 0..3 {
            let c2 = connects.clone();
            let ok = send_or_reconnect(
                &mut client,
                move || {
                    c2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    None // 模拟扩展未运行，连接失败
                },
                &[0u8; 16],
                4,
                4,
                16,
                0,
            );
            assert!(!ok, "第 {i} 帧在连接失败时应返回 false");
            assert!(client.is_none(), "连接失败后客户端应保持 None");
        }
        assert_eq!(
            connects.load(std::sync::atomic::Ordering::Relaxed),
            3,
            "每帧都应重试连接"
        );
    }
}
