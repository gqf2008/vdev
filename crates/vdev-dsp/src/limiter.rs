//! 前瞻式真峰值限幅器 + 4x 过采样真峰值测量（改进点 D）。
//!
//! # 算法依据
//!
//! | 本模块 | 依据 |
//! | --- | --- |
//! | [`Limiter`] | 经典 look-ahead peak limiter 结构（前瞻 + 滑动最小值 + 平滑释放） |
//! | [`true_peak_4x`] | ITU-R BS.1770 附录 2 的过采样真峰值（true peak）测量思路 |
//!
//! # 设计取舍（D）
//!
//! 「只做事后判定」的限幅是：先按电平算一个**已经迟了**的增益，再看输出有没有撞到 ceiling，
//! 撞了就在**几十秒的量级**上慢慢把目标压低。瞬时峰值越界时**根本没有保护**。
//! 本模块是真正的限幅器：
//!
//! 1. **前瞻**（默认 2 ms）：输入先入延迟线，增益由「还没输出的那一段」决定，
//!    因此峰值到达输出之前增益**已经开始下降**；
//! 2. **侧链测的是真峰值、不是离散样点**：用 4x 插值核（Hann 加窗 sinc，核半径
//!    `DET_R = 8` —— `DET_R` 是插值核在中心帧**每侧**覆盖的原始样本数）估计采样间峰值，
//!    **并且把中心帧自己的离散样点也算进去**——两条合起来保证 `峰值估计 ≥ |x[k]|`，
//!    于是增益在峰值到达输出**之前**就已压到位（这是「末端 clamp 很少咬合」的实现路径）；
//! 3. **下降是斜率的、不是跳变**：目标增益取
//!    `target[k] = min_{j∈[k, k+H]} ( g_req[j] + (j-k)/H )`，`H = 前瞻 − 核半径`。
//!    `j = k` 那一项就是 `g_req[k]`，所以不越界；而 `(j-k)/H` 这一项让增益**最多
//!    每帧下降 `1/H`**——大峰进来时从「进入前瞻窗口」到「峰值到达输出」正好 H 帧，
//!    增益以受限斜率线性压下去，而不是瞬间跳一个台阶（那是爆音的来源）；
//! 4. **上升是一阶平滑释放**（默认 60 ms），不会出现泵吸感。
//!
//! **「逐样点严格不越界」的严格性来自输出端的兜底夹取，不是侧链**：主路输出一律过一遍
//! `.clamp(-limit, limit)`（`limit = 离散 ceiling`），所以 `|out[k]| ≤ 离散 ceiling`
//! 是**无条件**成立的；真峰值侧链 + 受限斜率下降的作用是**提前把增益压下去、让 clamp
//! 尽量不咬合**（咬合越少失真越小，采样间峰值也基本不越界）。
//!
//! 侧链插值比主路晚 `DET_R` 帧，因此要求前瞻 ≥ `DET_R` 帧（2 ms @48k = 96 帧，
//! 远远够）；前瞻设为 0 时自动退回「只保证离散样点不越界」的模式（无斜坡，仅用于对照）。
//!
//! 另外，[`true_peak_oversampled`] 是**独立于侧链**的离线真峰值测量工具：它也能给出
//! 「这个缓冲区到底有没有越界」的第三方结论，验收测试用的就是它。

use core::f64::consts::PI;

/// 侧链真峰值插值核每侧覆盖的原始样本数。
///
/// **故意与离线工具 [`TP_R`] 取同一个半径、同一个窗**：侧链估的就是离线工具那套量，
/// 于是「限幅器认为不越界」和「验收工具量出来不越界」说的是同一件事，不会各说各话。
const DET_R: usize = 8;
/// 侧链真峰值过采样倍率。
const DET_PHASES: usize = 4;
/// 离线真峰值工具的插值核半径（比侧链更大，作为独立参照）。
const TP_R: i64 = 8;

/// 把 f64 收敛成有限 f32（越界/NaN 一律归零，绝不让脏数据扩散）。
#[inline]
fn sanitize32(v: f64) -> f32 {
    if v.is_finite() {
        v.clamp(-1.0e6, 1.0e6) as f32
    } else {
        0.0
    }
}

