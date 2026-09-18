//! 响度归一化验收：收敛性、真峰值、非对称时间常数、变化率、静音行为、鲁棒性。
// 测试代码里的数值转换（`i as f64`、`f64 as f32`、`secs * FS as usize`）与精确浮点比较
// 都是刻意的（见 src/lib.rs 顶部的 lint 说明），集成测试是独立 crate，需要在这里再声明一次。
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::float_cmp)]

mod common;

use common::*;
use vdev_dsp::{Leveling, LoudnessMode};

/// 目标 −20 LUFS、ceiling −1 dBFS 的默认配置下，跑几秒后输出响度应收敛到 ±1 dB。
#[test]
fn converges_to_target_loudness_within_1db() {
    let mut lv = Leveling::new(FS, 1);
    lv.set_target_loudness(-20.0);
    let mut sig = sine(997.0, 0.5, 6.0);
    lv.process(&mut sig);

    // 用**全新的**响度计测输出尾段（跳过收敛过程）
    let tail = &sig[4 * FS as usize..];
    let out = measure_loudness(tail, 1, LoudnessMode::KWeighted);
    let gain = lv.gain_db();
    println!("输出响度 {out:.3} LUFS（目标 −20），稳定增益 {gain:.3} dB");
    assert!(
        (out + 20.0).abs() < 1.0,
        "输出响度 {out:.3} LUFS 偏离目标超过 1 dB"
    );
    assert!((gain + 11.0).abs() < 1.5, "增益 {gain:.3} dB 与预期不符");
}

/// 输出真峰值（4x 过采样）不越过 ceiling。
#[test]
fn true_peak_stays_below_ceiling() {
    let ceiling = 10f64.powf(-1.0 / 20.0); // −1 dBFS
    let mut lv = Leveling::new(FS, 1);
    // 把目标抬高，让伺服真的往大推，从而触发限幅
    lv.set_target_loudness(-3.0);
    let mut sig = sine(997.0, 0.99, 4.0);
    lv.process(&mut sig);

    let tail = &sig[2 * FS as usize..];
    let tp = true_peak(tail);
    let disc = peak(tail);
    println!("离散峰值 {disc:.6}，真峰值 {tp:.6}，ceiling {ceiling:.6}");
    assert!(tp <= ceiling + 1e-6, "真峰值 {tp} 越过 ceiling {ceiling}");
}

/// 逐样本跟踪一段信号，返回（最大的「每样本归一化增益步长」, 最大总位移, 末态目标增益）。
///
/// 归一化步长 = |本样本增益变化| / |本样本开始时的剩余差距|。对一阶平滑
/// `g += (target - g) * coef` 来说它恒等于 `coef`（不被 dB/s 硬限截断时），
/// 所以它就是平滑系数本身——比「同样时间里走了目标的百分之多少」干净得多：
/// 后者会被测量端的滞后（400 ms 滑窗）稀释掉两个方向的差别。
fn track_servo(base: &Leveling, amp: f64, secs: f64) -> ServoTrace {
    let mut lv = base.clone();
    let n = (FS * secs) as usize;
    let g_from = lv.gain_db();
    let mut prev_gain = g_from;
    let mut worst_ratio = 0.0_f64;
    let mut max_move = 0.0_f64;
    let mut buf = [0.0_f32; 1];
    for i in 0..n {
        buf[0] = (amp * (2.0 * core::f64::consts::PI * 997.0 * i as f64 / FS).sin()) as f32;
        lv.process(&mut buf);
        let g = lv.gain_db();
        // 目标增益在本次 process() 里刚更新，本样本用的就是它
        let avail = lv.target_gain_db() - prev_gain;
        if avail.abs() > 1.0 {
            worst_ratio = worst_ratio.max((g - prev_gain).abs() / avail.abs());
        }
        max_move = max_move.max((g - g_from).abs());
        prev_gain = g;
    }
    ServoTrace {
        coef_estimate: worst_ratio,
        moved_db: max_move,
        target_db: lv.target_gain_db(),
        end_gain_db: lv.gain_db(),
    }
}

