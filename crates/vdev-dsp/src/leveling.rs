//! 响度归一化（改进点 A/B/C/E 的汇合点）。
//!
//! # 算法依据
//!
//! | 本模块 | 依据 |
//! | --- | --- |
//! | [`Leveling::process`] | 自动增益控制 / 响度归一化的通用伺服结构 |
//! | [`Leveling::target_gain_db`] | 由目标响度与实际测量值求目标增益 |
//! | [`Leveling::step_gain`] | 一阶平滑：下降快（attack）/ 上升慢（release） |
//! | [`Leveling::gain_floor_db`] | 长时间安静时允许的额外提升额度 |
//! | [`Leveling::max_gain_rate_db_per_sec`] | 独立的 dB/s 变化率上限（与块长无关的硬约束） |
//!
//! # 设计取舍
//!
//! - **取舍 A**：测量端换成[`LoudnessMeter`]（近似 K 加权 + 门控），不再用裸 RMS；
//!   仍然保留 `Rms` 模式做对照。
//!   伺服取用的是 **momentary（400 ms 未门控滑窗）** 响度：它的跟随时间只有 400 ms，
//!   一旦节目真的静下来就能在 400 ms 内察觉；3.2 s 的门控值（[`LoudnessMeter::loudness`]）
//!   仍然对外暴露，用于报告与验收（门控值在静音后会被「残留的响块」拖住 3 s 以上，
//!   不适合直接当伺服的误差信号）。
//! - **取舍 B**：以 **buffer 为粒度**做 alpha 混合（逐块线性插值）在 buffer 边界上是
//!   分段线性的，块大小一变行为就变；再叠加「每 buffer 最多衰减固定量」的硬限，
//!   「衰减速度」就直接取决于 buffer 长度。我们改成**逐样本一阶平滑**（attack 5 ms /
//!   release 250 ms）+ **独立的 dB/s 变化率上限**，两者都与 buffer 划分无关。
//! - **取舍 C**：增益是**一个标量**，作用于所有声道；检测器从所有声道聚合
//!   （见 [`LoudnessMeter`]）。逐声道独立算增益会让左右声道各自漂移，立体声像被拉扯。
//! - **取舍 D**：内置[`Limiter`]（前瞻真峰值限幅），而不是「撞到 ceiling 再慢慢收」。
//! - **取舍 E**：目标响度与增益都做斜坡/平滑，运行中改参数不会跳变；[`Leveling::reset`] 另给。
//!
//! 提升额度分两档：常态额度 [`Leveling::base_boost_limit_db`]（默认 +6 dB）
//! 随时可用；安静额度 [`Leveling::gain_floor_db`] 需要连续安静 `quiet_activation_secs`
//! 才按 `quiet_rise_db_per_sec` 往上爬、并在回到正常电平或静音后按
//! `quiet_decay_db_per_sec` 回落。纯静音段目标增益直接回 0 dB。
//! - 梯度预测 / 余量评分一类的事后启发式补偿，是在为「测量不准 + 按 buffer 混增益」
//!   打补丁。我们**刻意不做**这些补偿：
//!   测量换成 K 加权 + 门控之后，「糊/亮」「余量」这些问题在测量层就不存在了。

use crate::limiter::Limiter;
use crate::loudness::{LoudnessMeter, LoudnessMode};

/// 一阶平滑用不到的极小量，避免除零。
const MIN_DT: f64 = 1.0e-6;

/// 响度归一化器：把输入节目的短时响度拉到目标值附近，同时保证真峰值不越界。
#[derive(Clone, Debug)]
pub struct Leveling {
    fs: f64,
    channels: usize,
    meter: LoudnessMeter,
    limiter: Limiter,
    enabled: bool,
    target_loudness: f64,
    min_gain_db: f64,
    max_gain_db: f64,
    attack_ms: f64,
    release_ms: f64,
    attack_coef: f64,
    release_coef: f64,
    max_rate_db_per_sec: f64,
    max_step_db: f64,
    base_boost_limit_db: f64,
    quiet_threshold_lufs: f64,
    silence_rms: f64,
    silence_lufs: f64,
    quiet_activation_secs: f64,
    quiet_rise_db_per_sec: f64,
    quiet_decay_db_per_sec: f64,
    gain_floor_db: f64,
    quiet_timer_secs: f64,
    target_gain_db: f64,
    gain_db: f64,
}

