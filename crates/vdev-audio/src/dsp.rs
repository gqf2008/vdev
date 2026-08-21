//! 实时 DSP 管线：3 段 EQ（低架/峰值/高架，RBJ Audio EQ Cookbook）+ 总增益 + 软限幅。
//! 无分配、系数重算只在设参时发生；处理路径只做乘加。

use std::f32::consts::PI;

// ---- Biquad（transposed direct form II）----
#[derive(Clone, Copy)]
struct BiquadCoeffs {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

#[derive(Clone, Copy)]
struct BiquadState {
    s1: f32,
    s2: f32,
}

impl BiquadCoeffs {
    fn process(&self, st: &mut BiquadState, x: f32) -> f32 {
        let y = self.b0 * x + st.s1;
        st.s1 = self.b1 * x - self.a1 * y + st.s2;
        st.s2 = self.b2 * x - self.a2 * y;
        y
    }

    // RBJ Low Shelf
    fn low_shelf(f0: f32, gain_db: f32, fs: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * f0 / fs;
        let (sw, cw) = w0.sin_cos();
        let alpha = sw / 2.0 * ((a + 1.0 / a) * (1.0 / 1.0 - 1.0) + 2.0).sqrt(); // slope S=1
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let b0 = a * ((a + 1.0) - (a - 1.0) * cw + two_sqrt_a_alpha);
        let b1 = 2.0 * a * ((a - 1.0) - (a + 1.0) * cw);
        let b2 = a * ((a + 1.0) - (a - 1.0) * cw - two_sqrt_a_alpha);
        let a0 = (a + 1.0) + (a - 1.0) * cw + two_sqrt_a_alpha;
        let a1 = -2.0 * ((a - 1.0) + (a + 1.0) * cw);
        let a2 = (a + 1.0) + (a - 1.0) * cw - two_sqrt_a_alpha;
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    // RBJ Peaking EQ
    fn peaking(f0: f32, gain_db: f32, q: f32, fs: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * f0 / fs;
        let (sw, cw) = w0.sin_cos();
        let alpha = sw / (2.0 * q);
        let b0 = 1.0 + alpha * a;
        let b1 = -2.0 * cw;
        let b2 = 1.0 - alpha * a;
        let a0 = 1.0 + alpha / a;
        let a1 = -2.0 * cw;
        let a2 = 1.0 - alpha / a;
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    // RBJ High Shelf
    fn high_shelf(f0: f32, gain_db: f32, fs: f32) -> Self {
        let a = 10f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * f0 / fs;
        let (sw, cw) = w0.sin_cos();
        let alpha = sw / 2.0 * ((a + 1.0 / a) * (1.0 - 1.0) + 2.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        let b0 = a * ((a + 1.0) + (a - 1.0) * cw + two_sqrt_a_alpha);
        let b1 = -2.0 * a * ((a - 1.0) + (a + 1.0) * cw);
        let b2 = a * ((a + 1.0) + (a - 1.0) * cw - two_sqrt_a_alpha);
        let a0 = (a + 1.0) - (a - 1.0) * cw + two_sqrt_a_alpha;
        let a1 = 2.0 * ((a - 1.0) - (a + 1.0) * cw);
        let a2 = (a + 1.0) - (a - 1.0) * cw - two_sqrt_a_alpha;
        Self::normalize(b0, b1, b2, a0, a1, a2)
    }

    fn normalize(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    const IDENTITY: Self = Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };
}

// ---- DSP 管线 ----
#[derive(Clone, Copy)]
struct Channel {
    low: BiquadState,
    mid: BiquadState,
    high: BiquadState,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            low: BiquadState { s1: 0.0, s2: 0.0 },
            mid: BiquadState { s1: 0.0, s2: 0.0 },
            high: BiquadState { s1: 0.0, s2: 0.0 },
        }
    }
}

pub struct Dsp {
    gain_db: f32,
    low_db: f32,
    mid_db: f32,
    high_db: f32,
    low_coeff: BiquadCoeffs,
    mid_coeff: BiquadCoeffs,
    high_coeff: BiquadCoeffs,
    left: Channel,
    right: Channel,
}

impl Default for Dsp {
    fn default() -> Self {
        let mut d = Self {
            gain_db: 0.0,
            low_db: 0.0,
            mid_db: 0.0,
            high_db: 0.0,
            low_coeff: BiquadCoeffs::IDENTITY,
            mid_coeff: BiquadCoeffs::IDENTITY,
            high_coeff: BiquadCoeffs::IDENTITY,
            left: Channel::default(),
            right: Channel::default(),
        };
        d.recalc(48_000.0);
        d
    }
}

// 软限幅：正常范围（|x|<=1）完全直通，超出后平滑压回（防爆音，不改正常信号）
fn soft_limit(x: f32) -> f32 {
    if x > 1.0 {
        1.0 + (x - 1.0).tanh()
    } else if x < -1.0 {
        -1.0 + (x + 1.0).tanh()
    } else {
        x
    }
}

impl Dsp {
    fn recalc(&mut self, fs: f32) {
        self.low_coeff = BiquadCoeffs::low_shelf(120.0, self.low_db, fs);
        self.mid_coeff = BiquadCoeffs::peaking(1000.0, self.mid_db, 0.707, fs);
        self.high_coeff = BiquadCoeffs::high_shelf(8000.0, self.high_db, fs);
    }

