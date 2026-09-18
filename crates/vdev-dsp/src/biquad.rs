//! RBJ（Audio EQ Cookbook）二阶节与 SOS 级联。
//!
//! # 算法依据
//!
//! | 本模块 | 依据 |
//! | --- | --- |
//! | [`Coeffs`] | RBJ Audio EQ Cookbook 的 low/high shelf、peaking、LP/HP 系数公式 |
//! | [`Coeffs::process`] | transposed direct-form II（通用数字滤波器结构） |
//! | [`State`]、[`Section`]、[`Sos`] | 二阶节（SOS）级联的常规工程写法 |
//!
//! # 实现要点
//!
//! 1. **精度**：系数与状态一律 f64（若改用 `f32`，低频 shelf 的
//!    `(a+1)-(a-1)cos(w0)` 这类抵消项会掉到 7 位有效数字上）。设参时用 f64 算系数，
//!    运行时累加也用 f64，仅在缓冲区出入口做 f32 转换。
//! 2. **NaN 防御**：所有设计函数对 `f0/fs/q/gain` 做取值域收敛（非有限值、超 Nyquist、
//!    除零），保证**任何输入都返回有限系数**。
//! 3. **系数交叉淡化**（取舍 E）：[`Section`] 在系数变更时用新老两套系数**各自维持
//!    滤波器状态**、输出线性交叉淡化，而不是「算完新系数、留着老状态」——
//!    后者在低频段（状态里存着大量能量）会把系数突变直接变成听得到的爆音。
//! 4. **单位系数**：`bypass` 用真正的单位系数（`b0=1`，其余 0），因此旁路是**逐位恒等**，
//!    而不是「近似恒等」。

use core::f64::consts::PI;

/// 归一化（a0 = 1）的二阶节系数。
///
/// 传输函数：`H(z) = (b0 + b1 z^-1 + b2 z^-2) / (1 + a1 z^-1 + a2 z^-2)`。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    /// 分子直通项。
    pub b0: f64,
    /// 分子一阶项。
    pub b1: f64,
    /// 分子二阶项。
    pub b2: f64,
    /// 分母一阶项。
    pub a1: f64,
    /// 分母二阶项。
    pub a2: f64,
}

/// 一个二阶节的 transposed direct form II 状态。
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct State {
    s1: f64,
    s2: f64,
}

impl State {
    /// 全零状态。
    pub const ZERO: Self = Self { s1: 0.0, s2: 0.0 };

    /// 清空状态（`reset()` / 重新初始化时用）。
    pub fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }
}

/// 把任意 dB 增益收敛到可计算范围（避免 `powf` 溢出成 inf）。
fn amp_from_db(gain_db: f64) -> f64 {
    let g = if gain_db.is_finite() {
        gain_db.clamp(-60.0, 60.0)
    } else {
        0.0
    };
    10f64.powf(g / 40.0)
}

/// 把 `f0`（Hz）与 `fs`（Hz）收敛成合法的数字角频率 `w0`。
///
/// 任何非有限/越界的输入都被夹到 `(0, pi)` 内，因此调用方不需要做前置检查。
fn omega_of(f0: f64, fs: f64) -> f64 {
    let fs = if fs.is_finite() && fs > 0.0 {
        fs
    } else {
        48_000.0
    };
    let nyquist = fs * 0.5;
    let f = if f0.is_finite() {
        f0.clamp(f64::EPSILON.max(1.0e-3), nyquist * 0.999_999)
    } else {
        1_000.0
    };
    (PI * 2.0 * f / fs).clamp(1.0e-9, PI * 0.999_999)
}

/// 品质因数收敛：`q` 必须为正有限值，否则退回 0.7071。
fn sanitize_q(q: f64) -> f64 {
    if q.is_finite() && q > 1.0e-3 {
        q.clamp(1.0e-3, 1.0e3)
    } else {
        core::f64::consts::FRAC_1_SQRT_2
    }
}

impl Coeffs {
    /// 单位系数：`y = x`，逐位恒等。
    pub const IDENTITY: Self = Self {
        b0: 1.0,
        b1: 0.0,
        b2: 0.0,
        a1: 0.0,
        a2: 0.0,
    };

    /// 是否是单位系数（逐位比较，**故意不做容差**：旁路判定必须严格）。
    #[must_use]
    pub fn is_identity(self) -> bool {
        self.b0 == 1.0 && self.b1 == 0.0 && self.b2 == 0.0 && self.a1 == 0.0 && self.a2 == 0.0
    }

