//! 无头 UI 快照：把**真实的** `ui/main.slint` 用 Slint 自带的软件渲染器渲染进内存
//! 位图，再写成 BMP。用于在无法交互截图（会话锁定 / 无显示器）时留下可复现的 UI 证据。
//!
//! 用法（在 `crates/vdev-app-win` 下）：
//!
//! ```text
//! cargo run --offline --example ui_snapshot -- <输出目录>
//! ```
//!
//! 会先跑两组真实离线渲染（A：EQ/响度/限幅全关；B：10 段增益 + 响度归一化 + 限幅），
//! 把 `RenderReport::summary()` 的**真实数字**灌进面板，然后各截一张「音频增强」页快照。
//!
//! 关键点：这里没有独立绘制任何控件——渲染的是与正式程序同一份 Slint 组件树，
//! 曲线路径同样来自 `dsp_ui::curve_path`（即 vdev-dsp 的真实 biquad 系数）。
//! 软件渲染器来自 `slint::platform::software_renderer`，Slint 自带，**不新增任何依赖**。

use std::rc::Rc;

use slint::platform::software_renderer::{
    MinimalSoftwareWindow, PremultipliedRgbaColor, RepaintBufferType, TargetPixel,
};
use slint::platform::{PlatformError, WindowAdapter};
use slint::Model;
use vdev_app_win::dsp_ui::{self, DspParams};

slint::include_modules!();

/// 快照尺寸（物理像素）。比正式窗口高，用于一屏放下整块「音频增强」面板
/// （面板本体在外层 `ScrollView` 里，正式窗口高度不足时需要滚动）。
const SNAP_W: u32 = 900;
const SNAP_H: u32 = 1060;

thread_local! {
    static WINDOW: Rc<MinimalSoftwareWindow> =
        MinimalSoftwareWindow::new(RepaintBufferType::ReusedBuffer);
}

struct SnapPlatform;

impl slint::platform::Platform for SnapPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        Ok(WINDOW.with(|w| w.clone()))
    }
}

/// 目标像素：普通 8 位 RGB，按 Slint 的预乘 alpha 规则混合。
#[derive(Clone, Copy, Default)]
struct Rgb8 {
    r: u8,
    g: u8,
    b: u8,
}

impl TargetPixel for Rgb8 {
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        let inv_alpha = 255u32 - u32::from(color.alpha);
        self.r = (u32::from(color.red) + u32::from(self.r) * inv_alpha / 255).min(255) as u8;
        self.g = (u32::from(color.green) + u32::from(self.g) * inv_alpha / 255).min(255) as u8;
        self.b = (u32::from(color.blue) + u32::from(self.b) * inv_alpha / 255).min(255) as u8;
    }

    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// 24 位未压缩 BMP（零依赖；行按 4 字节对齐）。
fn write_bmp(path: &str, w: u32, h: u32, px: &[Rgb8]) -> std::io::Result<()> {
    let row = (w * 3 + 3) & !3;
    let data_len = row * h;
    let mut out = Vec::with_capacity((54 + data_len) as usize);
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + data_len).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for y in (0..h).rev() {
        let mut written = 0u32;
        for x in 0..w {
            let p = px[(y * w + x) as usize];
            out.push(p.b);
            out.push(p.g);
            out.push(p.r);
            written += 3;
        }
        while written < row {
            out.push(0);
            written += 1;
        }
    }
    std::fs::write(path, out)
}

/// 频点标签（与 formal UI 一致：31 / 62 / 125 / 1k / 2k …）。
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

/// 把一组参数 + 它的真实渲染结果灌进面板（与 main.rs 的初始化/回调走同一套属性）。
fn fill_panel(g: &AppState, p: &DspParams, report: &dsp_ui::RenderReport, input: &str) {
    g.set_current_tab(4);
    g.set_eq_bypass(p.eq_bypass);
    g.set_leveling_on(p.leveling_enabled);
    g.set_limiter_on(p.limiter_enabled);
    g.set_target_lufs(format!("{:.0}", p.target_lufs).into());
    g.set_ceiling_db(format!("{:.0}", p.ceiling_db).into());
    g.set_render_input(input.into());
    g.set_render_output(report.output_path.clone().into());
    g.set_render_result(report.summary().into());

    let gains = g.get_eq_gains();
    for i in 0..dsp_ui::EQ_BANDS {
        gains.set_row_data(i, p.gains_db[i] as f32);
    }

    // 曲线：与 main.rs::refresh_dsp_curve 相同的调用（真实 vdev-dsp 系数逐点求值）。
    let path = dsp_ui::curve_path(p, 556.0, 136.0, -12.0, 12.0);
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
    g.set_dsp_curve_path(path.into());
    g.set_dsp_curve_note(note.into());
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let src = format!("{dir}/vdev-ui-snapshot-src.wav");
    dsp_ui::synth_demo_wav(&src).expect("生成输入素材失败");

    // A：整链恒等（透明）。B：10 段增益 + 响度归一化 + 限幅。
    let a = DspParams {
        leveling_enabled: false,
        limiter_enabled: false,
        ..DspParams::default()
    };
    let b = DspParams {
        gains_db: [-6.0, -4.0, -2.0, 0.0, 2.0, 4.0, 5.0, 6.0, 6.0, 4.0],
        leveling_enabled: true,
        target_lufs: -18.0,
        limiter_enabled: true,
        ceiling_db: -3.0,
        ..DspParams::default()
    };

    let ra = dsp_ui::render_file(&src, &format!("{dir}/vdev-ui-snapshot-A.wav"), &a)
        .expect("A 组离线渲染失败");
    let rb = dsp_ui::render_file(&src, &format!("{dir}/vdev-ui-snapshot-B.wav"), &b)
        .expect("B 组离线渲染失败");

    slint::platform::set_platform(Box::new(SnapPlatform)).expect("安装软件渲染平台失败");
    let window = WINDOW.with(|w| w.clone());
    window.set_size(slint::PhysicalSize::new(SNAP_W, SNAP_H));

    let ui = MainWindow::new().expect("创建 MainWindow 失败");
    ui.show().expect("show 失败");

    {
        let g = ui.global::<AppState>();
        let labels: Vec<slint::SharedString> =
            dsp_ui::eq_band_frequencies_hz(dsp_ui::DspParams::default().sample_rate)
                .iter()
                .map(|f| band_label(*f).into())
                .collect();
        g.set_eq_labels(slint::ModelRc::new(slint::VecModel::from(labels)));
        g.set_dsp_curve_note("".into());
    }

    for (tag, p, r) in [("A", &a, &ra), ("B", &b, &rb)] {
        {
            let g = ui.global::<AppState>();
            fill_panel(&g, p, r, &src);
        }
        let mut buf = vec![Rgb8::default(); (SNAP_W * SNAP_H) as usize];
        window.request_redraw();
        window.draw_if_needed(|renderer| {
            renderer.render(buf.as_mut_slice(), SNAP_W as usize);
        });
        let out = format!("{dir}/vdev-ui-snapshot-{tag}.bmp");
        write_bmp(&out, SNAP_W, SNAP_H, &buf).expect("写 BMP 失败");
        println!(
            "[{tag}] 快照 {out} | 输出 RMS {:.3} dBFS | 真峰值 {:.3} dBTP (ceiling {:.2}) | 伺服增益 {:+.2} dB",
            r.out_rms_dbfs, r.out_true_peak_dbtp, r.ceiling_db, r.final_servo_gain_db
        );
    }
}
