//! 短时响度测量：近似 K 加权 + 相对门限门控（改进点 A）。
//!
//! # 算法依据
//!
//! | 本模块 | 依据 |
//! | --- | --- |
//! | [`LoudnessMode::KWeighted`] | ITU-R BS.1770：两级 shelving + highpass 的 K 加权，加门控 |
//! | [`LoudnessMode::Rms`] | 不做频率加权的裸 RMS（单位 dBFS），仅作对照口径 |
//! | [`LoudnessMeter::process`] | 短时滑窗功率 + 多块功率历史 |
//!
//! # 设计取舍（A）
//!
//! 「一阶高通之后的滑窗功率 RMS」这种做法有四个问题：
//! 1. **没有频率加权**：120 Hz 的一阶 HPF 在 1 kHz 以上几乎无作用，人耳在 2~5 kHz 的
//!    灵敏度（以及 20~200 Hz 的迟钝）完全没有体现，因此「听起来一样响」的两段素材
//!    会被测成差 6~10 dB。
//! 2. **没有门控**：安静的段（呼吸、房间底噪）会被算进平均，把整体响度拉低。
//! 3. **逐声道独立**（见取舍 C）：每个声道各自算电平、各自算增益，立体声像会漂。
//! 4. **梯度预测一类的事后补偿**：那是在为「按 buffer 混增益」这个架构打补丁，
//!    换架构后就不需要了。
//!
//! 本模块的做法：
//! - 两级 K 加权近似：`high_shelf(1681.97 Hz, +3.9998 dB)` + `high_pass(38.135 Hz, Q=0.5003)`，
//!   系数用 RBJ 公式在**实际采样率**上重算（BS.1770 只给了 48 kHz 的固定系数）。
//! - 滑窗（默认 400 ms）+ 100 ms 跳步，得到一串块功率；
//! - **绝对门限 −70 LUFS + 相对门限 −10 LU** 两级门控后取平均（BS.1770 精神，
//!   但窗口是滑动短时窗而非整段节目，因此**不是**合规的 BS.1770 实现，README 已注明）。
//!
//! 另外保留了 [`LoudnessMode::Rms`]：同样的测量框架、不做频率加权、输出 dBFS，
//! 用于和一个「裸 RMS」口径做对照（见 `examples/offline_report.rs`）。

use crate::biquad::{Coeffs, Sos};

/// 绝对门限（LUFS）。低于它的块直接丢弃。
pub const ABSOLUTE_GATE_LUFS: f64 = -70.0;
/// 相对门限（LU）：低于「绝对门控后均值 − 10 LU」的块丢弃。
pub const RELATIVE_GATE_LU: f64 = -10.0;
/// K 加权第一级（高架）中心频率。
pub const K_SHELF_FREQ: f64 = 1_681.974_450_955_533;
/// K 加权第一级（高架）增益。
pub const K_SHELF_GAIN_DB: f64 = 3.999_843_853_973_347;
/// K 加权第二级（RLB 高通）中心频率。
pub const K_HIGHPASS_FREQ: f64 = 38.135_470_876_024_44;
/// K 加权第二级（RLB 高通）Q。
pub const K_HIGHPASS_Q: f64 = 0.500_327_037_323_877_3;
/// 门控用的块数（32 块 × 100 ms ≈ 3.2 s 门控窗）。
const GATE_BLOCKS: usize = 32;

/// 测量模式。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoudnessMode {
    /// 近似 K 加权 + 门控，输出 LUFS（BS.1770 精神）。
    KWeighted,
    /// 不做频率加权的裸 RMS，输出 dBFS（对照口径用）。
    Rms,
}

impl LoudnessMode {
    /// 响度定义里的偏移（LUFS 的 −0.691；dBFS 为 0）。
    #[must_use]
    fn offset(self) -> f64 {
        match self {
            Self::KWeighted => -0.691,
            Self::Rms => 0.0,
        }
    }

