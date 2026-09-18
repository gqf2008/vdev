//! 验收测试的公共工具：确定性合成信号 + 客观度量。
//!
//! 这里所有的度量都是**独立重算**的（新建 [`LoudnessMeter`] / 直接扫样点），
//! 不复用被测对象内部的任何状态——避免「自己给自己判卷」。
#![allow(dead_code)]
// 测试代码里的数值转换（`i as f64`、`f64 as f32`、`secs * FS as usize`）与精确浮点比较
// 都是刻意的（见 lib.rs 顶部的 lint 说明），集成测试是独立 crate，要在这里再声明一次。
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::float_cmp)]

use vdev_dsp::{true_peak_4x, LoudnessMeter, LoudnessMode};

/// 测试采样率。
pub const FS: f64 = 48_000.0;

/// 生成 `frames` 帧正弦。
#[must_use]
pub fn sine_frames(freq: f64, amp: f64, frames: usize) -> Vec<f32> {
    (0..frames)
        .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * i as f64 / FS).sin()) as f32)
        .collect()
}

/// 生成 `secs` 秒正弦。
#[must_use]
pub fn sine(freq: f64, amp: f64, secs: f64) -> Vec<f32> {
    sine_frames(freq, amp, (FS * secs) as usize)
}

/// 确定性白噪声（xorshift32），输出落在 `[-amp, amp]`。
#[must_use]
pub fn noise_frames(frames: usize, amp: f32, seed: u32) -> Vec<f32> {
    let mut x = if seed == 0 { 0x1234_5678 } else { seed };
    (0..frames)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            let u = (x >> 8) as f32 / 8_388_608.0; // [0, 2)
            (u - 1.0) * amp
        })
        .collect()
}

/// 单声道 → 交错双声道。
#[must_use]
pub fn interleave(mono: &[f32], right: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(mono.len() * 2);
    for (l, r) in mono.iter().zip(right.iter()) {
        out.push(*l);
        out.push(*r);
    }
    out
}

/// 用**全新的**响度计测一段信号的响度（默认取整段；调用方可以只传尾段）。
#[must_use]
pub fn measure_loudness(sig: &[f32], channels: usize, mode: LoudnessMode) -> f64 {
    let mut m = LoudnessMeter::new(FS, channels, mode);
    m.process(sig);
    m.loudness()
}

/// 用**全新的**响度计测 RMS（dBFS）。
#[must_use]
pub fn measure_rms_db(sig: &[f32], channels: usize, mode: LoudnessMode) -> f64 {
    let mut m = LoudnessMeter::new(FS, channels, mode);
    m.process(sig);
    let r = m.window_rms();
    if r > 0.0 {
        20.0 * r.log10()
    } else {
        f64::NEG_INFINITY
    }
}

/// 离散样点峰值。
#[must_use]
pub fn peak(sig: &[f32]) -> f64 {
    sig.iter().fold(0.0_f64, |m, s| m.max(f64::from(*s).abs()))
}

/// 4x 过采样真峰值。
#[must_use]
pub fn true_peak(sig: &[f32]) -> f64 {
    true_peak_4x(sig)
}

/// 交错缓冲里每个声道的相邻样本最大跳变。
#[must_use]
pub fn max_step(sig: &[f32], channels: usize) -> f64 {
    let ch = channels.max(1);
    let mut worst = 0.0_f64;
    for c in 0..ch {
        let mut prev: Option<f64> = None;
        for frame in sig.chunks(ch) {
            if let Some(v) = frame.get(c) {
                let cur = f64::from(*v);
                if let Some(p) = prev {
                    worst = worst.max((cur - p).abs());
                }
                prev = Some(cur);
            }
        }
    }
    worst
}

/// 单声道序列的滑窗绝对值最大值（O(n) 分块前缀/后缀最大值实现）。
#[must_use]
pub fn window_abs_max(sig: &[f64], win: usize) -> Vec<f64> {
    let n = sig.len();
    let w = win.max(1);
    if n == 0 {
        return Vec::new();
    }
    let blocks = n.div_ceil(w);
    let mut prefix = vec![0.0_f64; n];
    let mut suffix = vec![0.0_f64; n];
    for b in 0..blocks {
        let lo = b * w;
        let hi = ((b + 1) * w).min(n);
        let mut m = 0.0_f64;
        for i in lo..hi {
            m = m.max(sig[i].abs());
            prefix[i] = m;
        }
        let mut m = 0.0_f64;
        for i in (lo..hi).rev() {
            m = m.max(sig[i].abs());
            suffix[i] = m;
        }
    }
    (0..n)
        .map(|i| {
            let j = (i + w - 1).min(n - 1);
            suffix[i].max(prefix[j])
        })
        .collect()
}

/// **归一化台阶指标**：相邻样本跳变 ÷「该处信号包络本来应有的固有跳变」。
///
/// `k = 参考信号（未经处理的输入）的 max|Δx| / max|x|`；对纯正弦就等于 `2·sin(πf/fs)`。
/// 输出包络用 `win` 帧滑窗最大值估计（`win` 取一个信号周期以上即可）。
/// **干净的处理链该比值 ≈ 1**；出现台阶（爆音）会明显大于 1。
///
/// 之所以不用「相邻样本差 < 某个常数」这种写法：997 Hz 满幅正弦 @48 kHz 的相邻样本差
/// 本身就有 0.13，固定阈值要么放过真正的台阶、要么把完全正常的信号判成爆音。
/// 归一化之后，阈值对信号频率与幅度都不再敏感。
#[must_use]
pub fn max_step_ratio(out: &[f32], reference: &[f32], channels: usize, win: usize) -> f64 {
    let ch = channels.max(1);
    let k = max_step(reference, ch) / peak(reference).max(1e-12);
    if k.is_nan() || k <= 0.0 {
        return 0.0;
    }
    let mut worst = 0.0_f64;
    for c in 0..ch {
        let sig: Vec<f64> = out
            .iter()
            .skip(c)
            .step_by(ch)
            .map(|s| f64::from(*s))
            .collect();
        if sig.len() < 2 {
            continue;
        }
        let env = window_abs_max(&sig, win);
        for i in 1..sig.len() {
            let expect = k * env[i].max(env[i - 1]);
            if expect > 1e-9 {
                worst = worst.max((sig[i] - sig[i - 1]).abs() / expect);
            }
        }
    }
    worst
}

/// 全部样点有限（无 NaN / Inf）。
#[must_use]
pub fn all_finite(sig: &[f32]) -> bool {
    sig.iter().all(|s| s.is_finite())
}
