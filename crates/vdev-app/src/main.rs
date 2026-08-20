//! VDCamera — vdev 摄像头宿主（Rust + Slint）
mod camera;
mod frame;
mod sysext;
mod vscreen;

use slint::Weak;
use std::sync::{Arc, Mutex};
use std::time::Duration;

slint::include_modules!();
slint_pixel::impl_title_bar_ui!(MainWindow);

const BUNDLE_ID: &str = "com.vdev.camera.host.extension";
const SETTINGS_URL: &str = "x-apple.systempreferences:com.apple.ExtensionsPreferences";
const QUICKTIME_PATH: &str = "/System/Applications/Quick Time Player.app";

type Logs = Arc<Mutex<Vec<String>>>;

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
        let cls = objc2::runtime::AnyClass::get(c"NSWorkspace").unwrap();
        let ws: *mut objc2::runtime::AnyObject = objc2::msg_send![cls, sharedWorkspace];
        let ns_url: *mut objc2::runtime::AnyObject =
            objc2::msg_send![objc2::runtime::AnyClass::get(c"NSURL").unwrap(), URLWithString: &*objc2_foundation::NSString::from_str(url)];
        if !ns_url.is_null() {
            let _: bool = objc2::msg_send![&*ws, openURL: &*ns_url];
        }
    }
}

fn open_quicktime() {
    let url = format!("file://{}", QUICKTIME_PATH);
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
    let ui = MainWindow::new()?;
    slint_pixel::install_title_bar_controls(&ui);
    let logs: Logs = Arc::new(Mutex::new(Vec::new()));
    append_log(&ui, &logs, "VDCamera 就绪（Rust + Slint + slint-pixel）");

    let ui_weak = ui.as_weak();

    // 安装
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_install(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, format!("激活扩展: {}", BUNDLE_ID));
            set_status(&ui, "⏳", "正在安装…", "正在请求系统激活摄像头扩展。");
            set_enabled(&ui, false, false, false, false);

            let cb_ui = ui.as_weak();
            let cb_logs = logs.clone();
            let wk_outer = weak.clone();
            let cb = move |ev: sysext::SysextEvent| {
                let ui = cb_ui.clone();
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
                                        append_log(&ui, &logs2, "15 秒内未检测到摄像头");
                                        set_status(&ui, "✕", "操作失败",
                                            "安装请求已完成，但暂未检测到摄像头。\n请点「刷新状态」重试；或重启电脑后自动恢复。");
                                        set_enabled(&ui, true, false, false, false);
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
        });
    }

    // 卸载
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_uninstall(move || {
            let Some(ui) = weak.upgrade() else { return };
            append_log(&ui, &logs, format!("停用扩展: {}", BUNDLE_ID));
            set_status(&ui, "⏳", "正在卸载…", "正在从系统移除摄像头扩展。");
            set_enabled(&ui, false, false, false, false);
            let cb_ui = ui.as_weak();
            let cb_logs = logs.clone();
            let wk_outer = weak.clone();
            let cb = move |ev: sysext::SysextEvent| {
                let ui = cb_ui.clone();
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
        });
    }

    // 刷新状态
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_refresh(move || {
            let Some(ui) = weak.upgrade() else { return };
            refresh_status(&ui, &logs);
        });
    }

    // 打开系统设置 / QuickTime
    {
        let weak = ui_weak.clone();
        ui.on_open_settings(move || {
            if let Some(ui) = weak.upgrade() {
                open_url(SETTINGS_URL);
            }
        });
        let weak = ui_weak.clone();
        ui.on_open_quicktime(move || {
            if let Some(ui) = weak.upgrade() {
                open_quicktime();
            }
        });
    }

    // 创建/销毁虚拟屏幕
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_vd_create(move || {
            let Some(ui) = weak.upgrade() else { return };
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
        });
    }

    // 屏幕推流 / 视频推流 / 推虚拟屏幕：本版先提示，推流引擎下一版接入
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_screen_push(move || {
            if let Some(ui) = weak.upgrade() {
                append_log(&ui, &logs, "屏幕推流：引擎迁移中（下一版接入）");
            }
        });
    }
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_video_push(move || {
            if let Some(ui) = weak.upgrade() {
                append_log(&ui, &logs, "视频推流：引擎迁移中（下一版接入）");
            }
        });
    }
    {
        let weak = ui_weak.clone();
        let logs = logs.clone();
        ui.on_vd_push(move || {
            if let Some(ui) = weak.upgrade() {
                append_log(&ui, &logs, "推虚拟屏幕：引擎迁移中（下一版接入）");
            }
        });
    }

    refresh_status(&ui, &logs);
    ui.run()?;
    Ok(())
}
