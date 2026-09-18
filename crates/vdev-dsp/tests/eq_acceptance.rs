//! 图示均衡器验收：旁路恒等、频响、频点表、Q、增益钳位。
// 测试代码里的数值转换（`i as f64`、`f64 as f32`、`secs * FS as usize`）与精确浮点比较
// 都是刻意的（见 src/lib.rs 顶部的 lint 说明），集成测试是独立 crate，需要在这里再声明一次。
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::float_cmp)]

mod common;

use common::*;
use vdev_dsp::{band_frequencies, GraphicEq, MAX_BAND_FREQ, MAX_GAIN_DB, MIN_BAND_FREQ};

/// 旁路必须是逐位恒等（误差 0，而不是「小于 1e-6」）。
#[test]
fn bypass_is_bitwise_identity() {
    let mut eq = GraphicEq::new(FS, 2, 10);
    eq.set_gain(2, 15.0);
    eq.set_gain(7, -15.0);
    eq.set_bypass(true);

    let mut buf = interleave(&sine(440.0, 0.6, 0.25), &sine(997.0, 0.3, 0.25));
    let before = buf.clone();
    eq.process(&mut buf);

    let max_err = buf
        .iter()
        .zip(before.iter())
        .map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs())
        .fold(0.0_f64, f64::max);
    assert!(max_err < 1e-6, "旁路误差 {max_err}");
    assert_eq!(eq.magnitude_response(1000.0), 1.0);
}

/// 10 段频点表：严格单调 + 落在 20 Hz..21000 Hz。
#[test]
fn ten_band_grid_is_strictly_monotonic_and_in_range() {
    let f = band_frequencies(10, MIN_BAND_FREQ, MAX_BAND_FREQ);
    assert_eq!(f.len(), 10);
    for (i, v) in f.iter().enumerate() {
        assert!(
            *v >= MIN_BAND_FREQ && *v <= MAX_BAND_FREQ,
            "第 {i} 段 {v} 越界"
        );
        if i > 0 {
            assert!(f[i] > f[i - 1], "第 {i} 段没有严格单调: {f:?}");
        }
    }
    println!("10 段频点表: {f:?}");
}

/// +6 dB @ 1 kHz：`magnitude_response` 与**实测时域峰值**双重验证 ≈ 2x。
#[test]
fn plus_6db_at_1khz_is_2x_in_both_domains() {
    let mut eq = GraphicEq::new(FS, 1, 1);
    eq.set_band_freq(0, 1000.0);
    assert!((eq.gains()[0]).abs() < 1e-12);

    let applied = eq.set_gain(0, 6.0);
    assert!((applied - 6.0).abs() < 1e-12);

    // 1) 频域：1 kHz 处恰好是 +6 dB（× 1.9953）
    let mag = eq.magnitude_response(1000.0);
    assert!((mag - 1.995_262).abs() < 0.005, "|H(1kHz)| = {mag}");
    assert!((eq.magnitude_response_db(1000.0) - 6.0).abs() < 0.02);
    // 远离中心频点的地方几乎不动
    assert!(eq.magnitude_response_db(20_000.0).abs() < 0.6);

    // 2) 时域：等长的 1 kHz 正弦，稳态峰值比应等于 |H(1kHz)|
    let mut sig = sine(1000.0, 0.25, 1.0);
    eq.process(&mut sig);
    let tail = &sig[(FS as usize) / 2..];
    let ratio = peak(tail) / 0.25;
    assert!((ratio - mag).abs() < 0.02, "时域增益 {ratio} vs 频域 {mag}");
    println!("+6 dB@1kHz: |H| = {mag:.6}, 时域 = {ratio:.6}");

    // 3) 钳位
    assert!((eq.set_gain(0, 99.0) - MAX_GAIN_DB).abs() < 1e-12);
    assert!((eq.set_gain(0, -99.0) + MAX_GAIN_DB).abs() < 1e-12);
}

