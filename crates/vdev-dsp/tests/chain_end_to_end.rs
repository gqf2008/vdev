//! 端到端验收：多种信号的有限性/峰值约束，以及**运行中反复改参数不爆音**（改进点 E）。
// 测试代码里的数值转换（`i as f64`、`f64 as f32`、`secs * FS as usize`）与精确浮点比较
// 都是刻意的（见 src/lib.rs 顶部的 lint 说明），集成测试是独立 crate，需要在这里再声明一次。
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::float_cmp)]

mod common;

use common::*;
use vdev_dsp::{true_peak_4x, Chain};

/// 5 秒正弦：无 NaN/Inf、真峰值不越界、相邻样本差有界。
#[test]
fn five_seconds_of_sine_is_clean() {
    let mut chain = Chain::new(FS, 2, 10);
    chain.eq_mut().set_smoothing_samples(240);
    chain.eq_mut().set_gain(5, 6.0);
    chain.leveling_mut().set_target_loudness(-20.0);

    let mono = sine(997.0, 0.6, 5.0);
    let reference = interleave(&mono, &mono);
    let mut buf = reference.clone();
    chain.process(&mut buf);

    assert!(all_finite(&buf));
    let ceiling = chain.leveling().limiter().ceiling_linear();
    let tp = true_peak(&buf);
    assert!(tp <= ceiling + 1e-6, "真峰值 {tp} > ceiling {ceiling}");
    // 997 Hz 满幅正弦自身相邻样本差就有 ≈0.13，直接卡常数毫无意义；
    // 改用「相对信号固有斜率的归一化台阶指标」（见 common::max_step_ratio）。
    let ratio = max_step_ratio(&buf, &reference, 2, 512);
    println!("正弦: 真峰值 {tp:.6}, 归一化台阶指标 {ratio:.4}");
    assert!(ratio < 1.5, "归一化台阶指标 {ratio} 过大，疑似爆音");
}

/// 白噪声：无 NaN/Inf、离散峰值与真峰值都受控。
#[test]
fn white_noise_is_bounded() {
    let mut chain = Chain::new(FS, 1, 10);
    chain.leveling_mut().set_target_loudness(-23.0);
    let mut buf = noise_frames(5 * FS as usize, 0.8, 0xCAFE);
    chain.process(&mut buf);

    assert!(all_finite(&buf));
    let ceiling = chain.leveling().limiter().ceiling_linear();
    let disc = peak(&buf);
    let tp = true_peak(&buf);
    println!("白噪声: 离散峰值 {disc:.6}, 真峰值 {tp:.6}, ceiling {ceiling:.6}");
    assert!(disc <= chain.leveling().limiter().discrete_ceiling() + 1e-9);
    // 过采样后的一点过冲来自插值核在 Nyquist 附近的能量，留 2% 余量并如实记录
    assert!(
        tp <= ceiling * 1.02,
        "白噪声真峰值 {tp} 明显越界（ceiling {ceiling}）"
    );
}

/// 「正弦 + 突发大脉冲」：脉冲必须被限幅器吃掉，输出仍然有限。
#[test]
fn sine_with_a_burst_impulse_is_limited() {
    let mut chain = Chain::new(FS, 1, 10);
    chain.leveling_mut().set_target_loudness(-20.0);
    let mut buf = sine(997.0, 0.4, 5.0);
    let at = 2 * FS as usize;
    for (k, s) in buf[at..at + 5].iter_mut().enumerate() {
        *s = if k % 2 == 0 { 4.0 } else { -4.0 };
    }
    chain.process(&mut buf);

    assert!(all_finite(&buf));
    let ceiling = chain.leveling().limiter().ceiling_linear();
    let disc = peak(&buf);
    let tp = true_peak_4x(&buf[at..at + 4800]);
    println!("脉冲段: 离散峰值 {disc:.6}, 真峰值 {tp:.6}, ceiling {ceiling:.6}");
    assert!(disc <= chain.leveling().limiter().discrete_ceiling() + 1e-9);
    assert!(tp <= ceiling * 1.02, "脉冲真峰值 {tp} 越界");
}

/// 运行中反复改参数（EQ 增益 / 段数 / Q / 目标响度 / 采样率）不爆音。
#[test]
fn live_parameter_changes_are_click_free() {
    let mut chain = Chain::new(FS, 1, 10);
    chain.eq_mut().set_smoothing_samples(240); // 5 ms 交叉淡化

    let total = 6 * FS as usize;
    let mut out: Vec<f32> = Vec::with_capacity(total);
    let mut reference: Vec<f32> = Vec::with_capacity(total);
    let mut pos = 0usize;
    let mut step = 0usize;
    while pos < total {
        let n = 480.min(total - pos);
        let mut block = vec![0.0_f32; n];
        // 用绝对时间保证正弦连续
        for (i, s) in block.iter_mut().enumerate() {
            let t = (pos + i) as f64 / FS;
            *s = (0.3 * (2.0 * std::f64::consts::PI * 997.0 * t).sin()) as f32;
        }
        reference.extend_from_slice(&block);
        chain.process(&mut block);
        out.extend_from_slice(&block);
        pos += n;
        step += 1;
        if step.is_multiple_of(4) {
            chain.eq_mut().set_gain(step % 10, 12.0);
            chain.eq_mut().set_gain((step + 3) % 10, -9.0);
        }
        if step.is_multiple_of(16) {
            chain.set_bands(if chain.bands() == 10 { 15 } else { 10 });
        }
        if step.is_multiple_of(12) {
            let q = if step.is_multiple_of(24) { 0.7 } else { 1.8 };
            chain.eq_mut().set_q_multiplier(q);
        }
        if step.is_multiple_of(20) {
            let t = if step.is_multiple_of(40) {
                -14.0
            } else {
                -23.0
            };
            chain.leveling_mut().set_target_loudness(t);
        }
    }

    assert!(all_finite(&out));
    let ceiling = chain.leveling().limiter().ceiling_linear();
    let tp = true_peak(&out);
    assert!(tp <= ceiling + 1e-6, "真峰值 {tp} > ceiling {ceiling}");

    // 0.3 幅度的 997 Hz 正弦本身相邻样本差 ≈ 0.039；系数淡化/增益平滑不允许带来
    // 明显超过「该处包络固有斜率」的台阶，这里用归一化指标卡（见 common::max_step_ratio）。
    let ratio = max_step_ratio(&out, &reference, 1, 512);
    println!("反复改参数: 归一化台阶指标 {ratio:.4}，真峰值 {tp:.6}");
    assert!(ratio < 1.5, "参数切换产生了台阶: 归一化台阶指标 {ratio}");
}

/// 采样率切换（44.1k ↔ 96k）后依然干净。
#[test]
fn sample_rate_switch_is_clean() {
    let mut chain = Chain::new(44_100.0, 1, 10);
    chain.eq_mut().set_gain(4, 9.0);
    let mut buf: Vec<f32> = (0..44_100)
        .map(|i| (0.4 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 44_100.0).sin()) as f32)
        .collect();
    chain.process(&mut buf);
    chain.set_sample_rate(96_000.0);
    let mut buf2: Vec<f32> = (0..96_000)
        .map(|i| (0.4 * (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / 96_000.0).sin()) as f32)
        .collect();
    chain.process(&mut buf2);
    assert!(all_finite(&buf));
    assert!(all_finite(&buf2));
    assert!((chain.eq().sample_rate() - 96_000.0).abs() < 1e-9);
}