fn sine_cardinal(u: f64) -> f64 {
    if u == 0.0 {
        1.0
    } else {
        let x = PI * u;
        x.sin() / x
    }
}

/// 逐相位归一化的 Hann 窗 sinc 插值核。
///
/// 传输函数是 `Σ_j h[j]·z^-j`；把每个相位的核归一化到**直流增益 = 1**，这样常量输入
/// 插值后仍是同一个常量（不归一化的话，截断窗会让直流被放大 13%，实测 1.0 → 1.1307）。
#[must_use]
fn build_kernel(radius: i64, phases: usize) -> (Vec<f64>, usize) {
    let taps = (2 * radius + 1) as usize;
    let mut kernel = vec![0.0_f64; phases * taps];
    for p in 0..phases {
        let mut sum = 0.0;
        for (j, slot) in kernel[p * taps..(p + 1) * taps].iter_mut().enumerate() {
            let off = j as f64 - radius as f64 + p as f64 / phases as f64;
            let h = if off.abs() < radius as f64 {
                sine_cardinal(off) * 0.5 * (1.0 + (PI * off / radius as f64).cos())
            } else {
                0.0
            };
            *slot = h;
            sum += h;
        }
        if sum.abs() > 1.0e-12 {
            for v in &mut kernel[p * taps..(p + 1) * taps] {
                *v /= sum;
            }
        }
    }
    (kernel, taps)
}

/// 用 `oversample` 倍过采样的方式测量真峰值（intersample peak）。
///
/// 做法是「理想插值核」的有限长近似：`y[i + p/l] = Σ_j h_p[j]·x[i + j − R]`，
/// 其中 `h_p` 是逐相位归一化到直流增益 1 的 Hann 窗 sinc。相位 0（`p = 0`）就是原始
/// 样本本身，所以返回的峰值**一定 ≥ 离散样本峰值**，`p = 1..l-1` 给出采样间的中间值。
///
/// 这是**离线验证工具**（复杂度 `O(N · oversample · taps)`），不要放进实时路径。
///
/// 精度自查（单测里有）：直流 → 1.0；`fs/4` 上被采样点错过的满幅正弦 → 1.0；
/// `997 Hz / 0.9` → 0.9。
#[must_use]
pub fn true_peak_oversampled(samples: &[f32], oversample: usize) -> f64 {
    let n = samples.len();
    if n == 0 {
        return 0.0;
    }
    let l = oversample.clamp(1, 16);
    let mut peak = 0.0_f64;
    for s in samples {
        let a = f64::from(*s).abs();
        if a.is_finite() && a > peak {
            peak = a;
        }
    }
    if l == 1 {
        return peak;
    }
    let (kernel, taps) = build_kernel(TP_R, l);
    let r = TP_R as usize;
    for p in 1..l {
        // 只测「插值窗口完整」的样点：窗口被边界截断时，sinc 的负旁瓣会让插值
        // 凭空过冲（直流信号在边界会被量成 1.13），那不是信号的真峰值。
        for i in r..n.saturating_sub(r) {
            let mut acc = 0.0;
            for j in 0..taps {
                acc += kernel[p * taps + j] * f64::from(samples[i + j - r]);
            }
            let a = acc.abs();
            if a.is_finite() && a > peak {
                peak = a;
            }
        }
    }
    peak
}

/// 4x 过采样的真峰值（默认验证手段）。
#[must_use]
pub fn true_peak_4x(samples: &[f32]) -> f64 {
    true_peak_oversampled(samples, 4)
}

/// 一帧里离散样本的绝对值峰值。
#[inline]
fn discrete_peak(frame: &[f32]) -> f64 {
    let mut peak = 0.0_f64;
    for s in frame {
        let a = f64::from(*s).abs();
        if a.is_finite() && a > peak {
            peak = a;
        }
    }
    peak
}

/// 侧链 4x 真峰值检测器（多声道，逐帧喂入）。
///
/// 契约：`push(frame_n)` 返回 **`DET_R` 帧之前那一帧**（记作中心帧）邻域的真峰值估计，
/// 且**恒有 `估计 ≥ |x[中心帧]|`**（中心帧的离散样点被显式计入）。
#[derive(Clone, Debug)]
struct TruePeakDetector {
    kernel: Vec<f64>,
    hist: Vec<f64>,
    taps: usize,
    pos: usize,
    frames: u64,
    channels: usize,
}

