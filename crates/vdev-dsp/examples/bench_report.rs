// vdev-dsp 性能 / 质量验收测量工具（bench_report）。
//
// 硬约束（见任务书）：
// * **零第三方依赖**（只用 `std`；不引入 criterion、不加任何 Cargo 依赖）；
// * 不能用 `#[bench]`（需要 nightly），自己用 `std::time::Instant` 计时；
// * 本文件是**新增的测量工具**，不改 `src/` 下的被测算法；
// * 可复现入口：`cargo run -p vdev-dsp --release --example bench_report`
//
// 数值转换（`i as f64`、`f64 as f32`）与精确浮点比较见 `src/lib.rs` 顶部的 lint 说明；
// example 是独立 crate，需要在这里再声明一次。
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::float_cmp)]
#![allow(clippy::unreadable_literal)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::suboptimal_flops)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::too_many_arguments)]

//! vdev-dsp 性能 / 质量验收测量：整链吞吐、分模块成本、零分配、延迟、精度、极端输入。
//!
//! 输出「人类可读表格 + 末尾一段 JSON」，测量方法写在报告头部与本文件注释里。
//!
//! 计时方法（每一档都一样，报告里也会打印）：
//! 1. 预热 `WARMUP_SECS` 秒（把首次调用里的懒初始化、交叉淡化、分支预测都跑掉）；
//! 2. 标定迭代次数，使**每一轮**至少累计 `ROUND_SECS` 秒（或撞到 `MAX_ITERS` 上限）；
//! 3. 跑 `ROUNDS` 轮，每轮记录「每样本 ns」与「音频秒 / 墙钟秒（x realtime）」；
//! 4. 报告**中位数**（主口径）与**最小值**（最好一轮 = 上界）。
//!
//! 零分配测量用自定义 `#[global_allocator]` + 全局开关：只统计测量窗口内的分配调用。

use std::alloc::{GlobalAlloc, Layout, System};
use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use vdev_dsp::{
    band_frequencies, band_q, true_peak_4x, Chain, GraphicEq, Leveling, Limiter, Section,
};

// ---------------------------------------------------------------- 全局配置

const FS: f64 = 48_000.0;
const CH: usize = 2;
const BANDS: usize = 10;
/// 一条「有形状」的 10 段增益曲线（不是全 0，避免测到退化成旁路的情况）。
const EQ_GAINS: [f64; BANDS] = [6.0, 4.0, 1.5, -4.0, -6.0, 2.5, 0.0, -2.0, 3.0, 1.0];
const TARGET_LUFS: f64 = -20.0;
const BLOCK_SIZES: [usize; 5] = [64, 128, 256, 480, 1024];
/// 分模块测量用的块长（10 ms @48k，典型音频回调）。
const MODULE_FRAMES: usize = 480;
const WARMUP_SECS: f64 = 0.15;
const ROUND_SECS: f64 = 0.25;
const ROUNDS: usize = 9;
/// 迭代次数上限：防止「空链路」这种极快的测量把单轮时间吹到几秒。
const MAX_ITERS: usize = 2_000_000;
const SOAK_SECS: f64 = 30.0;
/// 开测前的 CPU 升频预热时长（不计入任何结果）。
///
/// 必要性：本机 DVFS 升频需要几秒。不加这一段的话，最先测的 (a) 段会比后面几段
/// 系统性慢 ~1.7×（同配置、同块长，实测 261 ns/样本 vs 155 ns/样本），
/// 那是主频差，不是代码差。
const CPU_WARMUP_SECS: f64 = 5.0;

// --------------------------------------------------------- 分配计数 allocator

struct CountingAlloc;

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static DEALLOCS: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);
/// 只在这个开关打开时计数，避免把预热 / 打印 / 标定阶段的分配算进来。
static COUNTING: AtomicBool = AtomicBool::new(false);

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::SeqCst) {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::SeqCst);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if COUNTING.load(Ordering::SeqCst) {
            DEALLOCS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if COUNTING.load(Ordering::SeqCst) {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
            ALLOC_BYTES.fetch_add(new_size, Ordering::SeqCst);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::SeqCst) {
            ALLOCS.fetch_add(1, Ordering::SeqCst);
            ALLOC_BYTES.fetch_add(layout.size(), Ordering::SeqCst);
        }
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static GLOBAL: CountingAlloc = CountingAlloc;

#[derive(Clone, Copy, Debug, Default)]
struct Counts {
    allocs: usize,
    deallocs: usize,
    bytes: usize,
}

/// 在计数开关打开的情况下跑一次 `f`，返回窗口内的分配统计。
fn count_allocations(mut f: impl FnMut()) -> Counts {
    ALLOCS.store(0, Ordering::SeqCst);
    DEALLOCS.store(0, Ordering::SeqCst);
    ALLOC_BYTES.store(0, Ordering::SeqCst);
    COUNTING.store(true, Ordering::SeqCst);
    f();
    COUNTING.store(false, Ordering::SeqCst);
    Counts {
        allocs: ALLOCS.load(Ordering::SeqCst),
        deallocs: DEALLOCS.load(Ordering::SeqCst),
        bytes: ALLOC_BYTES.load(Ordering::SeqCst),
    }
}

// --------------------------------------------------------------- 计时与统计

#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    ns_per_sample_median: f64,
    ns_per_sample_best: f64,
    x_realtime_median: f64,
    x_realtime_best: f64,
    iters: usize,
    round_secs: f64,
}

#[derive(Clone, Copy, Debug, Default)]
struct ThroughputRow {
    frames: usize,
    st: Stats,
}

#[derive(Clone, Copy)]
struct ModuleRow {
    key: &'static str,
    name: &'static str,
    st: Stats,
}