/// Q 改变时曲线形状符合预期：中心不动、肩部随 Q 变窄。
#[test]
fn q_multiplier_changes_curve_shape() {
    let mut wide = GraphicEq::new(FS, 1, 10);
    let mut narrow = GraphicEq::new(FS, 1, 10);
    wide.set_q_multiplier(0.7);
    narrow.set_q_multiplier(2.0);
    wide.set_gain(5, 12.0);
    narrow.set_gain(5, 12.0);

    let fc = wide.frequencies()[5];
    // RBJ peaking：中心频点的增益与 Q 无关
    assert!(
        (wide.magnitude_response_db(fc) - 12.0).abs() < 0.05,
        "{}",
        wide.magnitude_response_db(fc)
    );
    assert!(
        (narrow.magnitude_response_db(fc) - 12.0).abs() < 0.05,
        "{}",
        narrow.magnitude_response_db(fc)
    );

    // 肩上：Q 越大越窄 → 提升越少
    let shoulder = fc * 1.5;
    let (dw, dn) = (
        wide.magnitude_response_db(shoulder),
        narrow.magnitude_response_db(shoulder),
    );
    assert!(dn < dw - 1.0, "Q 未改变形状: wide={dw:.2} narrow={dn:.2}");
    println!("肩部 {shoulder:.0}Hz: Q×0.7 = {dw:.2} dB, Q×2.0 = {dn:.2} dB");
}

/// 全 0 增益与旁路等价（误差 < 1e-6）。
#[test]
fn all_zero_gains_equals_bypass() {
    let mut eq = GraphicEq::new(FS, 2, 10);
    let mut buf = interleave(&noise_frames(24_000, 0.5, 7), &sine(300.0, 0.5, 0.5));
    let before = buf.clone();
    eq.process(&mut buf);
    let max_err = buf
        .iter()
        .zip(before.iter())
        .map(|(a, b)| (f64::from(*a) - f64::from(*b)).abs())
        .fold(0.0_f64, f64::max);
    assert!(max_err < 1e-6, "全 0 增益误差 {max_err}");
    assert!(eq.is_transparent());
    assert_eq!(eq.magnitude_response_db(1000.0), 0.0);
}

/// 段数变化时曲线形状被保留（重采样，而不是截断/补零）。
#[test]
fn band_count_change_keeps_shape() {
    let mut eq = GraphicEq::new(FS, 1, 10);
    eq.set_gains(&[6.0; 10]);
    assert!(eq.gains().iter().all(|g| (*g - 6.0).abs() < 1e-12));
    eq.set_num_bands(31);
    assert_eq!(eq.num_bands(), 31);
    assert!(
        eq.gains().iter().all(|g| *g > 5.9 && *g < 6.1),
        "{:?}",
        eq.gains()
    );
    eq.set_num_bands(5);
    assert!(
        eq.gains().iter().all(|g| *g > 5.9 && *g < 6.1),
        "{:?}",
        eq.gains()
    );
    // 非均匀曲线（低频 +12 dB、高频 −12 dB）重采样后形状仍在。
    //
    // 探针频点**故意避开 Nyquist 附近**：RBJ 的 `w0 = 2πf0/fs` 走的是双线性映射，
    // 中心频点越靠近 Nyquist，同一个「模拟域频比」对应的数字域响应就越压缩。
    // 顶段 21 kHz（fs = 48 kHz，Nyquist 24 kHz）在 19 kHz 处只给出 −3.3 dB，
    // 拿模拟域公式去卡它会把「正确的数字滤波器」判成错。
    let mut tilted = GraphicEq::new(FS, 1, 10);
    let ramp: Vec<f64> = (0..10).map(|i| 12.0 - 2.4 * i as f64).collect();
    tilted.set_gains(&ramp);
    let (low, mid, high) = (
        tilted.magnitude_response_db(40.0),
        tilted.magnitude_response_db(1_000.0),
        tilted.magnitude_response_db(4_000.0),
    );
    assert!(low > 5.0, "低频段没被提升: {low}");
    assert!(high < -5.0, "高频段没被衰减: {high}");
    assert!(
        low > mid && mid > high && low > high + 10.0,
        "倾斜曲线形状丢了: 40Hz={low:.2} 1kHz={mid:.2} 4kHz={high:.2}"
    );
    println!("倾斜曲线: 40Hz = {low:.2} dB, 1kHz = {mid:.2} dB, 4kHz = {high:.2} dB");
}
