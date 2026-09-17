//! End-to-end latency measurement.
//!
//! The acceptance criterion we have to answer for D3-D4 is "does a conference
//! call feel laggy?", and the honest way to answer it is to put a **marker in
//! the audio** and time it through the whole graph. Not to add up datasheet
//! buffer sizes -- those ignore scheduling, the plugin's ring, and (in the
//! acoustic case) the fact that input and output streams of one device are not
//! even the same sample clock.
//!
//! Two probes, because they answer different questions:
//!
//! * **Digital probe** (`--probe digital`). Inject a marker into the virtual
//!   microphone's output stream, then watch the same device's *input* stream
//!   (which the HAL plugin loops back) for that marker. This measures
//!   `inject -> playback buffer -> plugin ring -> capture buffer -> consumer`:
//!   the buffering the model's frame fill + 20 ms lookahead have to be added to. It needs no
//!   microphone and no quiet room, so it is the number CI can check.
//! * **Acoustic probe** (`--probe acoustic`). Play a click out of the physical
//!   speaker, let the physical microphone hear it, and watch for the marker in
//!   the virtual microphone. This is the number the *user* experiences: it adds
//!   the mic's own buffer, the ADC/DAC, and ~1 ms of sound propagation
//!   (~34 cm).
//!
//! Both use the same marker and the same detector, so the two numbers are
//! directly comparable and their difference is exactly the capture chain.

use std::time::Instant;

/// One RNNoise frame long, so the marker can be pushed through the pipeline on
/// the same cadence as audio without special-casing the frame splitter.
pub const MARKER_LEN: usize = 480;

/// Windowed linear chirp, 1 kHz -> 4 kHz.
///
/// A chirp rather than an impulse on purpose: an impulse has a flat spectrum
/// (unpleasant, and it clips the limiter), while a chirp concentrates the energy
/// where speech is, survives the model's spectral gating, and still
/// autocorrelates to a single sharp peak.
pub fn marker() -> Vec<f32> {
    let n = MARKER_LEN;
    let f0 = 1000.0f64;
    let f1 = 4000.0f64;
    let fs = 48_000.0f64;
    let t_total = n as f64 / fs;
    // linear chirp phase: phi(t) = 2*pi*(f0*t + (f1-f0)/(2*T)*t^2)
    let mut v = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 / fs;
        let phi = 2.0 * std::f64::consts::PI * (f0 * t + (f1 - f0) / (2.0 * t_total) * t * t);
        // Hann window: keeps the chirp's edges from splattering, which is what
        // makes the correlation peak sharp.
        let w = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (n as f64 - 1.0)).cos();
        v.push((phi.sin() * w) as f32);
    }
    // normalise to 0.5 full scale: loud enough to survive a -6 dB path, quiet
    // enough to never hit the plugin's soft limiter
    let peak = v.iter().fold(0.0f32, |m, x| m.max(x.abs())).max(1e-9);
    for x in v.iter_mut() {
        *x = *x / peak * 0.5;
    }
    v
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detection {
    /// Offset of the marker's first sample inside the searched buffer.
    pub start: usize,
    /// Normalised cross-correlation at the peak, in [-1, 1].
    pub ncc: f32,
}

/// Normalised cross-correlation search.
///
/// Returns the peak when it clears `min_ncc`. The threshold matters: a bare
/// "loudest window" detector fires on speech, which is exactly what is also in
/// the buffer during a real call.
///
/// O(n * m) with the window energy from a prefix sum, so it is O(n) memory and
/// no FFT dependency. `n` is one device buffer per call, so the cost is a few
/// hundred thousand multiplies -- fine for a diagnostic mode.
#[allow(dead_code)] // the offline convenience wrapper; the callback wants `detect_with`
pub fn detect(haystack: &[f32], needle: &[f32], min_ncc: f32) -> Option<Detection> {
    let mut prefix = Vec::new();
    detect_with(haystack, needle, min_ncc, &mut prefix)
}