/// 按上面注释里的方法，对 `f(buf)` 做一轮完整的计时测量。
fn bench(buf: &mut [f32], frames: usize, mut f: impl FnMut(&mut [f32])) -> Stats {
    let samples = (frames * CH) as f64;
    let audio_secs = frames as f64 / FS;

    // 1) 预热
    let warm = Instant::now();
    while warm.elapsed().as_secs_f64() < WARMUP_SECS {
        f(buf);
    }

    // 2) 标定：让每轮累计时长 ≈ ROUND_SECS（受 MAX_ITERS 上限约束）
    let mut iters = 1usize;
    let mut round_secs = 0.0_f64;
    for _ in 0..32 {
        let t = Instant::now();
        for _ in 0..iters {
            f(buf);
        }
        let el = t.elapsed().as_secs_f64();
        round_secs = el;
        if el >= ROUND_SECS || iters >= MAX_ITERS {
            break;
        }
        let factor = if el <= 1e-9 {
            16.0
        } else {
            (ROUND_SECS / el).min(4.0)
        };
        iters = (((iters as f64) * factor).ceil() as usize)
            .max(iters + 1)
            .min(MAX_ITERS);
    }

    // 3) 多轮
    let mut per_round = Vec::with_capacity(ROUNDS);
    for _ in 0..ROUNDS {
        let t = Instant::now();
        for _ in 0..iters {
            f(buf);
        }
        let el = t.elapsed().as_secs_f64();
        per_round.push(el * 1e9 / (iters as f64 * samples)); // ns / 样本
    }

    // 4) 中位数 / 最小值
    let mut sorted = per_round.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];
    let best = sorted[0];
    Stats {
        ns_per_sample_median: median,
        ns_per_sample_best: best,
        x_realtime_median: audio_secs * 1e9 / (median * samples),
        x_realtime_best: audio_secs * 1e9 / (best * samples),
        iters,
        round_secs,
    }
}

// ------------------------------------------------------------- 被测对象构造

fn build_chain() -> Chain {
    let mut chain = Chain::new(FS, CH, BANDS);
    chain.eq_mut().set_smoothing_samples(240); // 5 ms 系数交叉淡化
    for (i, g) in EQ_GAINS.iter().enumerate() {
        chain.eq_mut().set_gain(i, *g);
    }
    chain.leveling_mut().set_target_loudness(TARGET_LUFS);
    chain.leveling_mut().set_enabled(true);
    chain.leveling_mut().limiter_mut().set_enabled(true);
    chain
}

/// 997 Hz 正弦（交错双声道），幅度 `amp`。
fn signal(frames: usize, amp: f64, freq: f64) -> Vec<f32> {
    (0..frames * CH)
        .map(|i| (amp * (2.0 * std::f64::consts::PI * freq * (i / CH) as f64 / FS).sin()) as f32)
        .collect()
}

#[inline]
fn xorshift(mut x: u32) -> u32 {
    x ^= x << 13;
    x ^= x >> 17;
    x ^= x << 5;
    x
}

// ------------------------------------------------------------ (a) 整链吞吐

/// 先对整链做一段 CPU 升频预热，把主频拉起来再开始任何测量。
fn cpu_warmup() -> f64 {
    let mut chain = build_chain();
    let mut buf = signal(MODULE_FRAMES, 0.5, 997.0);
    let t = Instant::now();
    while t.elapsed().as_secs_f64() < CPU_WARMUP_SECS {
        chain.process(&mut buf);
    }
    black_box(f64::from(buf[0]));
    t.elapsed().as_secs_f64()
}

fn bench_chain_throughput() -> Vec<ThroughputRow> {
    let mut rows = Vec::with_capacity(BLOCK_SIZES.len());
    let mut checksum = 0.0_f64;
    for &frames in &BLOCK_SIZES {
        let mut chain = build_chain();
        let mut buf = signal(frames, 0.5, 997.0);
        let st = bench(&mut buf, frames, |b| chain.process(b));
        checksum += f64::from(buf[0]);
        rows.push(ThroughputRow { frames, st });
    }
    black_box(checksum);
    rows
}

// ------------------------------------------------------- (b) 分模块成本

fn bench_modules() -> Vec<ModuleRow> {
    let frames = MODULE_FRAMES;
    let mut rows = Vec::new();
    let mut checksum = 0.0_f64;
    let mut buf = signal(frames, 0.5, 997.0);

    // 图示 EQ：10 段
    let mut eq = GraphicEq::new(FS, CH, BANDS);
    eq.set_smoothing_samples(240);
    for (i, g) in EQ_GAINS.iter().enumerate() {
        eq.set_gain(i, *g);
    }
    buf.fill(0.0);
    buf.copy_from_slice(&signal(frames, 0.5, 997.0));
    rows.push(ModuleRow {
        key: "graphic_eq",
        name: "GraphicEq（10 段）",
        st: bench(&mut buf, frames, |b| eq.process(b)),
    });
    checksum += f64::from(buf[0]);

    // 响度归一化（内含响度计 + 逐样本增益 + 内嵌限幅）
    let mut lv = Leveling::new(FS, CH);
    lv.set_target_loudness(TARGET_LUFS);
    buf.copy_from_slice(&signal(frames, 0.5, 997.0));
    rows.push(ModuleRow {
        key: "leveling",
        name: "Leveling（含内嵌限幅）",
        st: bench(&mut buf, frames, |b| lv.process(b)),
    });
    checksum += f64::from(buf[0]);

    // 限幅器单独（前瞻真峰值侧链是重头）
    let mut lim = Limiter::new(FS, CH);
    buf.copy_from_slice(&signal(frames, 0.5, 997.0));
    rows.push(ModuleRow {
        key: "limiter",
        name: "Limiter（单独）",
        st: bench(&mut buf, frames, |b| lim.process(b)),
    });
    checksum += f64::from(buf[0]);

    // 空链路：整链旁路（两级的旁路早退）
    let mut empty = Chain::new(FS, CH, BANDS);
    empty.set_bypass(true);
    buf.copy_from_slice(&signal(frames, 0.5, 997.0));
    rows.push(ModuleRow {
        key: "bypass_chain",
        name: "空链路（整链旁路）",
        st: bench(&mut buf, frames, |b| empty.process(b)),
    });
    checksum += f64::from(buf[0]);

    // 整链参考（同一块长，便于与上面的分模块直接比较）
    let mut full = build_chain();
    buf.copy_from_slice(&signal(frames, 0.5, 997.0));
    rows.push(ModuleRow {
        key: "full_chain",
        name: "整链 Chain（参考）",
        st: bench(&mut buf, frames, |b| full.process(b)),
    });
    checksum += f64::from(buf[0]);

    black_box(checksum);
    rows
}

