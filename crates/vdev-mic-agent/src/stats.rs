//! Per-frame latency statistics.
//!
//! The point of collecting every single frame's wall time (instead of one
//! aggregate RTF) is p99: an audio callback misses its deadline once and the
//! user hears a click, so the tail matters far more than the mean.
//!
//! Two CPU numbers, deliberately, because they answer different questions:
//!   * `cpu_percent_realtime` -- what fraction of one core it costs to keep the
//!     stream running. This is the number that compares against the proposal's
//!     "< 10 % of a core" acceptance criterion.
//!   * `cpu_percent_during_run` -- how saturated the thread is *while* it is
//!     chewing through a file. On a single-threaded busy loop this is ~100 %,
//!     which is expected and not a problem: the frame budget is what matters.

use serde::Serialize;

#[derive(Debug, Default, Clone)]
pub struct TimingCollector {
    pub per_frame_ms: Vec<f64>,
}

#[derive(Debug, Serialize)]
pub struct TimingStats {
    pub frames: usize,
    pub frame_ms_target: f64,
    pub frame_ms_min: f64,
    pub frame_ms_avg: f64,
    pub frame_ms_p50: f64,
    pub frame_ms_p95: f64,
    pub frame_ms_p99: f64,
    pub frame_ms_max: f64,
    pub audio_seconds: f64,
    pub wall_seconds: f64,
    pub real_time_factor: f64,
    pub realtime_budget_used_pct: f64,
    pub cpu_seconds: f64,
    pub cpu_percent_realtime: f64,
    pub cpu_percent_during_run: f64,
    pub realtime_streams_per_core: f64,
}

impl TimingCollector {
    pub fn new() -> Self {
        Self {
            per_frame_ms: Vec::new(),
        }
    }

    pub fn push(&mut self, ms: f64) {
        self.per_frame_ms.push(ms);
    }

    pub fn finish(&self, frame_ms_target: f64, wall: f64, cpu: f64) -> TimingStats {
        let mut v = self.per_frame_ms.clone();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let n = v.len();
        if n == 0 {
            // Same empty-table guard as `LatencyProbe::stats`: all-zero stats
            // instead of an index panic on v[0] / v[n-1].
            return TimingStats {
                frames: 0,
                frame_ms_target,
                frame_ms_min: 0.0,
                frame_ms_avg: 0.0,
                frame_ms_p50: 0.0,
                frame_ms_p95: 0.0,
                frame_ms_p99: 0.0,
                frame_ms_max: 0.0,
                audio_seconds: 0.0,
                wall_seconds: wall,
                real_time_factor: 0.0,
                realtime_budget_used_pct: 0.0,
                cpu_seconds: cpu,
                cpu_percent_realtime: 0.0,
                cpu_percent_during_run: 0.0,
                realtime_streams_per_core: 0.0,
            };
        }
        let pick = |q: f64| v[((n as f64 - 1.0) * q).round() as usize];
        let audio_seconds = self.per_frame_ms.len() as f64 * frame_ms_target / 1000.0;
        let p99 = pick(0.99);
        TimingStats {
            frames: self.per_frame_ms.len(),
            frame_ms_target,
            frame_ms_min: v[0],
            frame_ms_avg: v.iter().sum::<f64>() / n as f64,
            frame_ms_p50: pick(0.50),
            frame_ms_p95: pick(0.95),
            frame_ms_p99: p99,
            frame_ms_max: v[n - 1],
            audio_seconds,
            wall_seconds: wall,
            real_time_factor: wall / audio_seconds.max(1e-9),
            realtime_budget_used_pct: p99 / frame_ms_target * 100.0,
            cpu_seconds: cpu,
            cpu_percent_realtime: cpu / audio_seconds.max(1e-9) * 100.0,
            cpu_percent_during_run: cpu / wall.max(1e-9) * 100.0,
            realtime_streams_per_core: audio_seconds / cpu.max(1e-9),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Empty collection: all-zero stats, never an index panic on v[0]/v[n-1].
    #[test]
    fn empty_collector_yields_all_zero_stats() {
        let c = TimingCollector::new();
        let s = c.finish(10.0, 1.0, 0.05);
        assert_eq!(s.frames, 0);
        assert!(s.frame_ms_min.is_finite() && s.frame_ms_min == 0.0);
        assert!(s.frame_ms_max == 0.0 && s.frame_ms_p99 == 0.0);
        assert!(s.frame_ms_avg.is_finite() && s.real_time_factor.is_finite());
        assert!(s.cpu_percent_realtime.is_finite());
    }

    #[test]
    fn populated_collector_keeps_percentiles() {
        let mut c = TimingCollector::new();
        for i in 0..=100 {
            c.push(i as f64);
        }
        let s = c.finish(10.0, 1.0, 0.05);
        assert_eq!(s.frames, 101);
        assert_eq!(s.frame_ms_min, 0.0);
        assert_eq!(s.frame_ms_max, 100.0);
        assert_eq!(s.frame_ms_p99, 99.0);
    }
}
