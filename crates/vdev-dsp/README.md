# vdev-dsp

`vdev` 的实时音频 DSP：**N 段图示均衡器 + 响度归一化 + 前瞻真峰值限幅**。

* **零第三方依赖**（只用 `core` / `std`），`edition 2021`，许可证 **MIT**。
* 实时路径：`process(&mut [f32])` **原地**处理、**零堆分配**、**无锁**、**无 panic 分支**。
* 目标平台无关：`f64` 内部精度，只在缓冲区出入口做 `f32` 转换。
* 公开 API：`GraphicEq` / `Leveling` / `Chain`，外加 `LoudnessMeter`、`Limiter`、
  `true_peak_4x()` 等可单独使用的组件。

算法全部取自公开文献与标准（见 §6），代码为独立实现。

---

## 1. 模块划分与算法来源

| 本 crate 文件 | 职责 | 算法来源（公开文献 / 标准） |
| --- | --- | --- |
| `src/biquad.rs` | RBJ 二阶节（low/high shelf、peaking、LP/HP）、transposed direct form II 状态、SOS 级联、**系数交叉淡化** | RBJ Audio EQ Cookbook（biquad 系数设计公式）；transposed direct-form II 为通用数字滤波器结构 |
| `src/graphic_eq.rs` | N 段图示 EQ（默认 10 段）：对数等间距严格单调频点表、Q 推导、增益钳位、段数变化时按对数频率重采样曲线、精确数字域 `magnitude_response()` | 多段图示均衡器的通用做法：对数等间距频点、段间交叠取 `Q = √r / (r − 1)`、单段增益钳位 |
| `src/loudness.rs` | 短时响度测量：近似 K 加权（high shelf + 高通，按实际采样率用 RBJ 重算）+ 绝对/相对双门限门控；另有 `Rms` 对照模式 | ITU-R BS.1770（K 加权与门控响度；**本实现为近似，非合规 LUFS 表**） |
| `src/leveling.rs` | 响度归一化伺服：目标增益、逐样本平滑（attack/release）、独立 dB/s 变化率上限、安静提升额度、静音回落、`reset()` | 自动增益控制 / 响度归一化的通用结构 |
| `src/limiter.rs` | 前瞻式 brickwall 真峰值限幅 + 4x 过采样真峰值测量工具 | 经典 look-ahead peak limiter 结构；ITU-R BS.1770 附录 2 的过采样真峰值（true peak）测量思路 |
| `src/chain.rs` | 固定顺序链路：EQ → 响度归一化（内含限幅），统一的采样率/声道/段数传播与 `reset()` | 固定处理顺序由本 crate 定义 |
| `src/lib.rs` | 公开 API 汇总、算法来源表、设计取舍表、lint 策略 | — |
| `examples/offline_report.rs` | 离线对照工具：打印处理前后的近似响度（两种口径）与真峰值 | — |
| `examples/bench_report.rs` | 性能与质量验收测量：整链/分模块吞吐、热路径零分配、限幅前瞻延迟、f64↔f32 精度差、30 s 白噪声与极端输入（stdout = 表格 + JSON） | — |

---

## 2. 六处设计取舍（A–F）

每一条都有**可客观断言的单测**，测试名见「证据」列。