impl TruePeakDetector {
    fn new(channels: usize) -> Self {
        let (kernel, taps) = build_kernel(DET_R as i64, DET_PHASES);
        let ch = channels.max(1);
        Self {
            kernel,
            hist: vec![0.0; taps * ch],
            taps,
            pos: 0,
            frames: 0,
            channels: ch,
        }
    }

    fn set_channels(&mut self, channels: usize) {
        let ch = channels.max(1);
        if ch != self.channels {
            self.channels = ch;
            self.hist = vec![0.0; self.taps * ch];
            self.reset();
        }
    }

    fn reset(&mut self) {
        for v in &mut self.hist {
            *v = 0.0;
        }
        self.pos = 0;
        self.frames = 0;
    }

    /// 推入一帧，返回中心帧邻域的真峰值估计。
    ///
    /// 前 `2·DET_R` 帧邻域还不完整，返回 0（= 不削减增益，保守）。
    fn push(&mut self, frame: &[f32]) -> f64 {
        let nch = self.channels;
        let taps = self.taps;
        let pos = self.pos;
        for (ch, s) in frame.iter().enumerate() {
            let v = f64::from(*s);
            if let Some(slot) = self.hist.get_mut(pos * nch + ch) {
                *slot = if v.is_finite() { v } else { 0.0 };
            }
        }
        self.pos = (pos + 1) % taps;
        self.frames += 1;
        let frames = self.frames;
        if frames < 2 * DET_R as u64 + 1 {
            return 0.0;
        }
        // 环形缓冲里最老的一帧（0 基帧号），窗口 = [first, first + taps - 1]
        let first = frames - 1 - (2 * DET_R) as u64;
        let center_slot = ((first + DET_R as u64) as usize) % taps;
        let mut peak = 0.0_f64;
        for ch in 0..nch {
            // 相位 0：中心帧自己的离散样点（插值核的 0 相位就是它，这里显式取）
            let v = self
                .hist
                .get(center_slot * nch + ch)
                .copied()
                .unwrap_or(0.0)
                .abs();
            if v > peak {
                peak = v;
            }
            for p in 1..DET_PHASES {
                let mut acc = 0.0;
                for j in 0..taps {
                    let slot = ((first + j as u64) as usize) % taps;
                    acc += self.kernel[p * taps + j]
                        * self.hist.get(slot * nch + ch).copied().unwrap_or(0.0);
                }
                let a = acc.abs();
                if a.is_finite() && a > peak {
                    peak = a;
                }
            }
        }
        peak
    }
}

/// 前瞻真峰值限幅器。
///
/// 增益对**所有声道联动**（改进点 C），因此限幅不会改变立体声像。
#[derive(Clone, Debug)]
pub struct Limiter {
    fs: f64,
    channels: usize,
    ceiling_db: f64,
    margin_db: f64,
    enabled: bool,
    lookahead: usize,
    delay: Vec<f64>,
    pos: usize,
    dq_idx: Vec<u64>,
    dq_val: Vec<f64>,
    dq_head: usize,
    dq_len: usize,
    gain: f64,
    release_ms: f64,
    release_coef: f64,
    n: u64,
    detector: TruePeakDetector,
}

impl Limiter {
    /// 默认：ceiling = −1 dBFS、前瞻 2 ms、释放 60 ms、真峰值余量 0.3 dB。
    #[must_use]
    pub fn new(fs: f64, channels: usize) -> Self {
        let fs = if fs.is_finite() && fs > 0.0 {
            fs
        } else {
            48_000.0
        };
        let ch = channels.clamp(1, 64);
        let mut l = Self {
            fs,
            channels: ch,
            ceiling_db: -1.0,
            margin_db: 0.3,
            enabled: true,
            lookahead: 0,
            delay: Vec::new(),
            pos: 0,
            dq_idx: Vec::new(),
            dq_val: Vec::new(),
            dq_head: 0,
            dq_len: 0,
            gain: 1.0,
            release_ms: 60.0,
            release_coef: 0.0,
            n: 0,
            detector: TruePeakDetector::new(ch),
        };
        l.release_coef = l.coef_for_ms(60.0);
        l.lookahead = ((2.0e-3 * fs).round() as usize).clamp(1, 4096);
        l.realloc();
        l
    }

