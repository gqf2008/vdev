//! N 段图示均衡器（默认 10 段，对数等间距频点 + 每段 peaking 节）。
//!
//! # 算法依据
//!
//! | 本模块 | 依据 |
//! | --- | --- |
//! | [`band_frequencies`] | 多段图示均衡器的通用做法：频点在**对数域**等间距分布 |
//! | [`band_q`] | 段间交叠取 `Q = √r / (r − 1)`（`r` 为相邻频点比），并设 Q 下限 |
//! | [`GraphicEq::set_gains`] | 段数变化时对增益曲线按对数频率重采样保形 |
//! | [`GraphicEq::set_gain`] | 单段增益钳位到 [`MAX_GAIN_DB`] |
//! | [`GraphicEq::process`] | 逐节 peaking 级联；零增益节跳过 |
//! | [`GraphicEq::magnitude_response`] | 数字域 `|H(f)|`，由 SOS 系数直接求值 |
//!
//! # 实现要点
//!
//! 1. **可测的频响**：本模块的 [`GraphicEq::magnitude_response`] 是数字域精确响应，
//!    单测直接对它下断言，而不是靠「跑一段正弦看峰值」这种间接手段。
//! 2. **频点表的确定性与单调性**：频点在**对数域**插值生成，生成后还有一次单调性修复，
//!    保证严格单调且落在 `[20 Hz, 21000 Hz]`，不依赖任何多分支的幂次拟合。
//! 3. **增益钳位常量独立导出**（[`MAX_GAIN_DB`]），且 `set_gain` 返回实际生效值，
//!    调用方不必猜自己被钳到哪。
//! 4. **旁路是逐位恒等**：`bypass == true` 时 `process()` 直接返回、连一次乘加都不做，
//!    且 `magnitude_response()` 恒返回 1.0。
//! 5. **设参不爆音**：系数变更走[`crate::biquad::Section`]的交叉淡化（取舍 E）。

use crate::biquad::{Coeffs, Sos};
use core::f64::consts::PI;

/// 频点下限（Hz）。
pub const MIN_BAND_FREQ: f64 = 20.0;
/// 频点上限（Hz）。
pub const MAX_BAND_FREQ: f64 = 21_000.0;
/// 单段允许的最大提升/衰减（dB）。
pub const MAX_GAIN_DB: f64 = 20.0;
/// 默认段数。
pub const DEFAULT_BANDS: usize = 10;
/// 允许的最小 Q。
pub const MIN_Q: f64 = 0.5;
/// 允许的最大 Q。
pub const MAX_Q: f64 = 12.0;

/// 把频率收敛到合法范围。
fn clamp_freq(f: f64) -> f64 {
    if f.is_finite() {
        f.clamp(MIN_BAND_FREQ, MAX_BAND_FREQ)
    } else {
        MIN_BAND_FREQ
    }
}

/// 生成 `num_bands` 个对数等间距频点，落在 `[min_freq, max_freq]` 内且**严格单调递增**。
///
/// `num_bands == 1` 时返回几何中心 `sqrt(min*max)`。
#[must_use]
pub fn band_frequencies(num_bands: usize, min_freq: f64, max_freq: f64) -> Vec<f64> {
    let n = num_bands.max(1);
    let lo = clamp_freq(min_freq);
    let hi = clamp_freq(max_freq).max(lo);
    if n == 1 {
        return vec![(lo * hi).sqrt().clamp(MIN_BAND_FREQ, MAX_BAND_FREQ)];
    }
    if hi <= lo {
        // 退化：全部落在同一个频点上也必须保持「非递减」，调用方拿到的仍是可用网格
        return vec![lo; n];
    }
    let ln_lo = lo.ln();
    let ln_hi = hi.ln();
    let span = ln_hi - ln_lo;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / (n - 1) as f64;
        let f = if i == 0 {
            lo
        } else if i == n - 1 {
            hi
        } else {
            (ln_lo + span * t).exp()
        };
        out.push(f.clamp(MIN_BAND_FREQ, MAX_BAND_FREQ));
    }
    // 单调性修复：对数插值本身单调，这里只是防御 powf/exp 的舍入
    for i in 1..n {
        if out[i] <= out[i - 1] {
            let bumped = out[i - 1] * (1.0 + 1.0e-9);
            out[i] = bumped.min(MAX_BAND_FREQ).max(out[i - 1]);
        }
    }
    out
}