/// Same as [`detect`], but the caller owns the prefix-sum scratch. The
/// callback in `platform::macos` must not allocate, so it keeps one of these
/// alive and reuses its capacity across buffers: the buffer only ever grows
/// when a longer search window arrives, and then never again.
pub fn detect_with(
    haystack: &[f32],
    needle: &[f32],
    min_ncc: f32,
    prefix: &mut Vec<f64>,
) -> Option<Detection> {
    let m = needle.len();
    if haystack.len() < m || m == 0 {
        return None;
    }
    let needle_energy: f64 = needle.iter().map(|&x| (x as f64) * (x as f64)).sum();
    if needle_energy <= 1e-12 {
        return None;
    }

    // prefix sums of x^2 for O(1) window energy
    prefix.clear();
    let n = haystack.len();
    prefix.reserve(n + 1);
    prefix.push(0.0f64);
    for &x in haystack {
        let prev = *prefix.last().unwrap();
        prefix.push(prev + (x as f64) * (x as f64));
    }

    let mut best = Detection {
        start: 0,
        ncc: -1.0,
    };

    // A cheap amplitude gate first: windows quieter than -60 dBFS relative to
    // the marker cannot be the marker, and skipping them is most of the work
    // during silence.
    let floor = needle_energy * 1e-6;

    for i in 0..=haystack.len() - m {
        let win_energy = prefix[i + m] - prefix[i];
        if win_energy <= floor {
            continue;
        }
        let mut dot = 0.0f64;
        for k in 0..m {
            dot += needle[k] as f64 * haystack[i + k] as f64;
        }
        let ncc = dot / (needle_energy * win_energy).sqrt();
        if ncc > best.ncc as f64 {
            best = Detection {
                start: i,
                ncc: ncc as f32,
            };
        }
    }

    if best.ncc >= min_ncc {
        Some(best)
    } else {
        None
    }
}

/// Hard cap on stored raw samples. `record()` runs on the realtime audio
/// callback path, so the buffer must have a bounded worst case and never
/// reallocate without limit. Once the cap is reached, further round trips
/// still count in `detected` but are *not* appended; the summary statistics
/// therefore describe the first [`MAX_SAMPLES`] detections only, and
/// `LatencyStats::overflowed` reports how many were left out.
const MAX_SAMPLES: usize = 4096;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LatencyStats {
    pub count: usize,
    pub injected: u64,
    pub detected: u64,
    pub rejected: u64,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    /// Standard deviation, in ms. A wide spread means the call will *occasionally*
    /// feel worse than the median suggests -- worth reporting next to p95.
    pub stdev_ms: f64,
    /// Round trips beyond [`MAX_SAMPLES`] that were counted but not stored:
    /// the percentile fields describe the first [`MAX_SAMPLES`] detections only.
    pub overflowed: u64,
}

/// Accumulates inject->hear deltas.
///
/// Wall-clock (`Instant`) rather than audio sample times on purpose: the
/// physical output device and the virtual microphone are different devices with
/// independent sample clocks, so their sample-time counters are not comparable
/// at all. `Instant` is monotonic and is the only clock both sides share.
#[derive(Debug)]
pub struct LatencyProbe {
    deltas_ms: Vec<f64>,
    overflowed: u64,
    injected: u64,
    detected: u64,
    rejected: u64,
}

impl Default for LatencyProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl LatencyProbe {
    pub fn new() -> Self {
        Self {
            deltas_ms: Vec::with_capacity(MAX_SAMPLES), // one upfront allocation on the RT path
            overflowed: 0,
            injected: 0,
            detected: 0,
            rejected: 0,
        }
    }

    #[allow(dead_code)] // only useful for a thread-fed emitter; the probe in
                        // `platform/macos` timestamps inside the callback instead
    pub fn mark_injected(&mut self) {
        self.injected += 1;
    }

    /// Record one successful round trip. The caller only calls this when the
    /// detector's correlation cleared its threshold -- a weak peak is not a
    /// latency measurement, and averaging it in would hide real jitter.
    pub fn record(&mut self, sent: Instant, heard: Instant) {
        self.detected += 1;
        // saturating: a clock that appears to run backwards reports 0, not a panic
        let ms = heard.duration_since(sent).as_secs_f64() * 1000.0;
        if self.deltas_ms.len() < MAX_SAMPLES {
            self.deltas_ms.push(ms);
        } else {
            self.overflowed += 1;
        }
    }

    pub fn record_rejected(&mut self) {
        self.rejected += 1;
    }

    #[allow(dead_code)] // raw deltas, for plotting a latency histogram
    pub fn samples(&self) -> &[f64] {
        &self.deltas_ms
    }

