//! vdev-dsp —— 实时音频 DSP：N 段图示均衡器 + 响度归一化 + 前瞻真峰值限幅。
//!
//! **零第三方依赖**（只用 `core`/`std`），实时路径无分配、无锁、无 panic 分支。
//!
//! # 算法来源（全部为公开文献与标准）
//!
//! | 模块 | 算法依据 |
//! | --- | --- |
//! | [`biquad`] | RBJ Audio EQ Cookbook（biquad 系数设计公式）；transposed direct-form II 为通用滤波器结构 |
//! | [`graphic_eq`] | 多段图示均衡器的通用做法：对数等间距频点、段间交叠取 `Q = √r / (r − 1)`、单段增益钳位 |
//! | [`loudness`] | ITU-R BS.1770 的 K 加权与门控响度（**本实现为近似，非合规 LUFS 表**） |
//! | [`leveling`] | 自动增益控制 / 响度归一化的通用结构 |
//! | [`limiter`] | 经典 look-ahead peak limiter；ITU-R BS.1770 附录 2 的过采样真峰值测量思路 |
//! | [`chain`] | 固定处理顺序由本 crate 定义 |
//!
//! 本 crate 为独立实现，以 **MIT** 分发，不含第三方代码或数据。
//!
//! # 六处设计取舍（逐条都有单测）
//!
//! | 编号 | 常见朴素做法 | 本 crate 做法 | 落点 |
//! | --- | --- | --- | --- |
//! | A | 侧链 120 Hz 一阶 HPF 后的**裸 RMS**（无频率加权、无门控） | 近似 **K 加权**（high shelf + 高通，按实际采样率用 RBJ 重算）+ **绝对/相对双门限**短时响度；保留 `Rms` 模式做对照 | [`loudness`] |
//! | B | 以 **buffer** 为粒度 alpha 混合增益，衰减速度随块长变化 | **逐样本**一阶平滑（attack 快 / release 慢）+ 独立的 **dB/s 变化率上限**，与 buffer 划分无关 | [`leveling`] |
//! | C | **逐声道独立**检测与增益，立体声像会被拉扯 | **声道联动**：检测器跨声道聚合、增益是所有声道共用的同一标量 | [`loudness`]、[`leveling`]、[`limiter`] |
//! | D | 只做事后 ceiling 判定 + 慢慢收目标增益，**瞬时峰值无保护** | **前瞻式 brickwall**（默认 2 ms 延迟线 + 滑动最小值 + 平滑释放），逐样本严格 `|out| ≤ ceiling`；另留真峰值余量，用 4x 过采样验证 | [`limiter`] |
//! | E | 设参重算系数但保留滤波器状态，低频段系数突变会爆音 | **系数交叉淡化**（新老两套系数各自带状态、输出线性混合） | [`biquad::Section`] |
//! | F | 只有「跑起来没崩」级别的验证 | 频响曲线、响度收敛、真峰值、增益变化率、静音行为**全部可客观断言** | 各模块 `#[cfg(test)]` + `tests/` |
//!
//! # 快速上手
//!
//! ```
//! use vdev_dsp::Chain;
//!
//! let mut chain = Chain::new(48_000.0, 2, 10); // 48 kHz / 立体声 / 10 段
//! chain.eq_mut().set_gain(5, 6.0);             // 第 6 段 +6 dB
//! chain.leveling_mut().set_target_loudness(-18.0);
//! chain.eq_mut().set_smoothing_samples(240);   // 系数交叉淡化 5 ms
//!
//! let mut buf = vec![0.0_f32; 960];            // 交错双声道，480 帧
//! for (i, s) in buf.iter_mut().enumerate() {
//!     *s = (0.3 * ((i / 2) as f32 * 0.01).sin()).clamp(-1.0, 1.0);
//! }
//! chain.process(&mut buf);
//! assert!(buf.iter().all(|s| s.is_finite()));
//! ```
//!
//! 只想知道 EQ 在某频点的实测增益时，直接问 [`graphic_eq::GraphicEq::magnitude_response`]，
//! 不需要跑正弦。

// 下面这些 lint 在本 crate 里是**故意的**，逐条说明理由：
// - cast_precision_loss / cast_possible_truncation：DSP 里「采样计数 → f64」「f64 → f32」
//   是设计的一部分（内部 f64 累加、出口回单精度），量级远达不到精度损失的边界。
// - unreadable_literal：BS.1770 / RBJ 的系数常量必须保持原始位数。
// - similar_names / many_single_char_names：滤波器公式里 `b0/b1/b2/a1/a2`、`x/y/s1/s2`
//   是领域通用写法，改名反而降低可读性。
// - float_cmp：多处**故意**做精确浮点比较（单位系数判定、旁路逐位恒等断言）。
// - must_use_candidate：本 crate 的 getter 全是纯读取，忽略返回值无害。
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::float_cmp)]
#![allow(clippy::must_use_candidate)]

pub mod biquad;
pub mod chain;
pub mod graphic_eq;
pub mod leveling;
pub mod limiter;
pub mod loudness;

pub use biquad::{Coeffs, Section, Sos, State};
pub use chain::Chain;
pub use graphic_eq::{
    band_frequencies, band_q, resample_gains, GraphicEq, MAX_BAND_FREQ, MAX_GAIN_DB, MAX_Q,
    MIN_BAND_FREQ, MIN_Q,
};
pub use leveling::Leveling;
pub use limiter::{true_peak_4x, true_peak_oversampled, Limiter};
pub use loudness::{LoudnessMeter, LoudnessMode};

/// 默认采样率（44.1 kHz 是历史默认值，这里取 48 kHz）。
pub const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;

/// 默认声道数。
pub const DEFAULT_CHANNELS: usize = 2;

/// 默认图示 EQ 段数。
pub const DEFAULT_BANDS: usize = graphic_eq::DEFAULT_BANDS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        assert_eq!(DEFAULT_BANDS, 10);
        assert_eq!(DEFAULT_CHANNELS, 2);
        let chain = Chain::new(DEFAULT_SAMPLE_RATE, DEFAULT_CHANNELS, DEFAULT_BANDS);
        assert_eq!(chain.eq().num_bands(), 10);
        assert_eq!(chain.eq().frequencies().len(), 10);
    }
}
