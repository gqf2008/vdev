//! 音频增强（DSP）面板的纯逻辑层：参数模型 → vdev-dsp 链路 → 频响曲线 / 离线渲染测量。
//!
//! 本模块 **headless**：不依赖 Slint、不依赖 Windows API，可在任何宿主 `cargo test --lib` 下验证。
//! 所有 DSP 运算都调用 `vdev-dsp` 的公开 API（不改其 `src/` 下任何算法，也不新增第三方依赖）。
//!
//! 链路顺序沿用 `vdev-dsp` 的约定：**图示 EQ → 响度归一化（伺服自动增益）→ 前瞻真峰值限幅**。
//! 与 `vdev_dsp::Chain` 的唯一差别：`Leveling::process()` 在 `enabled == false` 时会整段早退，
//! 其内嵌限幅也随之停摆；为了让 UI 上的「限幅」开关能独立于「响度归一化」生效，本模块把
//! 链路拆成三级（EQ / Leveling / 独立 Limiter），并把 `Leveling` 内嵌的那级限幅关掉以避免双级限幅。

use std::fmt::Write as _;
use std::time::Instant;

use vdev_dsp::{true_peak_4x, GraphicEq, Leveling, Limiter};

// ---------------------------------------------------------------- 参数范围

/// EQ 段数（与 vdev-dsp 默认网格一致）。
pub const EQ_BANDS: usize = 10;
/// 单段增益下限（dB）。
pub const MIN_GAIN_DB: f64 = -12.0;
/// 单段增益上限（dB）。
pub const MAX_GAIN_DB: f64 = 12.0;
/// 目标响度下限（LUFS）。
pub const MIN_TARGET_LUFS: f64 = -30.0;
/// 目标响度上限（LUFS）。
pub const MAX_TARGET_LUFS: f64 = -8.0;
/// 真峰值上限下限（dBFS）。
pub const MIN_CEILING_DB: f64 = -6.0;
/// 真峰值上限上限（dBFS）。
pub const MAX_CEILING_DB: f64 = 0.0;
/// 频响曲线采样点数。
pub const CURVE_POINTS: usize = 128;
/// 频响曲线左端（Hz）。
pub const CURVE_MIN_HZ: f64 = 20.0;
/// 频响曲线右端（Hz，实际还会按采样率收窄到 Nyquist 之下）。
pub const CURVE_MAX_HZ: f64 = 20_000.0;
/// 渲染链路使用的声道数。
pub const RENDER_CHANNELS: usize = 2;
/// 离线渲染的分块长度（毫秒）——与实时音频回调的粒度一致。
pub const BLOCK_MS: f64 = 10.0;

// ---------------------------------------------------------------- 参数模型

/// 音频增强面板的全部参数。
#[derive(Clone, Debug, PartialEq)]
pub struct DspParams {
    /// 10 段增益（dB），顺序与 [`eq_band_frequencies_hz`] 一致。
    pub gains_db: [f64; EQ_BANDS],
    /// EQ 旁路（旁路时逐位恒等）。
    pub eq_bypass: bool,
    /// 响度归一化（伺服自动增益）开关。
    pub leveling_enabled: bool,
    /// 目标响度（LUFS）。
    pub target_lufs: f64,
    /// 真峰值限幅开关（独立于响度归一化）。
    pub limiter_enabled: bool,
    /// 真峰值上限（dBFS）。
    pub ceiling_db: f64,
    /// 采样率（Hz）——离线渲染时会跟随被渲染文件的采样率。
    pub sample_rate: f64,
}

impl Default for DspParams {
    fn default() -> Self {
        Self {
            gains_db: [0.0; EQ_BANDS],
            eq_bypass: false,
            leveling_enabled: false,
            target_lufs: -14.0,
            limiter_enabled: false,
            ceiling_db: -1.0,
            sample_rate: vdev_dsp::DEFAULT_SAMPLE_RATE,
        }
    }
}

fn finite_in_range(value: f64, min: f64, max: f64, fallback: f64) -> f64 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

impl DspParams {
    /// 把每个数值夹进 UI 允许的范围（防御 UI/外部传入的越界值）。
    #[must_use]
    pub fn clamped(&self) -> Self {
        let mut gains = [0.0_f64; EQ_BANDS];
        for (dst, src) in gains.iter_mut().zip(self.gains_db.iter()) {
            *dst = finite_in_range(*src, MIN_GAIN_DB, MAX_GAIN_DB, 0.0);
        }
        let fs = if self.sample_rate.is_finite() && self.sample_rate > 0.0 {
            self.sample_rate
        } else {
            vdev_dsp::DEFAULT_SAMPLE_RATE
        };
        Self {
            gains_db: gains,
            eq_bypass: self.eq_bypass,
            leveling_enabled: self.leveling_enabled,
            target_lufs: finite_in_range(self.target_lufs, MIN_TARGET_LUFS, MAX_TARGET_LUFS, -14.0),
            limiter_enabled: self.limiter_enabled,
            ceiling_db: finite_in_range(self.ceiling_db, MIN_CEILING_DB, MAX_CEILING_DB, -1.0),
            sample_rate: fs,
        }
    }

