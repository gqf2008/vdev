//! VDCamera — vdev 摄像头宿主（Rust + Slint）
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

fn set_btn_texts(ui: &MainWindow) {
    let g = ui.global::<AppState>();
    let mode = *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner());
    g.set_screen_btn_text(if mode == Some(PushMode::ScreenMain) { "停止屏幕推流".into() } else { "屏幕推流".into() });
    g.set_video_btn_text(if mode == Some(PushMode::Video) { "停止视频推流".into() } else { "视频推流".into() });
    g.set_vd_push_btn_text(if mode == Some(PushMode::ScreenVd) { "停止推虚拟屏".into() } else { "推虚拟屏幕".into() });
}

fn stop_current_push(ui: &MainWindow, logs: &Logs) {
    let mode = *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner());
    match mode {
        Some(PushMode::ScreenMain) | Some(PushMode::ScreenVd) => {
            screen::stop();
            append_log(ui, logs, "屏幕推流已停止");
        }
        Some(PushMode::Video) => {
            // 视频线程通过 Arc<AtomicBool> 停止，简化：直接断开（线程内循环会因发送失败退出）
            append_log(ui, logs, "视频推流已停止");
        }
        None => {}
    }
    *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) = None;
    set_btn_texts(ui);
}

fn start_screen_push(ui: &MainWindow, logs: &Logs, display_id: u32, mode: PushMode) {
    stop_current_push(ui, logs);
    append_log(ui, logs, format!("屏幕推流开始（显示器 {:#x}）", display_id));
    *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) = Some(mode);
    set_btn_texts(ui);
    let client: Arc<Mutex<Option<frame::FrameClient>>> = Arc::new(Mutex::new(None));
    let client_cb = client.clone();
    if let Err(e) = screen::start(display_id, Box::new(move |buf, w, h, stride| {
        let mut guard = client_cb.lock().unwrap();
        if guard.is_none() {
            *guard = frame::connect().ok();
        }
        if let Some(c) = guard.as_mut() {
            let _ = c.send_frame(&buf, w, h, stride, video::host_time_ns());
        }
    })) {
        append_log(ui, logs, format!("屏幕推流失败: {}", e));
        *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) = None;
        set_btn_texts(ui);
    }
}


/// 自动修复 launchd 竞态：停用 → 等待清理 → 重新启用 → 确认摄像头。
/// log 在后台线程回调。
fn recover_extension(log: Arc<dyn Fn(String) + Send + Sync>) {
    log("自动修复：先停用再重新启用扩展…".to_string());
    let l2 = log.clone();
    let cb = move |ev: sysext::SysextEvent| {
        let lg = l2.clone();
        match ev {
            sysext::SysextEvent::Finished(_) => {
                lg("已停用，等待 6s 后重新启用…".to_string());
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
                                lg4("重新启用需要批准：系统设置 → … → 相机扩展".to_string())
                            }
                            sysext::SysextEvent::Failed(e) => {
                                lg4(format!("重新启用失败: {}", e))
                            }
                            _ => {}
                        }
                    };
                    let _ = sysext::submit(BUNDLE_ID, true, Box::new(cb3));
                });
            }
            sysext::SysextEvent::NeedsApproval => lg("停用需要系统确认".to_string()),
            sysext::SysextEvent::Failed(e) => lg(format!("停用失败: {}", e)),
            _ => {}
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