| 编号 | 常见朴素做法 | 本 crate 做法 | 代码落点 | 证据 |
| --- | --- | --- | --- | --- |
| **A** 响度测量 | 侧链 120 Hz **一阶 HPF** 之后取滑窗功率 RMS：**无频率加权**（1 kHz 以上 HPF 几乎无作用，人耳 2–5 kHz 的灵敏度完全没体现）、**无门控**（静音/底噪段被算进平均）、**逐声道独立** | 近似 **K 加权**（RBJ high shelf 1681.97 Hz/+4 dB + 高通 38.14 Hz/Q 0.5，按实际采样率重算系数）+ **绝对门限 −70 LUFS / 相对门限 −10 LU 双门限门控**的短时响度。**保留 `LoudnessMode::Rms`** 做裸 RMS 对照（单位 dBFS） | `src/loudness.rs` | `loudness::tests::k_weighting_boosts_presence_band_over_bass`、`relative_gate_ignores_quiet_gaps`、`rms_mode_matches_plain_rms`；`leveling_acceptance::converges_to_target_loudness_within_1db` |
| **B** 增益平滑 | 以 **buffer 为粒度**做 alpha 混合（逐块线性插值），块边界上是分段线性的，**行为随块大小改变**；再用「每 buffer 最多衰减固定量」做硬限，于是「衰减速度」直接由 buffer 长度决定 | **逐样本一阶平滑**：下降走 attack（默认 5 ms）、上升走 release（默认 250 ms）；**另有独立的 dB/s 变化率上限**（默认 200 dB/s）作为硬约束。两者都与 buffer 划分无关 | `src/leveling.rs` | `leveling_acceptance::gain_change_rate_is_bounded_for_any_block_size`（同一信号按 64/120/480/1024 帧喂，单块变化都在上限内）、`gain_drops_fast_and_recovers_slowly`（直接测出两个方向的一阶系数，比值 > 10×） |
| **C** 声道联动 | 每个声道各跑一遍增益计算：左右各自算电平、各自算增益 → **立体声像会被拉扯** | 检测器**跨声道聚合**（BS.1770 各声道权重为 1），增益是**所有声道共用的同一个标量**；限幅器同样联动 | `src/loudness.rs`、`src/leveling.rs`、`src/limiter.rs` | `loudness::tests::channels_are_aggregated_not_picked`、`leveling::tests::applies_same_gain_to_all_channels`、`limiter::tests::gain_is_linked_across_channels` |
| **D** 限幅 | 没有真正的限幅：先算一个**已经迟了**的 RMS 增益，再看输出有没有撞 ceiling，撞了就在**几十秒量级**上慢慢压目标。**瞬时峰值完全无保护** | **前瞻式 brickwall**：2 ms 延迟线 + 滑动最小值 + 受限斜率下降 + 60 ms 平滑释放。`|out[k]| ≤ 离散 ceiling` **逐样点严格成立**——严格性来自输出端兜底的 `.clamp(-limit, limit)`（`limit = 离散 ceiling`），不是侧链；真峰值侧链（4x Hann 窗 sinc 插值核，逐相位归一化到直流增益 1，并把中心帧的离散样点也算进去）的作用是**提前把增益压下去、让 clamp 尽量不咬合**（采样间峰值也基本不越界，咬合越少失真越小）。`ceiling_linear` 与 `discrete_ceiling` 之间的差值就是留给采样间过冲的余量 | `src/limiter.rs` | `true_peak_of_dc_is_unity`、`true_peak_resolves_fs_over_four_peak`、`limiter_never_exceeds_ceiling_on_samples`、`attack_is_a_ramp_not_a_step`、`lookahead_zero_still_bounded`；`leveling_acceptance::true_peak_stays_below_ceiling` |
| **E** 参数变更不爆音 | 重新设计系数但**保留旧滤波器状态**：低频段（状态里存着大量能量）系数一突变就是听得见的爆音 | `Section` 在系数变更时**新老两套系数各自维持滤波器状态、输出线性交叉淡化**（默认立即切换，实时链路上建议给 1–5 ms）；另有 `reset()`。增益侧同样做逐样本平滑，目标响度变化也是斜坡 | `src/biquad.rs`、`src/graphic_eq.rs`、`src/chain.rs` | `biquad::tests::coefficient_change_keeps_output_continuous`、`chain_end_to_end::live_parameter_changes_are_click_free`（运行中反复改增益/段数/Q/目标响度/采样率，用「归一化台阶指标」卡爆音） |
| **F** 可测性 | 只有「跑起来没崩」级别的验证 | 频响曲线（**解析式** `magnitude_response` + 时域峰值双重验证）、响度收敛（±1 dB）、真峰值（4x 过采样）、增益变化率、静音行为、旁路**逐位恒等**——全部客观断言 | 各模块 `#[cfg(test)]` + `tests/` | `cargo test -p vdev-dsp` 共 56 个测试全绿（37 lib + 18 集成 + 1 doctest） |

