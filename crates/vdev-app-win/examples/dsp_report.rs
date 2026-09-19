//! 离线验收证据：**同一 wav、两组不同参数**，过整链渲染后逐项对比数字。
//!
//! 用法（在 `crates/vdev-app-win` 下）：
//!
//! ```text
//! cargo run --offline --example dsp_report -- <输出目录>
//! ```
//!
//! 不给目录时用系统临时目录。会生成输入素材 `vdev-dsp-acceptance-src.wav`
//! 与两组输出 `vdev-dsp-acceptance-A.wav` / `-B.wav`，并把测量结果打到 stdout。
//!
//! 参数与 GUI 面板一一对应：10 段图示 EQ（-12..+12 dB）→ 响度归一化（目标 LUFS）
//! → 前瞻真峰值限幅（ceiling dBFS）。所有 DSP 运算都来自 `vdev_dsp`，本 example
//! 只做参数编排与读数。

use vdev_app_win::dsp_ui::{self, DspParams};

/// 打印一组参数与它的测量结果。
fn report(name: &str, params: &DspParams, report: &dsp_ui::RenderReport) {
    println!("[{name}] 参数");
    println!(
        "  EQ 10 段增益 (dB) = {:?}",
        params
            .gains_db
            .iter()
            .map(|g| (g * 10.0).round() / 10.0)
            .collect::<Vec<_>>()
    );
    println!(
        "  响度归一化 = {}（目标 {} LUFS）",
        if params.leveling_enabled {
            "开"
        } else {
            "关"
        },
        params.target_lufs
    );
    println!(
        "  真峰值限幅 = {}（ceiling {} dBFS）",
        if params.limiter_enabled { "开" } else { "关" },
        params.ceiling_db
    );
    println!("  输出 wav    = {}", report.output_path);
    println!(
        "  输出 RMS    = {:.3} dBFS（输入 {:.3} dBFS）",
        report.out_rms_dbfs, report.in_rms_dbfs
    );
    println!(
        "  真峰值      = {:.3} dBTP vs ceiling {:.2} dBFS（余量 {:+.3} dB）",
        report.out_true_peak_dbtp, report.ceiling_db, report.true_peak_over_ceiling_db
    );
    println!("  伺服增益    = {:+.2} dB", report.final_servo_gain_db);
    println!(
        "  耗时        = {:.3} s / {:.2} s 音频 = {:.1}x realtime",
        report.elapsed_secs, report.audio_secs, report.x_realtime
    );
    println!();
}

fn main() {
    let dir = std::env::args()
        .nth(1)
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let src = format!("{dir}/vdev-dsp-acceptance-src.wav");
    let data = dsp_ui::synth_demo_wav(&src).expect("生成输入素材失败");

    println!("输入素材 = {src}");
    println!(
        "  {} Hz / {} 声道 / {} 帧 / {:.2} s",
        data.sample_rate,
        data.channels,
        data.frames(),
        data.frames() as f64 / f64::from(data.sample_rate)
    );
    println!(
        "  输入 RMS {:.3} dBFS / 真峰值 {:.3} dBTP",
        dsp_ui::rms_dbfs(&data.samples),
        dsp_ui::true_peak_dbtp(&data.samples)
    );
    print!("  10 段中心频率 (Hz) =");
    for f in dsp_ui::eq_band_frequencies_hz(f64::from(data.sample_rate)) {
        print!(" {f:.1}");
    }
    println!("\n");

    // 组 A：全 0 增益 + 响度/限幅全关 —— 整链应当恒等（透明）。
    let a = DspParams {
        leveling_enabled: false,
        limiter_enabled: false,
        ..DspParams::default()
    };
    // 组 B：低切 + 中高频提升（对齐 vdev-dsp 默认 10 段网格）+ 响度归一化 + 限幅。
    let b = DspParams {
        gains_db: [-6.0, -4.0, -2.0, 0.0, 2.0, 4.0, 5.0, 6.0, 6.0, 4.0],
        leveling_enabled: true,
        target_lufs: -18.0,
        limiter_enabled: true,
        ceiling_db: -3.0,
        ..DspParams::default()
    };

    let mut reports = Vec::new();
    for (name, params) in [("A", &a), ("B", &b)] {
        let out = format!("{dir}/vdev-dsp-acceptance-{name}.wav");
        let r = dsp_ui::render_file(&src, &out, params).expect("离线渲染失败");
        report(name, params, &r);
        reports.push((name, r));
    }

    println!("| 参数组 | 输出 RMS dBFS | 真峰值 dBTP | ceiling dBFS | 伺服增益 dB | 耗时 s | x realtime |");
    println!("| --- | --- | --- | --- | --- | --- | --- |");
    for (name, r) in &reports {
        println!(
            "| {name} | {:.3} | {:.3} | {:.2} | {:+.2} | {:.3} | {:.1} |",
            r.out_rms_dbfs,
            r.out_true_peak_dbtp,
            r.ceiling_db,
            r.final_servo_gain_db,
            r.elapsed_secs,
            r.x_realtime
        );
    }
}