/// 由频点密度推出的每段 Q（段间交叠取 `Q = √r / (r − 1)`）。
///
/// `r` 是两个相邻频点的比值；再乘 `q_multiplier`、并夹到 `[MIN_Q, MAX_Q]`。
#[must_use]
pub fn band_q(num_bands: usize, min_freq: f64, max_freq: f64, q_multiplier: f64) -> f64 {
    let n = num_bands.max(1);
    let lo = clamp_freq(min_freq);
    let hi = clamp_freq(max_freq).max(lo);
    let mult = if q_multiplier.is_finite() && q_multiplier > 0.0 {
        q_multiplier.clamp(0.05, 20.0)
    } else {
        1.0
    };
    if n < 2 || hi <= lo {
        return mult.clamp(MIN_Q, MAX_Q);
    }
    let r = (hi / lo).powf(1.0 / (n as f64 - 1.0));
    let q = r.sqrt() / (r - 1.0);
    (q * mult).clamp(MIN_Q, MAX_Q)
}

/// 把任意段数的增益曲线重采样到 `dst_freqs`（对数频率域上的分段线性插值）。
///
/// 段数变化时按对数频率重采样增益曲线：视觉上的曲线形状被保留，
/// 而不是把增益数组截断或补零。
#[must_use]
pub fn resample_gains(src_freqs: &[f64], src_gains: &[f64], dst_freqs: &[f64]) -> Vec<f64> {
    let n = src_freqs.len().min(src_gains.len());
    if n == 0 {
        return vec![0.0; dst_freqs.len()];
    }
    if n == 1 {
        return vec![src_gains[0]; dst_freqs.len()];
    }
    let mut out = Vec::with_capacity(dst_freqs.len());
    for dst in dst_freqs {
        let x = dst.max(MIN_BAND_FREQ).ln();
        // 找到 src_freqs[i] <= dst < src_freqs[i+1]
        let mut seg = n - 2;
        for i in 0..n - 1 {
            if x < src_freqs[i + 1].ln() {
                seg = i;
                break;
            }
        }
        let (f0, f1) = (
            src_freqs[seg].max(MIN_BAND_FREQ),
            src_freqs[seg + 1].max(MIN_BAND_FREQ),
        );
        let (x0, x1) = (f0.ln(), f1.ln());
        let t = if x1 > x0 {
            ((x - x0) / (x1 - x0)).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let g0 = src_gains[seg];
        let g1 = src_gains[seg + 1];
        out.push(g0 + (g1 - g0) * t);
    }
    out
}

/// N 段图示均衡器。多声道交错（interleaved）处理，各声道有独立的滤波器状态。
#[derive(Clone, Debug)]
pub struct GraphicEq {
    fs: f64,
    channels: usize,
    min_freq: f64,
    max_freq: f64,
    freqs: Vec<f64>,
    gains_db: Vec<f64>,
    q_multiplier: f64,
    xfade_samples: u32,
    design: Vec<Coeffs>,
    /// 当前生效的滤波器级联（每声道一条 SOS 链）。
    filters: Vec<Sos>,
    /// 上一代级联：段数/网格变化时它继续跑，与新生代在 `fade_len` 个样本内交叉淡化。
    fading: Vec<Sos>,
    fade_len: u32,
    fade_left: u32,
    /// 系数已重算但还没写进滤波器——把同一批里的多次设参合并成一次交叉淡化。
    coeffs_pending: bool,
    /// 段数/网格变更待生效（新生代级联已预先分配好，等下一次 `process()` 开头换入）。
    grid_pending: bool,
    pending_filters: Vec<Sos>,
    bypassed: bool,
}

impl GraphicEq {
    /// 默认 10 段、`fs` 采样率、`channels` 声道、全 0 增益。
    #[must_use]
    pub fn new(fs: f64, channels: usize, num_bands: usize) -> Self {
        let fs = if fs.is_finite() && fs > 0.0 {
            fs
        } else {
            48_000.0
        };
        let channels = channels.min(64);
        let n = num_bands.max(1);
        let freqs = band_frequencies(n, MIN_BAND_FREQ, MAX_BAND_FREQ);
        let mut eq = Self {
            fs,
            channels,
            min_freq: MIN_BAND_FREQ,
            max_freq: MAX_BAND_FREQ,
            gains_db: vec![0.0; n],
            freqs,
            q_multiplier: 1.0,
            xfade_samples: 0,
            design: Vec::new(),
            filters: vec![Sos::new(n); channels],
            fading: Vec::new(),
            fade_len: 0,
            fade_left: 0,
            coeffs_pending: false,
            grid_pending: false,
            pending_filters: Vec::new(),
            bypassed: false,
        };
        eq.recalc_immediate();
        eq
    }

    /// 段数。
    #[must_use]
    pub fn num_bands(&self) -> usize {
        self.freqs.len()
    }

    /// 声道数。
    #[must_use]
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// 当前采样率。
    #[must_use]
    pub fn sample_rate(&self) -> f64 {
        self.fs
    }

    /// 各段中心频点。
    #[must_use]
    pub fn frequencies(&self) -> &[f64] {
        &self.freqs
    }

    /// 各段增益（dB）。
    #[must_use]
    pub fn gains(&self) -> &[f64] {
        &self.gains_db
    }

    /// 是否旁路。
    #[must_use]
    pub fn is_bypassed(&self) -> bool {
        self.bypassed
    }

    /// 是否完全透明（所有增益都是 0）。
    #[must_use]
    pub fn is_transparent(&self) -> bool {
        self.gains_db.iter().all(|g| *g == 0.0)
    }

    /// 是否正在做系数交叉淡化（含段数切换时的整段级联淡化）。
    #[must_use]
    pub fn is_smoothing(&self) -> bool {
        self.fade_left > 0
            || self.filters.first().is_some_and(|f| {
                (0..f.len()).any(|i| f.section(i).is_some_and(crate::biquad::Section::is_fading))
            })
    }

    /// 旁路开关。旁路时 `process()` 与 `magnitude_response()` 都是逐位恒等。
    pub fn set_bypass(&mut self, on: bool) {
        self.bypassed = on;
    }

    /// 系数交叉淡化的样本数（0 = 立即切换）。默认 0，实时链路上建议给 1~5 ms。
    pub fn set_smoothing_samples(&mut self, samples: u32) {
        self.xfade_samples = samples.min(48_000);
    }

    /// 换采样率：重算全部系数（走交叉淡化）。
    pub fn set_sample_rate(&mut self, fs: f64) {
        if fs.is_finite() && fs > 0.0 && (fs - self.fs).abs() > f64::EPSILON {
            self.fs = fs;
            self.recalc();
        }
    }

    /// 换声道数：重建滤波器状态（清空）。
    pub fn set_channels(&mut self, channels: usize) {
        let channels = channels.min(64);
        if channels != self.channels {
            self.channels = channels;
            self.filters = vec![Sos::new(self.freqs.len()); channels];
            self.fading = Vec::new();
            self.fade_len = 0;
            self.fade_left = 0;
            self.pending_filters = Vec::new();
            self.grid_pending = false;
            self.recalc_immediate();
        }
    }

    /// 换 Q 倍数。
    pub fn set_q_multiplier(&mut self, q_multiplier: f64) {
        if q_multiplier.is_finite() && (q_multiplier - self.q_multiplier).abs() > 1.0e-12 {
            self.q_multiplier = q_multiplier.clamp(0.05, 20.0);
            self.recalc();
        }
    }

    /// 当前 Q。
    #[must_use]
    pub fn q(&self) -> f64 {
        band_q(
            self.freqs.len(),
            self.min_freq,
            self.max_freq,
            self.q_multiplier,
        )
    }

    /// 改频点范围（重新生成网格，增益按对数频率重采样保留）。
    pub fn set_freq_range(&mut self, min_freq: f64, max_freq: f64) {
        let lo = clamp_freq(min_freq);
        let hi = clamp_freq(max_freq).max(lo);
        if (lo - self.min_freq).abs() < f64::EPSILON && (hi - self.max_freq).abs() < f64::EPSILON {
            return;
        }
        let old_freqs = self.freqs.clone();
        let old_gains = self.gains_db.clone();
        self.min_freq = lo;
        self.max_freq = hi;
        self.freqs = band_frequencies(self.freqs.len(), lo, hi);
        self.gains_db = resample_gains(&old_freqs, &old_gains, &self.freqs);
        self.recalc();
    }

    /// 改段数（增益曲线按对数频率重采样到新网格）。
    ///
    /// 级联**长度**变了，没法只换系数，所以这里改成**整段级联交叉淡化**：旧级联
    /// 连同它的全部状态继续跑，新级联从零状态淡入，`xfade_samples` 个样本后完成。
    /// 直接换一套 SOS 并清空状态是段数一切换就爆音的常见原因，这里改用交叉淡化。
    pub fn set_num_bands(&mut self, num_bands: usize) {
        let n = num_bands.clamp(1, 64);
        if n == self.freqs.len() {
            return;
        }
        let mut next = band_frequencies(n, self.min_freq, self.max_freq);
        if n == 1 {
            next = vec![(self.min_freq * self.max_freq).sqrt()];
        }
        let gains = resample_gains(&self.freqs, &self.gains_db, &next);
        self.freqs = next;
        self.gains_db = gains;
        self.rebuild();
        // 新生代在这里就分配好（设参路径，不在音频线程上做分配）
        let mut fresh = vec![Sos::new(n); self.channels];
        let design = core::mem::take(&mut self.design);
        for f in &mut fresh {
            f.set_coeffs(&design, 0);
        }
        self.design = design;
        self.pending_filters = fresh;
        self.grid_pending = true;
        self.coeffs_pending = false;
    }

    /// 改某一段的中心频点。
    pub fn set_band_freq(&mut self, band: usize, freq: f64) {
        if band >= self.freqs.len() {
            return;
        }
        let f = clamp_freq(freq);
        if (f - self.freqs[band]).abs() < f64::EPSILON {
            return;
        }
        self.freqs[band] = f;
        self.recalc();
    }

    /// 设某一段增益，返回**实际生效**的 dB 值（已被 [`MAX_GAIN_DB`] 钳位）。
    pub fn set_gain(&mut self, band: usize, gain_db: f64) -> f64 {
        let g = if gain_db.is_finite() {
            gain_db.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
        } else {
            0.0
        };
        if let Some(slot) = self.gains_db.get_mut(band) {
            *slot = g;
            self.recalc();
        }
        g
    }

    /// 一次性设置全部增益。
    ///
    /// 长度与段数一致时逐段赋值；不一致时按「对数频率分段线性」重采样
    /// （band 数不匹配时按对数频率插值保形）。
    pub fn set_gains(&mut self, gains: &[f64]) {
        if gains.is_empty() {
            return;
        }
        let src: Vec<f64> = gains
            .iter()
            .map(|g| {
                if g.is_finite() {
                    g.clamp(-MAX_GAIN_DB, MAX_GAIN_DB)
                } else {
                    0.0
                }
            })
            .collect();
        if src.len() == self.freqs.len() {
            self.gains_db = src;
        } else {
            let src_freqs = band_frequencies(src.len(), self.min_freq, self.max_freq);
            self.gains_db = resample_gains(&src_freqs, &src, &self.freqs);
        }
        self.recalc();
    }

    /// 全部增益归零。
    pub fn clear(&mut self) {
        if self.is_transparent() {
            return;
        }
        for g in &mut self.gains_db {
            *g = 0.0;
        }
        self.recalc();
    }

    /// 清零所有滤波器状态（断流/重新起播时调用，避免残留尾巴）。
    pub fn reset(&mut self) {
        for f in &mut self.filters {
            f.reset();
        }
        self.fading.clear();
        self.fade_len = 0;
        self.fade_left = 0;
        self.coeffs_pending = false;
        self.pending_filters.clear();
        self.grid_pending = false;
    }

    /// 全链路的数字域模响应 `|H(e^{jw})|`（线性，不是 dB）。
    ///
    /// 旁路时恒为 1.0。`freq <= 0` 返回直流增益；`freq >= fs/2` 按 Nyquist 处理。
    #[must_use]
    pub fn magnitude_response(&self, freq: f64) -> f64 {
        if self.bypassed {
            return 1.0;
        }
        if freq <= 0.0 {
            return self.design.iter().map(Coeffs::gain_at_zero).product();
        }
        let nyquist = self.fs * 0.5;
        let f = if freq.is_finite() {
            freq.min(nyquist * 0.999_999)
        } else {
            1_000.0
        };
        let w = (PI * 2.0 * f / self.fs).clamp(1.0e-12, PI);
        self.design.iter().map(|c| c.magnitude(w)).product()
    }

    /// dB 表示的模响应。
    #[must_use]
    pub fn magnitude_response_db(&self, freq: f64) -> f64 {
        let m = self.magnitude_response(freq);
        if m > 0.0 {
            20.0 * m.log10()
        } else {
            f64::NEG_INFINITY
        }
    }

    /// 原地处理交错多声道缓冲（实时路径：零分配、无锁、无 panic 分支）。
    ///
    /// 待生效的设参在这里**一次性合并应用**：同一批（两次 `process` 之间）里的多次
    /// `set_gain` / `set_q_multiplier` / `set_gains` 只会触发一次交叉淡化。
    pub fn process(&mut self, buf: &mut [f32]) {
        if self.bypassed || self.channels == 0 || buf.is_empty() {
            return;
        }
        self.flush_pending();
        let nch = self.channels;
        let mut fading = self.fade_left > 0 && self.fading.len() == nch;
        for frame in buf.chunks_mut(nch) {
            let t = if fading {
                1.0 - f64::from(self.fade_left) / f64::from(self.fade_len.max(1))
            } else {
                1.0
            };
            for (ch, sample) in frame.iter_mut().enumerate() {
                let x = f64::from(*sample);
                let y = match self.filters.get_mut(ch) {
                    Some(f) => f.process(x),
                    None => x,
                };
                let y = if fading {
                    let y_old = match self.fading.get_mut(ch) {
                        Some(f) => f.process(x),
                        None => x,
                    };
                    y_old + (y - y_old) * t
                } else {
                    y
                };
                *sample = if y.is_finite() {
                    y.clamp(-1.0e6, 1.0e6) as f32
                } else {
                    0.0
                };
            }
            if fading {
                self.fade_left -= 1;
                if self.fade_left == 0 {
                    fading = false;
                }
            }
        }
    }

    /// 应用待生效的设参（把同一批内的多次改动合并掉）。
    #[inline]
    fn flush_pending(&mut self) {
        if self.grid_pending {
            self.grid_pending = false;
            let next = core::mem::take(&mut self.pending_filters);
            // 旧级联整体留下来继续跑；`mem::replace` 只搬移缓冲区，不新增分配
            self.fading = core::mem::replace(&mut self.filters, next);
            self.fade_len = self.xfade_samples;
            self.fade_left = self.fade_len;
            self.coeffs_pending = false;
            return;
        }
        if self.coeffs_pending {
            self.coeffs_pending = false;
            let xf = self.xfade_samples;
            // `mem::take` 只搬移（不分配），避免同时可变借用 `design` 与 `filters`
            let design = core::mem::take(&mut self.design);
            for f in &mut self.filters {
                f.set_coeffs(&design, xf);
            }
            self.design = design;
        }
    }

    /// 重算系数：只更新设计（`magnitude_response` 立刻能看到新曲线），真正写进滤波器
    /// 的动作推迟到下一次 `process()`——这样同一批里的多次设参只触发一次交叉淡化。
    fn recalc(&mut self) {
        self.rebuild();
        self.coeffs_pending = true;
    }

    /// 重算系数：立即生效（构造/换声道时用）。
    fn recalc_immediate(&mut self) {
        self.rebuild();
        let design = core::mem::take(&mut self.design);
        for f in &mut self.filters {
            f.set_coeffs(&design, 0);
        }
        self.design = design;
        self.coeffs_pending = false;
        self.grid_pending = false;
    }

    fn rebuild(&mut self) {
        let q = self.q();
        let fs = self.fs;
        let mut design = Vec::with_capacity(self.freqs.len());
        for (i, f) in self.freqs.iter().enumerate() {
            let g = self.gains_db.get(i).copied().unwrap_or(0.0);
            // 增益为 0 的段直接用单位系数：既省算力，又让「全 0 增益」与旁路在数值上等价
            design.push(if g == 0.0 {
                Coeffs::IDENTITY
            } else {
                Coeffs::peaking(*f, g, q, fs)
            });
        }
        self.design = design;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_grid_is_strictly_monotonic_and_in_range() {
        for n in [1usize, 2, 3, 5, 10, 15, 20, 31, 40] {
            let f = band_frequencies(n, MIN_BAND_FREQ, MAX_BAND_FREQ);
            assert_eq!(f.len(), n);
            for (i, v) in f.iter().enumerate() {
                assert!(v.is_finite());
                assert!(
                    *v >= MIN_BAND_FREQ - 1e-9 && *v <= MAX_BAND_FREQ + 1e-9,
                    "n={n} f={v}"
                );
                if i > 0 {
                    assert!(f[i] > f[i - 1], "n={n} 频点非严格单调: {f:?}");
                }
            }
        }
    }

    #[test]
    fn grid_is_log_spaced() {
        let (lo, hi) = (MIN_BAND_FREQ, MAX_BAND_FREQ);
        let f = band_frequencies(10, lo, hi);
        for (i, v) in f.iter().enumerate() {
            let expect = (lo.ln() + (hi.ln() - lo.ln()) * i as f64 / 9.0).exp();
            assert!((v / expect - 1.0).abs() < 1e-12, "i={i} {v} vs {expect}");
        }
        // 奇数段时正中间那一段就是几何中心
        let f9 = band_frequencies(9, lo, hi);
        assert!((f9[4] / (lo * hi).sqrt() - 1.0).abs() < 1e-12);
    }

    #[test]
    fn q_follows_band_density() {
        let q5 = band_q(5, MIN_BAND_FREQ, MAX_BAND_FREQ, 1.0);
        let q10 = band_q(10, MIN_BAND_FREQ, MAX_BAND_FREQ, 1.0);
        let q31 = band_q(31, MIN_BAND_FREQ, MAX_BAND_FREQ, 1.0);
        assert!(q5 < q10 && q10 < q31, "{q5} {q10} {q31}");
        assert!((band_q(10, MIN_BAND_FREQ, MAX_BAND_FREQ, 2.0) - 2.0 * q10).abs() < 1e-9);
    }

    #[test]
    fn bypass_is_bit_exact() {
        let mut eq = GraphicEq::new(48_000.0, 2, 10);
        eq.set_gain(3, 9.0);
        eq.set_bypass(true);
        let mut buf: Vec<f32> = (0..1024).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        let before = buf.clone();
        eq.process(&mut buf);
        assert_eq!(buf, before);
        assert_eq!(eq.magnitude_response(1000.0), 1.0);
    }

    #[test]
    fn all_zero_gains_equal_bypass() {
        let mut eq = GraphicEq::new(48_000.0, 2, 10);
        let mut buf: Vec<f32> = (0..4096)
            .map(|i| ((i as f32) * 0.037).sin() * 0.5)
            .collect();
        let before = buf.clone();
        eq.process(&mut buf);
        let max_err = buf
            .iter()
            .zip(before.iter())
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(max_err < 1e-6, "全 0 增益与旁路的偏差 {max_err}");
        assert!(eq.is_transparent());
    }

    #[test]
    fn mismatched_band_count_is_resampled_not_truncated() {
        let mut eq = GraphicEq::new(48_000.0, 1, 10);
        eq.set_gains(&[12.0; 31]);
        assert_eq!(eq.gains().len(), 10);
        assert!(eq.gains().iter().all(|g| *g > 11.0));
    }
}