impl Leveling {
    /// 默认配置（48 kHz 假设，`fs` 由参数给定）：
    /// 目标 −20 LUFS、最大 +20 dB 提升 / −24 dB 衰减、attack 5 ms、release 250 ms、
    /// 增益变化率上限 200 dB/s、ceiling −1 dBFS。
    #[must_use]
    pub fn new(fs: f64, channels: usize) -> Self {
        let fs = if fs.is_finite() && fs > 0.0 {
            fs
        } else {
            48_000.0
        };
        let channels = channels.clamp(1, 64);
        let mut l = Self {
            fs,
            channels,
            meter: LoudnessMeter::new(fs, channels, LoudnessMode::KWeighted),
            limiter: Limiter::new(fs, channels),
            enabled: true,
            target_loudness: -20.0,
            min_gain_db: -24.0,
            max_gain_db: 20.0,
            attack_ms: 5.0,
            release_ms: 250.0,
            attack_coef: 0.0,
            release_coef: 0.0,
            max_rate_db_per_sec: 200.0,
            max_step_db: 0.0,
            base_boost_limit_db: 6.0,
            quiet_threshold_lufs: -40.0,
            silence_rms: 3.0e-4,
            silence_lufs: -70.0,
            quiet_activation_secs: 3.0,
            quiet_rise_db_per_sec: 4.0,
            quiet_decay_db_per_sec: 12.0,
            gain_floor_db: 0.0,
            quiet_timer_secs: 0.0,
            target_gain_db: 0.0,
            gain_db: 0.0,
        };
        l.recalc();
        l
    }

    fn recalc(&mut self) {
        let fs = self.fs;
        self.attack_coef = Self::one_pole_coef(self.attack_ms, fs);
        self.release_coef = Self::one_pole_coef(self.release_ms, fs);
        self.max_step_db = self.max_rate_db_per_sec / fs;
    }

    fn one_pole_coef(ms: f64, fs: f64) -> f64 {
        let tau = (ms.max(0.01) * 1.0e-3 * fs).max(1.0);
        1.0 - (-1.0 / tau).exp()
    }

    // ---- 配置 ----

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

    /// 是否启用（旁路时 `process()` 直接返回，逐位恒等）。
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// 启用 / 旁路。
    pub fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// 测量模式。
    #[must_use]
    pub fn mode(&self) -> LoudnessMode {
        self.meter.mode()
    }

    /// 换测量模式（`KWeighted` = 默认；`Rms` = 裸 RMS 对照）。
    pub fn set_mode(&mut self, mode: LoudnessMode) {
        self.meter.set_mode(mode);
    }

    /// 目标响度：`KWeighted` 模式单位是 LUFS，`Rms` 模式单位是 dBFS。
    #[must_use]
    pub fn target_loudness(&self) -> f64 {
        self.target_loudness
    }

    /// 设目标响度（非有限值忽略）。
    pub fn set_target_loudness(&mut self, value: f64) {
        if value.is_finite() {
            self.target_loudness = value.clamp(-60.0, 0.0);
        }
    }

    /// 提升上限（dB）。
    #[must_use]
    pub fn max_gain_db(&self) -> f64 {
        self.max_gain_db
    }

    /// 设提升上限（dB）。
    pub fn set_max_gain_db(&mut self, db: f64) {
        if db.is_finite() {
            self.max_gain_db = db.clamp(0.0, 40.0);
        }
    }

    /// 衰减下限（dB，负数）。
    #[must_use]
    pub fn min_gain_db(&self) -> f64 {
        self.min_gain_db
    }