    pub fn set_params(&mut self, gain_db: f32, low_db: f32, mid_db: f32, high_db: f32, fs: f32) {
        self.gain_db = gain_db;
        self.low_db = low_db;
        self.mid_db = mid_db;
        self.high_db = high_db;
        self.recalc(fs);
    }

    pub fn params(&self) -> [f32; 4] {
        [self.gain_db, self.low_db, self.mid_db, self.high_db]
    }

    // 处理交错立体声帧（实时路径：无分配、纯乘加）
    pub fn process(&mut self, data: &mut [f32]) {
        let gain = 10f32.powf(self.gain_db / 20.0);
        for frame in data.chunks_exact_mut(2) {
            // 左声道
            let l0 = frame[0] * gain;
            let l1 = self.low_coeff.process(&mut self.left.low, l0);
            let l2 = self.mid_coeff.process(&mut self.left.mid, l1);
            let l3 = self.high_coeff.process(&mut self.left.high, l2);
            frame[0] = soft_limit(l3);
            // 右声道
            let r0 = frame[1] * gain;
            let r1 = self.low_coeff.process(&mut self.right.low, r0);
            let r2 = self.mid_coeff.process(&mut self.right.mid, r1);
            let r3 = self.high_coeff.process(&mut self.right.high, r2);
            frame[1] = soft_limit(r3);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peak(data: &[f32]) -> f32 {
        data.iter().fold(0.0f32, |m, &v| m.max(v.abs()))
    }

    // 生成正弦帧（交错立体声），返回峰值约等于 amp
    fn sine(amp: f32, freq: f32, frames: usize) -> Vec<f32> {
        let mut v = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let x = amp * (2.0 * PI * freq * i as f32 / 48000.0).sin();
            v.push(x);
            v.push(x);
        }
        v
    }

    #[test]
    fn test_bypass() {
        let mut d = Dsp::default();
        let mut data = sine(0.5, 1000.0, 512);
        d.process(&mut data);
        assert!((peak(&data) - 0.5).abs() < 0.01, "直通峰值={}", peak(&data));
    }

    #[test]
    fn test_gain_6db() {
        let mut d = Dsp::default();
        d.set_params(6.0, 0.0, 0.0, 0.0, 48000.0);
        let mut data = sine(0.3, 1000.0, 512);
        d.process(&mut data);
        let p = peak(&data);
        assert!((p - 0.6).abs() < 0.03, "gain+6dB 峰值={}（期望≈0.6）", p);
    }

    #[test]
    fn test_mid_eq_boost() {
        let mut d = Dsp::default();
        d.set_params(0.0, 0.0, 6.0, 0.0, 48000.0);
        // 1kHz 应被提升 ~2x；100Hz 几乎不变
        let mut d1k = sine(0.5, 1000.0, 512);
        d.process(&mut d1k);
        let p1k = peak(&d1k);
        let mut d100 = sine(0.5, 100.0, 512);
        d.process(&mut d100);
        let p100 = peak(&d100);
        assert!((p1k - 1.0).abs() < 0.08, "1kHz 峰值={}", p1k);
        assert!((p100 - 0.5).abs() < 0.05, "100Hz 峰值={}", p100);
    }
}