    /// 10 段中心频点（Hz）。
    #[must_use]
    pub fn band_frequencies_hz(&self) -> [f64; EQ_BANDS] {
        eq_band_frequencies_hz(self.sample_rate)
    }
}

/// vdev-dsp 在 `sample_rate` 下 10 段图示 EQ 的默认中心频点。
///
/// 直接取自 `GraphicEq::new(..).frequencies()`，保证与算法库的默认对数等间距网格**逐位一致**
/// （不是在这里另写一张频率表）。
#[must_use]
pub fn eq_band_frequencies_hz(sample_rate: f64) -> [f64; EQ_BANDS] {
    let eq = GraphicEq::new(sample_rate, RENDER_CHANNELS, EQ_BANDS);
    let mut out = [0.0_f64; EQ_BANDS];
    for (dst, src) in out.iter_mut().zip(eq.frequencies().iter()) {
        *dst = *src;
    }
    out
}

// ---------------------------------------------------------------- 链路

/// 用 `vdev-dsp` 的 `Chain` 构建整链（EQ + 响度归一化，含内嵌限幅）。
///
/// 频响曲线与「全 0 增益 == 旁路」这条一致性检查都走它。
#[must_use]
pub fn build_chain(p: &DspParams) -> vdev_dsp::Chain {
    let p = p.clamped();
    let mut chain = vdev_dsp::Chain::new(p.sample_rate, RENDER_CHANNELS, EQ_BANDS);
    chain.eq_mut().set_smoothing_samples(240); // 5 ms 系数交叉淡化，防设参爆音
    for (i, g) in p.gains_db.iter().enumerate() {
        chain.eq_mut().set_gain(i, *g);
    }
    chain.eq_mut().set_bypass(p.eq_bypass);
    chain.leveling_mut().set_enabled(p.leveling_enabled);
    chain.leveling_mut().set_target_loudness(p.target_lufs);
    chain
        .leveling_mut()
        .limiter_mut()
        .set_enabled(p.limiter_enabled);
    chain
        .leveling_mut()
        .limiter_mut()
        .set_ceiling_db(p.ceiling_db);
    chain
}

/// 三级链路：`GraphicEq` → `Leveling`（伺服）→ 独立 `Limiter`。
#[derive(Debug)]
pub struct DspPipeline {
    eq: GraphicEq,
    leveling: Leveling,
    limiter: Limiter,
}

impl DspPipeline {
    /// 按参数构建链路，`channels` 为声道数（1 或 2）。
    #[must_use]
    pub fn new(p: &DspParams, channels: usize) -> Self {
        let p = p.clamped();
        let ch = channels.clamp(1, RENDER_CHANNELS);

        let mut eq = GraphicEq::new(p.sample_rate, ch, EQ_BANDS);
        eq.set_smoothing_samples(240); // 5 ms 系数交叉淡化，防设参爆音
        for (i, g) in p.gains_db.iter().enumerate() {
            eq.set_gain(i, *g);
        }
        eq.set_bypass(p.eq_bypass);

        let mut leveling = Leveling::new(p.sample_rate, ch);
        leveling.set_enabled(p.leveling_enabled);
        leveling.set_target_loudness(p.target_lufs);
        // 内嵌限幅关掉：否则「限幅」开关会被「响度归一化」开关绑架，见模块头注释。
        leveling.limiter_mut().set_enabled(false);

        let mut limiter = Limiter::new(p.sample_rate, ch);
        limiter.set_enabled(p.limiter_enabled);
        limiter.set_ceiling_db(p.ceiling_db);

        Self {
            eq,
            leveling,
            limiter,
        }
    }

    /// 原地处理交错多声道缓冲。
    pub fn process(&mut self, buf: &mut [f32]) {
        self.eq.process(buf);
        self.leveling.process(buf);
        self.limiter.process(buf);
    }

    /// EQ（只读）。
    #[must_use]
    pub fn eq(&self) -> &GraphicEq {
        &self.eq
    }

    /// 响度归一化（只读）。
    #[must_use]
    pub fn leveling(&self) -> &Leveling {
        &self.leveling
    }