    /// 归一化；`a0` 非有限或为 0 时退回单位系数（宁可不滤波，也不产生 NaN）。
    fn normalized(b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) -> Self {
        if !(a0.is_finite() && a0.abs() > 1.0e-30) {
            return Self::IDENTITY;
        }
        let inv = 1.0 / a0;
        let c = Self {
            b0: b0 * inv,
            b1: b1 * inv,
            b2: b2 * inv,
            a1: a1 * inv,
            a2: a2 * inv,
        };
        if c.b0.is_finite()
            && c.b1.is_finite()
            && c.b2.is_finite()
            && c.a1.is_finite()
            && c.a2.is_finite()
        {
            c
        } else {
            Self::IDENTITY
        }
    }

    /// RBJ low shelf。`slope` 为 shelf 斜率 S（1.0 为常用默认）。
    #[must_use]
    pub fn low_shelf(f0: f64, gain_db: f64, slope: f64, fs: f64) -> Self {
        let w0 = omega_of(f0, fs);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let a = amp_from_db(gain_db);
        let s = if slope.is_finite() {
            slope.clamp(0.05, 10.0)
        } else {
            1.0
        };
        let alpha = 0.5 * sin_w0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            a * ((a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
            2.0 * a * ((a - 1.0) - (a + 1.0) * cos_w0),
            a * ((a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
            (a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
            -2.0 * ((a - 1.0) + (a + 1.0) * cos_w0),
            (a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// RBJ high shelf。
    #[must_use]
    pub fn high_shelf(f0: f64, gain_db: f64, slope: f64, fs: f64) -> Self {
        let w0 = omega_of(f0, fs);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let a = amp_from_db(gain_db);
        let s = if slope.is_finite() {
            slope.clamp(0.05, 10.0)
        } else {
            1.0
        };
        let alpha = 0.5 * sin_w0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).max(0.0).sqrt();
        let two_sqrt_a_alpha = 2.0 * a.sqrt() * alpha;
        Self::normalized(
            a * ((a + 1.0) + (a - 1.0) * cos_w0 + two_sqrt_a_alpha),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * cos_w0),
            a * ((a + 1.0) + (a - 1.0) * cos_w0 - two_sqrt_a_alpha),
            (a + 1.0) - (a - 1.0) * cos_w0 + two_sqrt_a_alpha,
            2.0 * ((a - 1.0) - (a + 1.0) * cos_w0),
            (a + 1.0) - (a - 1.0) * cos_w0 - two_sqrt_a_alpha,
        )
    }

    /// RBJ peaking（参数均衡）。
    #[must_use]
    pub fn peaking(f0: f64, gain_db: f64, q: f64, fs: f64) -> Self {
        let w0 = omega_of(f0, fs);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let a = amp_from_db(gain_db);
        let alpha = sin_w0 / (2.0 * sanitize_q(q));
        Self::normalized(
            1.0 + alpha * a,
            -2.0 * cos_w0,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * cos_w0,
            1.0 - alpha / a,
        )
    }

    /// RBJ 二阶低通。
    #[must_use]
    pub fn low_pass(f0: f64, q: f64, fs: f64) -> Self {
        let w0 = omega_of(f0, fs);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * sanitize_q(q));
        let b0 = (1.0 - cos_w0) * 0.5;
        Self::normalized(
            b0,
            1.0 - cos_w0,
            b0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// RBJ 二阶高通（K 加权第二级、侧链 HPF 用得上）。
    #[must_use]
    pub fn high_pass(f0: f64, q: f64, fs: f64) -> Self {
        let w0 = omega_of(f0, fs);
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * sanitize_q(q));
        let b0 = (1.0 + cos_w0) * 0.5;
        Self::normalized(
            b0,
            -(1.0 + cos_w0),
            b0,
            1.0 + alpha,
            -2.0 * cos_w0,
            1.0 - alpha,
        )
    }

    /// 跑一拍 transposed direct form II，并顺手冲洗反规范化（denormal）状态。
    #[inline]
    fn run(&self, st: &mut State, x: f64) -> f64 {
        let y = self.b0.mul_add(x, st.s1);
        st.s1 = self.b1.mul_add(x, st.s2) - self.a1 * y;
        st.s2 = self.b2 * x - self.a2 * y;
        // 反规范化（denormal）冲洗：极小的状态值对听感无贡献，但会让 x86 慢 10~100 倍。
        if st.s1.abs() < 1.0e-30 {
            st.s1 = 0.0;
        }
        if st.s2.abs() < 1.0e-30 {
            st.s2 = 0.0;
        }
        y
    }

    /// 在数字角频率 `omega`（rad/sample）处的模响应 `|H(e^{jw})|`。
    ///
    /// 这是**数字域的精确响应**，不是模拟原型的近似——单测直接用它对频响曲线下断言。
    #[must_use]
    pub fn magnitude(&self, omega: f64) -> f64 {
        if omega <= 0.0 || !omega.is_finite() {
            return self.gain_at_zero();
        }
        let (s1, c1) = omega.sin_cos();
        let (s2, c2) = (2.0 * omega).sin_cos();
        // z^-1 = cos(w) - j sin(w)，z^-2 = cos(2w) - j sin(2w)
        let num_re = self.b0 + self.b1 * c1 + self.b2 * c2;
        let num_im = -(self.b1 * s1 + self.b2 * s2);
        let den_re = 1.0 + self.a1 * c1 + self.a2 * c2;
        let den_im = -(self.a1 * s1 + self.a2 * s2);
        let num = num_re.hypot(num_im);
        let den = den_re.hypot(den_im);
        if den > 1.0e-300 {
            num / den
        } else {
            0.0
        }
    }

    /// `omega -> 0` 的直流增益（`magnitude(0)` 走不了公式，单列出来）。
    #[must_use]
    pub fn gain_at_zero(&self) -> f64 {
        let num = (self.b0 + self.b1 + self.b2).abs();
        let den = (1.0 + self.a1 + self.a2).abs();
        if den > 1.0e-300 {
            num / den
        } else {
            0.0
        }
    }

    /// dB 表示的模响应。
    #[must_use]
    pub fn magnitude_db(&self, omega: f64) -> f64 {
        let m = self.magnitude(omega);
        if m > 0.0 {
            20.0 * m.log10()
        } else {
            f64::NEG_INFINITY
        }
    }
}

/// 带系数交叉淡化的二阶节。
///
/// 设参时**不丢状态**：新系数接着旧状态跑，旧系数也接着旧状态跑，两路输出在
/// `xfade_len` 个样本内线性混合。这样即使新老系数差异很大（比如低频段 +12 dB 跳变），
/// 输出也是连续的——相邻样本差有界（见改进点 E 的单测）。
#[derive(Clone, Copy, Debug)]
pub struct Section {
    cur: Coeffs,
    cur_state: State,
    prev: Coeffs,
    prev_state: State,
    xfade_len: u32,
    xfade_left: u32,
    /// 「上一次改系数之后还没有跑过样本」——同一批（没有 process 间隔）里的多次
    /// `set_coeffs` 必须**只更新目标、不覆盖正在淡出的旧支路**：否则第二次调用会把
    /// `prev` 覆盖成刚换上的 `cur`，两路一模一样，交叉淡化退化成瞬时切换，
    /// 而瞬时切换 `b0` 会直接把 `(b0_new - b0_old)·x` 加到输出上（实测可到 0.2 的台阶）。
    same_batch: bool,
}

impl Default for Section {
    fn default() -> Self {
        Self::identity()
    }
}

impl Section {
    /// 单位系数的节（什么都不做）。
    #[must_use]
    pub fn identity() -> Self {
        Self {
            cur: Coeffs::IDENTITY,
            cur_state: State::ZERO,
            prev: Coeffs::IDENTITY,
            prev_state: State::ZERO,
            xfade_len: 0,
            xfade_left: 0,
            same_batch: false,
        }
    }

    /// 用给定系数构造（不做淡化）。
    #[must_use]
    pub fn new(c: Coeffs) -> Self {
        let mut s = Self::identity();
        s.cur = c;
        s.prev = c;
        s
    }

    /// 当前目标系数。
    #[must_use]
    pub fn coeffs(&self) -> Coeffs {
        self.cur
    }

    /// 是否完全恒等（目标与正在淡出的旧系数都是单位系数）。
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.cur.is_identity() && (self.xfade_left == 0 || self.prev.is_identity())
    }

    /// 是否正在交叉淡化。
    #[must_use]
    pub fn is_fading(&self) -> bool {
        self.xfade_left > 0
    }

    /// 清状态、结束淡化（用于流重启/断流后重新起播）。
    pub fn reset(&mut self) {
        self.cur_state.reset();
        self.prev_state.reset();
        self.prev = self.cur;
        self.xfade_left = 0;
        self.xfade_len = 0;
        self.same_batch = false;
    }

    /// 换系数。`xfade_samples == 0` 表示立即切换（状态保留，不淡化）。
    ///
    /// 语义：把「当前系数 + 当前状态」整体存为旧的一路，新系数从同一份状态继续跑。
    /// 因此无论何时改参数，滤波器的**能量状态都不丢**，输出只可能连续变化。
    ///
    /// **同一批内的多次调用会被合并**（`same_batch`）：调用方在一次 `process()` 之前
    /// 连着改好几个参数时，只有第一次会把旧系数存成「淡出支路」，后续调用只换目标。
    /// 这样一批设参只产生一次交叉淡化，而不是「最后一次生效 + 前面几次瞬时切换」。
    pub fn set_coeffs(&mut self, c: Coeffs, xfade_samples: u32) {
        if c == self.cur && self.xfade_left == 0 {
            return; // 参数没变：不动状态，也不重启淡化
        }
        if !self.same_batch {
            self.prev = self.cur;
            self.prev_state = self.cur_state;
        }
        self.cur = c;
        self.same_batch = true;
        if xfade_samples == 0 {
            self.xfade_len = 0;
            self.xfade_left = 0;
        } else {
            self.xfade_len = xfade_samples;
            self.xfade_left = xfade_samples;
        }
    }

    /// 处理一个样本。
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        self.same_batch = false; // 跑过样本 → 下一批设参要重新建立淡出支路
        let y = self.cur.run(&mut self.cur_state, x);
        if self.xfade_left == 0 {
            return y;
        }
        let y_prev = self.prev.run(&mut self.prev_state, x);
        self.xfade_left -= 1;
        // t: 0 -> 1，`xfade_left == 0` 时恰好等于 1（即输出纯新系数结果）
        let span = f64::from(self.xfade_len);
        let t = if span > 0.0 {
            1.0 - f64::from(self.xfade_left) / span
        } else {
            1.0
        };
        y_prev + (y - y_prev) * t
    }
}

/// 一串串联的二阶节（SOS 级联）。
#[derive(Clone, Debug, Default)]
pub struct Sos {
    sections: Vec<Section>,
}

impl Sos {
    /// `n` 个单位节。
    #[must_use]
    pub fn new(n: usize) -> Self {
        Self {
            sections: vec![Section::identity(); n],
        }
    }

    /// 节数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.sections.len()
    }

    /// 是否没有节。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sections.is_empty()
    }

    /// 全部节是否恒等。
    #[must_use]
    pub fn is_identity(&self) -> bool {
        self.sections.iter().all(Section::is_identity)
    }

    /// 第 `i` 个节。
    #[must_use]
    pub fn section(&self, i: usize) -> Option<&Section> {
        self.sections.get(i)
    }

    /// 清空所有状态。
    pub fn reset(&mut self) {
        for s in &mut self.sections {
            s.reset();
        }
    }

    /// 整体替换系数序列；长度不一致时（重新划分节数）立即切换、不做淡化。
    pub fn set_coeffs(&mut self, coeffs: &[Coeffs], xfade_samples: u32) {
        if coeffs.len() != self.sections.len() {
            self.sections.clear();
            self.sections
                .extend(coeffs.iter().copied().map(Section::new));
            return;
        }
        for (s, c) in self.sections.iter_mut().zip(coeffs.iter()) {
            s.set_coeffs(*c, xfade_samples);
        }
    }

    /// 级联处理一个样本。
    #[inline]
    pub fn process(&mut self, x: f64) -> f64 {
        let mut v = x;
        for s in &mut self.sections {
            v = s.process(v);
        }
        v
    }

    /// 级联的模响应（各节相乘）。
    #[must_use]
    pub fn magnitude(&self, omega: f64) -> f64 {
        self.sections
            .iter()
            .map(|s| s.coeffs().magnitude(omega))
            .product()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    fn omega_at(f: f64) -> f64 {
        PI * 2.0 * f / FS
    }

    #[test]
    fn identity_coeffs_are_bitwise_transparent() {
        let c = Coeffs::IDENTITY;
        let mut st = State::ZERO;
        for i in 0..1000 {
            let x = ((i as f64) * 0.01).sin();
            assert_eq!(c.run(&mut st, x), x);
        }
        assert!(c.is_identity());
    }

    #[test]
    fn peaking_zero_gain_is_unity_within_roundoff() {
        // 0 dB 的 peaking：b 与 a 在数学上完全相同，只有最后一次浮点除法会留下 ~1e-16 偏差
        for &f0 in &[100.0, 1000.0, 8000.0] {
            let c = Coeffs::peaking(f0, 0.0, 1.4, FS);
            let m = c.magnitude(omega_at(f0));
            assert!((m - 1.0).abs() < 1e-12, "f0={f0} |H|={m}");
        }
    }

    #[test]
    fn peaking_boost_hits_nominal_gain_at_center() {
        for &g in &[-12.0, -6.0, 3.0, 6.0, 12.0] {
            let c = Coeffs::peaking(1000.0, g, 1.4, FS);
            let db = c.magnitude_db(omega_at(1000.0));
            assert!((db - g).abs() < 1e-6, "g={g} 实测={db}");
        }
    }

    #[test]
    fn shelves_hit_nominal_gain_away_from_transition() {
        let low = Coeffs::low_shelf(120.0, 6.0, 1.0, FS);
        assert!((low.magnitude_db(omega_at(10.0)) - 6.0).abs() < 0.1);
        assert!(low.magnitude_db(omega_at(8000.0)).abs() < 0.2);
        let high = Coeffs::high_shelf(8000.0, -6.0, 1.0, FS);
        assert!((high.magnitude_db(omega_at(20000.0)) + 6.0).abs() < 0.2);
        assert!(high.magnitude_db(omega_at(200.0)).abs() < 0.2);
    }

    #[test]
    fn high_pass_kills_dc() {
        let c = Coeffs::high_pass(38.0, 0.5, FS);
        assert!(c.gain_at_zero() < 1e-6);
        assert!((c.magnitude_db(omega_at(1000.0))).abs() < 0.05);
    }

    #[test]
    fn degenerate_inputs_never_produce_non_finite_coeffs() {
        let cases = [
            (f64::NAN, f64::NAN, f64::NAN, FS),
            (0.0, 0.0, 0.0, 0.0),
            (-100.0, 1e9, -1e9, -48_000.0),
            (1e9, -1e9, f64::INFINITY, f64::INFINITY),
        ];
        for (f0, g, q, fs) in cases {
            for c in [
                Coeffs::peaking(f0, g, q, fs),
                Coeffs::low_shelf(f0, g, q, fs),
                Coeffs::high_shelf(f0, g, q, fs),
                Coeffs::low_pass(f0, q, fs),
                Coeffs::high_pass(f0, q, fs),
            ] {
                for v in [c.b0, c.b1, c.b2, c.a1, c.a2] {
                    assert!(v.is_finite(), "非有限系数 {c:?}");
                }
                assert!(c.magnitude(omega_at(1000.0)).is_finite());
            }
        }
    }

    #[test]
    fn coefficient_change_keeps_output_continuous() {
        // 运行中把系数从单位换到 +12 dB@120Hz（状态里塞满了低频能量），
        // 淡化路径必须保证相邻样本差有界。
        let mut sec = Section::identity();
        let mut prev_out = 0.0;
        let mut max_jump = 0.0_f64;
        for i in 0..48_000 {
            let x = 0.8 * (2.0 * PI * 60.0 * i as f64 / FS).sin();
            if i == 12_000 {
                sec.set_coeffs(Coeffs::low_shelf(120.0, 12.0, 1.0, FS), 480);
            }
            let y = sec.process(x);
            if i > 0 {
                max_jump = max_jump.max((y - prev_out).abs());
            }
            prev_out = y;
        }
        // 60 Hz 满幅正弦本身的相邻样本差 < 0.01；系数跳变不允许带来更大的台阶
        assert!(max_jump < 0.05, "最大相邻样本差 {max_jump}");
        assert!(!sec.is_fading());
    }

    #[test]
    fn sos_cascade_magnitude_is_product() {
        let a = Coeffs::peaking(1000.0, 6.0, 1.4, FS);
        let b = Coeffs::peaking(4000.0, -6.0, 1.4, FS);
        let mut sos = Sos::new(2);
        sos.set_coeffs(&[a, b], 0);
        let expect = a.magnitude(omega_at(2000.0)) * b.magnitude(omega_at(2000.0));
        assert!((sos.magnitude(omega_at(2000.0)) - expect).abs() < 1e-12);
    }
}
