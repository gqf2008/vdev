//! Objective speech-quality metrics -- the Rust twin of the Python `metrics.py`,
//! so the numbers in both reports are directly comparable.
//!
//! All signals are in int16 scale (0 dBFS == 32768).
//!
//! The one subtlety that cost real debugging time: if `est[t] == ref[t - L]`
//! (the estimate is *delayed* by L), the normalised cross-correlation peak sits
//! at `est[L..] vs ref[..n-L]`. Searching the opposite direction silently
//! returns lag 0 and turns a 20 ms group delay into a fake "-18 dB SI-SDR".

use serde::Serialize;

pub const FRAME: usize = 480;
const FULL_SCALE: f64 = 32768.0;

/// The delay is constant for the whole file, so we can estimate it on a 2 s
/// window and then shift the full signal -- 9x cheaper than searching the whole
/// recording, for identical results.
const ALIGN_WINDOW: usize = 2 * super::wavio::SR as usize;

#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub si_sdr_db: f64,
    pub align_lag_samples: usize,
    pub align_lag_ms: f64,
    pub align_corr: f64,
    pub segsnr_db: f64,
    pub noise_floor_dbfs: f64,
    pub speech_rms_dbfs: f64,
}

#[derive(Debug, Serialize)]
pub struct MetricsReport {
    pub si_sdr_db: f64,
    pub align_lag_samples: usize,
    pub align_lag_ms: f64,
    pub align_corr: f64,
    pub segsnr_db: f64,
    pub noise_floor_dbfs: f64,
    pub speech_rms_dbfs: f64,
}