    /// 独立限幅（只读）。
    #[must_use]
    pub fn limiter(&self) -> &Limiter {
        &self.limiter
    }

    /// 伺服当前实际增益（dB）；伺服关闭时恒为 0。
    #[must_use]
    pub fn servo_gain_db(&self) -> f64 {
        if self.leveling.is_enabled() {
            self.leveling.gain_db()
        } else {
            0.0
        }
    }

    /// 按 `block_frames` 帧一块喂完整段信号（模拟实时回调的粒度）。
    pub fn process_blocked(&mut self, samples: &mut [f32], block_frames: usize, channels: usize) {
        let ch = channels.clamp(1, RENDER_CHANNELS);
        let step = block_frames.max(1) * ch;
        for block in samples.chunks_mut(step) {
            self.process(block);
        }
    }
}

// ---------------------------------------------------------------- 频响曲线

/// 频响曲线用的对数等间距频点（Hz），右端会收窄到 Nyquist 之下。
#[must_use]
pub fn curve_frequencies_hz(sample_rate: f64) -> Vec<f64> {
    let fs = if sample_rate.is_finite() && sample_rate > 0.0 {
        sample_rate
    } else {
        vdev_dsp::DEFAULT_SAMPLE_RATE
    };
    let lo = CURVE_MIN_HZ;
    let nyquist_guard = fs * 0.49;
    let hi = CURVE_MAX_HZ.min(nyquist_guard).max(lo * 1.001).max(lo);
    let (ln_lo, ln_hi) = (lo.ln(), hi.ln());
    (0..CURVE_POINTS)
        .map(|i| {
            let t = i as f64 / (CURVE_POINTS - 1) as f64;
            (ln_lo + (ln_hi - ln_lo) * t).exp()
        })
        .collect()
}

/// 实时频响曲线（dB）：每个频点都用 `GraphicEq::magnitude_response_db` 的真实系数求值。
///
/// 结果全部为有限值（异常值按 [`CURVE_FLOOR_DB`] 表示），便于直接喂给 UI。
#[must_use]
pub fn response_curve_db(p: &DspParams) -> Vec<f64> {
    let p = p.clamped();
    let chain = build_chain(&p);
    let eq = chain.eq();
    curve_frequencies_hz(p.sample_rate)
        .into_iter()
        .map(|f| sanitize_db(eq.magnitude_response_db(f)))
        .collect()
}

/// 曲线纵轴的下限（dB），用于兜底非有限值。
pub const CURVE_FLOOR_DB: f64 = -120.0;
/// 曲线纵轴的上限（dB），用于兜底非有限值。
pub const CURVE_CEIL_DB: f64 = 120.0;

fn sanitize_db(db: f64) -> f64 {
    if db.is_finite() {
        db.clamp(CURVE_FLOOR_DB, CURVE_CEIL_DB)
    } else {
        CURVE_FLOOR_DB
    }
}

/// 把频响曲线转成 Slint `Path { commands: ... }` 用的 SVG 风格字符串。
///
/// `db_min..db_max` 映射到纵向 `height..0`（`db_max` 在最上方）。
/// 输出只含有限的定点小数，绝无 `nan` / `inf` / 科学计数法。
#[must_use]
pub fn curve_path(p: &DspParams, width: f32, height: f32, db_min: f64, db_max: f64) -> String {
    let lo = if db_min.is_finite() { db_min } else { -12.0 };
    let hi = if db_max.is_finite() && db_max > lo {
        db_max
    } else {
        lo + 1.0
    };
    let w = if width.is_finite() && width > 0.0 {
        width
    } else {
        1.0
    };
    let h = if height.is_finite() && height > 0.0 {
        height
    } else {
        1.0
    };
    let points = response_curve_db(p);
    let denom = (points.len().saturating_sub(1)).max(1) as f32;
    let mut out = String::with_capacity(points.len() * 18);
    for (i, db) in points.iter().enumerate() {
        let x = w * (i as f32) / denom;
        let t = ((db - lo) / (hi - lo)).clamp(0.0, 1.0) as f32;
        let y = h * (1.0 - t);
        if i == 0 {
            let _ = write!(out, "M {x:.2} {y:.2}");
        } else {
            let _ = write!(out, " L {x:.2} {y:.2}");
        }
    }
    if out.is_empty() {
        out.push_str("M 0 0");
    }
    out
}

// ---------------------------------------------------------------- 测量