struct ServoTrace {
    /// 观测到的平滑系数（≈ `attack_coef` 或 `release_coef`）。
    coef_estimate: f64,
    /// 整个窗口里增益走过的最大位移（dB）。
    moved_db: f64,
    /// 末态目标增益（dB）。
    target_db: f64,
    /// 末态实际增益（dB）。
    end_gain_db: f64,
}

/// 输入变大 → 增益下降；输入变小 → 增益回升，且**下降明显快于上升**。
///
/// 直接比较两个方向的一阶平滑系数：它们就是 attack / release 的每样本系数，
/// 期望比值 ≈ `release_ms / attack_ms` = 250 / 5 = 50，这里只要求 > 10 倍。
/// 独立于平滑的 dB/s 硬限由 `gain_change_rate_is_bounded_for_any_block_size` 单独测，
/// 本用例把它放大到几乎不生效，以免污染对平滑系数的观测。
#[test]
fn gain_drops_fast_and_recovers_slowly() {
    let mut base = Leveling::new(FS, 1);
    base.set_target_loudness(-20.0);
    base.set_max_gain_rate_db_per_sec(10_000.0);

    // 先把状态推到「正在压增益」的位置
    base.process(&mut sine(997.0, 0.5, 2.0));
    let g0 = base.gain_db();
    assert!(g0 < -3.0, "基准增益没有压下来: {g0:.2} dB");

    // 测量端是 400 ms 滑窗，所以窗口取 1 s，保证两个方向的目标都真的动过
    let down = track_servo(&base, 1.0, 1.0); // 更响 → 继续下压
    let up = track_servo(&base, 0.05, 1.0); // 更轻 → 缓慢回升
    let tau = 1.0 / down.coef_estimate / FS * 1.0e3;
    let tau_up = 1.0 / up.coef_estimate / FS * 1.0e3;
    println!(
        "下压：目标 {:.2} dB、增益 {:.2} → {:.2} dB、每样本步长 {:.3e}（≈τ {tau:.1} ms）",
        down.target_db, g0, down.end_gain_db, down.coef_estimate
    );
    println!(
        "回升：目标 {:.2} dB、增益 {:.2} → {:.2} dB、每样本步长 {:.3e}（≈τ {tau_up:.1} ms）",
        up.target_db, g0, up.end_gain_db, up.coef_estimate
    );
    println!(
        "系数比 {:.1}x：下压走了 {:.2} dB，回升走了 {:.2} dB",
        down.coef_estimate / up.coef_estimate,
        down.moved_db,
        up.moved_db
    );

    assert!(
        down.moved_db > 3.0,
        "响信号没有把增益继续压下去: {:.3} dB",
        down.moved_db
    );
    assert!(
        up.moved_db > 1.0,
        "轻信号没有让增益回升: {:.3} dB",
        up.moved_db
    );
    assert!(
        up.target_db > down.target_db + 5.0,
        "两个方向的目标没有拉开: {:.2} vs {:.2}",
        down.target_db,
        up.target_db
    );
    assert!(
        down.coef_estimate > up.coef_estimate * 10.0,
        "非对称性不足：下压系数 {:.3e} vs 回升系数 {:.3e}",
        down.coef_estimate,
        up.coef_estimate
    );
}

/// 增益变化率有界：与 buffer 划分无关（同样的信号，按不同块长喂进去，每块的变化都在上限内）。
#[test]
fn gain_change_rate_is_bounded_for_any_block_size() {
    for block in [64usize, 120, 480, 1024] {
        let mut lv = Leveling::new(FS, 1);
        lv.set_max_gain_rate_db_per_sec(200.0);
        let mut sig = sine(997.0, 0.5, 1.0);
        let mut prev = lv.gain_db();
        let mut worst = 0.0_f64;
        for chunk in sig.chunks_mut(block) {
            lv.process(chunk);
            let d = (lv.gain_db() - prev).abs();
            worst = worst.max(d);
            prev = lv.gain_db();
        }
        let allowed = 200.0 * (block as f64 / FS) + 1e-9;
        assert!(
            worst <= allowed,
            "block={block} 单块变化 {worst} dB > 上限 {allowed}"
        );
        println!("block={block}: 单块最大变化 {worst:.5} dB（上限 {allowed:.5}）");
    }
}