    /// 是否要做 K 加权。
    #[must_use]
    fn is_weighted(self) -> bool {
        matches!(self, Self::KWeighted)
    }
}

/// 功率 → 响度（LUFS 或 dBFS）。功率为 0 时返回 `-inf`。
#[must_use]
fn power_to_loudness(power: f64, offset: f64) -> f64 {
    if power > 0.0 && power.is_finite() {
        offset + 10.0 * power.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// 短时响度计。对交错多声道输入做**跨声道聚合**（改进点 C 的测量侧）。
#[derive(Clone, Debug)]
pub struct LoudnessMeter {
    fs: f64,
    channels: usize,
    mode: LoudnessMode,
    filters: Vec<Sos>,
    window_secs: f64,
    hop_secs: f64,
    ring: Vec<f64>,
    ring_pos: usize,
    ring_filled: usize,
    ring_sum: f64,
    win_samples: usize,
    hop_samples: usize,
    hop_left: usize,
    blocks: Vec<f64>,
    block_pos: usize,
    block_filled: usize,
    raw_power: f64,
    ema_coef: f64,
    loudness: f64,
    ungated_loudness: f64,
    momentary: f64,
}

impl LoudnessMeter {
    /// 新建响度计：400 ms 窗口、100 ms 跳步。
    #[must_use]
    pub fn new(fs: f64, channels: usize, mode: LoudnessMode) -> Self {
        let fs = if fs.is_finite() && fs > 0.0 {
            fs
        } else {
            48_000.0
        };
        let mut m = Self {
            fs,
            channels: 0,
            mode,
            filters: Vec::new(),
            window_secs: 0.4,
            hop_secs: 0.1,
            ring: Vec::new(),
            ring_pos: 0,
            ring_filled: 0,
            ring_sum: 0.0,
            win_samples: 0,
            hop_samples: 0,
            hop_left: 0,
            blocks: Vec::new(),
            block_pos: 0,
            block_filled: 0,
            raw_power: 0.0,
            ema_coef: 0.0,
            loudness: f64::NEG_INFINITY,
            ungated_loudness: f64::NEG_INFINITY,
            momentary: f64::NEG_INFINITY,
        };
        m.set_channels(channels);
        m
    }

    /// 采样率。
    #[must_use]
    pub fn sample_rate(&self) -> f64 {
        self.fs
    }

    /// 声道数。
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// 模式。
    #[must_use]
    pub fn mode(&self) -> LoudnessMode {
        self.mode
    }

    /// 滑窗时间（秒）。
    #[must_use]
    pub fn window_secs(&self) -> f64 {
        self.window_secs
    }

    /// 滑窗长度（样本）。
    #[must_use]
    pub fn window_samples(&self) -> usize {
        self.win_samples
    }

    /// 换采样率（重建 K 加权系数与环形缓冲）。
    pub fn set_sample_rate(&mut self, fs: f64) {
        if fs.is_finite() && fs > 0.0 && (fs - self.fs).abs() > f64::EPSILON {
            self.fs = fs;
            let ch = self.channels;
            self.set_channels(ch);
        }
    }

    /// 换声道数（重建滤波器与环形缓冲，状态清零）。
    pub fn set_channels(&mut self, channels: usize) {
        let channels = channels.min(64);
        self.channels = channels;
        self.filters = if self.mode.is_weighted() {
            let stage1 = Coeffs::high_shelf(K_SHELF_FREQ, K_SHELF_GAIN_DB, 1.0, self.fs);
            let stage2 = Coeffs::high_pass(K_HIGHPASS_FREQ, K_HIGHPASS_Q, self.fs);
            (0..channels)
                .map(|_| {
                    let mut s = Sos::new(2);
                    s.set_coeffs(&[stage1, stage2], 0);
                    s
                })
                .collect()
        } else {
            Vec::new()
        };
        self.realloc();
    }

    /// 换模式（重建滤波器与归一化系数）。
    pub fn set_mode(&mut self, mode: LoudnessMode) {
        if mode != self.mode {
            self.mode = mode;
            let ch = self.channels;
            self.set_channels(ch);
        }
    }

    /// 换滑窗时间（秒，≤ 0 会被夹到 10 ms）。
    pub fn set_window_secs(&mut self, secs: f64) {
        let s = if secs.is_finite() {
            secs.clamp(0.01, 10.0)
        } else {
            0.4
        };
        if (s - self.window_secs).abs() > 1.0e-12 {
            self.window_secs = s;
            self.realloc();
        }
    }

    /// 换跳步时间（秒，决定门控块的密度）。
    pub fn set_hop_secs(&mut self, secs: f64) {
        let s = if secs.is_finite() {
            secs.clamp(0.005, 1.0)
        } else {
            0.1
        };
        if (s - self.hop_secs).abs() > 1.0e-12 {
            self.hop_secs = s;
            self.realloc();
        }
    }

    fn realloc(&mut self) {
        self.win_samples = ((self.window_secs * self.fs).round() as usize).max(1);
        self.hop_samples = ((self.hop_secs * self.fs).round() as usize).clamp(1, self.win_samples);
        self.ring = vec![0.0; self.win_samples];
        self.blocks = vec![0.0; GATE_BLOCKS];
        // 原始功率（不做频率加权）的 EMA，时间常数 100 ms：只用于静音判定
        self.ema_coef = (1.0 / (0.1 * self.fs)).clamp(1.0e-6, 1.0);
        self.reset();
    }

    /// 清状态（保留配置）。
    pub fn reset(&mut self) {
        for v in &mut self.ring {
            *v = 0.0;
        }
        for v in &mut self.blocks {
            *v = 0.0;
        }
        for f in &mut self.filters {
            f.reset();
        }
        self.ring_pos = 0;
        self.ring_filled = 0;
        self.ring_sum = 0.0;
        self.hop_left = self.hop_samples;
        self.block_pos = 0;
        self.block_filled = 0;
        self.raw_power = 0.0;
        self.loudness = f64::NEG_INFINITY;
        self.ungated_loudness = f64::NEG_INFINITY;
        self.momentary = f64::NEG_INFINITY;
    }

    /// 喂入一段交错多声道缓冲。**只测量，不改数据**。
    pub fn process(&mut self, buf: &[f32]) {
        let nch = self.channels;
        if nch == 0 || buf.is_empty() {
            return;
        }
        for frame in buf.chunks(nch) {
            let mut weighted = 0.0;
            let mut raw = 0.0;
            let mut cnt = 0usize;
            for (ch, s) in frame.iter().enumerate() {
                let x = f64::from(*s);
                if !x.is_finite() {
                    cnt += 1;
                    continue;
                }
                raw += x * x;
                let y = match self.filters.get_mut(ch) {
                    Some(f) if !f.is_empty() => f.process(x),
                    _ => x,
                };
                weighted += y * y;
                cnt += 1;
            }
            if cnt == 0 {
                continue;
            }
            self.push_frame(weighted, raw / cnt as f64);
        }
    }

    #[inline]
    fn push_frame(&mut self, weighted_power: f64, raw_power: f64) {
        if self.ring.is_empty() {
            return;
        }
        let w = if weighted_power.is_finite() {
            weighted_power
        } else {
            0.0
        };
        if self.ring_filled < self.ring.len() {
            self.ring_sum += w;
            self.ring_filled += 1;
        } else if let Some(slot) = self.ring.get(self.ring_pos) {
            self.ring_sum += w - *slot;
        }
        if let Some(slot) = self.ring.get_mut(self.ring_pos) {
            *slot = w;
        }
        self.ring_pos = (self.ring_pos + 1) % self.ring.len();

        let r = if raw_power.is_finite() {
            raw_power
        } else {
            0.0
        };
        self.raw_power += (r - self.raw_power) * self.ema_coef;

        self.hop_left = self.hop_left.saturating_sub(1);
        if self.hop_left == 0 {
            self.hop_left = self.hop_samples;
            self.finish_block();
        }
    }

    fn finish_block(&mut self) {
        if self.ring_filled == 0 || self.blocks.is_empty() {
            return;
        }
        let divisor = match self.mode {
            // BS.1770：各声道权重为 1，直接累加各声道均方
            LoudnessMode::KWeighted => 1.0,
            // 对照模式：还原成「每声道均方」，即 sum_squares/(samples*channels) 同量纲
            LoudnessMode::Rms => self.channels.max(1) as f64,
        };
        let z = self.ring_sum / (self.ring_filled as f64 * divisor);
        // 未门控的滑窗值（BS.1770 的 momentary loudness 定义），跟随时间 = 滑窗长度，
        // 因此对「突然静音」的反应是 400 ms 级而不是 3.2 s 级——响度归一化的伺服用它。
        self.momentary = power_to_loudness(z, self.mode.offset());
        if self.block_filled < self.blocks.len() {
            self.block_filled += 1;
        }
        if let Some(slot) = self.blocks.get_mut(self.block_pos) {
            *slot = z;
        }
        self.block_pos = (self.block_pos + 1) % self.blocks.len();
        self.recompute_loudness();
    }

    fn recompute_loudness(&mut self) {
        let n = self.block_filled;
        if n == 0 {
            self.loudness = f64::NEG_INFINITY;
            self.ungated_loudness = f64::NEG_INFINITY;
            return;
        }
        let offset = self.mode.offset();
        let Some(slice) = self.blocks.get(..n) else {
            return;
        };

        // 第一级：绝对门限
        let mut abs_sum = 0.0;
        let mut abs_cnt = 0usize;
        for z in slice {
            if power_to_loudness(*z, offset) >= ABSOLUTE_GATE_LUFS {
                abs_sum += *z;
                abs_cnt += 1;
            }
        }
        if abs_cnt == 0 {
            // 整窗都在绝对门限以下：给出未门控值，让上层知道「非常安静」而不是「无数据」
            let mean = slice.iter().sum::<f64>() / n as f64;
            let l = power_to_loudness(mean, offset);
            self.loudness = l;
            self.ungated_loudness = l;
            return;
        }
        let ungated = power_to_loudness(abs_sum / abs_cnt as f64, offset);
        self.ungated_loudness = ungated;

        // 第二级：相对门限
        let rel_threshold = ungated + RELATIVE_GATE_LU;
        let mut sum = 0.0;
        let mut cnt = 0usize;
        for z in slice {
            let l = power_to_loudness(*z, offset);
            if l >= ABSOLUTE_GATE_LUFS && l >= rel_threshold {
                sum += *z;
                cnt += 1;
            }
        }
        self.loudness = if cnt > 0 {
            power_to_loudness(sum / cnt as f64, offset)
        } else {
            ungated
        };
    }

    /// 门控后的短时响度（LUFS 或 dBFS）。无数据 / 全静音时返回 `-inf`。
    #[must_use]
    pub fn loudness(&self) -> f64 {
        self.loudness
    }

    /// 未门控的滑窗（momentary）响度：跟随时间等于滑窗长度（默认 400 ms）。
    ///
    /// 全静音时为 `-inf`；IIR 侧链的数值尾巴会给出很负的有限值。伺服逻辑
    /// （[`crate::Leveling`]）用它做快速反应，用 [`Self::loudness`] 做报告与验收。
    #[must_use]
    pub fn momentary_loudness(&self) -> f64 {
        self.momentary
    }

    /// 未加相对门限的响度（调试/对照用）。
    #[must_use]
    pub fn ungated_loudness(&self) -> f64 {
        self.ungated_loudness
    }

    /// 输入信号的均方（跨声道均值，**不含** K 加权），用于静音判定。
    #[must_use]
    pub fn raw_power(&self) -> f64 {
        self.raw_power
    }

    /// 输入信号的 RMS（跨声道均值）。
    #[must_use]
    pub fn window_rms(&self) -> f64 {
        if self.raw_power > 0.0 {
            self.raw_power.sqrt()
        } else {
            0.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(fs: f64, f: f64, amp: f64, secs: f64) -> Vec<f32> {
        let n = (fs * secs) as usize;
        (0..n)
            .map(|i| (amp * (2.0 * core::f64::consts::PI * f * i as f64 / fs).sin()) as f32)
            .collect()
    }

    #[test]
    fn k_weighting_boosts_presence_band_over_bass() {
        // 同样振幅的 3 kHz 正弦应该比 60 Hz 测得更高（K 加权曲线形状）
        let fs = 48_000.0;
        let mut a = LoudnessMeter::new(fs, 1, LoudnessMode::KWeighted);
        let mut b = LoudnessMeter::new(fs, 1, LoudnessMode::KWeighted);
        a.process(&sine(fs, 3_000.0, 0.5, 2.0));
        b.process(&sine(fs, 60.0, 0.5, 2.0));
        let (la, lb) = (a.loudness(), b.loudness());
        assert!(la > lb + 4.0, "3kHz={la} 60Hz={lb}");
    }

    #[test]
    fn rms_mode_matches_plain_rms() {
        let fs = 48_000.0;
        let amp = 0.5;
        let mut m = LoudnessMeter::new(fs, 1, LoudnessMode::Rms);
        m.process(&sine(fs, 1_000.0, amp, 2.0));
        // 正弦 RMS = amp/sqrt(2) → dBFS
        let expect = 20.0 * (amp / 2f64.sqrt()).log10();
        assert!(
            (m.loudness() - expect).abs() < 0.2,
            "实测 {} 期望 {expect}",
            m.loudness()
        );
    }

    #[test]
    fn silence_gives_negative_infinity_not_nan() {
        let mut m = LoudnessMeter::new(48_000.0, 2, LoudnessMode::KWeighted);
        m.process(&vec![0.0f32; 48_000]);
        assert!(m.loudness().is_infinite() && m.loudness() < 0.0);
        assert_eq!(m.window_rms(), 0.0);
    }

    #[test]
    fn channels_are_aggregated_not_picked() {
        // BS.1770 各声道权重都是 1：内容相同的立体声应当比单声道高 3.01 dB，
        // 而不是「取最大声道」得到的 0 dB。
        let fs = 48_000.0;
        let s = sine(fs, 1_000.0, 0.5, 2.0);
        let mut stereo = Vec::with_capacity(s.len() * 2);
        for v in &s {
            stereo.push(*v);
            stereo.push(*v);
        }
        let mut mono = LoudnessMeter::new(fs, 1, LoudnessMode::KWeighted);
        let mut both = LoudnessMeter::new(fs, 2, LoudnessMode::KWeighted);
        mono.process(&s);
        both.process(&stereo);
        let delta = both.loudness() - mono.loudness();
        assert!((delta - 3.01).abs() < 0.2, "立体声/单声道差 {delta} dB");
    }

    #[test]
    fn relative_gate_ignores_quiet_gaps() {
        // 响 1 s + 静 1 s：门控后的响度应接近「只有响段」时的值
        let fs = 48_000.0;
        let loud = sine(fs, 1_000.0, 0.5, 1.0);
        let mut only = LoudnessMeter::new(fs, 1, LoudnessMode::KWeighted);
        only.process(&loud);
        let l_only = only.loudness();

        let mut with_gap = LoudnessMeter::new(fs, 1, LoudnessMode::KWeighted);
        with_gap.process(&loud);
        with_gap.process(&vec![0.0f32; 24_000]);
        let l_gap = with_gap.loudness();
        assert!(
            (l_gap - l_only).abs() < 1.0,
            "门控失效: {l_gap} vs {l_only}"
        );
    }
}
