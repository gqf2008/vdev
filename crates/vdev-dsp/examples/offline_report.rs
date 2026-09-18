// 示例代码里的数值转换（`i as f64`、`f64 as f32`）与精确浮点比较见 src/lib.rs 顶部的 lint 说明；
// example 是独立 crate，需要在这里再声明一次。
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::float_cmp)]

//! 离线对照工具：对同一段合成素材，打印**处理前后**的近似响度与真峰值。
//!
//! 它是「响度一致性」的第三方证据：响度与真峰值都由**独立新建的**测量对象算出，
//! 不复用链路内部状态（避免自己给自己判卷）。同时并排给出两种测量口径：
//!
//! * `KWeighted`：本 crate 的默认口径（近似 K 加权 + 相对门限门控，单位 LUFS）；
//! * `Rms`：裸 RMS 口径（120 Hz 一阶 HPF 后的滑窗
//!   功率 RMS，单位 dBFS），用来对照「同一个增益，两种量法各看到什么」。
//!
//! 处理按 10 ms 一块喂进去——和真实音频回调一样，**不是**把 8 秒整段一次性塞进去
//! （那样伺服一整个缓冲区只更新一次目标增益，量出来的东西没有意义）。
//!
//! 用法：
//!
//! ```text
//! cargo run -p vdev-dsp --release --example offline_report
//! cargo run -p vdev-dsp --release --example offline_report -- --target -16 --bands 15
//! cargo run -p vdev-dsp --release --example offline_report -- --out before_after.raw
//! ```
//!
//! `--out FILE` 会把**处理后的交错 f32（小端、无头）**写到文件，方便丢进别的工具里看。

use std::fs::File;
use std::io::{BufWriter, Write};
use std::process::ExitCode;

use vdev_dsp::{true_peak_4x, Chain, LoudnessMeter, LoudnessMode};

const FS: f64 = 48_000.0;
const CHANNELS: usize = 2;
/// 每次 `process()` 的帧数（10 ms @48k），模拟音频回调的粒度。
const BLOCK_FRAMES: usize = 480;

/// 合成素材的一段。
struct Segment {
    label: &'static str,
    gain_db: f64,
    secs: f64,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// 多音叠加的「节目」。
    Programme,
    /// 单频正弦。
    Tone,
    /// 白噪声。
    Noise,
    /// 纯静音。
    Silence,
}

const PLAN: [Segment; 5] = [
    Segment {
        label: "节目（多音，偏响）",
        gain_db: -9.0,
        secs: 3.0,
        kind: Kind::Programme,
    },
    Segment {
        label: "节目（多音，很轻）",
        gain_db: -42.0,
        secs: 3.0,
        kind: Kind::Programme,
    },
    Segment {
        label: "白噪声",
        gain_db: -20.0,
        secs: 2.0,
        kind: Kind::Noise,
    },
    Segment {
        label: "单频 997 Hz",
        gain_db: -20.0,
        secs: 2.0,
        kind: Kind::Tone,
    },
    Segment {
        label: "纯静音",
        gain_db: -120.0,
        secs: 2.0,
        kind: Kind::Silence,
    },
];

struct Config {
    target_lufs: f64,
    bands: usize,
    out_path: Option<String>,
}

/// 命令行参数。
fn parse_args() -> Result<Config, String> {
    let mut cfg = Config {
        target_lufs: -20.0,
        bands: 10,
        out_path: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--target" => {
                let v = args.next().ok_or("--target 需要一个数值")?;
                cfg.target_lufs = v.parse::<f64>().map_err(|e| format!("--target {v}: {e}"))?;
            }
            "--bands" => {
                let v = args.next().ok_or("--bands 需要一个整数")?;
                cfg.bands = v
                    .parse::<usize>()
                    .map_err(|e| format!("--bands {v}: {e}"))?;
            }
            "--out" => cfg.out_path = Some(args.next().ok_or("--out 需要一个文件名")?),
            "--help" | "-h" => return Err(String::new()),
            other => return Err(format!("未知参数 {other}（--help 看用法）")),
        }
    }
    Ok(cfg)
}