/// 整段 RMS（dBFS）；全零返回 `-inf`。
#[must_use]
pub fn rms_dbfs(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let sum: f64 = samples.iter().map(|s| f64::from(*s) * f64::from(*s)).sum();
    let rms = (sum / samples.len() as f64).sqrt();
    if rms > 0.0 {
        20.0 * rms.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// 真峰值（dBTP），用 `vdev-dsp` 的 4x 过采样测量。
#[must_use]
pub fn true_peak_dbtp(samples: &[f32]) -> f64 {
    if samples.is_empty() {
        return f64::NEG_INFINITY;
    }
    let tp = true_peak_4x(samples);
    if tp > 0.0 {
        20.0 * tp.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// 线性幅度 → dB。
#[must_use]
pub fn linear_to_db(x: f64) -> f64 {
    if x > 0.0 {
        20.0 * x.log10()
    } else {
        f64::NEG_INFINITY
    }
}

// ---------------------------------------------------------------- WAV 读写

/// 解出来的交错 f32 音频。
#[derive(Clone, Debug, PartialEq)]
pub struct WavData {
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 声道数（只支持 1 / 2）。
    pub channels: u16,
    /// 交错采样。
    pub samples: Vec<f32>,
}

impl WavData {
    /// 帧数。
    #[must_use]
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }
}

/// 读 wav（支持 PCM 8/16/24/32bit、IEEE float 32/64bit、WAVE_FORMAT_EXTENSIBLE）。
pub fn wav_read(path: &str) -> Result<WavData, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("读 {path} 失败: {e}"))?;
    parse_wav(&bytes)
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// 解析 RIFF/WAVE 字节流。
pub fn parse_wav(bytes: &[u8]) -> Result<WavData, String> {
    if bytes.len() < 12 {
        return Err("文件太短，不是 RIFF/WAVE".to_string());
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("不是 RIFF/WAVE 文件".to_string());
    }

    let mut pos = 12_usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (tag, channels, rate, bits)
    let mut data: Option<&[u8]> = None;

    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(bytes, pos + 4) as usize;
        let body_start = pos + 8;
        let Some(body_end) = body_start.checked_add(size) else {
            break;
        };
        if body_end > bytes.len() {
            break; // 容忍被截断的尾部 chunk
        }
        let body = &bytes[body_start..body_end];
        match id {
            b"fmt " => {
                if body.len() < 16 {
                    return Err("fmt chunk 太短".to_string());
                }
                let mut tag = u16_at(body, 0);
                let channels = u16_at(body, 2);
                let rate = u32_at(body, 4);
                let bits = u16_at(body, 14);
                // WAVE_FORMAT_EXTENSIBLE：真正的 tag 在 SubFormat 的头 2 字节。
                if tag == 0xFFFE && body.len() >= 26 {
                    tag = u16_at(body, 24);
                }
                fmt = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        pos = body_end + (size & 1); // chunk 对齐到偶数边界
    }

    let (tag, channels, rate, bits) = fmt.ok_or_else(|| "缺少 fmt chunk".to_string())?;
    let data = data.ok_or_else(|| "缺少 data chunk".to_string())?;
    if channels == 0 || channels > RENDER_CHANNELS as u16 {
        return Err(format!("不支持 {channels} 声道（只支持 1/2）"));
    }
    if rate == 0 {
        return Err("采样率为 0".to_string());
    }

    let samples = decode_samples(tag, bits, data)?;
    Ok(WavData {
        sample_rate: rate,
        channels,
        samples,
    })
}

fn decode_samples(tag: u16, bits: u16, data: &[u8]) -> Result<Vec<f32>, String> {
    let mut out = Vec::with_capacity(data.len() / 2);
    match (tag, bits) {
        (1, 8) => {
            for b in data {
                out.push((f32::from(*b) - 128.0) / 128.0);
            }
        }
        (1, 16) => {
            for c in data.chunks_exact(2) {
                out.push(f32::from(i16::from_le_bytes([c[0], c[1]])) / 32_768.0);
            }
        }
        (1, 24) => {
            for c in data.chunks_exact(3) {
                let v =
                    ((i32::from(c[2]) << 24) | (i32::from(c[1]) << 16) | (i32::from(c[0]) << 8))
                        >> 8;
                out.push(v as f32 / 8_388_608.0);
            }
        }
        (1, 32) => {
            for c in data.chunks_exact(4) {
                let v = i32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                out.push(v as f32 / 2_147_483_648.0);
            }
        }
        (3, 32) => {
            for c in data.chunks_exact(4) {
                out.push(f32::from_le_bytes([c[0], c[1], c[2], c[3]]));
            }
        }
        (3, 64) => {
            for c in data.chunks_exact(8) {
                let v = f64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]);
                out.push(v as f32);
            }
        }
        _ => return Err(format!("不支持的采样格式：format tag={tag}, {bits} bit")),
    }
    Ok(out)
}

/// 写 32-bit IEEE float wav。
pub fn wav_write_f32(
    path: &str,
    sample_rate: u32,
    channels: u16,
    samples: &[f32],
) -> Result<(), String> {
    if channels == 0 || channels > RENDER_CHANNELS as u16 {
        return Err(format!("只支持 1/2 声道，收到 {channels}"));
    }
    let data_bytes = u32::try_from(samples.len() * 4).map_err(|_| "音频太长".to_string())?;
    let mut out = Vec::with_capacity(44 + samples.len() * 4);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16_u32.to_le_bytes());
    out.extend_from_slice(&3_u16.to_le_bytes()); // WAVE_FORMAT_IEEE_FLOAT
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * u32::from(channels) * 4;
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&(channels * 4).to_le_bytes());
    out.extend_from_slice(&32_u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_bytes.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, out).map_err(|e| format!("写 {path} 失败: {e}"))
}

