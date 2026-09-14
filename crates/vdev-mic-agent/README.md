# vdev-mic-agent

AI microphone front-end for [vdev](https://github.com/gqf2008/vdev):
**capture → denoise → inject**, with per-frame timing instrumentation.

It is the userland half of vdev's virtual sound card: **the virtual microphone
endpoint already exists, what is missing is the AI chain that feeds it.** No new
driver is involved — the agent is just another CoreAudio / WASAPI client.

D1–D2 wires capture and injection to WAV files, which answers "how much CPU /
how much algorithmic latency". D3–D4 (`live`) drives the same core from real
audio devices — CoreAudio callbacks on macOS, a WASAPI polling thread on
Windows — and measures the buffering a WAV cannot show.

The proposal, the raw measurement data and the A/B audio live in the archive repo
([`vdev-ai-virtual-mic-2026-09`](https://github.com/gqf2008/ventures/tree/main/content/product-proposals/vdev-ai-virtual-mic-2026-09/demo-ai-mic)); upstream issue:
[#10](https://github.com/gqf2008/vdev/issues/10).

```
 physical mic ──▶ capture callback ──▶ [ vdev-mic-agent ] ──▶ "vdev microphone" ──▶ Zoom/微信
                    480 samples/frame      RNNoise denoise            virtual device
                                           + adaptive dry/wet
```

The crate is the Rust twin of the C/Python harness that lives beside the
proposal, and produces **numerically identical audio** (100 % of samples within
1 LSB, see `Cross-checking` below) while being the shape the product needs.

## Build & run

```bash
cargo test -p vdev-mic-agent          # 41 unit tests on macOS (metrics, mixer, ring, frames, latency, stats, wavio)

# D3-D4 macOS backend: build the binary (a plain `cargo check` will NOT catch
# FFI link errors — AudioUnit symbols must resolve against AudioToolbox)
cargo build --release -p vdev-mic-agent

# Windows live path: same checks against the WASAPI backend (see below)
cargo check  -p vdev-mic-agent --target x86_64-pc-windows-msvc
cargo clippy -p vdev-mic-agent --all-targets --target x86_64-pc-windows-msvc -- -D warnings

# one file
cargo run -p vdev-mic-agent --release -- run noisy_snr5db.wav \
    --out /tmp/clean.wav --reference clean_ref.wav --stats /tmp/report.json

# adaptive gate (see below)
cargo run -p vdev-mic-agent --release -- run noisy_snr30db.wav --adaptive

# the whole D1-D2 matrix -> results_rust.json (+ rendered WAVs)
cargo run -p vdev-mic-agent --release -- bench --dir <demo>/audio --out results_rust.json

# cross-check two renders (used against the C and Python harnesses)
cargo run -p vdev-mic-agent --release -- diff a.wav b.wav
```

`live` is the D3–D4 path. macOS drives it from CoreAudio callbacks; Windows
drives the same pipeline from a WASAPI polling thread (see the Windows backend
below). On any other host it prints why and exits, rather than pretending to run:

| platform | `run` / `bench` / `diff` | `live` + probes | status |
|---|---|---|---|
| macOS | ✅ | ✅ CoreAudio callbacks | live available |
| Windows | ✅ | ✅ WASAPI polling + kernel loopback | **已真机实测（2026-09-14，Win10 19045 x64）** |
| other | ✅ | ❌ prints why and exits | — |

### Windows live 实测数字（2026-09-14，Win10 19045 x64 + vdev-audio-win 0.3.9.0）

本机没有物理麦克风，输入源用 Realtek 的 **Stereo Mix**（系统输出环回）代替；扬声器侧往
Realtek 输出播 1 kHz / 0.5 幅度正弦，链路为 *Realtek 出音 → Stereo Mix 采 → RNNoise →
注入 vdev 扬声器 → 驱动环回 → vdev 麦克风*，`vdev-audio-win capture` 在末端量电平：

| 项 | 结果 |
|---|---|
| `live --probe digital`（不需要 RNNoise） | 20 s 内 65 次往返、0 rejected；**latency min 0.00 / mean 9.96 / p50 10.16 / p95 11.16 / max 21.15 ms（stdev 2.02）** |
| `live --adaptive`（完整降噪） | 20.000 s 音频在 20.0014 s 内跑完（实时）；帧时 **p50 26.8 µs / p95 39.4 / p99 46.2 / max 120.6 µs**；CPU 0.0312 s = **0.156 % 单核**；ring dropped 0 |
| 端到端 A/B（同一信号） | `--mix 0` 直通：vdev 麦克风 **RMS −5.8 dBFS / peak 0.0 dBFS**；`--adaptive` 降噪后：**RMS −34.1 dBFS / peak −29.7 dBFS**（纯音被 RNNoise 判为噪声，抑制约 28 dB） |

> 延迟口径别混：探测器的 ~10 ms 量的是 **WASAPI 对渲染端点的 loopback 捕获**；
> 而「vdev 扬声器 → 驱动环形缓冲 → vdev 麦克风」是另一条路径：driver 0.3.9.0 的 1 MB 环形缓冲
> 对应约 **5.46 s** 积压（实测 ≈5.14 s，按设备格式 16bit/48k/2ch = 192000 B/s 算）；
> 0.3.10.0 起缓冲改为 256 KB，对应约 **1.37 s**。两者是不同路径。

构建提示：本机无 MSYS2，RNNoise 用 **Strawberry Perl 自带的 MinGW GCC** 从 `xiph/rnnoise`
v0.1.1 构建（`gcc -shared -o librnnoise-0.dll ... -DRNNOISE_BUILD -DDLL_EXPORT`，
导出 `rnnoise_create/destroy/process_frame`），放到 `crates/vdev-mic-agent/third_party/native/`
即被加载器命中；上游 master 需要生成头文件、MSVC 编不过，故用 v0.1.1。

```bash
# live denoise into the virtual microphone, recording both sides
target/release/vdev-mic-agent live --adaptive --seconds 20 \
    --record-in /tmp/heard.wav --record-out /tmp/sent.wav --report /tmp/live.json

# digital probe: inject -> plugin ring -> capture. No microphone, no room.
target/release/vdev-mic-agent live --probe digital --seconds 30 --report /tmp/digital.json

# acoustic probe: physical speaker -> room -> physical mic -> virtual mic
target/release/vdev-mic-agent live --probe acoustic --seconds 30 --report /tmp/acoustic.json
```

The denoise backend is loaded **at runtime** (`libloading`), never linked — so
there is no link-time dependency on RNNoise. Loading is still a hard
requirement for every denoise path: `run`, `bench`, and `live` (denoise and
`--probe acoustic`) print a load error and exit when `librnnoise` is missing.
The one exception is `live --probe digital`, the digital loopback probe, which
needs no backend and runs without it. `librnnoise-0.dll` /
`librnnoise.dylib` is resolved in order: `--dll <path>`, a non-empty
`$RNNOISE_DLL`, then each executable ancestor directory up to 5 levels — for
each level the directory itself first, then its vendored sub-paths
(`third_party/native/`, `third_party/`, …) — and finally the current directory.
So `crates/vdev-mic-agent/third_party/native/` works from a workspace build, and
the sibling tree works from the demo checkout. It is git-ignored: build it from
[xiph/rnnoise](https://github.com/xiph/rnnoise) or take the MSYS2 `ucrt64`
package (the archive's `third_party/FETCH.md` has the exact steps).

## Layout

| file | role | product equivalent |
|---|---|---|
| `rnnoise.rs` | `libloading` bindings, RAII denoiser | the denoise backend behind a trait |
| `wavio.rs` | mono/48 kHz/PCM16 WAV I/O | capture & render callbacks |
| `agent.rs` | the frame loop, timing, dry/wet blending | the pipeline |
| `mixer.rs` | adaptive dry/wet (noise-floor + VAD) | "is this room noisy?" policy |
| `stats.rs` | p50/p95/p99/max frame times, RTF, CPU | telemetry |
| `metrics.rs` | SI-SDR / segSNR / noise floor | offline scoring (not shipped) |
| `proctime.rs` | `GetProcessTimes` via `kernel32` | telemetry |
| `ring.rs` | lock-free SPSC sample ring | capture thread → render thread |
| `frames.rs` | re-blocking to 480 samples, dry-path delay line | what makes a callback drive a frame model |
| `latency.rs` | chirp marker + normalised cross-correlation | end-to-end measurement |
| `platform/` | CoreAudio HAL (macOS) / WASAPI polling (Windows): capture, inject, probes | the live backend |
| `main.rs` | `run` / `bench` / `diff` / `live` CLI | the agent binary |

Everything here is userland: `platform/macos.rs` drives `vdev-audio`'s HAL
plugin the same way `test_loopback.sh` does, so the driver needs no change.

## Measured (i7-11700, Win10, 48 kHz, 480-sample frames)

Per-frame wall time of the denoise call only, 1806 frames per case:

| | value |
|---|---|
| frame time avg / p50 | 0.078 ms / 0.073 ms |
| frame time p95 / p99 / max | ~0.11 ms / 0.135 ms / 0.18 ms |
| frame budget used (p99) | **~1.4 %** of 10 ms |
| RTF | **0.0081** |
| CPU to run realtime | **0.78 % of one core** |
| realtime streams per core | ~128 |
| RNN state | 32 688 B per stream |
| algorithm latency | 960 samples = **20.0 ms** (measured by cross-correlation) |

## Two design notes that are load-bearing

### 1. Dry/wet blending requires delay compensation

RNNoise is causal but not zero-latency: its output trails its input by
2 frames. Blending the model output with the *undelayed* dry signal creates a
comb filter. Measured on the +30 dB case:

| | SI-SDR |
|---|---|
| model only | 14.14 dB |
| 50/50 blend, **without** delay compensation | **−7.7 dB** |
| model only, delay-compensated bypass path | 26.29 dB |

So whenever `mix < 1` or the adaptive gate can back off, the dry path is delayed
by the model lookahead first (`--lookahead-samples`, default 960). The real
agent has to do the same on the injected stream.

### 2. The adaptive gate is what makes a clean mic *stay* clean

RNNoise attenuates every frame, including ones that were already clean. Pushing
a perfectly clean recording through it costs SI-SDR (329 → 14.7 dB; audible as a
faint metallic colour) for zero benefit.

`--adaptive` fixes that with two slow estimators (VAD-gated minimum-statistics
noise floor + smoothed speech level) and a hysteresis gate at 25 dB long-term
SNR: above it the model is bypassed, below it the model runs fully.

| input (global SNR) | full wet | adaptive gate |
|---|---|---|
| clean reference | 14.72 dB | **30.57 dB** (wet 0.06) |
| +30 dB | 14.14 dB | **26.29 dB** (wet 0.13) |
| +10 dB | 11.04 dB | 11.57 dB (wet 0.85) |
| +5 dB | 7.78 dB | 7.79 dB (wet 1.00) |
| 0 dB | 2.49 dB | 2.49 dB (wet 1.00) |

Read it as: *the gate never degrades a noisy mic, and it stops the model from
damaging a clean one.* Everything is deliberately slow (hundreds of ms) — a
per-frame SNR would flip the gain on every 10 ms boundary and pump audibly.

## Cross-checking against the C and Python harnesses

Same DLL, same frame loop, same warm-up, so the three implementations should
agree sample for sample. `diff` on a 18.06 s file:

```
samples      : 866880
bit-exact    : 49.91 %
within 1 LSB : 100.0000 %
mean |diff|  : 0.5009 LSB
max  |diff|  : 1.0 LSB
```

The 0.5 LSB mean is pure float→int16 rounding: libsndfile (Python) and
`hound`+`round()` (Rust) disagree on exact halves. The DSP itself is identical.

## D3–D4: the live path (`live`)

The macOS backend first — the same `rnnoise` + `mixer` core, driven by real
CoreAudio callbacks:

```
 physical mic ──▶ AudioUnit(HALOutput) ──▶ FrameAssembler ──▶ RNNoise + gate
                      480-sample frames                          │
                                                                 ▼
                             "vdev-audio A" ◀── AudioDeviceIOProc ◀── SpscRing
                                    │
                                    └─ HAL plugin loopback ──▶ Zoom/微信
```

The plugin needs no change: `vdev-audio` already loops its output stream into
its own input stream, so playing into it is enough to feed it. The agent is just
another CoreAudio client.

Four things a file-based harness could not answer, and where they live:

* **The device's buffer size is not ours to choose.** It is 512 frames by
  default on the vdev device, 128–1024 in the wild, and it can change while
  running, while the model accepts exactly 480 samples. `frames::FrameAssembler`
  is the pure re-blocking stage in between; it adds *zero* samples of latency.
* **The lookahead compensation is a delay *line*, not a shifted `Vec`.**
  Live, it is 960 samples of history that has to survive callback boundaries
  (`frames::DelayLine`), and it has to produce the same samples as the batch
  path — which the unit tests assert sample-for-sample.
* **Callbacks are realtime.** They run on a `coreaudiod` thread: no allocation,
  no blocking lock, no unwinding. The model runs inline in the capture callback
  (0.08 ms of a 10 ms budget, from D1–D2) and the hand-off to the render side is
  the lock-free ring in `ring.rs`, whose two counters separate "we dropped
  audio" from "we played silence".
* **Latency is not the sum of the datasheets.** `latency.rs` puts a windowed
  1–4 kHz chirp into the audio and times it with a normalised cross-correlation:
  `--probe digital` (inject → plugin ring → capture, no microphone and no room,
  safe to run in CI) and `--probe acoustic` (speaker → room → microphone →
  virtual mic, which is the number a participant feels). The difference between
  the two is the capture chain, and the model's 20 ms is added to the digital
  number, not to the acoustic one.

**Status: the macOS live path is available; the Windows backend compiles and
every gate is green (check / `clippy -D warnings` for `x86_64-pc-windows-msvc`,
plus the macOS-side check + 41 unit tests), but its runtime audio behavior —
loopback data flow, padding alignment, the acoustic probe — has not been
verified on a real Windows machine with vdev-audio-win installed (driver-side
bring-up is on the project author).** Correctness of the Windows-side numbers
rests on the five `cfg(windows)` unit tests (marker period/start, pending
pairing, search-window cap, acoustic source semantics), which compile with the
cross checks and run on a Windows host. macOS still needs the driver installed
(`make -C crates/vdev-audio install`): the injection side is a CoreAudio client
of the HAL plugin, and both probes need a real device to measure.

### The Windows backend: one polling thread, zero driver changes

`platform/windows.rs` runs the same pipeline — shared-mode capture of the
physical mic, the unchanged `rnnoise` + `mixer` + `frames` core, and the
result written to the *render* endpoint of the vdev-audio-win virtual device,
whose driver loops its render pin back to its capture pin in the kernel:

```
 physical mic ──▶ WASAPI shared capture ──▶ FrameAssembler ──▶ RNNoise + gate
                     (48 kHz mix format)                                │
                          vdev-audio-win render endpoint ◀── SpscRing ◀─┘
                                 │  kernel loopback: render pin → capture pin
                                 └──▶ Zoom/微信 picks it as the microphone
```

It polls instead of using WASAPI's event mode, and that is deliberate: the
probe's capture client is a WASAPI *loopback* capture
(`AUDCLNT_STREAMFLAGS_LOOPBACK` on the vdev render endpoint), and loopback
clients get no dependable buffer event — no packets while nothing renders.
`timeBeginPeriod(1)` keeps the 2 ms poll sleep honest, and each pass tops the
40 ms endpoint buffer up to `GetCurrentPadding`, so the queued depth (the
injection latency) stays constant. Marker timestamps are
`now + padding × frame time` — the estimated playback instant — with the queue
documented in the probe report's `interpretation`.

Windows-specific gates and limits:

* Shared mode does **not** resample: `check_mix_format` refuses endpoints whose
  mix format is not 48 kHz / 32-bit float, with the exact fix (set the device
  default format to 48000 Hz in Control Panel, or pick another endpoint via
  `--input` / `--vdev`). No SRC in v1.
* The macOS default hint `vdev-audio-A-device` is re-mapped to the `vdev`
  substring on Windows (the INF names its endpoints "vdev 扬声器" /
  "vdev 麦克风"); an explicit `--vdev` is used verbatim, and a missing endpoint
  errors with the driver-install hint plus the installed endpoint list.
* No hot-plug / format-change recovery yet (`IMMNotificationClient`), and no
  exclusive mode — shared + polling only; the poll thread allows allocation on
  purpose (it is not a hard-RT callback).
* Both probes are the same machinery as live: digital feeds the marker straight
  into the vdev render and scans the loopback (no mic, no model); acoustic
  plays it from the physical speaker through the room and the mic (pipeline
  dry, mix = 0, delay line engaged) into the same loopback. A virtual device
  other than vdev works too — any loopback sound card passed via `--vdev`
  (e.g. VB-Cable's `CABLE Input`).

```bash
# gates (cross-checked from macOS; CI also runs them natively on windows-latest)
cargo check  -p vdev-mic-agent --target x86_64-pc-windows-msvc
cargo clippy -p vdev-mic-agent --all-targets --target x86_64-pc-windows-msvc -- -D warnings

# on a Windows host, with the vdev-audio-win driver installed (test-signed):
target/release/vdev-mic-agent live --adaptive --seconds 20 --report /tmp/live.json
target/release/vdev-mic-agent live --probe digital --seconds 30 --report /tmp/digital.json
target/release/vdev-mic-agent live --probe acoustic --seconds 30 --report /tmp/acoustic.json
```

## Not done yet

* **Real-device probe numbers.** The expected result of
  `live --probe digital` is the device buffer plus the plugin's ring; anything
  near that is a pass. The Windows live path additionally needs first contact
  with a real machine (see the status note above).
* **AEC.** There is no loudspeaker feedback path here; real speakerphone use
  needs the far-end reference the virtual speaker can provide (二期, can reuse
  vox-seat).
* **Engine selection.** DeepFilterNet3 is ~9× the CPU for clearly better audio;
  the tiering policy is a product decision, not a code one.
* **Subjective listening.** Every number here is objective; a blind A/B is owed.
* `cpu_seconds()` is a Windows implementation. macOS returns 0.0, so until
  `task_info`/`clock_gettime` lands the CPU columns print `n/a` (CPU % of one
  core, streams per core) instead of the meaningless numbers 0 CPU time would
  produce.

## Known limitations

* A pure-silence reference WAV gives `segSNR = NaN`, which JSON cannot
  represent, so `run --reference` / `bench` exit with a serialization error.
  That is expected — there is no meaningful SNR against digital silence — treat
  the clean error exit as intended, not a crash.