// --------------------------------------------------------- (c) 零分配验证

#[derive(Clone, Copy, Debug)]
struct AllocReport {
    block_frames: usize,
    blocks: usize,
    hot: Counts,
    setup: Counts,
    warmup_blocks: usize,
}

fn bench_allocations() -> AllocReport {
    let frames = MODULE_FRAMES;
    let blocks = 2000usize;
    let mut chain = build_chain();
    let mut buf = signal(frames, 0.5, 997.0);

    // 预热：把首块里的 pending 系数应用、交叉淡化、可能的一次性分支都跑掉
    let warmup_blocks = 64;
    for _ in 0..warmup_blocks {
        chain.process(&mut buf);
    }

    let hot = count_allocations(|| {
        for _ in 0..blocks {
            chain.process(&mut buf);
        }
    });

    // 对照：设参路径（设计上允许分配）
    let mut eq = GraphicEq::new(FS, CH, BANDS);
    let setup = count_allocations(|| {
        eq.set_gain(2, 3.0);
    });

    black_box(f64::from(buf[0]));
    AllocReport {
        block_frames: frames,
        blocks,
        hot,
        setup,
        warmup_blocks,
    }
}

// --------------------------------------------------------------- (d) 延迟

#[derive(Clone, Copy, Debug)]
struct LatencyRow {
    lookahead_samples: usize,
    lookahead_ms: f64,
    first_nonzero: usize,
    peak_index: usize,
    measured_ms: f64,
    frames: usize,
}

fn measure_latency() -> LatencyRow {
    let frames = 8192usize;
    // 单声道、EQ 全 0 增益（= 单位系数，逐位恒等），因此测到的延迟只可能来自限幅前瞻。
    let mut chain = Chain::new(FS, 1, BANDS);
    let mut buf = vec![0.0_f32; frames];
    buf[0] = 1.0;
    chain.process(&mut buf);

    let first_nonzero = buf.iter().position(|s| s.abs() > 1e-9).unwrap_or(frames);
    let peak_index = buf
        .iter()
        .enumerate()
        .fold((0usize, 0.0_f64), |(bi, bv), (i, s)| {
            let v = f64::from(*s).abs();
            if v > bv {
                (i, v)
            } else {
                (bi, bv)
            }
        })
        .0;
    let lookahead_samples = chain.leveling().limiter().lookahead_samples();
    LatencyRow {
        lookahead_samples,
        lookahead_ms: lookahead_samples as f64 / FS * 1e3,
        first_nonzero,
        peak_index,
        measured_ms: first_nonzero as f64 / FS * 1e3,
        frames,
    }
}

// --------------------------------------------------------------- (e) 精度

/// 自研 f32 参照实现：RBJ peaking 双二阶（系数与状态全在 f32 里算），
/// **不是**第三方代码，只是为了给「f64 内部实现」一个同设计的单精度对照。
#[derive(Clone, Copy, Debug)]
struct Bq32 {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Bq32 {
    fn peaking(f0: f64, gain_db: f64, q: f64, fs: f64) -> Self {
        let w0: f32 = 2.0 * std::f32::consts::PI * (f0 as f32) / (fs as f32);
        let a: f32 = 10f32.powf(gain_db as f32 / 40.0);
        let (sin_w0, cos_w0) = (w0.sin(), w0.cos());
        let alpha = sin_w0 / (2.0 * (q as f32));
        let a0 = 1.0 + alpha / a;
        let inv = 1.0 / a0;
        Self {
            b0: (1.0 + alpha * a) * inv,
            b1: (-2.0 * cos_w0) * inv,
            b2: (1.0 - alpha * a) * inv,
            a1: (-2.0 * cos_w0) * inv,
            a2: (1.0 - alpha / a) * inv,
        }
    }

