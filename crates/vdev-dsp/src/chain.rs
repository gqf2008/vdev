//! 端到端链路：图示均衡器 → 响度归一化（内含前瞻真峰值限幅）。
//!
//! # 结构
//!
//! 把「先 EQ、后响度归一化」这个顺序固化成一个类型，顺带把「谁先谁后、谁改了采样率」
//! 这类容易出错的地方收进一个 `set_sample_rate()`。

use crate::graphic_eq::GraphicEq;
use crate::leveling::Leveling;

/// EQ + 响度归一化的固定顺序链路。
///
/// 顺序固定为「先 EQ、后归一化」，且**归一化自带限幅**，因此链路末端天然满足
/// 真峰值约束（改进点 D）。EQ 与归一化都有独立的旁路开关。
#[derive(Clone, Debug)]
pub struct Chain {
    fs: f64,
    channels: usize,
    bands: usize,
    eq: GraphicEq,
    leveling: Leveling,
}

impl Chain {
    /// `fs` 采样率、`channels` 声道、`bands` 段图示 EQ。
    #[must_use]
    pub fn new(fs: f64, channels: usize, bands: usize) -> Self {
        let fs = if fs.is_finite() && fs > 0.0 {
            fs
        } else {
            48_000.0
        };
        let channels = channels.clamp(1, 64);
        let bands = bands.clamp(1, 64);
        let eq = GraphicEq::new(fs, channels, bands);
        let leveling = Leveling::new(fs, channels);
        Self {
            fs,
            channels,
            bands,
            eq,
            leveling,
        }
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

    /// EQ 段数。
    #[must_use]
    pub fn bands(&self) -> usize {
        self.bands
    }

    /// EQ（只读）。
    #[must_use]
    pub fn eq(&self) -> &GraphicEq {
        &self.eq
    }

    /// EQ（可变）。
    pub fn eq_mut(&mut self) -> &mut GraphicEq {
        &mut self.eq
    }

    /// 响度归一化（只读）。
    #[must_use]
    pub fn leveling(&self) -> &Leveling {
        &self.leveling
    }

    /// 响度归一化（可变）。
    pub fn leveling_mut(&mut self) -> &mut Leveling {
        &mut self.leveling
    }

    /// 整链旁路（EQ 与归一化同时开关）。
    pub fn set_bypass(&mut self, on: bool) {
        self.eq.set_bypass(on);
        self.leveling.set_enabled(!on);
    }

    /// 是否整链旁路。
    #[must_use]
    pub fn is_bypassed(&self) -> bool {
        self.eq.is_bypassed() && !self.leveling.is_enabled()
    }

    /// 换采样率（两级一起改，避免出现「EQ 用新采样率、限幅器用旧采样率」）。
    pub fn set_sample_rate(&mut self, fs: f64) {
        if fs.is_finite() && fs > 0.0 && (fs - self.fs).abs() > f64::EPSILON {
            self.fs = fs;
            self.eq.set_sample_rate(fs);
            self.leveling.set_sample_rate(fs);
        }
    }

    /// 换声道数。
    pub fn set_channels(&mut self, channels: usize) {
        let ch = channels.clamp(1, 64);
        if ch != self.channels {
            self.channels = ch;
            self.eq.set_channels(ch);
            self.leveling.set_channels(ch);
        }
    }

    /// 换 EQ 段数。
    pub fn set_bands(&mut self, bands: usize) {
        let b = bands.clamp(1, 64);
        if b != self.bands {
            self.bands = b;
            self.eq.set_num_bands(b);
        }
    }

    /// 清两级的状态。
    pub fn reset(&mut self) {
        self.eq.reset();
        self.leveling.reset();
    }

    /// 原地处理交错多声道缓冲。
    pub fn process(&mut self, buf: &mut [f32]) {
        self.eq.process(buf);
        self.leveling.process(buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_bypass_is_bit_exact() {
        let mut chain = Chain::new(48_000.0, 2, 10);
        chain.eq_mut().set_gain(4, 12.0);
        chain.set_bypass(true);
        let mut buf: Vec<f32> = (0..2048).map(|i| ((i as f32) * 0.02).sin() * 0.5).collect();
        let before = buf.clone();
        chain.process(&mut buf);
        assert_eq!(buf, before);
        assert!(chain.is_bypassed());
    }

    #[test]
    fn sample_rate_change_propagates() {
        let mut chain = Chain::new(44_100.0, 2, 10);
        chain.set_sample_rate(96_000.0);
        assert!((chain.eq().sample_rate() - 96_000.0).abs() < 1e-9);
        assert!((chain.leveling().sample_rate() - 96_000.0).abs() < 1e-9);
        assert!((chain.leveling().limiter().sample_rate() - 96_000.0).abs() < 1e-9);
    }
}
