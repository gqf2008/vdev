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
//! * `windows` -- WASAPI: shared-mode capture of the physical microphone,
//!   render-side injection into the vdev-audio-win virtual device (whose
//!   driver loops its render pin back to its capture pin). `windows::run`
//!   drives the same pipeline as `macos::run` on one polling thread.
//! * everything else -- the subcommands exist but report that live audio is
//!   unavailable, rather than pretending. The offline matrix and every unit
//!   test still run on Windows/Linux, which is where the model work happened.

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

use anyhow::Result;
use std::path::PathBuf;

/// What to probe. See `latency.rs` for why these are two different numbers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeMode {
    /// Inject -> virtual-mic loopback -> capture. No microphone, no acoustics;
    /// safe to run on CI. Add the model's frame fill (0-10 ms, 5 ms on average)
    /// and its 20 ms lookahead for the model total.
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
    /// Target IO buffer size in frames. The HAL owns
    /// `kAudioDevicePropertyBufferFrameSize` for AudioServerPlugIn devices (the
    /// plugin's own copy of that property is not consulted — measured), so a
    /// client that wants low latency must set it here before starting IO.
    /// 128 frames @48k = 2.67 ms per hop.
    pub buffer_frames: u32,
    /// Marker cadence, in milliseconds.
    pub probe_interval_ms: u64,
    /// Also write what we injected, and what we captured, as WAVs.
    pub record_in: Option<PathBuf>,
    pub record_out: Option<PathBuf>,
    /// JSON report (device info + latency stats + per-frame timings).
    pub report: Option<PathBuf>,
}

/// The conclusion line a digital probe prints about the model's cost.
///
/// One function so macOS and Windows cannot drift, and so a unit test can pin
/// the wording: the model adds **frame fill** (it needs a whole frame before it
/// can emit anything) *plus* its lookahead -- not the lookahead alone. Leaving
/// the fill out made users add 20 ms to the probe number when the real figure
/// is ~25 ms on average.
pub(crate) fn digital_model_cost_note(frame_ms: f64) -> String {
    format!(
        "The model adds its frame fill (0-{:.0} ms, {:.0} ms on average) plus {:.0} ms of lookahead on top of this.",
        frame_ms,
        frame_ms / 2.0,
        frame_ms * 2.0
    )
}

/// Length of the dry-path delay line for this run, in samples.
///
/// Note the two places "20 ms" lives: the live graph hardcodes the model's two
/// frames here (`frame_size * 2`, because `Engine` owns the real frame size),
/// while the offline harness exposes the same number as
/// `--lookahead-samples` (default 960). Changing one without the other makes the
/// live and measured accounts disagree.
///
/// The model's lookahead (RNNoise: 2 frames = 20 ms) only has to be paid when
/// the dry signal can reach the blend -- see [`crate::agent::needs_aligned_dry`].
/// The acoustic probe is the deliberate exception: it runs dry *with* the delay
/// so its timing matches the real chain it is measuring.
pub(crate) fn delay_line_len(cfg: &LiveConfig, frame_size: usize) -> usize {
    let adaptive = cfg.adaptive && cfg.probe.is_none();
    if crate::agent::needs_aligned_dry(cfg.mix, adaptive) || cfg.probe == Some(ProbeMode::Acoustic)
    {
        frame_size * 2
    } else {
        0
    }
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
            buffer_frames: 128,
            probe_interval_ms: 300,
            record_in: None,
            record_out: None,
            report: None,
        }
    }
}

/// Run the agent against real devices. macOS drives the full pipeline on
/// CoreAudio callbacks; Windows drives the same pipeline on a WASAPI polling
/// thread (see `windows::run`).
pub fn run_live(cfg: LiveConfig) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        macos::run(cfg)
    }
    #[cfg(target_os = "windows")]
    {
        windows::run(cfg)
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = cfg;
        anyhow::bail!(
            "live capture/injection is implemented for macOS and Windows, not for this host.\n\
             The D1-D2 offline harness (`run`, `bench`, `diff`) is fully functional here;\n\
             D3-D4 needs a real audio stack: build on macOS with vdev-audio.driver installed\n\
             (make -C crates/vdev-audio install) or on Windows with vdev-audio-win installed,\n\
             then re-run `live` / `probe` there."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The digital probe's conclusion line must keep saying "frame fill **plus**
    /// lookahead": dropping the fill (the old wording) made the model look 5 ms
    /// cheaper than it is.
    #[test]
    fn digital_note_counts_frame_fill_and_lookahead() {
        let note = digital_model_cost_note(10.0);
        assert!(note.contains("frame fill"), "{note}");
        assert!(note.contains("0-10 ms"), "{note}");
        assert!(note.contains("5 ms on average"), "{note}");
        assert!(note.contains("20 ms of lookahead"), "{note}");
    }

    fn cfg(mix: f32, adaptive: bool, probe: Option<ProbeMode>) -> LiveConfig {
        LiveConfig {
            mix,
            adaptive,
            probe,
            ..Default::default()
        }
    }

    /// `--mix 0` is documented as a pure bypass: it must not pay the model's
    /// 20 ms for an alignment that never happens (vdev issue
    /// `mic-agent-bypass-latency-1`). Everything that really blends keeps it.
    #[test]
    fn delay_line_is_only_paid_when_the_dry_path_can_reach_the_blend() {
        assert_eq!(delay_line_len(&cfg(1.0, false, None), 480), 0, "wet only");
        assert_eq!(
            delay_line_len(&cfg(0.0, false, None), 480),
            0,
            "pure bypass"
        );
        assert_eq!(
            delay_line_len(&cfg(0.5, false, None), 480),
            960,
            "partial mix"
        );
        assert_eq!(
            delay_line_len(&cfg(0.0, true, None), 480),
            960,
            "adaptive gate"
        );
        // the acoustic probe keeps the delay on purpose: the marker has to travel
        // the same timing the real chain has
        assert_eq!(
            delay_line_len(&cfg(0.0, false, Some(ProbeMode::Acoustic)), 480),
            960
        );
        assert_eq!(
            delay_line_len(&cfg(1.0, true, Some(ProbeMode::Acoustic)), 480),
            960
        );
    }
}
