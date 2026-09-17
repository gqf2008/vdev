//! The agent pipeline: capture -> denoise -> inject.
//!
//! In the live path (`platform/`) the denoise runs **inline** in the capture
//! callback/thread: capture -> `FrameAssembler` -> `Core::on_frame` (RNNoise +
//! dry/wet) -> one SPSC ring -> render. There is no separate denoise worker
//! thread. For D1-D2 the capture is a WAV and the injection is a WAV, but the
//! denoise stage is the same code path: one 480-sample frame in, one 480-sample
//! frame out, timed per call.
//!
//! ## Dry/wet mixing needs delay compensation
//!
//! RNNoise is causal but not zero-latency: its output is 2 frames (20 ms) behind
//! its input. Blending the model output with the *undelayed* dry signal creates
//! a comb filter -- measured SI-SDR collapses to -7.7 dB, i.e. far worse than
//! either signal alone. So whenever `mix < 1` (or the adaptive mixer can back
//! off) the dry path is delayed by exactly the model's lookahead before the
//! blend. In the real agent the same thing has to happen on the injected
//! stream.

use crate::mixer::AdaptiveMixer;
use crate::proctime::cpu_seconds;
use crate::rnnoise::{Denoiser, Engine};
use crate::stats::{TimingCollector, TimingStats};
use anyhow::Result;
use serde::Serialize;
use std::time::Instant;

