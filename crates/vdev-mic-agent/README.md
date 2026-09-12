# vdev-mic-agent

AI microphone front-end for [vdev](https://github.com/gqf2008/vdev):
**capture → denoise → inject**, with per-frame timing instrumentation.

It is the userland half of vdev's virtual sound card: **the virtual microphone
endpoint already exists, what is missing is the AI chain that feeds it.** No new
driver is involved — the agent is just another CoreAudio / WASAPI client.

D1–D2 wires capture and injection to WAV files, which answers "how much CPU /
how much algorithmic latency". D3–D4 (`live`) drives the same core from real
callbacks and measures the buffering a WAV cannot show.

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
cargo test -p vdev-mic-agent          # 35 unit tests (metrics, mixer, ring, frames, latency, stats, wavio)

# D3-D4 lives behind CoreAudio; this only type-checks the macOS backend
cargo check -p vdev-mic-agent --target x86_64-apple-darwin

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

`live` is the D3–D4 path and is macOS-only for now; on any other host it prints
why and exits, rather than pretending to run:

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
`librnnoise.dylib` is resolved in order: `--dll <path>`, `$RNNOISE_DLL`, next to
the executable, then a `third_party/` tree walked up to 5 levels — so
`crates/vdev-mic-agent/third_party/native/` works from a workspace build, and
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
| `platform/` | CoreAudio HAL: capture AU, inject IOProc, probes | the live backend |
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

The same `rnnoise` + `mixer` core, driven by real CoreAudio callbacks:

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

**Status: this compiles (`cargo check --target x86_64-apple-darwin`) and every
unit test passes on Windows, but none of it has run against real hardware yet.**
It needs a Mac with the driver installed (`make -C crates/vdev-audio install`):
the injection side is a CoreAudio client of the HAL plugin, and both probes need
a real device to measure.

## Not done yet

* **First contact with hardware.** The expected result of
  `live --probe digital` is the device buffer plus the plugin's ring; anything
  near that is a pass. Nothing below has been validated on a Mac.
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