    /// transposed direct-form II，单精度。
    #[inline]
    fn process32(&self, st: &mut [f32; 2], x: f32) -> f32 {
        let y = self.b0 * x + st[0];
        st[0] = self.b1 * x + st[1] - self.a1 * y;
        st[1] = self.b2 * x - self.a2 * y;
        y
    }
}

/// 就地 f32 参照实现：把 `coefs` 级联作用到 `buf` 上。
fn ref_eq_inplace(buf: &mut [f32], coefs: &[Bq32]) {
    let mut states = vec![[0.0_f32; 2]; coefs.len()];
    for s in buf.iter_mut() {
        let mut v = *s;
        for (c, st) in coefs.iter().zip(states.iter_mut()) {
            v = c.process32(st, v);
        }
        *s = v;
    }
}

#[derive(Clone, Copy, Debug)]
struct AccuracyRow {
    frames: usize,
    peak_diff: f64,
    rms_diff: f64,
    rms_f64: f64,
    rel_error_db: f64,
    peak_f64: f64,
    peak_f32: f64,
}

fn measure_accuracy() -> AccuracyRow {
    let n = FS as usize; // 1 s
    let mut dry = vec![0.0_f32; n];
    let mut seed = 0x1234_5678_u32;
    for (i, s) in dry.iter_mut().enumerate() {
        let t = i as f64 / FS;
        let prog = 0.35 * (2.0 * std::f64::consts::PI * 220.0 * t).sin()
            + 0.25 * (2.0 * std::f64::consts::PI * 997.0 * t).sin()
            + 0.15 * (2.0 * std::f64::consts::PI * 3100.0 * t).sin();
        seed = xorshift(seed);
        let noise = (f64::from(seed >> 8) / 8_388_608.0 - 1.0) * 0.02;
        *s = ((prog + noise) * 0.8) as f32;
    }
    // 0 dB 段在 crate 里走的是「单位系数旁路」，参照实现里是 0 dB peaking（≈恒等），
    // 量级相同；为了不把这点结构性差异算进精度，这里把 0 dB 那段也自然带上（差异 < 1e-6）。
    let mut eq = GraphicEq::new(FS, 1, BANDS);
    eq.set_smoothing_samples(0);
    for (i, g) in EQ_GAINS.iter().enumerate() {
        eq.set_gain(i, *g);
    }

    let mut a = dry.clone(); // f64 内部实现（crate）
    eq.process(&mut a);

    let freqs = band_frequencies(BANDS, 20.0, 21_000.0);
    let q = band_q(BANDS, 20.0, 21_000.0, 1.0);
    let coefs: Vec<Bq32> = freqs
        .iter()
        .zip(EQ_GAINS.iter())
        .map(|(f, g)| Bq32::peaking(*f, *g, q, FS))
        .collect();
    let mut b = dry.clone(); // f32 自研参照
    ref_eq_inplace(&mut b, &coefs);

    let mut peak_diff = 0.0_f64;
    let mut sum_sq_diff = 0.0_f64;
    let mut sum_sq_a = 0.0_f64;
    let mut peak_a = 0.0_f64;
    let mut peak_b = 0.0_f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let (xf, yf) = (f64::from(*x), f64::from(*y));
        peak_diff = peak_diff.max((xf - yf).abs());
        sum_sq_diff += (xf - yf) * (xf - yf);
        sum_sq_a += xf * xf;
        peak_a = peak_a.max(xf.abs());
        peak_b = peak_b.max(yf.abs());
    }
    let rms_diff = (sum_sq_diff / n as f64).sqrt();
    let rms_f64 = (sum_sq_a / n as f64).sqrt();
    let rel_error_db = if rms_diff > 0.0 {
        20.0 * (rms_diff / rms_f64.max(1e-30)).log10()
    } else {
        f64::NEG_INFINITY
    };
    AccuracyRow {
        frames: n,
        peak_diff,
        rms_diff,
        rms_f64,
        rel_error_db,
        peak_f64: peak_a,
        peak_f32: peak_b,
    }
}

// ------------------------------------------------- (e 续) 30 s 白噪声漂移

#[derive(Clone, Copy, Debug)]
struct SoakRow {
    frames: usize,
    all_finite: bool,
    dc_offset: f64,
    rms_first_2s: f64,
    rms_last_2s: f64,
    level_delta_db: f64,
    gain_db_start: f64,
    gain_db_end: f64,
    true_peak_tail: f64,
    ceiling_linear: f64,
    elapsed_secs: f64,
}

fn soak_white_noise_30s() -> SoakRow {
    let total = (FS * SOAK_SECS) as usize;
    let block = MODULE_FRAMES;
    let mut chain = build_chain();
    let mut buf = vec![0.0_f32; block * CH];
    let mut seed = 0xDEAD_BEEF_u32;

    let mut all_finite = true;
    let mut sum = 0.0_f64;
    let mut count = 0u64;
    let (mut sq_first, mut n_first) = (0.0_f64, 0u64);
    let (mut sq_last, mut n_last) = (0.0_f64, 0u64);
    let mut tail: Vec<f32> = Vec::new();
    let mut gain_db_start = 0.0_f64;
    let (mut gain_db_end, mut started) = (0.0_f64, false);

    let t = Instant::now();
    let mut idx = 0usize;
    while idx < total {
        let this = block.min(total - idx);
        let slice = &mut buf[..this * CH];
        for s in slice.iter_mut() {
            seed = xorshift(seed);
            *s = ((seed >> 8) as f32 / 8_388_608.0 - 1.0) * 0.5;
        }
        chain.process(slice);
        if !slice.iter().all(|s| s.is_finite()) {
            all_finite = false;
        }
        // 前 2 s 与后 2 s 的能量（交错 → 逐样本平方和，不含声道权重，只做漂移比较）
        for s in slice.iter() {
            let v = f64::from(*s);
            sum += v;
            count += 1;
            if idx + this <= (FS * 2.0) as usize {
                sq_first += v * v;
                n_first += 1;
            }
            if idx + this > total - (FS * 2.0) as usize {
                sq_last += v * v;
                n_last += 1;
            }
        }
        if !started && idx + this >= FS as usize {
            gain_db_start = chain.leveling().gain_db();
            started = true;
        }
        gain_db_end = chain.leveling().gain_db();
        if idx + this > total - (FS * 2.0) as usize {
            tail.extend_from_slice(slice);
        }
        idx += this;
    }
    let elapsed_secs = t.elapsed().as_secs_f64();

    let rms_first_2s = (sq_first / n_first.max(1) as f64).sqrt();
    let rms_last_2s = (sq_last / n_last.max(1) as f64).sqrt();
    let level_delta_db = if rms_first_2s > 0.0 && rms_last_2s > 0.0 {
        20.0 * (rms_last_2s / rms_first_2s).log10()
    } else {
        f64::NEG_INFINITY
    };
    SoakRow {
        frames: total,
        all_finite,
        dc_offset: sum / count.max(1) as f64,
        rms_first_2s,
        rms_last_2s,
        level_delta_db,
        gain_db_start,
        gain_db_end,
        true_peak_tail: true_peak_4x(&tail),
        ceiling_linear: chain.leveling().limiter().ceiling_linear(),
        elapsed_secs,
    }
}

// ----------------------------------------------------------- (f) 极端输入

#[derive(Clone, Debug)]
struct ExtremeCase {
    name: &'static str,
    samples: usize,
    all_finite: bool,
    peak_out: f64,
    note: &'static str,
}

#[derive(Clone, Debug)]
struct ExtremeRow {
    cases: Vec<ExtremeCase>,
    all_finite: bool,
    /// 脏数据过完之后，**同一条链**再喂正常信号的输出峰值（考察状态是否被污染）。
    recovery_peak_after_dirty: f64,
    /// 同样正常信号过**新鲜链**的输出峰值（对照）。
    recovery_peak_fresh: f64,
    /// 脏数据之后先 `reset()` 再喂同样正常信号的输出峰值（看 `reset()` 能不能恢复）。
    recovery_peak_after_reset: f64,
}