    /// 设衰减下限（dB）。
    pub fn set_min_gain_db(&mut self, db: f64) {
        if db.is_finite() {
            self.min_gain_db = db.clamp(-60.0, 0.0);
        }
    }

    /// attack 时间常数（ms，增益**下降**方向）。
    #[must_use]
    pub fn attack_ms(&self) -> f64 {
        self.attack_ms
    }

    /// release 时间常数（ms，增益**上升**方向）。
    #[must_use]
    pub fn release_ms(&self) -> f64 {
        self.release_ms
    }

    /// 设 attack / release（ms）。attack 会被夹到 ≤ release，保证「下降快于上升」。
    pub fn set_times_ms(&mut self, attack_ms: f64, release_ms: f64) {
        let a = if attack_ms.is_finite() {
            attack_ms.clamp(0.05, 5000.0)
        } else {
            5.0
        };
        let r = if release_ms.is_finite() {
            release_ms.clamp(0.05, 5000.0)
        } else {
            250.0
        };
        self.attack_ms = a.min(r);
        self.release_ms = r.max(a);
        self.recalc();
    }

    /// 增益变化率上限（dB/s）。
    #[must_use]
    pub fn max_gain_rate_db_per_sec(&self) -> f64 {
        self.max_rate_db_per_sec
    }

    /// 设增益变化率上限（dB/s）。这是**独立于** attack/release 的硬约束。
    pub fn set_max_gain_rate_db_per_sec(&mut self, rate: f64) {
        if rate.is_finite() {
            self.max_rate_db_per_sec = rate.clamp(1.0, 10_000.0);
            self.recalc();
        }
    }

    /// 常态提升上限（dB）：不依赖任何计时，伺服随时可以用的提升额度。
    ///
    /// 若把提升额度完全绑在「连续安静若干秒」上，
    /// 结果是「一段整体偏轻但完全正常的录音」永远拿不到提升。这里把它拆成两档：
    /// 常态额度 `base_boost_limit_db`（默认 +6 dB，立即可用）与安静额度
    /// `gain_floor_db`（需要连续安静才爬升，最高到 [`Self::max_gain_db`]）。
    /// 实际可用额度 = `max(常态额度, 安静额度)`；纯静音段则直接回到 0 dB。
    #[must_use]
    pub fn base_boost_limit_db(&self) -> f64 {
        self.base_boost_limit_db
    }

    /// 设常态提升上限（dB）。
    pub fn set_base_boost_limit_db(&mut self, db: f64) {
        if db.is_finite() {
            self.base_boost_limit_db = db.clamp(0.0, 40.0);
        }
    }

    /// 「算安静」的门限（LUFS / dBFS）：低于它的信号才可能触发安静提升。
    #[must_use]
    pub fn quiet_threshold(&self) -> f64 {
        self.quiet_threshold_lufs
    }

    /// 设安静门限。
    pub fn set_quiet_threshold(&mut self, lufs: f64) {
        if lufs.is_finite() {
            self.quiet_threshold_lufs = lufs.clamp(-90.0, -10.0);
        }
    }

    /// 静音判定用的 RMS 门限（线性）。
    #[must_use]
    pub fn silence_rms(&self) -> f64 {
        self.silence_rms
    }

    /// 设静音判定 RMS 门限。
    pub fn set_silence_rms(&mut self, rms: f64) {
        if rms.is_finite() {
            self.silence_rms = rms.clamp(0.0, 0.1);
        }
    }

    /// 静音判定用的响度门限（LUFS / dBFS）。
    ///
    /// 只看一个瞬时「很安静」阈值是不够的：IIR 侧链的尾巴
    /// （能量已经低到听不见、但数值上还不是 0）会被判成「很轻的节目」而触发提升。
    /// 这里额外要求响度本身低于门限，两种判据任一命中就算静音。
    #[must_use]
    pub fn silence_loudness(&self) -> f64 {
        self.silence_lufs
    }