#[derive(Debug, Clone, Copy)]
pub struct RunConfig {
    /// Fixed dry/wet ratio. 1.0 = model output only, 0.0 = pure bypass.
    pub mix: f32,
    /// Ignore `mix`, derive the wet ratio per frame from the room estimate.
    pub adaptive: bool,
    /// Frames pushed through before timing starts, so the RNN state is settled.
    /// Mirrors the native and Python harnesses exactly (same warm-up frames,
    /// then the whole file is processed again).
    pub warmup_frames: usize,
    /// Model lookahead in samples, used to delay the dry path before blending.
    /// RNNoise: 2 frames = 960 samples @ 48 kHz.
    pub lookahead_samples: usize,
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            mix: 1.0,
            adaptive: false,
            warmup_frames: 100,
            lookahead_samples: 960,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RunReport {
    pub frames: usize,
    pub frame_ms: f64,
    pub vad_mean: f64,
    pub wet_ratio_mean: f64,
    pub wet_ratio_min: f64,
    pub wet_ratio_max: f64,
    /// Final noise-floor estimate of the adaptive mixer, in dBFS.
    pub noise_floor_dbfs: f64,
    /// Long-term SNR the adaptive gate decided on.
    pub long_term_snr_db: f64,
    pub dry_delay_samples: usize,
    pub timing: TimingStats,
}

/// Does the dry path have to be delayed to the model's lookahead?
///
/// Only when the dry signal can actually reach the blend: the adaptive mixer, or
/// a genuine partial mix. A pure bypass (`mix == 0`) and a pure wet mix
/// (`mix >= 1`) never blend, so delaying them by the model's 20 ms would charge
/// latency for an alignment that never happens. This is the single source of
/// truth for that rule -- the live paths (`platform/`) call it too, so the
/// offline and live accounts cannot drift apart.
pub fn needs_aligned_dry(mix: f32, adaptive: bool) -> bool {
    adaptive || (mix > 0.0 && mix < 1.0)
}

pub struct RunOutcome {
    pub samples: Vec<f32>,
    pub report: RunReport,
}

pub fn process(input: &[f32], engine: &Engine, cfg: &RunConfig) -> Result<RunOutcome> {
    let mut den: Denoiser = engine.denoiser()?;
    let fs = den.frame_size();
    let n_frames = input.len() / fs;

    // ---- warm-up: settle the RNN state, not timed -------------------------
    for i in 0..cfg.warmup_frames.min(n_frames) {
        let _ = den.process(&input[i * fs..(i + 1) * fs]);
    }

    // ---- dry path, delayed by the model lookahead ------------------------
    let delay = if needs_aligned_dry(cfg.mix, cfg.adaptive) {
        cfg.lookahead_samples.min(input.len())
    } else {
        0
    };
    let dry_delayed: Vec<f32> = if delay == 0 {
        input.to_vec()
    } else {
        let mut v = vec![0.0f32; input.len()];
        v[delay..].copy_from_slice(&input[..input.len() - delay]);
        v
    };

    let mut out = vec![0.0f32; n_frames * fs];
    let mut mixer = AdaptiveMixer::new(cfg.adaptive);
    let mut timing = TimingCollector::new();
    let (mut vad_sum, mut wet_sum) = (0.0f64, 0.0f64);
    let (mut wet_min, mut wet_max) = (f64::MAX, f64::MIN);

    let cpu0 = cpu_seconds();
    let wall0 = Instant::now();

    for i in 0..n_frames {
        let dry = &input[i * fs..(i + 1) * fs];

        // ---- the stage we are measuring ----------------------------------
        let t0 = Instant::now();
        let (vad, wet_frame) = den.process(dry);
        let dt_ms = t0.elapsed().as_secs_f64() * 1000.0;
        timing.push(dt_ms);

        // ---- dry/wet -----------------------------------------------------
        let w = if cfg.adaptive {
            mixer.update(dry, vad)
        } else {
            cfg.mix as f64
        };
        let dst = &mut out[i * fs..(i + 1) * fs];
        if w >= 1.0 {
            dst.copy_from_slice(wet_frame);
        } else if w <= 0.0 {
            dst.copy_from_slice(&dry_delayed[i * fs..(i + 1) * fs]);
        } else {
            let d = &dry_delayed[i * fs..(i + 1) * fs];
            for k in 0..fs {
                dst[k] = (w * wet_frame[k] as f64 + (1.0 - w) * d[k] as f64) as f32;
            }
        }

        vad_sum += vad as f64;
        wet_sum += w;
        wet_min = wet_min.min(w);
        wet_max = wet_max.max(w);
    }

    let wall = wall0.elapsed().as_secs_f64();
    let cpu = (cpu_seconds() - cpu0).max(0.0);
    let n = n_frames.max(1) as f64;

    Ok(RunOutcome {
        samples: out,
        report: RunReport {
            frames: n_frames,
            frame_ms: fs as f64 / crate::wavio::SR as f64 * 1000.0,
            vad_mean: vad_sum / n,
            wet_ratio_mean: wet_sum / n,
            wet_ratio_min: if wet_min == f64::MAX { 0.0 } else { wet_min },
            wet_ratio_max: if wet_max == f64::MIN { 0.0 } else { wet_max },
            noise_floor_dbfs: mixer.noise_floor_dbfs(),
            long_term_snr_db: mixer.long_term_snr_db(),
            dry_delay_samples: delay,
            timing: timing.finish(fs as f64 / crate::wavio::SR as f64 * 1000.0, wall, cpu),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::needs_aligned_dry;

    /// The dry path only needs the model's lookahead when it can actually be
    /// blended; `--mix 0` used to pay 20 ms for nothing (see
    /// `platform::tests::delay_line_is_only_paid_when_the_dry_path_can_reach_the_blend`).
    #[test]
    fn dry_alignment_is_only_owed_when_blending() {
        assert!(!needs_aligned_dry(0.0, false), "pure bypass");
        assert!(!needs_aligned_dry(1.0, false), "wet only");
        assert!(!needs_aligned_dry(1.5, false), "wet only, clamped mix");
        assert!(needs_aligned_dry(0.5, false), "partial mix");
        assert!(needs_aligned_dry(0.0, true), "adaptive gate can reopen");
        assert!(needs_aligned_dry(1.0, true), "adaptive gate");
    }
}