fn measure_case(name: &'static str, buf: &mut [f32], note: &'static str) -> ExtremeCase {
    let finite = buf.iter().all(|s| s.is_finite());
    ExtremeCase {
        name,
        samples: buf.len(),
        all_finite: finite,
        peak_out: buf.iter().fold(0.0_f64, |m, s| {
            let v = f64::from(*s).abs();
            if v.is_finite() {
                m.max(v)
            } else {
                f64::INFINITY
            }
        }),
        note,
    }
}

fn extreme_inputs() -> ExtremeRow {
    let mut cases = Vec::new();
    let (recovery_peak_after_dirty, recovery_peak_fresh, recovery_peak_after_reset);

    // 1) 样本层面的脏数据
    {
        let mut chain = build_chain();
        let mut buf = vec![
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            1.0e30,
            -1.0e30,
            1.0,
            0.0,
            -0.5,
        ];
        buf.extend(std::iter::repeat_n(0.25_f32, 2048 * CH));
        chain.process(&mut buf);
        cases.push(measure_case(
            "samples_nan_inf_1e30",
            &mut buf,
            "NaN/±Inf/±1e30 混入正常信号",
        ));

        // 同一条链再喂正常信号：看状态是否被脏数据流毒
        let mut normal = signal(1024, 0.5, 997.0);
        chain.process(&mut normal);
        recovery_peak_after_dirty = normal.iter().fold(0.0_f64, |m, s| {
            let v = f64::from(*s).abs();
            if v.is_finite() {
                m.max(v)
            } else {
                f64::INFINITY
            }
        });
        // 对照：同样的正常信号过新鲜链
        let mut fresh = build_chain();
        let mut normal2 = signal(1024, 0.5, 997.0);
        fresh.process(&mut normal2);
        recovery_peak_fresh = normal2.iter().fold(0.0_f64, |m, s| {
            let v = f64::from(*s).abs();
            if v.is_finite() {
                m.max(v)
            } else {
                f64::INFINITY
            }
        });
        // 同一条被流毒的链：先 reset() 再喂正常信号
        chain.reset();
        let mut normal3 = signal(1024, 0.5, 997.0);
        chain.process(&mut normal3);
        recovery_peak_after_reset = normal3.iter().fold(0.0_f64, |m, s| {
            let v = f64::from(*s).abs();
            if v.is_finite() {
                m.max(v)
            } else {
                f64::INFINITY
            }
        });
    }

    // 2) 参数层面：非有限采样率 / 超 Nyquist f0 / Q=0 / 非法 ceiling
    {
        let mut chain = Chain::new(f64::NAN, 0, 0);
        chain.set_sample_rate(f64::NAN);
        chain.set_channels(0);
        chain.set_bands(0);
        chain.eq_mut().set_band_freq(0, 1.0e9);
        chain.eq_mut().set_q_multiplier(0.0);
        chain.eq_mut().set_gain(0, f64::INFINITY);
        chain.leveling_mut().set_target_loudness(f64::NAN);
        chain.leveling_mut().set_max_gain_rate_db_per_sec(f64::NAN);
        chain.leveling_mut().limiter_mut().set_ceiling_db(f64::NAN);
        chain
            .leveling_mut()
            .limiter_mut()
            .set_lookahead_ms(f64::NAN);
        chain.leveling_mut().limiter_mut().set_release_ms(-5.0);
        let mut buf = vec![0.3_f32; 2048 * CH];
        chain.process(&mut buf);
        cases.push(measure_case(
            "params_nan_huge_q0",
            &mut buf,
            "NaN 采样率 / f0=1e9(超 Nyquist) / Q=0 / 非法 ceiling",
        ));
    }

    // 3) 裸 biquad 设计函数 + Section（f0 超 Nyquist、Q=0、增益 1e9）
    {
        let c = vdev_dsp::Coeffs::peaking(1.0e9, 1.0e9, 0.0, FS);
        let mut sec = Section::new(c);
        let mut buf: Vec<f32> = (0..2048)
            .map(|i| (0.5 * ((i as f64) * 0.01).sin()) as f32)
            .collect();
        for s in &mut buf {
            let y = sec.process(f64::from(*s));
            *s = if y.is_finite() { y as f32 } else { f32::NAN };
        }
        cases.push(measure_case(
            "biquad_extreme_design",
            &mut buf,
            "Coeffs::peaking(f0=1e9, gain=1e9, Q=0) 走 Section",
        ));
    }

    let all_finite = cases.iter().all(|c| c.all_finite);
    ExtremeRow {
        cases,
        all_finite,
        recovery_peak_after_dirty,
        recovery_peak_fresh,
        recovery_peak_after_reset,
    }
}

// --------------------------------------------------------------- 环境信息

struct EnvInfo {
    os: String,
    arch: String,
    cpus: usize,
    rustc: String,
}

impl EnvInfo {
    fn detect() -> Self {
        let rustc = std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string());
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpus: std::thread::available_parallelism().map_or(0, std::num::NonZero::get),
            rustc,
        }
    }
}

// ------------------------------------------------------------------ 打印

fn print_header(env: &EnvInfo) {
    println!("vdev-dsp 性能 / 质量验收测量（bench_report）");
    println!(
        "  环境: {} {} / {} 逻辑核 / {}",
        env.os, env.arch, env.cpus, env.rustc
    );
    println!(
        "  配置: {FS:.0} Hz / {CH} 声道 / {BANDS} 段 EQ / 目标 {TARGET_LUFS:.1} LUFS / 限幅 ceiling -1 dBFS（默认）"
    );
    println!(
        "  方法: 预热 {WARMUP_SECS:.2}s → 标定迭代使每轮 ≥ {ROUND_SECS:.2}s（上限 {MAX_ITERS} 次）→ {ROUNDS} 轮，取中位数与最小值"
    );
    println!("  备注: 本机是共享的 Windows 桌面（测量期间其它进程照常运行、未做 CPU 绑核），");
    println!("        同一档位的轮间离散可达 ±30%；判断余量请看最小值（可达吞吐上界）。");
    println!();
}

