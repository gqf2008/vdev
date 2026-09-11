//! Platform audio backends.
//!
//! D1-D2 had no backend at all: capture was a WAV and injection was a WAV, and
//! that was enough to answer "how much CPU / how much algorithmic latency".
//! D3-D4 is exactly the question that a file cannot answer -- **buffering and
//! scheduling** -- so the same `rnnoise` + `mixer` + `frames` core is now
//! driven by real device callbacks.
//!
//! Split by OS so the offline harness stays buildable everywhere:
//!
//! * `macos` -- CoreAudio HAL: `AudioUnit` for the physical microphone, an
//!   `AudioDeviceIOProc` on the vdev virtual device for injection and for both
//!   latency probes. The half that D3-D4 is about.
//! * everything else -- the subcommands exist but report that live audio is
//!   unavailable, rather than pretending. The offline matrix and every unit
//!   test still run on Windows/Linux, which is where the model work happened.

#[cfg(target_os = "macos")]
pub mod macos;

use anyhow::Result;
use std::path::PathBuf;

/// What to probe. See `latency.rs` for why these are two different numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMode {
    /// Inject -> virtual-mic loopback -> capture. No microphone, no acoustics;
    /// safe to run on CI. Add the model lookahead (20 ms) for the model total.
    Digital,
    /// Physical speaker -> room -> physical microphone -> pipeline -> virtual
    /// microphone. This is the number the user feels.
    Acoustic,
}

#[derive(Debug, Clone)]
// Every field is read by the macOS backend; on other hosts `run_live` can
// only bail, so the struct is data the compiler cannot see anyone consume.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub struct LiveConfig {
    pub dll: Option<PathBuf>,
    pub adaptive: bool,
    /// Fixed dry/wet ratio (ignored when `adaptive`).
    pub mix: f32,
    /// Virtual device to inject into, matched by UID or name substring.
    /// Default is the plugin's device A (`vdev-audio-A-device`).
    pub vdev: String,
    /// Physical capture device, by UID or name substring. `None` = system default.
    pub input: Option<String>,
    /// Run for this many seconds (wall clock), then stop cleanly.
    pub seconds: f64,
    /// `Some` switches the run into a latency probe instead of a live denoise.
    pub probe: Option<ProbeMode>,
    /// Marker cadence, in milliseconds.
    pub probe_interval_ms: u64,
    /// Also write what we injected, and what we captured, as WAVs.
    pub record_in: Option<PathBuf>,
    pub record_out: Option<PathBuf>,
    /// JSON report (device info + latency stats + per-frame timings).
    pub report: Option<PathBuf>,
}

impl Default for LiveConfig {
    fn default() -> Self {
        Self {
            dll: None,
            adaptive: true,
            mix: 1.0,
            vdev: "vdev-audio-A-device".to_string(),
            input: None,
            seconds: 20.0,
            probe: None,
            probe_interval_ms: 300,
            record_in: None,
            record_out: None,
            report: None,
        }
    }
}

/// Run the agent against real devices. macOS only in this build.
pub fn run_live(cfg: LiveConfig) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        return macos::run(cfg);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = cfg;
        anyhow::bail!(
            "live capture/injection is only implemented for macOS in this build.\n\
             The D1-D2 offline harness (`run`, `bench`, `diff`) is fully functional here;\n\
             D3-D4 needs CoreAudio, so build this crate on the Mac that has vdev-audio.driver\n\
             installed (make -C crates/vdev-audio install) and re-run `live` / `probe` there."
        )
    }
}