    fn coef_for_ms(&self, ms: f64) -> f64 {
        let t = (ms.max(0.01) * 1.0e-3 * self.fs).max(1.0);
        1.0 - (-1.0 / t).exp()
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

    /// 是否启用。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 启用/旁路（旁路时 `process()` 直接返回，逐位恒等）。
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
        if !on {
            self.gain = 1.0;
        }
    }

    /// 真峰值上限（dBFS）。
    pub fn set_ceiling_db(&mut self, db: f64) {
        if db.is_finite() {
            self.ceiling_db = db.clamp(-60.0, 6.0);
        }
    }

    /// 真峰值上限（dBFS）。
    #[must_use]
    pub fn ceiling_db(&self) -> f64 {
        self.ceiling_db
    }

    /// 离散样本上的额外余量（dB）：真正夹的点 = ceiling − margin。
    ///
    /// 侧链估计的是**采样间**峰值，而 [`true_peak_4x`] 用的是另一套（半径更大的）核，
    /// 两者会有零点几个 dB 的分歧；这段余量就是留给这个分歧的。
    pub fn set_true_peak_margin_db(&mut self, db: f64) {
        if db.is_finite() {
            self.margin_db = db.clamp(0.0, 6.0);
        }
    }

    /// 离散样本余量（dB）。
    #[must_use]
    pub fn true_peak_margin_db(&self) -> f64 {
        self.margin_db
    }

    /// 离散样本上限（线性）——侧链实际夹的那个点。
    #[must_use]
    pub fn discrete_ceiling(&self) -> f64 {
        10f64.powf((self.ceiling_db - self.margin_db) / 20.0)
    }

    /// 真峰值上限（线性）。
    #[must_use]
    pub fn ceiling_linear(&self) -> f64 {
        10f64.powf(self.ceiling_db / 20.0)
    }

    /// 起始延迟（样本数）= 前瞻长度。
    #[must_use]
    pub fn lookahead_samples(&self) -> usize {
        self.lookahead
    }

    /// 侧链是否在做真峰值检测（前瞻 ≥ 插值核半径时才可能）。
    #[must_use]
    pub fn is_true_peak_detection_active(&self) -> bool {
        self.lookahead >= DET_R
    }

    /// 下降斜坡长度（帧）。这是「增益每帧最多下降 `1/ramp_frames`」里的 H。
    #[must_use]
    pub fn ramp_frames(&self) -> usize {
        if self.is_true_peak_detection_active() {
            self.lookahead - DET_R
        } else {
            self.lookahead
        }
    }

    /// 当前增益（线性，≤ 1）。
    #[must_use]
    pub fn gain(&self) -> f64 {
        self.gain
    }

    /// 当前增益（dB，≤ 0）。
    #[must_use]
    pub fn gain_db(&self) -> f64 {
        if self.gain > 0.0 {
            20.0 * self.gain.log10()
        } else {
            f64::NEG_INFINITY
        }
    }

    /// 前瞻时间（ms，0 = 关闭前瞻）。
    pub fn set_lookahead_ms(&mut self, ms: f64) {
        let s = if ms.is_finite() {
            ms.clamp(0.0, 50.0)
        } else {
            2.0
        };
        let lookahead = ((s * 1.0e-3 * self.fs).round() as usize).min(4096);
        if lookahead != self.lookahead {
            self.lookahead = lookahead;
            self.realloc();
        }
    }

    /// 释放时间（ms）。
    pub fn set_release_ms(&mut self, ms: f64) {
        let s = if ms.is_finite() {
            ms.clamp(1.0, 5000.0)
        } else {
            60.0
        };
        self.release_ms = s;
        self.release_coef = self.coef_for_ms(s);
    }

    /// 释放时间（ms）。
    #[must_use]
    pub fn release_ms(&self) -> f64 {
        self.release_ms
    }