fn main() -> ExitCode {
    let cfg = match parse_args() {
        Ok(c) => c,
        Err(e) => {
            if e.is_empty() {
                println!("用法: offline_report [--target LUFS] [--bands N] [--out FILE]");
                return ExitCode::SUCCESS;
            }
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = run(&cfg) {
        eprintln!("{e}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn run(cfg: &Config) -> Result<(), String> {
    let (dry, marks) = build_material();
    let frames = dry.len() / CHANNELS;

    let mut chain = Chain::new(FS, CHANNELS, cfg.bands);
    chain.eq_mut().set_smoothing_samples(240); // 5 ms 系数交叉淡化
    chain.eq_mut().set_gain(1, 3.0); // 低段抬一点
    chain.eq_mut().set_gain(5, -2.5); // 中段压一点
    chain.eq_mut().set_gain(7, 2.0); // 高段抬一点
    chain.leveling_mut().set_target_loudness(cfg.target_lufs);

    // 逐块处理，并记录每段末尾的实际增益
    let mut wet = dry.clone();
    let mut gains = Vec::with_capacity(marks.len());
    for (i, (start, _)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(frames, |(s, _)| *s);
        for block in wet[start * CHANNELS..end * CHANNELS].chunks_mut(BLOCK_FRAMES * CHANNELS) {
            chain.process(block);
        }
        gains.push(chain.leveling().gain_db());
    }

    report(cfg, &dry, &wet, &marks, &gains, &chain);
    if let Some(path) = &cfg.out_path {
        write_raw(path, &wet).map_err(|e| format!("写 {path} 失败: {e}"))?;
        println!("\n  已写出处理后交错 f32（小端无头）: {path}");
    }
    Ok(())
}

/// 造素材，返回交错缓冲与每段的起始帧号。
fn build_material() -> (Vec<f32>, Vec<(usize, &'static str)>) {
    let mut dry: Vec<f32> = Vec::new();
    let mut marks: Vec<(usize, &'static str)> = Vec::new();
    let mut seed = 0x2545_F491_u32;
    for seg in &PLAN {
        marks.push((dry.len() / CHANNELS, seg.label));
        let n = (FS * seg.secs) as usize;
        let amp = 10f64.powf(seg.gain_db / 20.0);
        for i in 0..n {
            let t = i as f64 / FS;
            let mono = match seg.kind {
                Kind::Programme => {
                    // 三个不对齐的分音 + 缓变包络，避免「单频测出漂亮数字」
                    let env = 0.75 + 0.25 * (2.0 * core::f64::consts::PI * 0.7 * t).sin();
                    env * (0.6 * (2.0 * core::f64::consts::PI * 220.0 * t).sin()
                        + 0.3 * (2.0 * core::f64::consts::PI * 997.0 * t).sin()
                        + 0.1 * (2.0 * core::f64::consts::PI * 3_100.0 * t).sin())
                }
                Kind::Tone => (2.0 * core::f64::consts::PI * 997.0 * t).sin(),
                Kind::Noise => {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (f64::from(seed >> 8) / 8_388_608.0 - 1.0) * 0.9
                }
                Kind::Silence => 0.0,
            };
            let v = (mono * amp) as f32;
            dry.push(v);
            dry.push(v);
        }
    }
    (dry, marks)
}

fn report(
    cfg: &Config,
    dry: &[f32],
    wet: &[f32],
    marks: &[(usize, &'static str)],
    gains: &[f64],
    chain: &Chain,
) {
    let frames = dry.len() / CHANNELS;
    let dry_k = loudness(dry, LoudnessMode::KWeighted);
    let wet_k = loudness(wet, LoudnessMode::KWeighted);
    let dry_r = loudness(dry, LoudnessMode::Rms);
    let wet_r = loudness(wet, LoudnessMode::Rms);

    println!("vdev-dsp 离线对照报告");
    println!(
        "  采样率 {FS:.0} Hz / {CHANNELS} 声道 / {} 段 EQ / 目标 {:.1} LUFS",
        cfg.bands, cfg.target_lufs
    );
    println!(
        "  素材总长 {frames} 帧（{:.2} s），按 {BLOCK_FRAMES} 帧一块处理",
        frames as f64 / FS
    );
    println!();
    println!("  口径           处理前      处理后      变化");
    println!(
        "  K 加权 LUFS    {dry_k:>8.3}    {wet_k:>8.3}    {:+.3}",
        wet_k - dry_k
    );
    println!(
        "  RMS   dBFS     {dry_r:>8.3}    {wet_r:>8.3}    {:+.3}",
        wet_r - dry_r
    );
    let (tp_dry, tp_wet) = (true_peak_4x(dry), true_peak_4x(wet));
    println!(
        "  真峰值(4x)     {tp_dry:>8.6}    {tp_wet:>8.6}    {:+.6}",
        tp_wet - tp_dry
    );
    println!(
        "  限幅器 ceiling {:.6}（离散上限 {:.6}）",
        chain.leveling().limiter().ceiling_linear(),
        chain.leveling().limiter().discrete_ceiling()
    );
    println!();
    println!("  逐段（每段用新的响度计独立测量，互不干扰）：");
    println!("    段                    干 K-LUFS   湿 K-LUFS   湿真峰值   段末增益");
    for (i, (start, label)) in marks.iter().enumerate() {
        let end = marks.get(i + 1).map_or(frames, |(s, _)| *s);
        let d = &dry[start * CHANNELS..end * CHANNELS];
        let w = &wet[start * CHANNELS..end * CHANNELS];
        println!(
            "    {label:<20} {:>8.2}   {:>8.2}   {:>8.6}   {:>+7.2} dB",
            loudness(d, LoudnessMode::KWeighted),
            loudness(w, LoudnessMode::KWeighted),
            true_peak_4x(w),
            gains.get(i).copied().unwrap_or(0.0)
        );
    }
    println!();
    println!("  说明：K 加权与 RMS 是两套口径，同一个增益在两者上看到的变化量**不相等**；");
    println!("        RMS 口径不做频率加权，放在这里是为了对照。");
    println!("        纯静音段的「湿真峰值」只剩 EQ 与限幅器延迟线的尾巴（段首几十毫秒的衰减，之后是 0）。");
}

/// 整段响度：`KWeighted` 返回门控 LUFS，`Rms` 返回 dBFS。
fn loudness(sig: &[f32], mode: LoudnessMode) -> f64 {
    let mut m = LoudnessMeter::new(FS, CHANNELS, mode);
    m.process(sig);
    if mode == LoudnessMode::Rms {
        let r = m.window_rms();
        if r > 0.0 {
            return 20.0 * r.log10();
        }
        return f64::NEG_INFINITY;
    }
    m.loudness()
}

fn write_raw(path: &str, sig: &[f32]) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    let mut bytes = Vec::with_capacity(sig.len() * 4);
    for s in sig {
        bytes.extend_from_slice(&s.to_le_bytes());
    }
    w.write_all(&bytes)?;
    w.flush()
}