// ---------------------------------------------------------------- 测试素材

fn xorshift(state: &mut u32) -> f64 {
    let mut x = *state;
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    *state = x;
    f64::from(x >> 8) / 8_388_608.0 - 1.0
}

/// 生成一段确定性的测试素材（48 kHz / 立体声 / 约 6 s）并写成 wav。
///
/// 素材分三段：偏响的多音「节目」→ 很轻的节目 → 白噪声，用来同时看到
/// 伺服提升/衰减与限幅的动作。
pub fn synth_demo_wav(path: &str) -> Result<WavData, String> {
    const FS: f64 = 48_000.0;
    let channels = RENDER_CHANNELS;
    let mut samples: Vec<f32> = Vec::new();
    let mut seed = 0x2545_F491_u32;

    let push_frame = |l: f32, r: f32, samples: &mut Vec<f32>| {
        samples.push(l);
        samples.push(r);
    };

    // 段 1：0..2.5 s，偏响的多音节目（峰值 ~0.85）
    for i in 0..(FS * 2.5) as usize {
        let t = i as f64 / FS;
        let env = 0.75 + 0.25 * (2.0 * std::f64::consts::PI * 0.7 * t).sin();
        let mono = env
            * (0.6 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()
                + 0.3 * (2.0 * std::f64::consts::PI * 997.0 * t).sin()
                + 0.1 * (2.0 * std::f64::consts::PI * 3_100.0 * t).sin());
        let v = (mono * 0.85) as f32;
        push_frame(v, v * 0.9, &mut samples);
    }
    // 段 2：2.5..4.5 s，很轻的节目（-42 dB 量级），用来逼出伺服的提升
    for i in 0..(FS * 2.0) as usize {
        let t = i as f64 / FS;
        let mono = 0.6 * (2.0 * std::f64::consts::PI * 440.0 * t).sin()
            + 0.4 * (2.0 * std::f64::consts::PI * 1_760.0 * t).sin();
        let v = (mono * 0.008) as f32;
        push_frame(v, v, &mut samples);
    }
    // 段 3：4.5..6.0 s，白噪声（峰值 ~0.5）
    for _ in 0..(FS * 1.5) as usize {
        let n = xorshift(&mut seed) * 0.5;
        let v = n as f32;
        push_frame(v, -v, &mut samples);
    }

    let data = WavData {
        sample_rate: FS as u32,
        channels: channels as u16,
        samples,
    };
    wav_write_f32(path, data.sample_rate, data.channels, &data.samples)?;
    Ok(data)
}

// ---------------------------------------------------------------- 离线渲染

/// 一次离线渲染的测量结果。
#[derive(Clone, Debug, PartialEq)]
pub struct RenderReport {
    /// 输入 wav 路径。
    pub input_path: String,
    /// 输出 wav 路径。
    pub output_path: String,
    /// 采样率（Hz）。
    pub sample_rate: u32,
    /// 声道数。
    pub channels: u16,
    /// 帧数。
    pub frames: usize,
    /// 音频时长（s）。
    pub audio_secs: f64,
    /// 输入整段 RMS（dBFS）。
    pub in_rms_dbfs: f64,
    /// 输出整段 RMS（dBFS）。
    pub out_rms_dbfs: f64,
    /// 输出真峰值（dBTP，4x 过采样）。
    pub out_true_peak_dbtp: f64,
    /// 限幅 ceiling（dBFS，参数值）。
    pub ceiling_db: f64,
    /// 限幅 ceiling（线性）。
    pub ceiling_linear: f64,
    /// 真峰值相对 ceiling 的余量（dB，负数 = 没顶到）。
    pub true_peak_over_ceiling_db: f64,
    /// 段末伺服增益（dB）。
    pub final_servo_gain_db: f64,
    /// 渲染耗时（s）。
    pub elapsed_secs: f64,
    /// 相对实时的倍速（>1 表示比实时快）。
    pub x_realtime: f64,
}