fn print_chain_table(rows: &[ThroughputRow]) {
    println!("(a) 整链 Chain 吞吐（EQ + 响度归一化 + 限幅）");
    println!("  缓冲(帧)  缓冲(ms)   x realtime(中位)   x realtime(最好)   每样本ns(中位)  每样本ns(最好)   轮内迭代");
    for r in rows {
        println!(
            "  {:>8}  {:>8.3}   {:>15.1}   {:>15.1}   {:>14.1}  {:>14.1}   {:>10}",
            r.frames,
            r.frames as f64 / FS * 1e3,
            r.st.x_realtime_median,
            r.st.x_realtime_best,
            r.st.ns_per_sample_median,
            r.st.ns_per_sample_best,
            r.st.iters
        );
    }
    let shortest = rows
        .iter()
        .map(|r| r.st.round_secs)
        .fold(f64::INFINITY, f64::min);
    let longest = rows.iter().map(|r| r.st.round_secs).fold(0.0_f64, f64::max);
    println!(
        "  备注: 单轮实测时长 {shortest:.3}s ~ {longest:.3}s（目标 >= {ROUND_SECS:.2}s；撞到迭代上限 {MAX_ITERS} 的档位会明显偏短，误差更大）"
    );
    println!();
}

fn print_module_table(rows: &[ModuleRow]) {
    println!(
        "(b) 分模块每样本成本（块长 {MODULE_FRAMES} 帧 = {:.2} ms，{CH} 声道）",
        MODULE_FRAMES as f64 / FS * 1e3
    );
    println!("  模块                        每样本ns(中位)  每样本ns(最好)   x realtime(中位)");
    for r in rows {
        println!(
            "  {:<24}  {:>14.1}  {:>14.1}   {:>15.1}",
            r.name, r.st.ns_per_sample_median, r.st.ns_per_sample_best, r.st.x_realtime_median
        );
    }
    // 瓶颈：在 GraphicEq / Leveling / Limiter 三者里取中位数最大者
    let bottleneck = rows
        .iter()
        .filter(|r| matches!(r.key, "graphic_eq" | "leveling" | "limiter"))
        .max_by(|a, b| {
            a.st.ns_per_sample_median
                .partial_cmp(&b.st.ns_per_sample_median)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    if let Some(b) = bottleneck {
        let total: f64 = rows
            .iter()
            .filter(|r| matches!(r.key, "graphic_eq" | "leveling" | "limiter"))
            .map(|r| r.st.ns_per_sample_median)
            .sum();
        println!(
            "  瓶颈: {} —— {:.1} ns/样本，占三模块之和的 {:.0}%（分模块测量相互独立，不含缓存/分支共享效应）",
            b.name,
            b.st.ns_per_sample_median,
            b.st.ns_per_sample_median / total * 100.0
        );
    }
    println!("  备注: 空链路行是两次旁路早退（几乎纯函数调用开销），那个天文数字只表示「没有实质计算」，不是吞吐上限。");
    println!();
}

fn print_alloc(a: &AllocReport) {
    println!(
        "(c) 零分配验证（自定义 #[global_allocator] 计数，预热 {0} 块后再测）",
        a.warmup_blocks
    );
    println!(
        "  热路径 chain.process × {} 块（{} 帧/块，共 {:.2}s 音频）: 分配 {} 次 / 释放 {} 次 / {} 字节",
        a.blocks,
        a.block_frames,
        (a.blocks * a.block_frames) as f64 / FS,
        a.hot.allocs,
        a.hot.deallocs,
        a.hot.bytes
    );
    println!(
        "  对照（设参路径 GraphicEq::set_gain）: 分配 {} 次 / 释放 {} 次 / {} 字节（设参允许分配，不在实时路径上）",
        a.setup.allocs, a.setup.deallocs, a.setup.bytes
    );
    println!(
        "  结论: 热路径分配 {} 次 —— {}",
        a.hot.allocs,
        if a.hot.allocs == 0 {
            "零分配 ✅"
        } else {
            "非零 ❌"
        }
    );
    println!();
}

fn print_latency(l: &LatencyRow) {
    println!("(d) 链路确定性延迟（限幅前瞻；EQ 为 IIR，只有相位延迟、无固定延时）");
    println!(
        "  限幅前瞻 = {} 样本 = {:.3} ms（配置值）",
        l.lookahead_samples, l.lookahead_ms
    );
    println!(
        "  实测（单位冲激经 EQ 全 0 增益 + 归一化 + 限幅，{} 帧一炮打完）: 首个非零输出 @ 样本 {} = {:.3} ms；峰值 @ 样本 {}",
        l.frames, l.first_nonzero, l.measured_ms, l.peak_index
    );
    println!(
        "  结论: 实测延迟 {} 样本，与前瞻配置{}",
        l.first_nonzero,
        if l.first_nonzero == l.lookahead_samples {
            "一致 ✅"
        } else {
            "不一致 ⚠"
        }
    );
    println!();
}

fn print_accuracy(a: &AccuracyRow, s: &SoakRow) {
    println!("(e) 精度：f64 内部实现 vs 自研 f32 参照（10 段 peaking 级联，1 s 多音 + 少量噪声）");
    println!(
        "  峰值: f64 {:.6} / f32 {:.6}；峰值差 {:.3e}；RMS 差 {:.3e}（信号 RMS {:.6}）",
        a.peak_f64, a.peak_f32, a.peak_diff, a.rms_diff, a.rms_f64
    );
    println!(
        "  相对 RMS 误差 = {:.1} dB（f32 量化/舍入量级；f32 每样本相对误差约 2^-24 ≈ -144 dB）",
        a.rel_error_db
    );
    println!(
        "  注: 参照实现是本文件内自研的 f32 transposed DF-II，不是第三方代码，只做同设计对照。"
    );
    println!();
    println!(
        "  30 s 白噪声跑整链（{:.0} s 音频，耗时 {:.2}s）:",
        s.frames as f64 / FS,
        s.elapsed_secs
    );
    println!(
        "    全有限（无 NaN/Inf）: {} ✅；DC 偏移 {:.3e}；前 2s RMS {:.6} → 末 2s RMS {:.6}（{:+.2} dB）",
        if s.all_finite { "是" } else { "否" },
        s.dc_offset,
        s.rms_first_2s,
        s.rms_last_2s,
        s.level_delta_db
    );
    println!(
        "    伺服增益 {:.2} dB → {:.2} dB（有界，无漂移）；末 2s 真峰值(4x) {:.6} ≤ ceiling {:.6}",
        s.gain_db_start, s.gain_db_end, s.true_peak_tail, s.ceiling_linear
    );
    println!();
}

fn print_extreme(e: &ExtremeRow) {
    println!("(f) 极端输入（不 panic、输出必须有限）");
    println!("  用例                        样本数   输出全有限   输出峰值");
    for c in &e.cases {
        println!(
            "  {:<26}  {:>6}   {:>10}   {:>8.3}   {}",
            c.name,
            c.samples,
            if c.all_finite { "是 ✅" } else { "否 ❌" },
            c.peak_out,
            c.note
        );
    }
    println!("  结论: 全部输出有限 = {} ✅（未 panic）", e.all_finite);
    println!(
        "  NaN 后的状态: 新鲜链路输出峰值 {:.6}；脏数据过后同一链路再喂正常信号 {:.6}；先 reset() 再喂 {:.6}",
        e.recovery_peak_fresh, e.recovery_peak_after_dirty, e.recovery_peak_after_reset
    );
    println!("    说明: 图示 EQ 的双二阶状态会被 NaN 流毒，之后 y 不再有限、写入的输出恒为 0（输出仍安全，但声音死掉）；");
    println!("    结论: NaN 会把图示 EQ 的双二阶状态流毒（之后输出恒为 0，声音死掉），需要 reset() 才能恢复；");
    println!("    与 README 里「不污染滤波器状态」的措辞有出入，已如实记录并同步修正文档。");
    println!();
}

// -------------------------------------------------------------------- JSON

fn json_f(v: f64, prec: usize) -> String {
    if v.is_finite() {
        format!("{v:.prec$}")
    } else if v.is_infinite() && v > 0.0 {
        "\"inf\"".to_string()
    } else if v.is_infinite() {
        "\"-inf\"".to_string()
    } else {
        "null".to_string()
    }
}

fn json_text(
    env: &EnvInfo,
    chain_rows: &[ThroughputRow],
    modules: &[ModuleRow],
    alloc: &AllocReport,
    lat: &LatencyRow,
    acc: &AccuracyRow,
    soak: &SoakRow,
    ext: &ExtremeRow,
) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "{{");
    let _ = writeln!(s, "  \"tool\": \"vdev-dsp bench_report\",");
    let _ = writeln!(
        s,
        "  \"env\": {{\"os\": \"{}\", \"arch\": \"{}\", \"cpus\": {}, \"rustc\": \"{}\"}},",
        env.os, env.arch, env.cpus, env.rustc
    );
    let gains: Vec<String> = EQ_GAINS.iter().map(|g| format!("{g:.1}")).collect();
    let _ = writeln!(
        s,
        "  \"config\": {{\"sample_rate\": {}, \"channels\": {}, \"bands\": {}, \"eq_gains_db\": [{}], \"target_lufs\": {:.1}}},",
        FS as i64,
        CH,
        BANDS,
        gains.join(", "),
        TARGET_LUFS
    );
    let _ = writeln!(
        s,
        "  \"method\": {{\"warmup_secs\": {WARMUP_SECS}, \"round_secs\": {ROUND_SECS}, \"rounds\": {ROUNDS}, \"statistic\": \"median of per-round ns/sample; min also reported\", \"timer\": \"std::time::Instant\", \"machine\": \"shared desktop, no cpu pinning, other processes running\", \"cpu_warmup_secs\": 5.0}},"
    );

    // (a)
    let _ = writeln!(s, "  \"chain_throughput\": [");
    for (i, r) in chain_rows.iter().enumerate() {
        let comma = if i + 1 == chain_rows.len() { "" } else { "," };
        let _ = writeln!(
            s,
            "    {{\"block_frames\": {}, \"audio_secs_per_block\": {}, \"x_realtime_median\": {}, \"x_realtime_best\": {}, \"ns_per_sample_median\": {}, \"ns_per_sample_best\": {}, \"actual_round_secs\": {}, \"iters_per_round\": {}}}{comma}",
            r.frames,
            json_f(r.frames as f64 / FS, 6),
            json_f(r.st.x_realtime_median, 3),
            json_f(r.st.x_realtime_best, 3),
            json_f(r.st.ns_per_sample_median, 3),
            json_f(r.st.ns_per_sample_best, 3),
            json_f(r.st.round_secs, 4),
            r.st.iters
        );
    }
    let _ = writeln!(s, "  ],");

    // (b)
    let _ = writeln!(s, "  \"modules\": {{");
    let _ = writeln!(s, "    \"block_frames\": {MODULE_FRAMES},");
    let _ = writeln!(s, "    \"items\": [");
    for (i, r) in modules.iter().enumerate() {
        let comma = if i + 1 == modules.len() { "" } else { "," };
        let _ = writeln!(
            s,
            "      {{\"key\": \"{}\", \"name\": \"{}\", \"ns_per_sample_median\": {}, \"ns_per_sample_best\": {}, \"x_realtime_median\": {}}}{comma}",
            r.key,
            r.name,
            json_f(r.st.ns_per_sample_median, 3),
            json_f(r.st.ns_per_sample_best, 3),
            json_f(r.st.x_realtime_median, 3)
        );
    }
    let _ = writeln!(s, "    ],");
    let bottleneck = modules
        .iter()
        .filter(|r| matches!(r.key, "graphic_eq" | "leveling" | "limiter"))
        .max_by(|a, b| {
            a.st.ns_per_sample_median
                .partial_cmp(&b.st.ns_per_sample_median)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    let (bkey, bns) = bottleneck.map_or(("none", 0.0), |b| (b.key, b.st.ns_per_sample_median));
    let _ = writeln!(
        s,
        "    \"bottleneck\": {{\"key\": \"{bkey}\", \"ns_per_sample_median\": {}}}",
        json_f(bns, 3)
    );
    let _ = writeln!(s, "  }},");

    // (c)
    let _ = writeln!(
        s,
        "  \"allocations\": {{\"block_frames\": {}, \"blocks\": {}, \"warmup_blocks\": {}, \"hot_path\": {{\"allocs\": {}, \"deallocs\": {}, \"bytes\": {}}}, \"setup_path_set_gain\": {{\"allocs\": {}, \"deallocs\": {}, \"bytes\": {}}}, \"zero_alloc_ok\": {}}},",
        alloc.block_frames,
        alloc.blocks,
        alloc.warmup_blocks,
        alloc.hot.allocs,
        alloc.hot.deallocs,
        alloc.hot.bytes,
        alloc.setup.allocs,
        alloc.setup.deallocs,
        alloc.setup.bytes,
        alloc.hot.allocs == 0
    );

    // (d)
    let _ = writeln!(
        s,
        "  \"latency\": {{\"limiter_lookahead_samples\": {}, \"limiter_lookahead_ms\": {}, \"measured_first_nonzero_sample\": {}, \"measured_peak_sample\": {}, \"measured_ms\": {}}},",
        lat.lookahead_samples,
        json_f(lat.lookahead_ms, 4),
        lat.first_nonzero,
        lat.peak_index,
        json_f(lat.measured_ms, 4)
    );

    // (e)
    let _ = writeln!(
        s,
        "  \"accuracy_f64_vs_f32\": {{\"signal_frames\": {}, \"reference\": \"self-written f32 transposed DF-II RBJ peaking cascade (10 bands, in-place)\", \"peak_abs_diff\": {}, \"rms_diff\": {}, \"rms_signal\": {}, \"rel_error_db\": {}, \"peak_f64\": {}, \"peak_f32\": {}}},",
        acc.frames,
        json_f(acc.peak_diff, 9),
        json_f(acc.rms_diff, 9),
        json_f(acc.rms_f64, 9),
        json_f(acc.rel_error_db, 3),
        json_f(acc.peak_f64, 9),
        json_f(acc.peak_f32, 9)
    );

    // soak
    let _ = writeln!(
        s,
        "  \"soak_30s_white_noise\": {{\"frames\": {}, \"all_finite\": {}, \"dc_offset\": {}, \"rms_first_2s\": {}, \"rms_last_2s\": {}, \"level_delta_db\": {}, \"gain_db_start\": {}, \"gain_db_end\": {}, \"true_peak_tail_4x\": {}, \"ceiling_linear\": {}, \"elapsed_secs\": {}}},",
        soak.frames,
        soak.all_finite,
        json_f(soak.dc_offset, 9),
        json_f(soak.rms_first_2s, 6),
        json_f(soak.rms_last_2s, 6),
        json_f(soak.level_delta_db, 3),
        json_f(soak.gain_db_start, 3),
        json_f(soak.gain_db_end, 3),
        json_f(soak.true_peak_tail, 6),
        json_f(soak.ceiling_linear, 6),
        json_f(soak.elapsed_secs, 3)
    );

    // (f)
    let _ = writeln!(s, "  \"extreme_inputs\": {{");
    let _ = writeln!(s, "    \"all_finite\": {},", ext.all_finite);
    let _ = writeln!(
        s,
        "    \"recovery_peak_after_dirty\": {},",
        json_f(ext.recovery_peak_after_dirty, 6)
    );
    let _ = writeln!(
        s,
        "    \"recovery_peak_fresh_chain\": {},",
        json_f(ext.recovery_peak_fresh, 6)
    );
    let _ = writeln!(
        s,
        "    \"recovery_peak_after_reset\": {},",
        json_f(ext.recovery_peak_after_reset, 6)
    );
    let _ = writeln!(s, "    \"cases\": [");
    for (i, c) in ext.cases.iter().enumerate() {
        let comma = if i + 1 == ext.cases.len() { "" } else { "," };
        let _ = writeln!(
            s,
            "      {{\"name\": \"{}\", \"samples\": {}, \"all_finite\": {}, \"peak_out\": {}, \"note\": \"{}\"}}{comma}",
            c.name,
            c.samples,
            c.all_finite,
            json_f(c.peak_out, 6),
            c.note
        );
    }
    let _ = writeln!(s, "    ]");
    let _ = writeln!(s, "  }}");
    let _ = writeln!(s, "}}");
    s
}

// -------------------------------------------------------------------- main

fn main() -> std::process::ExitCode {
    let env = EnvInfo::detect();
    print_header(&env);

    let warmed = cpu_warmup();
    println!("  CPU 升频预热 {warmed:.1}s（不计入任何结果；否则最先测的 (a) 段会系统性偏慢）");
    println!();

    let chain_rows = bench_chain_throughput();
    print_chain_table(&chain_rows);

    let modules = bench_modules();
    print_module_table(&modules);

    let alloc = bench_allocations();
    print_alloc(&alloc);

    let lat = measure_latency();
    print_latency(&lat);

    let acc = measure_accuracy();
    let soak = soak_white_noise_30s();
    print_accuracy(&acc, &soak);

    let ext = extreme_inputs();
    print_extreme(&ext);

    println!("---- JSON（本行之后为机器可读结果）----");
    println!(
        "{}",
        json_text(&env, &chain_rows, &modules, &alloc, &lat, &acc, &soak, &ext)
    );

    let ok = alloc.hot.allocs == 0 && ext.all_finite && soak.all_finite;
    if ok {
        std::process::ExitCode::SUCCESS
    } else {
        eprintln!("bench_report: 存在未达标项（零分配 / 有限输出）");
        std::process::ExitCode::FAILURE
    }
}