/// 纯静音段：安静提升上限衰减回 0，增益回到 0 dB 附近，且**不会**去放大底噪。
#[test]
fn silence_decays_quiet_floor_and_returns_to_unity() {
    let mut lv = Leveling::new(FS, 1);
    // 8 s 的极轻信号，把安静上限抬起来
    for _ in 0..40 {
        lv.process(&mut sine(997.0, 0.004, 0.2));
    }
    let floor_loud = lv.gain_floor_db();
    let gain_loud = lv.gain_db();
    assert!(floor_loud > 5.0, "安静上限没抬起来: {floor_loud}");
    assert!(gain_loud > 5.0, "轻信号没被提升: {gain_loud}");

    // 2 s 纯静音
    let mut floor_prev = floor_loud;
    let mut monotone = true;
    for _ in 0..10 {
        lv.process(&mut vec![0.0_f32; (FS * 0.2) as usize]);
        if lv.gain_floor_db() > floor_prev + 1e-12 {
            monotone = false;
        }
        floor_prev = lv.gain_floor_db();
    }
    println!(
        "安静上限 {floor_loud:.3} → {:.3}；增益 {gain_loud:.3} → {:.3}",
        lv.gain_floor_db(),
        lv.gain_db()
    );
    assert!(monotone, "静音期间上限必须单调不增");
    assert!(
        lv.gain_floor_db() == 0.0,
        "静音后上限应为 0，实测 {}",
        lv.gain_floor_db()
    );
    assert!(
        lv.gain_db().abs() < 1.0,
        "静音后增益应回到 0 dB 附近，实测 {}",
        lv.gain_db()
    );
}

/// 极端/脏数据不产生 NaN/Inf，也不 panic（实时路径不允许 panic 分支）。
#[test]
fn extreme_inputs_never_produce_non_finite_output() {
    let mut lv = Leveling::new(FS, 2);
    let mut cases: Vec<Vec<f32>> = vec![
        vec![0.0; 4800],
        vec![1.0e9; 4800],
        vec![-1.0e9; 4800],
        vec![f32::NAN; 4800],
        vec![f32::INFINITY; 4800],
        vec![f32::NEG_INFINITY; 4800],
    ];
    let mut mixed: Vec<f32> = Vec::new();
    for i in 0..4800 {
        mixed.push(match i % 6 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => 1.0e30,
            3 => -1.0e30,
            4 => 0.5,
            _ => -0.5,
        });
    }
    cases.push(mixed);

    for (i, case) in cases.iter_mut().enumerate() {
        lv.process(case);
        assert!(all_finite(case), "case {i} 输出出现 NaN/Inf");
    }
    // 脏数据之后仍然能正常工作
    let mut sig = sine(997.0, 0.5, 1.0);
    lv.process(&mut sig);
    assert!(all_finite(&sig));
    assert!(lv.gain_db().is_finite());
}

/// RMS 对照模式：能收敛到目标 dBFS，用于口径对照。
#[test]
fn rms_mode_converges_to_target_dbfs() {
    let mut lv = Leveling::new(FS, 1);
    lv.set_mode(LoudnessMode::Rms);
    lv.set_target_loudness(-30.0); // 在 Rms 模式下就是 −30 dBFS
    let mut sig = sine(997.0, 0.5, 6.0);
    lv.process(&mut sig);
    let tail = &sig[4 * FS as usize..];
    let out = measure_rms_db(tail, 1, LoudnessMode::Rms);
    println!("Rms 模式输出 {out:.3} dBFS（目标 −30）");
    assert!(
        (out + 30.0).abs() < 1.5,
        "Rms 模式输出 {out:.3} dBFS 偏离目标"
    );
}