    /// 设静音判定响度门限。
    pub fn set_silence_loudness(&mut self, lufs: f64) {
        if lufs.is_finite() {
            self.silence_lufs = lufs.clamp(-120.0, -20.0);
        }
    }

    /// 安静多久之后才开始提升（秒）。
    #[must_use]
    pub fn quiet_activation_secs(&self) -> f64 {
        self.quiet_activation_secs
    }

    /// 设安静激活时间（秒）。
    pub fn set_quiet_activation_secs(&mut self, secs: f64) {
        if secs.is_finite() {
            self.quiet_activation_secs = secs.clamp(0.0, 60.0);
        }
    }

    /// 安静提升斜坡速度（dB/s）与静音衰减速度（dB/s）。
    pub fn set_quiet_rates(&mut self, rise_db_per_sec: f64, decay_db_per_sec: f64) {
        if rise_db_per_sec.is_finite() {
            self.quiet_rise_db_per_sec = rise_db_per_sec.clamp(0.1, 100.0);
        }
        if decay_db_per_sec.is_finite() {
            self.quiet_decay_db_per_sec = decay_db_per_sec.clamp(0.1, 200.0);
        }
    }

    /// 内嵌限幅器（真峰值、前瞻等都在这里配）。
    #[must_use]
    pub fn limiter(&self) -> &Limiter {
        &self.limiter
    }

    /// 内嵌限幅器（可变）。
    pub fn limiter_mut(&mut self) -> &mut Limiter {
        &mut self.limiter
    }

    /// 内嵌响度计（只读；调试/验收用）。
    #[must_use]
    pub fn meter(&self) -> &LoudnessMeter {
        &self.meter
    }

    // ---- 运行时状态 ----

    /// 当前实际施加的增益（dB）。
    #[must_use]
    pub fn gain_db(&self) -> f64 {
        self.gain_db
    }

    /// 当前实际施加的增益（线性）。
    #[must_use]
    pub fn gain_linear(&self) -> f64 {
        10f64.powf(self.gain_db / 20.0)
    }

    /// 目标增益（dB，未平滑）。
    #[must_use]
    pub fn target_gain_db(&self) -> f64 {
        self.target_gain_db
    }

    /// 安静提升的当前上限（dB，≥ 0）。
    ///
    /// 平时是 0（不允许任何提升）；持续安静 `quiet_activation_secs` 之后按
    /// `quiet_rise_db_per_sec` 往上爬；一旦静音或回到正常电平就按
    /// `quiet_decay_db_per_sec` 衰减回 0——纯静音段**不会**把底噪放大。
    #[must_use]
    pub fn gain_floor_db(&self) -> f64 {
        self.gain_floor_db
    }

    /// 清状态（增益回到 0 dB、上限归零、滤波器与限幅器状态清零）。
    pub fn reset(&mut self) {
        self.gain_db = 0.0;
        self.target_gain_db = 0.0;
        self.gain_floor_db = 0.0;
        self.quiet_timer_secs = 0.0;
        self.meter.reset();
        self.limiter.reset();
    }

    /// 换采样率。
    pub fn set_sample_rate(&mut self, fs: f64) {
        if fs.is_finite() && fs > 0.0 && (fs - self.fs).abs() > f64::EPSILON {
            self.fs = fs;
            self.meter.set_sample_rate(fs);
            self.limiter.set_sample_rate(fs);
            self.recalc();
        }
    }

    /// 换声道数。
    pub fn set_channels(&mut self, channels: usize) {
        let ch = channels.clamp(1, 64);
        if ch != self.channels {
            self.channels = ch;
            self.meter.set_channels(ch);
            self.limiter.set_channels(ch);
        }
    }