    /// 换采样率（前瞻时间保持不变）。
    pub fn set_sample_rate(&mut self, fs: f64) {
        if fs.is_finite() && fs > 0.0 && (fs - self.fs).abs() > f64::EPSILON {
            let ms = self.release_ms;
            let la_ms = self.lookahead as f64 / self.fs * 1.0e3;
            self.fs = fs;
            self.release_coef = self.coef_for_ms(ms);
            self.lookahead = ((la_ms * 1.0e-3 * fs).round() as usize).min(4096);
            self.realloc();
        }
    }

    /// 换声道数（重建延迟线，状态清零）。
    pub fn set_channels(&mut self, channels: usize) {
        let ch = channels.clamp(1, 64);
        if ch != self.channels {
            self.channels = ch;
            self.detector.set_channels(ch);
            self.realloc();
        }
    }

    fn realloc(&mut self) {
        if self.lookahead == 0 {
            self.delay = Vec::new();
        } else {
            self.delay = vec![0.0; self.lookahead * self.channels];
        }
        // 单调双端队列：窗口最小值的摊销 O(1) 实现，容量 = 窗口长度 + 2
        let cap = self.lookahead + 2;
        self.dq_idx = vec![0; cap];
        self.dq_val = vec![0.0; cap];
        self.dq_head = 0;
        self.dq_len = 0;
        self.pos = 0;
        self.n = 0;
        self.gain = 1.0;
        self.detector.reset();
    }

    /// 清状态。
    pub fn reset(&mut self) {
        for v in &mut self.delay {
            *v = 0.0;
        }
        self.pos = 0;
        self.n = 0;
        self.dq_head = 0;
        self.dq_len = 0;
        self.gain = 1.0;
        self.detector.reset();
    }

    fn dq_push(&mut self, idx: u64, v: f64) {
        if self.dq_val.is_empty() {
            return;
        }
        let cap = self.dq_val.len();
        while self.dq_len > 0 {
            let back = (self.dq_head + self.dq_len - 1) % cap;
            if self.dq_val.get(back).copied().unwrap_or(0.0) >= v {
                self.dq_len -= 1;
            } else {
                break;
            }
        }
        if self.dq_len == cap {
            self.dq_head = (self.dq_head + 1) % cap;
            self.dq_len -= 1;
        }
        let at = (self.dq_head + self.dq_len) % cap;
        if let Some(slot) = self.dq_idx.get_mut(at) {
            *slot = idx;
        }
        if let Some(slot) = self.dq_val.get_mut(at) {
            *slot = v;
        }
        self.dq_len += 1;
    }

    fn dq_evict(&mut self, keep_from: u64) {
        let cap = self.dq_val.len();
        if cap == 0 {
            return;
        }
        while self.dq_len > 0 {
            let f = self.dq_idx.get(self.dq_head).copied().unwrap_or(u64::MAX);
            if f < keep_from {
                self.dq_head = (self.dq_head + 1) % cap;
                self.dq_len -= 1;
            } else {
                break;
            }
        }
    }

    fn dq_min(&self) -> f64 {
        self.dq_val.get(self.dq_head).copied().unwrap_or(1.0)
    }