impl RenderReport {
    /// 一行摘要（给 UI 显示 / 日志用）。
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "输出 {} | {:.0} Hz / {}ch / {} 帧（{:.2} s）\n\
             输出 RMS {:.3} dBFS（输入 {:.3}）\n\
             真峰值 {:.3} dBTP vs ceiling {:.2} dBFS（余量 {:+.3} dB）\n\
             伺服增益 {:+.2} dB\n\
             耗时 {:.3} s = {:.1}x realtime",
            self.output_path,
            self.sample_rate,
            self.channels,
            self.frames,
            self.audio_secs,
            self.out_rms_dbfs,
            self.in_rms_dbfs,
            self.out_true_peak_dbtp,
            self.ceiling_db,
            self.true_peak_over_ceiling_db,
            self.final_servo_gain_db,
            self.elapsed_secs,
            self.x_realtime
        )
    }
}

/// 渲染：`WavData` → 整链 → 写出 wav + 测量。
///
/// 链路采样率**跟随文件采样率**（`p.sample_rate` 只用于频响曲线等 UI 侧计算）。
/// 处理按 [`BLOCK_MS`] 毫秒一块喂入，和实时回调粒度一致——整段一次塞进去会让伺服
/// 只更新一次目标增益，量出来的数字没有意义。
pub fn render_offline(
    input: &WavData,
    output_path: &str,
    p: &DspParams,
) -> Result<RenderReport, String> {
    let ch = input.channels as usize;
    if input.sample_rate == 0 {
        return Err("输入采样率为 0".to_string());
    }
    let frames = input.frames();
    if frames == 0 {
        return Err("输入 wav 没有采样".to_string());
    }

    let mut params = p.clamped();
    params.sample_rate = f64::from(input.sample_rate);

    let mut pipeline = DspPipeline::new(&params, ch);
    let mut buf = input.samples.clone();
    let block_frames = ((f64::from(input.sample_rate) * BLOCK_MS / 1000.0).round() as usize).max(1);

    let started = Instant::now();
    pipeline.process_blocked(&mut buf, block_frames, ch);
    let elapsed_secs = started.elapsed().as_secs_f64();

    wav_write_f32(output_path, input.sample_rate, input.channels, &buf)?;

    let audio_secs = frames as f64 / f64::from(input.sample_rate);
    let out_true_peak_dbtp = true_peak_dbtp(&buf);
    let ceiling_db = pipeline.limiter().ceiling_db();
    let x_realtime = if elapsed_secs > 0.0 {
        audio_secs / elapsed_secs
    } else {
        f64::INFINITY
    };

    Ok(RenderReport {
        input_path: String::new(),
        output_path: output_path.to_string(),
        sample_rate: input.sample_rate,
        channels: input.channels,
        frames,
        audio_secs,
        in_rms_dbfs: rms_dbfs(&input.samples),
        out_rms_dbfs: rms_dbfs(&buf),
        out_true_peak_dbtp,
        ceiling_db,
        ceiling_linear: pipeline.limiter().ceiling_linear(),
        true_peak_over_ceiling_db: out_true_peak_dbtp - ceiling_db,
        final_servo_gain_db: pipeline.servo_gain_db(),
        elapsed_secs,
        x_realtime,
    })
}