    /// 原地处理交错多声道缓冲（实时路径：零分配、无锁、无 panic 分支）。
    ///
    /// 处理顺序：先测输入响度（feed-forward）→ 更新目标增益 → 逐样本平滑增益并施加
    /// → 前瞻真峰值限幅。
    pub fn process(&mut self, buf: &mut [f32]) {
        if !self.enabled || self.channels == 0 || buf.is_empty() {
            return;
        }
        let nch = self.channels;
        self.meter.process(buf);
        let frames = buf.len() / nch;
        if frames > 0 {
            self.update_target(frames);
        }
        for frame in buf.chunks_mut(nch) {
            let g_db = self.step_gain();
            // 10^(x/20) = 2^(x · log2(10)/20)，exp2 比 powf 便宜，且这里每帧都要算
            let g = (g_db * 0.166_096_404_043_275_2).exp2();
            if g <= 0.0 || !g.is_finite() {
                continue;
            }
            for s in frame.iter_mut() {
                let v = f64::from(*s) * g;
                *s = if v.is_finite() {
                    v.clamp(-1.0e6, 1.0e6) as f32
                } else {
                    0.0
                };
            }
        }
        self.limiter.process(buf);
    }

    /// 每 buffer 一次：把「输入响度 → 目标增益」以及安静上限更新一遍。
    fn update_target(&mut self, frames: usize) {
        let dt = (frames as f64 / self.fs).clamp(MIN_DT, 1.0);
        let l = self.meter.momentary_loudness();
        let rms = self.meter.window_rms();
        if !l.is_finite() || l < self.silence_lufs || rms < self.silence_rms {
            // 纯静音：目标回到 0 dB，安静上限按衰减速度回落到 0
            self.quiet_timer_secs = 0.0;
            self.gain_floor_db = (self.gain_floor_db - self.quiet_decay_db_per_sec * dt).max(0.0);
            self.target_gain_db = 0.0;
            return;
        }
        let base = (self.target_loudness - l).clamp(self.min_gain_db, self.max_gain_db);
        if l < self.quiet_threshold_lufs {
            self.quiet_timer_secs += dt;
            if self.quiet_timer_secs >= self.quiet_activation_secs {
                self.gain_floor_db =
                    (self.gain_floor_db + self.quiet_rise_db_per_sec * dt).min(self.max_gain_db);
            }
        } else {
            self.quiet_timer_secs = 0.0;
            self.gain_floor_db = (self.gain_floor_db - self.quiet_decay_db_per_sec * dt).max(0.0);
        }
        // 提升不能超过「常态额度 / 安静额度」里的较大者；衰减不受它约束
        let cap = self.gain_floor_db.max(self.base_boost_limit_db);
        self.target_gain_db = base.min(cap);
    }