另外还有两处**顺手做得更稳**的地方（不属于 A–F）：

* **旁路是逐位恒等**：`bypass == true` 时 `process()` 直接返回、一次乘加都不做；全 0 增益
  则走真正的单位系数（`b0=1`，其余全 0，见 `src/graphic_eq.rs` 的 `rebuild()`），而不是
  「近似恒等」。严格口径的断言在 `graphic_eq::tests::bypass_is_bit_exact`
  （`assert_eq!(buf, before)`，逐元素相等）与 `eq_acceptance::bypass_is_bitwise_identity`
  （逐样点比较位型 `to_bits()`）；`eq_acceptance::all_zero_gains_equals_bypass` 用 `< 1e-6` 卡，
  `Coeffs::is_identity()` / `GraphicEq::is_transparent()` 则是精确比较、不做容差。
* **脏数据防御**：任何输入（NaN / ±Inf / 1e30 / 超 Nyquist 的 `f0` / `Q=0`）都不产生
  NaN/Inf、不 panic，**输出恒为有限值**。这是本实现额外加的一层防御。
  **实测限度（如实记录）**：`NaN` 样点会污染图示 EQ 的双二阶状态——之后该链的输出
  **恒为 0**（声音死掉，但输出仍然安全），必须 `Chain::reset()` 才能恢复；
  `examples/bench_report.rs` 的「极端输入」段落把「新鲜链 / 脏数据后 / `reset()` 后」
  三个输出峰值都打了出来。

---

## 3. 快速上手

```rust
use vdev_dsp::Chain;

let mut chain = Chain::new(48_000.0, 2, 10); // 48 kHz / 立体声 / 10 段
chain.eq_mut().set_smoothing_samples(240);   // 系数交叉淡化 5 ms
chain.eq_mut().set_gain(5, 6.0);             // 第 6 段 +6 dB
chain.leveling_mut().set_target_loudness(-18.0);
chain.leveling_mut().set_max_gain_rate_db_per_sec(200.0);

let mut buf = vec![0.0_f32; 960];            // 交错双声道，480 帧
for (i, s) in buf.iter_mut().enumerate() {
    *s = (0.3 * ((i / 2) as f32 * 0.01).sin()).clamp(-1.0, 1.0);
}
chain.process(&mut buf);                     // 原地处理
assert!(buf.iter().all(|s| s.is_finite()));
```

只想知道 EQ 在某频点的**解析增益**时，直接问 `GraphicEq::magnitude_response()` /
`magnitude_response_db()`，不需要跑正弦。

离线对照工具：

```text
cargo run -p vdev-dsp --release --example offline_report -- --target -20 --bands 10
```

---

## 4. 验收与测试

```text
cargo fmt     -p vdev-dsp -- --check
cargo clippy  -p vdev-dsp --all-targets -- -D warnings
cargo test    -p vdev-dsp
cargo build   -p vdev-dsp --release
```

四条命令在本机（Windows / rustc 1.97.1）均通过（`cargo test` 共 56 个测试全绿：
37 lib + 5 `chain_end_to_end` + 6 `eq_acceptance` + 7 `leveling_acceptance` + 1 doctest）。

性能 / 质量测量（`examples/bench_report.rs`，零第三方依赖、不需要 nightly）：

```text
cargo run -p vdev-dsp --release --example bench_report
```

一条命令打出：整链各块长的吞吐（x realtime 与每样本 ns）、分模块每样本成本（含瓶颈）、
自定义 `#[global_allocator]` 计数验证的**热路径零分配**、限幅前瞻引入的确定性延迟、
f64 内部实现与自研 f32 参照的精度差、30 s 白噪声漂移与极端输入；末尾再给一段 JSON。
计时方法：预热 → 每轮 ≥ 0.25 s、共 9 轮 → 取中位数与最小值（本机为共享桌面、未绑核，
轮间离散可达 ±30%）。**性能数字的权威口径就是这条命令的实跑输出**（`cargo run -p vdev-dsp
--release --example bench_report`，stdout 末尾另带机器可读 JSON 段）：提交信息 / 归档里引用的
单个数值（例如 155.9 ns·样本⁻¹ / 66.8 x 与 146.9 ns·样本⁻¹ / 70.9 x 的差别）都只是同一分布里的
一次采样，相差 ~6% 属正常离散，不作为验收阈值。测试覆盖：