    /// 原地限幅。输出相对输入有 `lookahead_samples()` 的延迟。
    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.enabled || self.channels == 0 || buf.is_empty() {
            return;
        }
        let nch = self.channels;
        let limit = self.discrete_ceiling();
        let tp_mode = self.is_true_peak_detection_active();
        let horizon = self.ramp_frames();
        let hf = horizon.max(1) as f64;
        for frame in buf.chunks_mut(nch) {
            self.n = self.n.saturating_add(1);
            let n = self.n; // 1 基帧号

            // 1) 侧链：(索引, 该索引处的峰值估计)
            let (idx, pk) = if tp_mode {
                (
                    n.saturating_sub(1 + DET_R as u64),
                    self.detector.push(frame),
                )
            } else {
                (n.saturating_sub(1), discrete_peak(frame))
            };
            let g_req = if pk > limit && pk > 0.0 {
                limit / pk
            } else {
                1.0
            };

            // 2) 滑动最小值：W[j] = g_req[j]·H + j，于是
            //    min_{j∈[k,k+H]} W[j] − k = H·min(g_req[j] + (j−k)/H)
            self.dq_push(idx, g_req * hf + idx as f64);
            let k = n.saturating_sub(1 + self.lookahead as u64); // 当前真正要输出的帧号
            self.dq_evict(k);
            let target = ((self.dq_min() - k as f64) / hf).clamp(0.0, 1.0);

            // 3) 下降：严格跟随目标（目标本身已按 1/H 的斜率限制过）；
            //    上升：一阶平滑释放，且不得越过目标（否则会越界）
            if target < self.gain {
                self.gain = target;
            } else {
                self.gain += (target - self.gain) * self.release_coef;
                if self.gain > target {
                    self.gain = target;
                }
            }

            // 4) 延迟线：写新样本、读 `lookahead` 帧之前的样本
            if self.lookahead > 0 && self.delay.len() >= nch * self.lookahead {
                let base = self.pos * nch;
                for (ch, s) in frame.iter_mut().enumerate() {
                    let x = f64::from(*s);
                    let at = base + ch;
                    let d = self.delay.get(at).copied().unwrap_or(0.0);
                    if let Some(slot) = self.delay.get_mut(at) {
                        *slot = if x.is_finite() { x } else { 0.0 };
                    }
                    *s = sanitize32((d * self.gain).clamp(-limit, limit));
                }
                self.pos += 1;
                if self.pos >= self.lookahead {
                    self.pos = 0;
                }
            } else {
                // 无前瞻：只能夹当前帧，不保证采样间峰值
                for s in frame.iter_mut() {
                    *s = sanitize32((f64::from(*s) * self.gain).clamp(-limit, limit));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(n: usize, amp: f32, seed: u32) -> Vec<f32> {
        let mut x = if seed == 0 { 0x2468_ACE1 } else { seed };
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let u = (x >> 8) as f32 / 8_388_608.0;
                (u - 1.0) * amp
            })
            .collect()
    }

    #[test]
    fn true_peak_of_dc_is_unity() {
        // 不归一化插值核时这里会给出 1.13 —— 归一化之后必须严格是 1
        let dc = vec![1.0_f32; 512];
        assert!((true_peak_4x(&dc) - 1.0).abs() < 1e-9);
        let dc2 = vec![0.25_f32; 512];
        assert!((true_peak_4x(&dc2) - 0.25).abs() < 1e-9);
    }

    #[test]
    fn true_peak_resolves_fs_over_four_peak() {
        // fs/4 的正弦，采样相位偏 45°：离散峰值只有 0.7071，真峰值是 1.0。
        // 这是一个**有解析解**的采样间过冲场景。
        let fs = 48_000.0;
        let sig: Vec<f32> = (0..512)
            .map(|i| {
                let w = 2.0 * PI * (fs / 4.0) * i as f64 / fs;
                (w + PI / 4.0).sin() as f32
            })
            .collect();
        let disc = discrete_peak(&sig);
        let tp = true_peak_4x(&sig);
        assert!(
            (disc - core::f64::consts::FRAC_1_SQRT_2).abs() < 1e-6,
            "离散峰值 {disc}"
        );
        assert!((tp - 1.0).abs() < 5e-3, "真峰值 {tp} 期望 1.0");
    }

    #[test]
    fn true_peak_of_sine_matches_amplitude() {
        let fs = 48_000.0;
        let amp = 0.9;
        let sig: Vec<f32> = (0..48_000)
            .map(|i| (amp * (2.0 * PI * 997.0 * i as f64 / fs).sin()) as f32)
            .collect();
        let tp = true_peak_4x(&sig);
        assert!((tp - amp).abs() < 2.0e-3, "真峰值 {tp} 期望 {amp}");
    }

    #[test]
    fn limiter_never_exceeds_ceiling_on_samples() {
        let fs = 48_000.0;
        let mut lim = Limiter::new(fs, 1);
        lim.set_lookahead_ms(2.0);
        let ceiling = lim.discrete_ceiling();
        let mut sig: Vec<f32> = (0..48_000)
            .map(|i| {
                let t = i as f64 / fs;
                (0.95 * (2.0 * PI * 220.0 * t).sin() + 0.5 * (2.0 * PI * 3000.0 * t).sin()) as f32
            })
            .collect();
        lim.process(&mut sig);
        // 4800 帧之后延迟线已充满，前面避不开起播瞬态
        for (i, s) in sig.iter().enumerate().skip(4800) {
            let v = f64::from(*s).abs();
            // 余量 1e-6：输出存的是 f32，0.86 附近的 f32 量化步长约 6e-8
            assert!(v <= ceiling + 1.0e-6, "样本 {i} 越界 {s}（上限 {ceiling}）");
        }
        assert!(lim.gain() < 0.99);
    }

    #[test]
    fn true_peak_of_noise_stays_below_ceiling() {
        let mut lim = Limiter::new(48_000.0, 1);
        let ceiling = lim.ceiling_linear();
        let mut sig = noise(48_000, 0.95, 0x5EED);
        lim.process(&mut sig);
        let tail = &sig[4800..];
        let disc = discrete_peak(tail);
        let tp = true_peak_4x(tail);
        assert!(disc <= lim.discrete_ceiling() + 1e-6, "离散峰值 {disc}");
        assert!(tp <= ceiling, "白噪声真峰值 {tp} > ceiling {ceiling}");
    }

    #[test]
    fn true_peak_of_fs_over_four_stays_below_ceiling() {
        // 采样点刚好错过峰值的 fs/4 正弦：离散限幅完全挡不住，必须靠真峰值侧链
        let mut lim = Limiter::new(48_000.0, 1);
        let ceiling = lim.ceiling_linear();
        let mut sig: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * PI * (48_000.0 / 4.0) * i as f64 / 48_000.0 + PI / 4.0).sin() as f32)
            .collect();
        lim.process(&mut sig);
        let tail = &sig[4800..];
        let tp = true_peak_4x(tail);
        assert!(tp <= ceiling, "fs/4 正弦真峰值 {tp} > ceiling {ceiling}");
    }

    #[test]
    fn lookahead_zero_still_bounded() {
        let mut lim = Limiter::new(48_000.0, 1);
        lim.set_lookahead_ms(0.0);
        assert!(!lim.is_true_peak_detection_active());
        let ceiling = lim.discrete_ceiling();
        let mut sig: Vec<f32> = (0..4800)
            .map(|i| if i == 1000 { 4.0 } else { 0.1 })
            .collect();
        lim.process(&mut sig);
        assert!(sig.iter().all(|s| f64::from(*s).abs() <= ceiling + 1e-6));
    }

    #[test]
    fn gain_is_linked_across_channels() {
        let mut lim = Limiter::new(48_000.0, 2);
        let ceiling = lim.discrete_ceiling();
        // 左右都超过 ceiling（2.0 / 0.5）：限幅器必须用同一个增益缩放两个声道，
        // 否则音像会被拉到中间。
        let mut buf = vec![0.0f32; 4096 * 2];
        for fr in buf.chunks_mut(2) {
            fr[0] = 2.0;
            fr[1] = 0.5;
        }
        lim.process(&mut buf);
        let last = buf.len() - 4;
        let (l, r) = (f64::from(buf[last]), f64::from(buf[last + 1]));
        assert!((l / r - 4.0).abs() < 1e-9, "声道比例被破坏 {l}/{r}");
        assert!(l.abs() <= ceiling + 1e-6, "越界 {l}");
        assert!((l.abs() - ceiling).abs() < 1e-6, "应当被压到 ceiling: {l}");
        assert!(lim.gain() < 1.0);
    }

    #[test]
    fn attack_is_a_ramp_not_a_step() {
        // 突然进来的大信号：增益必须在 `ramp_frames()` 帧内**逐帧**降下去，
        // 而不是一帧跳到位（后者就是爆音）。
        let fs = 48_000.0;
        let mut lim = Limiter::new(fs, 1);
        let h = lim.ramp_frames() as f64;
        let mut sig = vec![0.0_f32; 4096];
        for (i, s) in sig.iter_mut().enumerate() {
            if i >= 1024 {
                *s = 0.95;
            }
        }
        // 逐帧记录增益
        let mut gains = Vec::new();
        for chunk in sig.chunks_mut(1) {
            lim.process(chunk);
            gains.push(lim.gain());
        }
        let mut worst = 0.0_f64;
        for w in gains.windows(2) {
            worst = worst.max((w[1] - w[0]).abs());
        }
        assert!(
            worst <= 1.0 / h + 1e-9,
            "增益单帧下降 {worst} 超过 1/H = {}",
            1.0 / h
        );
        assert!(lim.gain() < 0.95);
    }
}