    /// 逐样本平滑：下降走 attack、上升走 release，且每样本变化量不超过 dB/s 上限。
    #[inline]
    fn step_gain(&mut self) -> f64 {
        let target = self
            .target_gain_db
            .clamp(self.min_gain_db, self.max_gain_db);
        let delta = target - self.gain_db;
        if delta == 0.0 {
            return self.gain_db;
        }
        let coef = if delta < 0.0 {
            self.attack_coef
        } else {
            self.release_coef
        };
        let mut step = delta * coef;
        if step > self.max_step_db {
            step = self.max_step_db;
        } else if step < -self.max_step_db {
            step = -self.max_step_db;
        }
        let next = self.gain_db + step;
        self.gain_db = if next.is_finite() { next } else { self.gain_db };
        self.gain_db
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
    fn silence_keeps_gain_floor_at_zero() {
        let mut lv = Leveling::new(48_000.0, 1);
        lv.process(&mut vec![0.0f32; 48_000]);
        assert_eq!(lv.gain_floor_db(), 0.0);
        assert_eq!(lv.target_gain_db(), 0.0);
        assert!(lv.gain_db().abs() < 1e-12);
    }

    #[test]
    fn step_is_rate_limited_per_sample() {
        let fs = 48_000.0;
        let mut lv = Leveling::new(fs, 1);
        lv.set_max_gain_rate_db_per_sec(200.0);
        let block = 480; // 10 ms
        let mut gain = lv.gain_db();
        let mut buf = sine(fs, 1_000.0, 0.9, 0.5);
        for chunk in buf.chunks_mut(block) {
            lv.process(chunk);
            let d = (lv.gain_db() - gain).abs();
            assert!(
                d <= 200.0 * (block as f64 / fs) + 1e-9,
                "每块变化 {d} dB 超过上限"
            );
            gain = lv.gain_db();
        }
    }

    #[test]
    fn asymmetric_attack_release() {
        let fs = 48_000.0;
        let mut base = Leveling::new(fs, 1);
        base.process(&mut sine(fs, 1_000.0, 0.5, 2.0));
        let g0 = base.gain_db();
        assert!(g0 < -3.0, "基准增益应该已经压下来 {g0}");

        // 同一状态出发：一路遇到更响的信号（要求继续压低），一路遇到更轻的信号（要求回升）
        let mut louder = base.clone();
        let mut quieter = base;
        louder.process(&mut sine(fs, 1_000.0, 1.0, 0.2));
        quieter.process(&mut sine(fs, 1_000.0, 0.25, 0.2));

        let down = (louder.gain_db() - g0).abs();
        let up = (quieter.gain_db() - g0).abs();
        assert!(down > up * 1.5, "下降 {down} dB 应当明显快于上升 {up} dB");
    }

    #[test]
    fn quiet_boost_rises_then_decays_in_silence() {
        let fs = 48_000.0;
        let mut lv = Leveling::new(fs, 1);
        // 10 s 的「很轻但不是静音」的正弦
        for _ in 0..50 {
            lv.process(&mut sine(fs, 1_000.0, 0.004, 0.2));
        }
        let floor_loud = lv.gain_floor_db();
        assert!(floor_loud > 6.0, "安静提升上限没有爬升: {floor_loud}");
        assert!(lv.gain_db() > 6.0, "安静信号没有被提升: {}", lv.gain_db());

        // 2 s 纯静音：上限必须衰减回 0
        for _ in 0..10 {
            lv.process(&mut vec![0.0f32; (fs * 0.2) as usize]);
        }
        assert!(
            lv.gain_floor_db() == 0.0,
            "静音后上限 {} 应当归零",
            lv.gain_floor_db()
        );
        assert!(
            lv.gain_db() < 1.0,
            "静音后增益 {} 应当回到 0 dB 附近",
            lv.gain_db()
        );
    }

    #[test]
    fn loud_input_is_not_boosted_without_quiet_time() {
        let fs = 48_000.0;
        let mut lv = Leveling::new(fs, 1);
        lv.process(&mut sine(fs, 1_000.0, 0.9, 0.5));
        assert!(lv.gain_db() <= 0.0);
        assert_eq!(lv.gain_floor_db(), 0.0);
    }

    #[test]
    fn applies_same_gain_to_all_channels() {
        let fs = 48_000.0;
        let mut lv = Leveling::new(fs, 2);
        let n = (fs * 0.5) as usize;
        let mut buf: Vec<f32> = (0..n * 2)
            .map(|i| {
                let f = if i % 2 == 0 { 0.6 } else { 0.15 };
                (f * (2.0 * core::f64::consts::PI * 1_000.0 * (i / 2) as f64 / fs).sin()) as f32
            })
            .collect();
        let before_ratio = 0.6f32 / 0.15;
        lv.process(&mut buf);
        // 只看后 1/4（增益已收敛、限幅器已充满延迟线）
        let start = n / 2 * 2;
        let mut worst = 0.0f64;
        for fr in buf[start..].chunks_exact(2) {
            let l = f64::from(fr[0]).abs();
            let r = f64::from(fr[1]).abs();
            if r > 1.0e-4 && l > 1.0e-4 {
                let ratio = l / r;
                worst =
                    worst.max((ratio - f64::from(before_ratio)).abs() / f64::from(before_ratio));
            }
        }
        assert!(worst < 0.02, "左右声道增益不一致，比例偏差 {worst}");
    }
}