/// 读文件 → 渲染 → 写文件，一步到位（UI 的「渲染并测量」走这条）。
pub fn render_file(
    input_path: &str,
    output_path: &str,
    p: &DspParams,
) -> Result<RenderReport, String> {
    let input = wav_read(input_path)?;
    let mut report = render_offline(&input, output_path, p)?;
    report.input_path = input_path.to_string();
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, fs: f64, secs: f64, amp: f64, channels: usize) -> Vec<f32> {
        let frames = (fs * secs) as usize;
        let mut out = Vec::with_capacity(frames * channels);
        for i in 0..frames {
            let v = (amp * (2.0 * std::f64::consts::PI * freq * (i as f64) / fs).sin()) as f32;
            for c in 0..channels {
                out.push(if c == 0 { v } else { v * 0.5 });
            }
        }
        out
    }

    /// 用一个独立新建的测量对象算某段信号的 RMS（dB）。
    fn rms_of(buf: &[f32], channels: usize) -> f64 {
        let start = (buf.len() / 2) / channels * channels; // 丢掉前一半，避开启动瞬态
        rms_dbfs(&buf[start..])
    }

    // ①全 0 增益 == 旁路：整条链路（含响度归一化与限幅）逐位相等。
    #[test]
    fn zero_gains_equals_bypass() {
        let on = DspParams {
            eq_bypass: false,
            leveling_enabled: true,
            limiter_enabled: true,
            ..DspParams::default()
        };
        let mut off = on.clone();
        off.eq_bypass = true;

        let dry = sine(997.0, 48_000.0, 1.0, 0.5, 2);
        let mut a = dry.clone();
        let mut b = dry.clone();
        DspPipeline::new(&on, 2).process_blocked(&mut a, 480, 2);
        DspPipeline::new(&off, 2).process_blocked(&mut b, 480, 2);

        assert_eq!(a, b, "全 0 增益与旁路的输出必须逐位相等");
        // 旁路链本身也会被响度归一化改动，所以这里不比 dry；另测纯 EQ 恒等：
        let mut c = dry.clone();
        let flat = DspParams {
            eq_bypass: true,
            ..DspParams::default()
        };
        DspPipeline::new(&flat, 2).process_blocked(&mut c, 480, 2);
        assert_eq!(c, dry, "整链旁路（EQ 旁路 + 响度关 + 限幅关）必须逐位恒等");
    }

    // ②单段 +6 dB：在该段中心频率处实测增益 ≈ +6 dB。
    #[test]
    fn single_band_plus6db_at_center() {
        let fs = 48_000.0;
        let freqs = eq_band_frequencies_hz(fs);
        let band = 5;
        let fc = freqs[band];

        let mut p = DspParams::default();
        p.gains_db[band] = 6.0;

        let dry = sine(fc, fs, 2.0, 0.25, 2);
        let mut wet = dry.clone();
        DspPipeline::new(&p, 2).process_blocked(&mut wet, 480, 2);

        let measured = rms_of(&wet, 2) - rms_of(&dry, 2);
        assert!(
            (measured - 6.0).abs() < 1.0,
            "第 {band} 段（{fc:.1} Hz）+6 dB 实测 {measured:.3} dB，超出 ±1 dB"
        );

        // 与解析式频响互相印证（同一批系数，两条路的口径必须接近）。
        let analytic = build_chain(&p).eq().magnitude_response_db(fc);
        assert!(
            (analytic - 6.0).abs() < 1.0,
            "解析式频响 {analytic:.3} dB 与 +6 dB 不符"
        );
    }

    // ③ceiling 调低 → 输出真峰值下降。
    #[test]
    fn lower_ceiling_lowers_true_peak() {
        let fs = 48_000.0;
        let dry = sine(997.0, fs, 1.0, 1.5, 2); // 明显过 0 dBFS

        let high = DspParams {
            limiter_enabled: true,
            ceiling_db: MAX_CEILING_DB, // 0 dBFS
            ..DspParams::default()
        };
        let mut low = high.clone();
        low.ceiling_db = MIN_CEILING_DB; // -6 dBFS

        let mut a = dry.clone();
        let mut b = dry.clone();
        DspPipeline::new(&high, 2).process_blocked(&mut a, 480, 2);
        DspPipeline::new(&low, 2).process_blocked(&mut b, 480, 2);

        let (tpa, tpb) = (true_peak_dbtp(&a), true_peak_dbtp(&b));
        assert!(
            tpb < tpa,
            "ceiling 从 {MAX_CEILING_DB} dB 降到 {MIN_CEILING_DB} dB 后真峰值应下降（{tpa:.3} -> {tpb:.3} dBTP）"
        );
    }

    // ④目标 LUFS 改变 → 伺服增益随之变化（方向正确）。
    #[test]
    fn target_lufs_moves_servo_gain() {
        let fs = 48_000.0;
        let dry = sine(997.0, fs, 3.0, 0.5, 2);

        let loud = DspParams {
            leveling_enabled: true,
            target_lufs: -14.0,
            ..DspParams::default()
        };
        let mut quiet = loud.clone();
        quiet.target_lufs = -23.0;

        let mut a = dry.clone();
        let mut b = dry.clone();
        let mut pa = DspPipeline::new(&loud, 2);
        pa.process_blocked(&mut a, 480, 2);
        let mut pb = DspPipeline::new(&quiet, 2);
        pb.process_blocked(&mut b, 480, 2);

        let (ga, gb) = (pa.servo_gain_db(), pb.servo_gain_db());
        assert!(
            gb < ga - 3.0,
            "目标从 -14 降到 -23 LUFS 后伺服增益应明显更低（{ga:+.2} -> {gb:+.2} dB）"
        );
    }

    #[test]
    fn band_frequencies_match_the_library_grid() {
        let fs = 48_000.0;
        let ours = eq_band_frequencies_hz(fs);
        let eq = GraphicEq::new(fs, 2, EQ_BANDS);
        assert_eq!(ours.len(), eq.frequencies().len());
        for (a, b) in ours.iter().zip(eq.frequencies().iter()) {
            assert_eq!(a, b, "频点必须与 vdev-dsp 默认网格逐位一致");
        }
        assert!(ours.windows(2).all(|w| w[1] > w[0]), "频点必须严格递增");
    }

    #[test]
    fn curve_is_finite_and_flat_when_flat() {
        let p = DspParams::default();
        let curve = response_curve_db(&p);
        assert_eq!(curve.len(), CURVE_POINTS);
        assert!(curve.iter().all(|v| v.is_finite()), "曲线必须是有限值");
        assert!(
            curve.iter().all(|v| v.abs() < 1.0e-3),
            "全 0 增益的频响应当平坦在 0 dB"
        );
        let path = curve_path(&p, 512.0, 128.0, -12.0, 12.0);
        assert!(path.starts_with("M "));
        assert!(path.matches('L').count() == CURVE_POINTS - 1);
        assert!(!path.contains("nan") && !path.contains("inf"));
    }

    #[test]
    fn curve_reacts_to_gain_changes() {
        let flat = DspParams::default();
        let mut boosted = flat.clone();
        boosted.gains_db[9] = 12.0;
        let a = response_curve_db(&flat);
        let b = response_curve_db(&boosted);
        let max_delta = a
            .iter()
            .zip(b.iter())
            .map(|(x, y)| (y - x).abs())
            .fold(0.0_f64, f64::max);
        assert!(
            max_delta > 6.0,
            "高段 +12 dB 后曲线必须有明显变化（实测最大差 {max_delta:.3} dB）"
        );
    }

    #[test]
    fn wav_round_trip() {
        let dir = std::env::temp_dir();
        let path = dir.join("vdev_app_win_dsp_roundtrip.wav");
        let path_s = path.to_string_lossy().to_string();
        let data = synth_demo_wav(&path_s).expect("生成素材");
        let back = wav_read(&path_s).expect("读回素材");
        assert_eq!(back.sample_rate, data.sample_rate);
        assert_eq!(back.channels, data.channels);
        assert_eq!(back.samples.len(), data.samples.len());
        for (a, b) in back.samples.iter().zip(data.samples.iter()) {
            assert!((a - b).abs() < 1.0e-6, "float32 wav 往返必须无损");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn render_moves_numbers_with_params() {
        let dir = std::env::temp_dir();
        let src = dir.join("vdev_app_win_dsp_render_src.wav");
        let src_s = src.to_string_lossy().to_string();
        synth_demo_wav(&src_s).expect("生成素材");
        let input = wav_read(&src_s).expect("读素材");

        let a = DspParams {
            limiter_enabled: true,
            ceiling_db: -1.0,
            ..DspParams::default()
        };
        let mut b = a.clone();
        b.gains_db = [6.0, 4.0, 0.0, -6.0, -3.0, 2.0, 0.0, 1.0, 2.0, 4.0];
        b.leveling_enabled = true;
        b.target_lufs = -20.0;
        b.ceiling_db = -6.0;

        let out_a = dir
            .join("vdev_app_win_dsp_render_a.wav")
            .to_string_lossy()
            .to_string();
        let out_b = dir
            .join("vdev_app_win_dsp_render_b.wav")
            .to_string_lossy()
            .to_string();
        let ra = render_offline(&input, &out_a, &a).expect("渲染 A");
        let rb = render_offline(&input, &out_b, &b).expect("渲染 B");

        assert!(ra.out_rms_dbfs.is_finite() && rb.out_rms_dbfs.is_finite());
        assert!(ra.x_realtime > 0.0 && rb.x_realtime > 0.0);
        assert!(
            (rb.out_rms_dbfs - ra.out_rms_dbfs).abs() > 0.5,
            "两组参数的输出 RMS 必须不同（{:.3} vs {:.3} dBFS）",
            ra.out_rms_dbfs,
            rb.out_rms_dbfs
        );
        assert!(
            rb.out_true_peak_dbtp < ra.out_true_peak_dbtp,
            "ceiling 从 -1 降到 -6 后真峰值应更低（{:.3} vs {:.3} dBTP）",
            ra.out_true_peak_dbtp,
            rb.out_true_peak_dbtp
        );
        for f in [
            src,
            std::path::PathBuf::from(out_a),
            std::path::PathBuf::from(out_b),
        ] {
            let _ = std::fs::remove_file(f);
        }
    }
}