* `src/**/tests`：单元级（系数、频响、门控、限幅核、联动、静音）。
* `tests/eq_acceptance.rs`：旁路逐位恒等、+6 dB@1 kHz 实测约 2×（解析与时域双验证）、
  10 段频点表严格单调且在 20 Hz–21 kHz 内、全 0 增益等价旁路、Q 影响形状、段数变化保形。
* `tests/leveling_acceptance.rs`：±1 dB 收敛、真峰值不越界、**下降快于上升**、
  变化率有界且与块长无关、静音回落、极端输入、Rms 对照模式。
* `tests/chain_end_to_end.rs`：5 s 正弦 / 白噪声 / 正弦+突发脉冲 / 反复改参数 / 采样率切换。

---

## 5. 已知限度与**未做**项

**已知限度（如实记录）**

* **K 加权是近似，不是合规 BS.1770 实现**：用的是「high shelf + RLB 高通」两级近似，
  窗口是滑动短时窗而非整段节目，门控块只有 32 × 100 ms。量级上与人耳判断一致，
  但**不能**当作合规的 LUFS 表使用。
* **伺服误差信号用的是 momentary（400 ms 未门控滑窗）响度，不是门控值**：
  门控值在节目静下来之后会被「残留的响块」拖住 3 s 以上，不适合做反馈信号。
  对外报告用的仍是通过门控的 `LoudnessMeter::loudness()`。
* **真峰值过冲余量**：离散样点限幅是严格的；4x 过采样测出的真峰值在某些
  宽带信号（白噪声）上仍可能有约 **2%** 的过冲（插值核在 Nyquist 附近的能量），
  这一余量按 `tp <= ceiling * 1.02` 断言（`tests/chain_end_to_end.rs` 的
  `white_noise_is_bounded` 与 `sine_with_a_burst_impulse_is_limited` 两条），**明确记录**，
  没有假装是 0；其余真峰值断言（`leveling_acceptance::true_peak_stays_below_ceiling`、
  `five_seconds_of_sine_is_clean`、`live_parameter_changes_are_click_free`）用的是无余量口径
  `tp <= ceiling + 1e-6`。
* **段数很多 + Q 很高时**，级联 peaking 的并联近似在高频端会有可见的曲线偏差
  （对数重采样保形，但不是数学等价的滤波器组）。

**未做（明确说明，不是「忘了」）**

* **Linkwitz-Riley 分频带模式**：未做。当前只有级联 peaking 一种模式。
* **接入 `vdev-audio` 的音频热路径**：未做，且**故意不做**——那是后续单独一步，
  需要在 macOS 真机上验证（本 crate 自带测试与离线工具，与平台无关）。
* **梯度预测 / 余量评分一类的事后补偿**：**刻意不做**。那类补偿是在为「测量不准 +
  按 buffer 混增益」打补丁；换成 K 加权 + 门控测量、逐样本平滑之后，这些问题在
  测量层就不存在了（`src/leveling.rs` 顶部有说明）。

---

## 6. 算法来源与许可

* 算法与公式全部取自**公开文献与标准**：

  * **RBJ Audio EQ Cookbook**（Robert Bristow-Johnson）—— biquad 系数设计公式；
  * **ITU-R BS.1770** —— K 加权与门控响度（本实现为**近似**，不能当合规 LUFS 表使用）；
  * **ITU-R BS.1770 附录 2** —— 过采样真峰值（true peak）测量思路；
  * **经典 look-ahead peak limiter** 结构；
  * **transposed direct-form II** —— 通用数字滤波器结构。

* 本 crate 为**独立实现**，以 **MIT** 分发。
* 本 crate 不含任何第三方代码、数据文件或测量数据集（未使用任何 HRTF / 脉冲响应数据）。
* 若要把外部实现或数据引入本仓库，请自行确认其许可证与 MIT 的兼容性，并按其条款保留署名。