    pub fn stats(&self) -> LatencyStats {
        let mut v = self.deltas_ms.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = v.len();
        if n == 0 {
            return LatencyStats {
                count: 0,
                injected: self.injected,
                detected: self.detected,
                rejected: self.rejected,
                min_ms: 0.0,
                mean_ms: 0.0,
                p50_ms: 0.0,
                p95_ms: 0.0,
                max_ms: 0.0,
                stdev_ms: 0.0,
                overflowed: self.overflowed,
            };
        }
        let pick = |q: f64| v[(((n as f64 - 1.0) * q).round() as usize).min(n - 1)];
        let mean = v.iter().sum::<f64>() / n as f64;
        let var = v.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n as f64;
        LatencyStats {
            count: n,
            injected: self.injected,
            detected: self.detected,
            rejected: self.rejected,
            min_ms: v[0],
            mean_ms: mean,
            p50_ms: pick(0.50),
            p95_ms: pick(0.95),
            max_ms: v[n - 1],
            stdev_ms: var.sqrt(),
            overflowed: self.overflowed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_is_full_scale_half_and_frame_sized() {
        let m = marker();
        assert_eq!(m.len(), MARKER_LEN);
        let peak = m.iter().fold(0.0f32, |a, x| a.max(x.abs()));
        assert!((peak - 0.5).abs() < 1e-4, "peak {peak}");
        // windowed: the edges must be near silence, otherwise the correlator
        // sees a discontinuity
        assert!(m[0].abs() < 0.02 && m[MARKER_LEN - 1].abs() < 0.02);
    }

    #[test]
    fn detects_marker_at_the_exact_offset() {
        let m = marker();
        let mut buf = vec![0.0f32; 1600];
        let at = 733;
        buf[at..at + MARKER_LEN].copy_from_slice(&m);
        let d = detect(&buf, &m, 0.5).expect("marker present");
        assert_eq!(d.start, at);
        assert!(d.ncc > 0.99, "ncc {}", d.ncc);
    }

    #[test]
    fn detects_marker_under_noise_and_attenuation() {
        let m = marker();
        // deterministic pseudo-noise
        let mut s = 12345u64;
        let mut buf: Vec<f32> = (0..2048)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .map(|x| x * 0.3)
            .collect();
        let at = 500;
        for k in 0..MARKER_LEN {
            buf[at + k] += m[k] * 0.25; // -12 dB relative to the marker
        }
        let d = detect(&buf, &m, 0.3).expect("marker present under noise");
        assert_eq!(d.start, at);
    }

    #[test]
    fn rejects_when_absent() {
        let m = marker();
        let mut s = 999u64;
        let buf: Vec<f32> = (0..2048)
            .map(|_| {
                s = s
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                ((s >> 33) as f32 / (1u64 << 31) as f32) - 0.5
            })
            .collect();
        assert!(detect(&buf, &m, 0.35).is_none());
    }

    #[test]
    fn rejects_silence_and_short_buffers() {
        let m = marker();
        assert!(detect(&vec![0.0f32; 4096], &m, 0.1).is_none());
        assert!(detect(&[1.0f32; 10], &m, 0.1).is_none());
    }

    #[test]
    fn latency_stats_percentiles() {
        let mut p = LatencyProbe::new();
        let base = Instant::now();
        for i in 0..=100 {
            p.mark_injected();
            // 100 samples, 0..99 ms
            p.deltas_ms.push(i as f64);
        }
        let s = p.stats();
        assert_eq!(s.count, 101);
        assert_eq!(s.injected, 101);
        assert_eq!(s.min_ms, 0.0);
        assert_eq!(s.max_ms, 100.0);
        assert_eq!(s.p50_ms, 50.0);
        assert_eq!(s.p95_ms, 95.0);
        assert!((s.mean_ms - 50.0).abs() < 1e-9);
        assert!((s.stdev_ms - 29.1548).abs() < 0.01, "{}", s.stdev_ms);
        let _ = base;
    }

    #[test]
    fn empty_probe_is_all_zero_not_nan() {
        let p = LatencyProbe::new();
        let s = p.stats();
        assert_eq!(s.count, 0);
        assert!(s.p95_ms.is_finite() && s.mean_ms.is_finite());
    }

    /// record() must never grow the buffer past MAX_SAMPLES (it runs on the
    /// realtime callback path); extra detections are counted, stats stay valid.
    #[test]
    fn record_is_bounded_and_stats_stay_usable() {
        let mut p = LatencyProbe::new();
        let t0 = Instant::now();
        for _ in 0..(MAX_SAMPLES as u64 + 500) {
            p.record(t0, t0);
        }
        assert_eq!(p.samples().len(), MAX_SAMPLES);
        let s = p.stats();
        assert_eq!(s.count, MAX_SAMPLES);
        assert_eq!(s.detected, MAX_SAMPLES as u64 + 500);
        assert_eq!(s.overflowed, 500);
        assert!(s.mean_ms.is_finite() && s.max_ms.is_finite() && s.stdev_ms.is_finite());
    }
}