fn set_enabled(ui: &MainWindow, install: bool, uninstall: bool, push: bool, vd_push: bool) {
    let g = ui.global::<AppState>();
    g.set_can_install(install);
    g.set_can_uninstall(uninstall);
    g.set_can_push(push);
    g.set_can_vd_push(vd_push);
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

fn open_url(url: &str) {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSWorkspace") else { return };
        let ws: *mut objc2::runtime::AnyObject = objc2::msg_send![cls, sharedWorkspace];
        let Some(url_cls) = objc2::runtime::AnyClass::get(c"NSURL") else { return };
        let ns_url: *mut objc2::runtime::AnyObject =
            objc2::msg_send![url_cls, URLWithString: &*objc2_foundation::NSString::from_str(url)];
        if !ns_url.is_null() {
            let _: bool = objc2::msg_send![&*ws, openURL: &*ns_url];
        }
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
        set_status(ui, "✓", "已安装，摄像头可用",
            "系统已能看到 vdev-camera。打开 QuickTime → 新建影片录制 → 选择 vdev-camera。");
        set_enabled(ui, false, true, true, vscreen::display_id().is_some());
    } else {
        set_status(ui, "○", "虚拟摄像头未安装",
            "点击「安装虚拟摄像头」，然后在系统提示中允许扩展。");
        set_enabled(ui, true, false, false, false);
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // panic 落盘：GUI 无 stderr，崩溃后可读 /tmp/vdev-panic.log 定位
    std::panic::set_hook(Box::new(|info| {
        let msg = format!("[unix={}] {}\n", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), info);
        // 沙盒 App 写不了 /tmp，优先写 $HOME（容器目录）
        let path = std::env::var("HOME")
            .map(|h| format!("{}/vdev-panic.log", h))
            .unwrap_or_else(|_| "/tmp/vdev-panic.log".to_string());
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
        eprintln!("{}", info);
    }));
    video::ensure_nsapp();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--selftest-openpanel") {
        // 只验证 NSOpenPanel 可创建（不弹窗），沙盒权限自测用
        match video::openpanel_selftest() {
            Ok(()) => println!("selftest-openpanel: OK"),
            Err(e) => println!("selftest-openpanel: FAIL {}", e),
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
        let sent: Arc<std::sync::atomic::AtomicU64> = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let client_cb = client.clone();
        let sent_cb = sent.clone();
        println!("selftest: 屏幕推流 {dur}s …");
        screen::start(
            screen::main_display_id(),
            Box::new(move |buf, w, h, stride| {
                let mut guard = client_cb.lock().unwrap();
                if guard.is_none() {
                    *guard = frame::connect().ok();
                }
                if let Some(c) = guard.as_mut() {
                    if c.send_frame(&buf, w, h, stride, video::host_time_ns()).is_ok() {
                        let n = sent_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                        if n % 60 == 0 {
                            println!("selftest: 已推 {n} 帧");
                        }
                    } else {
                        *guard = None; // 连接断了，下一帧自动重连
                    }
                }
            }),
        )?;
        std::thread::sleep(std::time::Duration::from_secs(dur));
        screen::stop();
        println!("selftest: 共推 {} 帧", sent.load(std::sync::atomic::Ordering::Relaxed));
        return Ok(());
    }
    if args.iter().any(|a| a == "--selftest-screen-vd") {
        // 诊断：推虚拟屏（静态画面），验证 idle-hold 长时间是否仍发帧
        let dur = args.iter().position(|a| a == "--dur")
            .and_then(|i| args.get(i + 1)).and_then(|x| x.parse::<u64>().ok()).unwrap_or(20);
        vscreen::destroy();
        match vscreen::create_display() {
            Ok(id) => {
                println!("selftest-screen-vd: 虚拟屏 {:#x} 推 {dur}s …", id);
                let sent: Arc<std::sync::atomic::AtomicU64> = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let sent_cb = sent.clone();
                screen::start(id, Box::new(move |buf, w, h, stride| {
                    let mut guard = VIDEO_CLIENT.lock().unwrap_or_else(|e| e.into_inner());
                    if guard.is_none() { *guard = frame::connect().ok(); }
                    if let Some(c) = guard.as_mut() {
                        if c.send_frame(&buf, w, h, stride, video::host_time_ns()).is_ok() {
                            let n = sent_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                            if n % 60 == 0 { println!("selftest-screen-vd: 已推 {n} 帧"); }
                        } else {
                            *guard = None; // 连接断了，下一帧自动重连
                        }
                    }
                }))?;
                std::thread::sleep(std::time::Duration::from_secs(dur));
                screen::stop();
                vscreen::destroy();
                println!("selftest-screen-vd: 共推 {} 帧", sent.load(std::sync::atomic::Ordering::Relaxed));
            }
            Err(e) => println!("selftest-screen-vd: 创建虚拟屏失败: {}", e),
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
                    sysext::SysextEvent::Finished(n) => format!("Finished({})", n),
                    sysext::SysextEvent::Failed(e) => format!("Failed({})", e),
                };
                println!("selftest-sysext: {}", s);
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
        let sent: Arc<std::sync::atomic::AtomicU64> = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let sent_cb = sent.clone();
        // 诊断统计：帧间隔 + 发送耗时，暴露 >2s 停顿（会导致扩展回落彩条）
        let last_cb: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let max_gap: Arc<Mutex<u128>> = Arc::new(Mutex::new(0));
        let max_send: Arc<Mutex<u128>> = Arc::new(Mutex::new(0));
        let diag_last = last_cb.clone();
        let diag_gap = max_gap.clone();
        let diag_send = max_send.clone();
        println!("selftest-video: {} 推流 {dur}s …", path);
        video::push_video(
            &path,
            video::FileAccess::none(),
            1920,
            1080,
            60,
            move |buf, w, h, stride| {
                let t0 = std::time::Instant::now();
                let mut lg = diag_last.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(prev) = *lg {
                    let gap = t0.duration_since(prev).as_millis();
                    let mut mg = diag_gap.lock().unwrap_or_else(|e| e.into_inner());
                    if gap > *mg { *mg = gap; }
                }
                *lg = Some(t0);
                drop(lg);
                let mut guard = VIDEO_CLIENT.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_none() {
                    *guard = frame::connect().ok();
                }
                let _send_ms = if let Some(c) = guard.as_mut() {
                    let s0 = std::time::Instant::now();
                    let ok = c.send_frame(&buf, w, h, stride, video::host_time_ns()).is_ok();
                    let ms = s0.elapsed().as_millis();
                    let mut msx = diag_send.lock().unwrap_or_else(|e| e.into_inner());
                    if ms > *msx { *msx = ms; }
                    if !ok { println!("selftest-video: send FAILED at #{}", sent_cb.load(std::sync::atomic::Ordering::Relaxed)); }
                    ms
                } else { 0 };
                let n = sent_cb.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n % 60 == 0 {
                    let g = *diag_gap.lock().unwrap_or_else(|e| e.into_inner());
                    let s = *diag_send.lock().unwrap_or_else(|e| e.into_inner());
                    println!("selftest-video: 已推 {n} 帧 | 最大帧间隔 {g}ms | 最大发送耗时 {s}ms");
                    *diag_gap.lock().unwrap_or_else(|e| e.into_inner()) = 0;
                    *diag_send.lock().unwrap_or_else(|e| e.into_inner()) = 0;
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
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            append_log(&ui, &logs, format!("激活扩展: {}", BUNDLE_ID));
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
                                        let log = {
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
                                        recover_extension(log);
                                    }
                                });
                            });
                        }
                        sysext::SysextEvent::Finished(n) => {
                            set_status(&ui, "✕", "操作失败", format!("系统返回未预期结果 {}", n).as_str());
                        }
                        sysext::SysextEvent::Failed(msg) => {
                            set_status(&ui, "✕", "操作失败", msg.as_str());
                            set_enabled(&ui, true, false, false, false);
                        }
                    }
                });
            };
            if let Err(e) = sysext::submit(BUNDLE_ID, true, Box::new(cb)) {
                append_log(&ui, &logs, format!("提交失败: {}", e));
            }
        }));});
    }

    // 卸载
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_uninstall(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            append_log(&ui, &logs, format!("停用扩展: {}", BUNDLE_ID));
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
                append_log(&ui, &logs, format!("提交失败: {}", e));
            }
        }));});
    }

    // 自动修复（停用→重新启用，解决 launchd 竞态）
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_recover(move || {
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            append_log(&ui, &logs, "手动触发自动修复…");
            set_status(&ui, "⏳", "正在自动修复…", "停用→重新启用扩展，最长约 30 秒。");
            let log = {
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
            recover_extension(log);
        });
    }

    // 刷新状态
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_refresh(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            refresh_status(&ui, &logs);
        }));});
    }

    // 打开系统设置 / QuickTime
    {
        let weak = ui_weak.clone();
        ui.on_open_settings(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if weak.upgrade().is_some() {
                open_url(SETTINGS_URL);
            }
        }));});
        let weak = ui_weak.clone();
        ui.on_open_quicktime(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if weak.upgrade().is_some() {
                open_quicktime();
            }
        }));});
    }

    // 创建/销毁虚拟屏幕
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_vd_create(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            if vscreen::display_id().is_some() {
                vscreen::destroy();
                append_log(&ui, &logs, "虚拟屏幕已销毁");
                ui.global::<AppState>().set_vd_btn_text("创建虚拟屏幕".into());
                set_enabled(&ui, false, true, true, false);
            } else {
                match vscreen::create_display() {
                    Ok(id) => {
                        append_log(&ui, &logs, format!("虚拟屏幕已创建 0x{:x}", id));
                        ui.global::<AppState>().set_vd_btn_text("销毁虚拟屏幕".into());
                        set_enabled(&ui, false, true, true, true);
                    }
                    Err(e) => {
                        append_log(&ui, &logs, format!("创建虚拟屏幕失败: {}", e));
                        set_status(&ui, "✕", "操作失败", e.to_string().as_str());
                    }
                }
            }
        }));});
    }

    // 屏幕推流（主显示器）
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_screen_push(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            if *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) == Some(PushMode::ScreenMain) {
                stop_current_push(&ui, &logs);
            } else {
                start_screen_push(&ui, &logs, screen::main_display_id(), PushMode::ScreenMain);
            }
        }));});
    }
    // 视频推流
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_video_push(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            if *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) == Some(PushMode::Video) {
                VIDEO_STOP.store(true, std::sync::atomic::Ordering::SeqCst);
                stop_current_push(&ui, &logs);
                return;
            }
            // NSOpenPanel 必须在主线程：经 invoke_from_event_loop 派发到 UI 线程
            append_log(&ui, &logs, "正在打开文件选择器…");
            let ui2 = ui.as_weak();
            let logs2 = logs.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let ui = match ui2.upgrade() { Some(ui) => ui, None => return };
                    let picked = match video::pick_video() {
                        Ok(Some(p)) => p,
                        Ok(None) => {
                            append_log(&ui, &logs2, "未选择视频（已取消）");
                            return;
                        }
                        Err(e) => {
                            append_log(&ui, &logs2, format!("打开文件选择器失败: {}", e));
                            return;
                        }
                    };
                    let path = picked.path;
                    append_log(&ui, &logs2, format!("已选择: {}", path));
                    if path.starts_with("/Volumes/") {
                        append_log(&ui, &logs2, "正在准备视频（外置/网络卷，首次会复制到本地缓存，请稍候）…");
                    }
                    stop_current_push(&ui, &logs2);
                    *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) = Some(PushMode::Video);
                    VIDEO_STOP.store(false, std::sync::atomic::Ordering::SeqCst);
                    set_btn_texts(&ui);
                    append_log(&ui, &logs2, format!("视频推流开始: {}", path));
                    let done_ui = ui.as_weak();
                    let logs3 = logs2.clone();
                    if let Err(e) = video::push_video(
                        &path,
                        picked.access,
                        1920,
                        1080,
                        60,
                        move |buf, w, h, stride| {
                            let mut guard = VIDEO_CLIENT.lock().unwrap_or_else(|e| e.into_inner());
                            if guard.is_none() {
                                *guard = frame::connect().ok();
                            }
                            if let Some(c) = guard.as_mut() {
                                let _ = c.send_frame(&buf, w, h, stride, video::host_time_ns());
                            }
                        },
                        move || {
                            let _ = slint::invoke_from_event_loop(move || {
                                if let Some(ui) = done_ui.upgrade() {
                                    if *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) == Some(PushMode::Video) {
                                        *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) = None;
                                        set_btn_texts(&ui);
                                    }
                                }
                            });
                        },
                    ) {
                        append_log(&ui, &logs3, format!("视频推流启动失败: {}", e));
                        *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
            let ui = match weak.upgrade() { Some(ui) => ui, None => return };
            if *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()) == Some(PushMode::ScreenVd) {
                stop_current_push(&ui, &logs);
                return;
            }
            let Some(id) = vscreen::display_id() else {
                append_log(&ui, &logs, "请先创建虚拟屏幕");
                return;
            };
            start_screen_push(&ui, &logs, id, PushMode::ScreenVd);
        }));});
    }

    }

    wire_ui(&ui, &logs);

    if args.iter().any(|a| a == "--selftest-pick") {
        println!("selftest-pick: 打开文件选择器…");
        match video::pick_video() {
            Ok(Some(p)) => println!("selftest-pick: result={:?}", p.path),
            Ok(None) => println!("selftest-pick: cancelled"),
            Err(e) => println!("selftest-pick: ERROR {}", e),
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
                    sysext::SysextEvent::Finished(n) => format!("Finished({})", n),
                    sysext::SysextEvent::Failed(e) => format!("Failed({})", e),
                };
                println!("install-extension: {}", s);
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
        let log = Arc::new(|line: String| println!("recover: {}", line));
        recover_extension(log);
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
        println!("ui-selftest: vd_create -> display_id={}", vscreen::display_id().is_some());

        ui2.invoke_vd_push();
        std::thread::sleep(Duration::from_secs(4));
        println!("ui-selftest: vd_push -> mode={:?}", *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()));

        ui2.invoke_vd_push();
        std::thread::sleep(Duration::from_secs(2));
        println!("ui-selftest: vd_push stop -> mode={:?}", *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()));

        ui2.invoke_screen_push();
        std::thread::sleep(Duration::from_secs(3));
        println!("ui-selftest: screen_push -> mode={:?}", *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()));

        ui2.invoke_screen_push();
        std::thread::sleep(Duration::from_secs(2));
        println!("ui-selftest: screen_push stop -> mode={:?}", *PUSH_MODE.lock().unwrap_or_else(|e| e.into_inner()));

        // 剩余安全回调：refresh（摄像头检测）+ install（同版本 no-op）
        ui2.invoke_refresh();
        ui2.invoke_install();
        std::thread::sleep(Duration::from_secs(3));
        println!("ui-selftest: refresh/install ok");

        vscreen::destroy();
        return Ok(());
    }

    refresh_status(&ui, &logs);

    if args.iter().any(|a| a == "--auto-pick-test") {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let log_path = format!("{}/pick-gui.log", home);
        let _ = std::fs::write(&log_path, "scheduled\n");
        let _ = slint::invoke_from_event_loop(move || {
            let _ = std::fs::write(&log_path, "calling pick\n");
            let r = video::pick_video();
            let _ = std::fs::write(&log_path, format!("result={:?}\n", r.map(|o| o.map(|p| p.path))));
        });
    }

    ui.run()?;
    Ok(())
}