impl Metrics {
    pub fn report(&self) -> MetricsReport {
        MetricsReport {
            si_sdr_db: round2(self.si_sdr_db),
            align_lag_samples: self.align_lag_samples,
            align_lag_ms: round2(self.align_lag_ms),
            align_corr: round4(self.align_corr),
            segsnr_db: round2(self.segsnr_db),
            noise_floor_dbfs: round2(self.noise_floor_dbfs),
            speech_rms_dbfs: round2(self.speech_rms_dbfs),
        }
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}
fn round4(v: f64) -> f64 {
    (v * 10000.0).round() / 10000.0
}

/// Delay of `est` w.r.t. `ref`, in samples. Returns (aligned_est, lag, corr).
pub fn align(ref_sig: &[f32], est: &[f32], max_lag: usize) -> (Vec<f32>, usize, f64) {
    let n_full = ref_sig.len().min(est.len());
    let n = n_full.min(ALIGN_WINDOW);
    let max_lag = max_lag.min(n.saturating_sub(1));
    let ref64: Vec<f64> = ref_sig[..n].iter().map(|&v| v as f64).collect();
    let est64: Vec<f64> = est[..n].iter().map(|&v| v as f64).collect();

    // prefix sums of squares -> O(1) norms for every candidate lag
    let mut ss_ref = vec![0.0f64; n + 1];
    let mut ss_est = vec![0.0f64; n + 1];
    for i in 0..n {
        ss_ref[i + 1] = ss_ref[i] + ref64[i] * ref64[i];
        ss_est[i + 1] = ss_est[i] + est64[i] * est64[i];
    }

    let mut best_c = f64::NEG_INFINITY;
    let mut best_lag = 0usize;
    for lag in 0..=max_lag {
        let m = n - lag;
        let numer: f64 = est64[lag..].iter().zip(&ref64[..m]).map(|(a, b)| a * b).sum();
        let den = ((ss_est[n] - ss_est[lag]).max(0.0) * ss_ref[m]).sqrt();
        if den <= 0.0 {
            continue;
        }
        let c = numer / den;
        if c > best_c {
            best_c = c;
            best_lag = lag;
        }
    }

    let mut aligned: Vec<f32> = Vec::with_capacity(n_full);
    if best_lag == 0 {
        aligned.extend_from_slice(&est[..n_full]);
    } else {
        aligned.extend(est[best_lag..n_full].iter().copied());
        aligned.resize(n_full, 0.0);
    }
    (aligned, best_lag, best_c)
}

pub fn si_sdr(ref_sig: &[f32], est: &[f32], max_lag: usize) -> Metrics {
    let (aligned, lag, corr) = align(ref_sig, est, max_lag);
    let n = ref_sig.len().min(aligned.len());
    let r = &ref_sig[..n];
    let e = &aligned[..n];
    let mr = mean(r);
    let me = mean(e);
    let rr: Vec<f64> = r.iter().map(|&v| v as f64 - mr).collect();
    let ee: Vec<f64> = e.iter().map(|&v| v as f64 - me).collect();

    let dot: f64 = ee.iter().zip(&rr).map(|(a, b)| a * b).sum();
    let denom: f64 = rr.iter().map(|v| v * v).sum();
    let a = dot / (denom + 1e-20);
    let target: Vec<f64> = rr.iter().map(|v| a * v).collect();
    let resid: f64 = ee
        .iter()
        .zip(&target)
        .map(|(e, t)| {
            let d = e - t;
            d * d
        })
        .sum();
    let tgt: f64 = target.iter().map(|v| v * v).sum();
    let s = 10.0 * ((tgt + 1e-20) / (resid + 1e-20)).log10();

    Metrics {
        si_sdr_db: s,
        align_lag_samples: lag,
        align_lag_ms: lag as f64 / super::wavio::SR as f64 * 1000.0,
        align_corr: corr,
        segsnr_db: seg_snr_aligned(r, e),
        noise_floor_dbfs: noise_floor_aligned(r, e),
        speech_rms_dbfs: speech_rms_aligned(r, e),
    }
}

fn frame_energies(x: &[f32]) -> Vec<f64> {
    x.chunks_exact(FRAME)
        .map(|f| f.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / FRAME as f64)
        .collect()
}

fn seg_snr_aligned(r: &[f32], e: &[f32]) -> f64 {
    let n = (r.len().min(e.len()) / FRAME) * FRAME;
    let pr = frame_energies(&r[..n]);
    let pe: Vec<f64> = e[..n]
        .chunks_exact(FRAME)
        .zip(r[..n].chunks_exact(FRAME))
        .map(|(ef, rf)| {
            ef.iter()
                .zip(rf)
                .map(|(&a, &b)| {
                    let d = a as f64 - b as f64;
                    d * d
                })
                .sum::<f64>()
                / FRAME as f64
        })
        .collect();
    let max_p = pr.iter().cloned().fold(f64::MIN, f64::max);
    let thr = max_p * 10f64.powf(-15.0 / 10.0);
    let mut acc = 0.0;
    let mut cnt = 0usize;
    for i in 0..pr.len() {
        if pr[i] > thr {
            let v = 10.0 * ((pr[i] + 1e-20) / (pe[i] + 1e-20)).log10();
            acc += v.clamp(-10.0, 35.0);
            cnt += 1;
        }
    }
    if cnt == 0 {
        f64::NAN
    } else {
        acc / cnt as f64
    }
}

/// Level of the quietest 10 % of reference frames -> residual noise floor.
fn noise_floor_aligned(r: &[f32], e: &[f32]) -> f64 {
    let n = (r.len().min(e.len()) / FRAME) * FRAME;
    let pr = frame_energies(&r[..n]);
    let ef: Vec<&[f32]> = e[..n].chunks_exact(FRAME).collect();
    let k = ((pr.len() as f64 * 0.10) as usize).max(1);
    let mut idx: Vec<usize> = (0..pr.len()).collect();
    idx.sort_by(|&a, &b| pr[a].partial_cmp(&pr[b]).unwrap_or(std::cmp::Ordering::Equal));
    let samples: f64 = idx[..k]
        .iter()
        .map(|&i| ef[i].iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>())
        .sum::<f64>()
        / (k * FRAME) as f64;
    20.0 * (samples.sqrt() / FULL_SCALE + 1e-20).log10()
}

fn speech_rms_aligned(r: &[f32], e: &[f32]) -> f64 {
    let n = (r.len().min(e.len()) / FRAME) * FRAME;
    let pr = frame_energies(&r[..n]);
    let max_p = pr.iter().cloned().fold(f64::MIN, f64::max);
    let thr = max_p * 10f64.powf(-10.0 / 10.0);
    let ef: Vec<&[f32]> = e[..n].chunks_exact(FRAME).collect();
    let mut acc = 0.0;
    let mut cnt = 0usize;
    for i in 0..pr.len() {
        if pr[i] > thr {
            acc += ef[i].iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>();
            cnt += 1;
        }
    }
    if cnt == 0 {
        return f64::NAN;
    }
    let rms = (acc / (cnt * FRAME) as f64).sqrt();
    20.0 * (rms / FULL_SCALE + 1e-20).log10()
}

fn mean(x: &[f32]) -> f64 {
    x.iter().map(|&v| v as f64).sum::<f64>() / x.len().max(1) as f64
}

/// RMS in dBFS, for reporting input levels.
pub fn dbfs(x: &[f32]) -> f64 {
    let ms = x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len().max(1) as f64;
    20.0 * (ms.sqrt() / FULL_SCALE + 1e-20).log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_a_known_delay() {
        let n = 48_000;
        let mut sig = vec![0.0f32; n];
        for i in 0..n {
            sig[i] = ((i as f64 * 0.01).sin() * 1000.0) as f32;
        }
        let mut delayed = vec![0.0f32; n];
        let lag = 960;
        delayed[lag..].copy_from_slice(&sig[..n - lag]);
        let (_, found, corr) = align(&sig, &delayed, 3 * FRAME);
        assert_eq!(found, lag);
        assert!(corr > 0.99, "corr {corr}");
    }

    #[test]
    fn reports_full_si_sdr_on_identical_signals() {
        let n = 48_000;
        let sig: Vec<f32> = (0..n).map(|i| ((i as f64 * 0.05).sin() * 5000.0) as f32).collect();
        let m = si_sdr(&sig, &sig, 3 * FRAME);
        assert!(m.si_sdr_db > 100.0, "got {}", m.si_sdr_db);
    }
}
