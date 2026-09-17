//! WASAPI backend for Windows.
//!
//! Same pipeline shape as `macos.rs`, different system: capture the physical
//! microphone through WASAPI shared mode, run it through the same
//! `rnnoise` + `mixer` + `frames` core, and write the result to the *render*
//! endpoint of the vdev-audio-win virtual device. The driver's render pin is
//! looped back to its capture pin, so a meeting app that picks the virtual
//! capture endpoint hears the denoised signal -- no driver changes needed.
//!
//! The live loop ([`run`]) drives one thread through a **polling** design
//! (timer-driven, `GetCurrentPadding`-aligned) rather than WASAPI's
//! event-driven mode:
//!
//! * the digital probe's capture client is a WASAPI *loopback* capture
//!   (`AUDCLNT_STREAMFLAGS_LOOPBACK` on the vdev render endpoint), and a
//!   loopback capture client gets no dependable buffer event -- WASAPI only
//!   signals capture events for real capture endpoints, and the loopback
//!   buffer simply sees no packets while nothing renders;
//! * one uniform poll pass (capture drain -> render top-up -> loopback scan)
//!   therefore covers every stream kind, live and both probe modes, with no
//!   per-stream event plumbing and no scheduling hop between the marker
//!   write and its timestamp;
//! * pacing comes from `GetCurrentPadding`: each pass writes every frame the
//!   endpoint buffer can still take, so the queued depth (and with it the
//!   injection latency) stays constant instead of drifting;
//! * `timeBeginPeriod(1)` keeps the 2 ms poll sleep honest; without it the
//!   default ~15.6 ms scheduler granularity would starve a 20 ms buffer.
//!
//! The audio path itself is macOS-identical: `FrameAssembler` ->
//! `Denoiser` -> `AdaptiveMixer` + `DelayLine` -> `SpscRing` -> render. All
//! of that lives in the platform-neutral modules so the offline numbers stay
//! valid; nothing here re-implements DSP.
//!
//! * `ComGuard` -- COM apartment lifetime (WASAPI lives on COM).
//! * `list_endpoints` / `select_capture` / `select_vdev_render` -- endpoint
//!   enumeration and the two device choices the pipeline needs.
//! * `AudioClient` -- shared-mode `IAudioClient` wrapper: init from the mix
//!   format (with stream flags / buffer duration for the loopback client),
//!   buffer queries, start/stop, capture/render service accessors.
//! * `MixFormat` -- owned `GetMixFormat` allocation (may be a
//!   `WAVEFORMATEXTENSIBLE`, so it stays a raw `CoTaskMem` allocation).
//!
//! Channel conversion lives in `crate::mixer` (`to_mono`), where its unit
//! tests can run on any host.
//!
//! Known v1 limits (see the module bottom): no device-lost / format-change
//! recovery (`IMMNotificationClient`), no resampling away from 48 kHz, and
//! the poll thread allows allocations (it is not a hard-RT callback, which
//! is exactly why polling was chosen over callbacks here).

// The cfg(windows) module keeps a couple of caller-facing accessors
// (`kind()`, `list_endpoints`) that no Windows caller exercises yet; the
// blanket allow keeps that scaffolding lint-clean until it grows a consumer.
#![allow(dead_code)]

use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{BSTR, GUID};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::Media::Audio::{
    eCapture, eCommunications, eConsole, eRender, EDataFlow, IAudioCaptureClient, IAudioClient,
    IAudioRenderClient, IMMDevice, IMMDeviceCollection, IMMDeviceEnumerator,
    AUDCLNT_BUFFERFLAGS_SILENT, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::Media::{timeBeginPeriod, timeEndPeriod};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL,
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

use crate::frames::{DelayLine, FrameAssembler};
use crate::latency::{detect_with, marker, LatencyProbe, LatencyStats, MARKER_LEN};
use crate::mixer::{to_mono, AdaptiveMixer};
use crate::proctime::cpu_seconds;
use crate::ring::SpscRing;
use crate::rnnoise::{Denoiser, Engine, FRAME};

use super::{LiveConfig, ProbeMode};

/// `CLSID_MMDeviceEnumerator` ({BCDE0395-E52F-467C-8E3D-C4579291692E}). The
/// windows 0.58 crate does not export MMDeviceAPI class ids as constants, so
/// the well-known value is defined here and cross-checked by every endpoint
/// call actually reaching the audio service.
const CLSID_MMDEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE0395_E52F_467C_8E3D_C4579291692E);

/// The pipeline rate. RNNoise and the whole `frames`/`latency` core are
/// fixed at 48 kHz (`FRAME` = 480 = 10 ms); macOS asks the HAL to convert,
/// while WASAPI shared mode does not resample at all -- see
/// [`check_mix_format`], which refuses non-48 kHz endpoints with the fix.
const PIPELINE_HZ: u32 = 48_000;

/// Endpoint-buffer request for every stream: 40 ms in 100 ns units. Big
/// enough that a scheduler hiccup cannot starve the render side between two
/// 2 ms poll passes, small enough that the queue depth it adds to the
/// measured probe latencies stays a small, documented constant. Shared mode
/// may round the request up to the engine period.
const STREAM_BUFFER_HNS: i64 = 400_000;

/// Poll-pass interval. With `timeBeginPeriod(1)` this wakes every ~2 ms, so
/// a full 40 ms buffer is topped up long before the engine can drain it.
const POLL_INTERVAL_MS: u64 = 2;

/// Rolling search-window cap for the marker detector: 1 s of mono samples,
/// which covers a 300 ms marker cadence with room to spare (same as the
/// macOS `SEARCH_CAP`).
const SEARCH_CAP: usize = 48_000;

/// Raw A/B recording cap: 120 s per side, shared with the macOS backend so
/// `--record-in/--record-out` produce identically bounded files.
const RECORD_SECONDS: usize = 120;
const RECORD_SAMPLES: usize = 48_000 * RECORD_SECONDS;

/// Live entry point (`platform::run_live` dispatches here on Windows).
///
/// Contract (mirrors `platform::macos::run`): select devices, open shared
/// mode, run for `cfg.seconds` (wall clock), stop cleanly, print a summary
/// and optionally write `record_in` / `record_out` WAVs and a JSON report.
///
/// Mode matrix:
/// * live denoise: mic -> core -> vdev render;
/// * `--probe digital`: no mic, no engine; the marker emitter feeds the vdev
///   render directly and the driver's render->capture loopback is scanned
///   for it;
/// * `--probe acoustic`: physical speaker plays the marker, the room and
///   the mic are the channel, the pipeline runs dry (mix = 0, delay line
///   engaged) into the vdev render, and the same loopback scan detects.
pub fn run(cfg: LiveConfig) -> Result<()> {
    // Declared first => dropped last: the COM apartment must outlive every
    // interface pointer below (Rust drops locals in reverse declaration
    // order). Same invariant the macOS backend encodes with its leaked
    // callback contexts; here plain drop order suffices because there are
    // no callbacks, only this one thread.
    let _com = ComGuard::new().context("initialising COM for WASAPI failed")?;
    let _tp = TimePeriod::new();

    let digital = cfg.probe == Some(ProbeMode::Digital);
    let acoustic = cfg.probe == Some(ProbeMode::Acoustic);
    let is_probe = cfg.probe.is_some();

    // ---- engine (not needed for the digital probe) ------------------------
    let engine = match cfg.probe {
        Some(ProbeMode::Digital) => None,
        _ => Some(Engine::load(cfg.dll.as_deref())?),
    };
    if let Some(e) = &engine {
        println!(
            "engine      : {}  ({} B state/stream)",
            e.path.display(),
            e.state_bytes
        );
    }
    let frame_size = engine.as_ref().map(|e| e.frame_size).unwrap_or(FRAME);
    let frame_ms = frame_size as f64 / 48.0;

    // ---- devices ----------------------------------------------------------
    // The macOS default hint (`vdev-audio-A-device`) names the HAL plugin's
    // device A and matches nothing on Windows, where the driver names its
    // endpoints with a bare `vdev` prefix ("Speakers (vdev 扬声器)"). Only the
    // default is re-mapped; an explicit `--vdev` value is used verbatim.
    let vdev_hint = if cfg.vdev == "vdev-audio-A-device" {
        "vdev".to_string()
    } else {
        cfg.vdev.clone()
    };
    let vdev = select_vdev_render(&vdev_hint).with_context(|| {
        format!(
            "virtual render endpoint matching {vdev_hint:?} not found.\n\
             Install the vdev-audio-win driver (crates/vdev-audio-win), or point\n\
             --vdev at a name substring of its render endpoint (the installed\n\
             render endpoints are listed in the error above)."
        )
    })?;

    let mic = if digital {
        None
    } else {
        Some(select_capture(cfg.input.as_deref()).with_context(|| {
            "no usable capture endpoint. Pass --input <name-substring> to pick the \
                 physical microphone explicitly (the installed capture endpoints are \
                 listed in the error above)."
        })?)
    };
    let speaker = if acoustic {
        Some(default_endpoint(eRender).context(
            "the acoustic probe needs the default render endpoint (physical speakers) \
             to play the marker, but none is available",
        )?)
    } else {
        None
    };

    // ---- stream clients ---------------------------------------------------
    // Injection target: shared-mode render on the virtual device.
    let render =
        AudioClient::open_shared_flags(vdev.device(), StreamKind::Render, 0, STREAM_BUFFER_HNS)
            .with_context(|| {
                format!("opening shared-mode render on {:?} failed", vdev.info.name)
            })?;
    check_mix_format(render.format(), &vdev.info, "render")?;
    let vdev_ch = render.format().channels().max(1) as usize;
    let vdev_rc = render.render_client()?;
    println!(
        "vdev render : \"{}\"  {} Hz, {} ch, buffer {} frames",
        vdev.info.name,
        render.format().sample_rate(),
        vdev_ch,
        render.buffer_frames()
    );
    println!("              id={}", vdev.info.id);

    // Digital probe / acoustic probe: loopback capture pinned to the same
    // render endpoint (the driver loops its render pin back to its capture
    // pin; this client is how we observe it). Per Microsoft's loopback
    // contract the client must use the render endpoint's mix format and gets
    // no reliable event, hence polling.
    let lb_client = if is_probe {
        Some(
            AudioClient::open_shared_flags(
                vdev.device(),
                StreamKind::Capture,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                STREAM_BUFFER_HNS,
            )
            .with_context(|| {
                format!(
                    "opening WASAPI loopback capture on {:?} failed -- the endpoint must \
                     be a render endpoint of a running audio device",
                    vdev.info.name
                )
            })?,
        )
    } else {
        None
    };
    let lb_rx = match &lb_client {
        Some(c) => Some((c.capture_client()?, c.format().channels().max(1) as usize)),
        None => None,
    };

    // Physical microphone (live denoise + acoustic probe).
    let mic_client = match &mic {
        Some(ep) => Some(
            AudioClient::open_shared_flags(ep.device(), StreamKind::Capture, 0, STREAM_BUFFER_HNS)
                .with_context(|| {
                    format!("opening shared-mode capture on {:?} failed", ep.info.name)
                })?,
        ),
        None => None,
    };
    if let (Some(ep), Some(c)) = (&mic, &mic_client) {
        check_mix_format(c.format(), &ep.info, "capture")?;
        println!(
            "capture     : \"{}\"  {} Hz, {} ch",
            ep.info.name,
            c.format().sample_rate(),
            c.format().channels()
        );
    }
    if mic.is_none() {
        println!("capture     : (none -- digital probe only)");
    }

    let mic_rx = match &mic_client {
        Some(c) => Some((c.capture_client()?, c.format().channels().max(1) as usize)),
        None => None,
    };

    // Physical speaker (acoustic probe only).
    let spk_client =
        match &speaker {
            Some(ep) => Some(
                AudioClient::open_shared_flags(
                    ep.device(),
                    StreamKind::Render,
                    0,
                    STREAM_BUFFER_HNS,
                )
                .with_context(|| {
                    format!("opening shared-mode render on {:?} failed", ep.info.name)
                })?,
            ),
            None => None,
        };
    if let (Some(ep), Some(c)) = (&speaker, &spk_client) {
        check_mix_format(c.format(), &ep.info, "speaker")?;
        println!(
            "speaker     : \"{}\"  {} Hz, {} ch",
            ep.info.name,
            c.format().sample_rate(),
            c.format().channels()
        );
    }

    let spk_rx = match &spk_client {
        Some(c) => Some((c.render_client()?, c.format().channels().max(1) as usize)),
        None => None,
    };

    // ---- probe state ------------------------------------------------------
    let probe_period_frames = (cfg.probe_interval_ms.max(50) as f64 * 48.0) as u64;
    // Single thread: the pending queue needs no mutex (macOS shares one
    // between two audio callbacks; here the fill and the scan are sequential
    // statements of the same loop).
    let mut pending: VecDeque<Instant> = VecDeque::with_capacity(64);
    let mut search = is_probe.then(|| ProbeSearch::new(acoustic));
    let mut emitter = digital.then(|| MarkerEmitter::new(probe_period_frames));
    let mut speaker_src = acoustic.then(|| SpeakerSource::new(probe_period_frames));

    // ---- the pipeline (macOS-identical path) ------------------------------
    let out_ring = Arc::new(SpscRing::new(48_000)); // ~1.4 s of slack
    let mut asm = FrameAssembler::new();
    let mut core = match engine.as_ref() {
        // Digital probe: no mic, no model -- the emitter is the whole source.
        None => None,
        Some(e) => Some(Core::new(e, &cfg, Arc::clone(&out_ring))?),
    };
    let mut staging: Vec<f32> = Vec::new(); // interleaved packet scratch
    let mut mono_scratch = vec![0.0f32; render.buffer_frames() as usize];

    // ---- start everything -------------------------------------------------
    if let Some(c) = &mic_client {
        c.start().context("start(capture) failed")?;
    }
    if let Some(c) = &spk_client {
        c.start().context("start(speaker) failed")?;
    }
    // The render side goes live before the loopback client so the driver has
    // an active render stream to loop back from.
    render.start().context("start(vdev render) failed")?;
    if let Some(c) = &lb_client {
        c.start().context("start(vdev loopback) failed")?;
    }

    println!("\nrunning for {:.0} s ...", cfg.seconds);
    let wall0 = Instant::now();
    let cpu0 = cpu_seconds();
    let target = Duration::from_secs_f64(cfg.seconds.max(1.0));
    let mut last_mark = 0u64;
    let mut markers_started: u64 = 0;

    while wall0.elapsed() < target {
        std::thread::sleep(Duration::from_millis(POLL_INTERVAL_MS));

        // 1) physical microphone -> model -> ring.
        if let Some((cc, ch)) = &mic_rx {
            let core_ref = core
                .as_mut()
                .context("live pipeline missing while capture is open")?;
            drain_capture(cc, &mut staging, *ch, &mut |mono| {
                asm.push(mono, |frame| core_ref.on_frame(frame));
            })?;
        }

        // 2) speaker fill (acoustic probe): the marker leaves here.
        if let (Some(cl), Some((rc, ch)), Some(src)) =
            (spk_client.as_ref(), spk_rx.as_ref(), speaker_src.as_mut())
        {
            let padding = cl.current_padding()?;
            let avail = cl.buffer_frames().saturating_sub(padding);
            if avail > 0 {
                // Timestamp the marker at its estimated *playback* instant:
                // what we write now sits `padding` frames deep, so the write
                // time plus that queue (plus the marker's offset inside this
                // block, applied by fill_render_events) is the honest
                // round-trip zero (macOS gets this for free because its
                // callback runs on the device clock).
                markers_started += u64::from(fill_render_events(
                    rc,
                    avail,
                    *ch,
                    src,
                    &mut pending,
                    padding,
                )?);
            }
        }

        // 3) vdev render top-up: ring-fed (live / acoustic) or marker-fed
        //    (digital probe). `GetCurrentPadding` decides how much fits; the
        //    queue depth stays constant, so nothing drifts.
        let padding = render.current_padding()?;
        let avail = render.buffer_frames().saturating_sub(padding);
        if avail > 0 {
            if let Some(em) = emitter.as_mut() {
                markers_started += u64::from(fill_render_events(
                    &vdev_rc,
                    avail,
                    vdev_ch,
                    em,
                    &mut pending,
                    padding,
                )?);
            } else {
                fill_render_ring(&vdev_rc, avail, vdev_ch, &out_ring, &mut mono_scratch)?;
            }
        }

        // 4) probe scan: drain the loopback capture and look for the marker.
        if let (Some((cc, ch)), Some(srch)) = (&lb_rx, search.as_mut()) {
            drain_capture(cc, &mut staging, *ch, &mut |mono| {
                srch.feed(mono, &mut pending)
            })?;
        }

        // Progress, every ~500 ms like the macOS loop.
        let el_ms = wall0.elapsed().as_millis() as u64;
        if el_ms / 500 > last_mark {
            last_mark = el_ms / 500;
            if is_probe {
                let n = search.as_ref().map(|s| s.stats().count).unwrap_or(0);
                println!(
                    "  {:.0}s  markers detected: {}",
                    wall0.elapsed().as_secs_f64(),
                    n
                );
            } else if let Some(c) = &core {
                println!(
                    "  {:.0}s  frames: {}",
                    wall0.elapsed().as_secs_f64(),
                    c.frames
                );
            }
        }
    }
    let wall = wall0.elapsed().as_secs_f64();
    let cpu = (cpu_seconds() - cpu0).max(0.0);

    // ---- stop (reverse start order; teardown errors must not mask the run)
    if let Some(c) = &lb_client {
        quiet_stop("vdev loopback", c);
    }
    quiet_stop("vdev render", &render);
    if let Some(c) = &spk_client {
        quiet_stop("speaker", c);
    }
    if let Some(c) = &mic_client {
        quiet_stop("capture", c);
    }

    // ---- report -----------------------------------------------------------
    let mut timing = core.as_ref().map(|c| c.timing.clone()).unwrap_or_default();
    timing.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let audio_seconds = core
        .as_ref()
        .map(|c| c.frames as f64 * frame_ms / 1000.0)
        .unwrap_or(0.0);
    let mix_used = core.as_ref().map(|c| c.mix).unwrap_or(cfg.mix);
    let report = WinReport {
        mode: match cfg.probe {
            None => "live denoise".into(),
            Some(ProbeMode::Digital) => "probe: digital".into(),
            Some(ProbeMode::Acoustic) => "probe: acoustic".into(),
        },
        seconds_requested: cfg.seconds,
        sample_rate: PIPELINE_HZ,
        frame_samples: frame_size,
        frame_ms,
        engine: engine.as_ref().map(|e| e.path.display().to_string()),
        engine_state_bytes: engine.as_ref().map(|e| e.state_bytes),
        adaptive: core.as_ref().map(|c| c.adaptive).unwrap_or(false),
        mix: mix_used,
        dry_delay_samples: core.as_ref().map(|c| c.dry.delay()).unwrap_or(0),
        vdev: EndpointOut {
            id: vdev.info.id.clone(),
            name: vdev.info.name.clone(),
        },
        vdev_mix: mix_out(render.format()),
        capture: mic.as_ref().map(|ep| EndpointOut {
            id: ep.info.id.clone(),
            name: ep.info.name.clone(),
        }),
        capture_mix: mic_client.as_ref().map(|c| mix_out(c.format())),
        speaker: speaker.as_ref().map(|ep| EndpointOut {
            id: ep.info.id.clone(),
            name: ep.info.name.clone(),
        }),
        frames: core.as_ref().map(|c| c.frames).unwrap_or(0),
        audio_seconds,
        wall_seconds: wall,
        vad_mean: core
            .as_ref()
            .map(|c| c.vad_sum / c.frames.max(1) as f64)
            .unwrap_or(0.0),
        wet_ratio_mean: core
            .as_ref()
            .map(|c| c.wet_sum / c.frames.max(1) as f64)
            .unwrap_or(0.0),
        wet_ratio_min: core
            .as_ref()
            .and_then(|c| (c.wet_min != f64::MAX).then_some(c.wet_min))
            .unwrap_or(0.0),
        wet_ratio_max: core
            .as_ref()
            .and_then(|c| (c.wet_max != f64::MIN).then_some(c.wet_max))
            .unwrap_or(0.0),
        frame_ms_p50: percentile(&timing, 0.50),
        frame_ms_p95: percentile(&timing, 0.95),
        frame_ms_p99: percentile(&timing, 0.99),
        frame_ms_max: timing.last().copied().unwrap_or(0.0),
        ring_dropped_samples: out_ring.dropped_samples(),
        ring_starved_samples: out_ring.starved_samples(),
        cpu_seconds: cpu,
        cpu_percent_of_one_core: if audio_seconds > 0.0 {
            cpu / audio_seconds * 100.0
        } else {
            0.0
        },
        latency: if is_probe {
            let s = search
                .as_ref()
                .map(|srch| srch.stats())
                .unwrap_or_else(|| LatencyProbe::new().stats());
            Some(LatencyStatsOut {
                count: s.count,
                injected: markers_started,
                detected: s.detected,
                rejected: s.rejected,
                min_ms: s.min_ms,
                mean_ms: s.mean_ms,
                p50_ms: s.p50_ms,
                p95_ms: s.p95_ms,
                max_ms: s.max_ms,
                stdev_ms: s.stdev_ms,
                interpretation: match cfg.probe {
                    Some(ProbeMode::Digital) => format!(
                        "inject -> driver loopback -> virtual-mic capture (includes the \
                         ~{:.0} ms render queue). {}",
                        STREAM_BUFFER_HNS as f64 / 10_000.0,
                        super::digital_model_cost_note(frame_ms)
                    ),
                    _ => "physical speaker -> room -> physical mic -> denoise -> virtual \
                          mic. End to end; already includes the model's delay line."
                        .into(),
                },
            })
        } else {
            None
        },
    };

    print_report(&report);

    // ---- A/B material from the live run -----------------------------------
    if let Some(c) = core.as_ref() {
        if let Some(p) = &cfg.record_in {
            crate::wavio::write(p, &c.rec_in)
                .with_context(|| format!("writing {}", p.display()))?;
            println!(
                "captured    : {} ({:.1} s)",
                p.display(),
                c.rec_in.len() as f64 / 48_000.0
            );
        }
        if let Some(p) = &cfg.record_out {
            crate::wavio::write(p, &c.rec_out)
                .with_context(|| format!("writing {}", p.display()))?;
            println!(
                "injected    : {} ({:.1} s)",
                p.display(),
                c.rec_out.len() as f64 / 48_000.0
            );
        }
    }
    if let Some(p) = &cfg.report {
        std::fs::write(p, serde_json::to_string_pretty(&report)?)?;
        println!("report      : {}", p.display());
    }
    Ok(())
}

// ===========================================================================
// COM lifetime
// ===========================================================================

/// COM apartment guard for one thread.
///
/// Create it *before* any COM pointer exists and drop it *after* all of them
/// are released; `run` declares it first for exactly that reason.
#[derive(Debug)]
pub struct ComGuard {
    /// Whether *this* guard initialized COM (`S_OK`) and therefore must
    /// balance it with `CoUninitialize`. `S_FALSE` and `RPC_E_CHANGED_MODE`
    /// mean the apartment is already owned elsewhere; uninitializing their
    /// COM under them would break the owner.
    owns_init: bool,
}

impl ComGuard {
    /// Initialize COM on the calling thread for WASAPI use (STA, with the
    /// OLE1/DDE layer disabled -- the standard audio-client recipe).
    pub fn new() -> Result<Self> {
        // SAFETY: CoInitializeEx takes no pointers and only touches per-thread
        // COM state. The flags request the apartment; the result is checked
        // against every documented outcome below.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr == S_OK {
            Ok(Self { owns_init: true })
        } else if hr == S_FALSE || hr == RPC_E_CHANGED_MODE {
            // Already initialized (same apartment / a different one). WASAPI
            // runs on either apartment, so adopt it without owning it.
            Ok(Self { owns_init: false })
        } else {
            Err(anyhow!("CoInitializeEx failed: {hr}"))
        }
    }
}

impl Default for ComGuard {
    fn default() -> Self {
        // Default cannot fail; a failed init simply yields a non-owning
        // guard, and the first real COM call will surface the reason.
        Self::new().unwrap_or(Self { owns_init: false })
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.owns_init {
            // SAFETY: balances the CoInitializeEx that returned S_OK. Runs
            // after every COM pointer of the session is released, because the
            // guard is declared before (and therefore dropped after) all of
            // them in `run`.
            unsafe { CoUninitialize() };
        }
    }
}

/// Raises the system timer resolution to 1 ms for the lifetime of the guard,
/// so the 2 ms poll sleep is actually ~2 ms (the default ~15.6 ms scheduling
/// granularity would drain a shared-mode render buffer between two passes).
///
/// A failure is degraded to a warning, not an error: the loop still works,
/// just with coarser pacing, and the ring's starve counters will say so.
struct TimePeriod {
    armed: bool,
}

impl TimePeriod {
    fn new() -> Self {
        // SAFETY: timeBeginPeriod only adjusts the global multimedia timer
        // resolution; 0 (TIMERR_NOERROR) means it took effect.
        let rc = unsafe { timeBeginPeriod(1) };
        let armed = rc == 0;
        if !armed {
            eprintln!(
                "vdev-mic-agent: timeBeginPeriod(1) failed ({rc}); poll pacing may be \
                 coarse and the render side may starve under load"
            );
        }
        Self { armed }
    }
}

impl Drop for TimePeriod {
    fn drop(&mut self) {
        if self.armed {
            // SAFETY: balances the timeBeginPeriod(1) that returned 0.
            unsafe { timeEndPeriod(1) };
        }
    }
}

// ===========================================================================
// Endpoint enumeration / selection
// ===========================================================================

/// The strings an endpoint is chosen by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointInfo {
    /// WASAPI endpoint id (`IMMDevice::GetId`), stable across sessions.
    pub id: String,
    /// `PKEY_Device_FriendlyName`, e.g. `Microphone (Realtek(R) Audio)` or
    /// `Speakers (vdev 扬声器)`.
    pub name: String,
}

/// One active endpoint plus its raw `IMMDevice`.
pub struct Endpoint {
    device: IMMDevice,
    pub info: EndpointInfo,
    pub flow: EDataFlow,
}

impl Endpoint {
    fn from_device(device: IMMDevice, flow: EDataFlow) -> Result<Self> {
        let id = endpoint_id(&device)?;
        let name = friendly_name(&device)?;
        Ok(Self {
            device,
            info: EndpointInfo { id, name },
            flow,
        })
    }

    /// The raw WASAPI device handle, for [`AudioClient::open_shared_flags`].
    #[must_use]
    pub fn device(&self) -> &IMMDevice {
        &self.device
    }
}

impl std::fmt::Debug for Endpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Endpoint")
            .field("info", &self.info)
            .field("flow", &self.flow)
            .finish()
    }
}

/// `IMMDevice::GetId` copied out, with the returned allocation freed.
fn endpoint_id(device: &IMMDevice) -> Result<String> {
    // SAFETY: plain COM call; on success MMDeviceAPI hands us a
    // CoTaskMemAlloc'ed UTF-16 string we own and must free (below, on both
    // the Ok and the Err exit).
    let pw = unsafe { device.GetId() }.context("IMMDevice::GetId failed")?;
    // SAFETY: the string is a valid NUL-terminated UTF-16 allocation until
    // the CoTaskMemFree right after the copy-out.
    let copied = unsafe { pw.to_string() };
    // SAFETY: release the GetId allocation exactly once, after copying out
    // (the copy result is mapped to an error only after the free, so the
    // invalid-UTF-16 path cannot leak the allocation).
    unsafe { CoTaskMemFree(Some(pw.as_ptr().cast_const().cast())) };
    copied.map_err(|e| anyhow!("endpoint id is not valid UTF-16: {e}"))
}

/// `PKEY_Device_FriendlyName` as a `String`.
fn friendly_name(device: &IMMDevice) -> Result<String> {
    // SAFETY: store/pv are owned COM results; the PROPVARIANT frees itself on
    // drop (windows-core implements Drop for PROPVARIANT via PropVariantClear).
    unsafe {
        let store: IPropertyStore = device
            .OpenPropertyStore(STGM_READ)
            .context("IMMDevice::OpenPropertyStore(STGM_READ) failed")?;
        let pv = store
            .GetValue(&PKEY_Device_FriendlyName)
            .context("IPropertyStore::GetValue(PKEY_Device_FriendlyName) failed")?;
        BSTR::try_from(&pv)
            .map(|b| b.to_string())
            .map_err(|e| anyhow!("PKEY_Device_FriendlyName is not a string value: {e}"))
    }
}

/// The active-endpoint enumerator.
fn enumerator() -> Result<IMMDeviceEnumerator> {
    // SAFETY: CLSID is the well-known MMDeviceEnumerator class id; a ComGuard
    // must be alive on this thread before this call (declaration order in
    // `run`).
    unsafe {
        CoCreateInstance::<_, IMMDeviceEnumerator>(&CLSID_MMDEVICE_ENUMERATOR, None, CLSCTX_ALL)
    }
    .context(
        "CoCreateInstance(IMMDeviceEnumerator) failed -- is the Windows audio service running?",
    )
}

/// Every active endpoint of one direction (`eCapture` / `eRender`).
pub fn list_endpoints(flow: EDataFlow) -> Result<Vec<Endpoint>> {
    let en = enumerator()?;
    // SAFETY: plain COM enumeration; every item is an owned interface pointer
    // wrapped in a Result.
    unsafe {
        let col: IMMDeviceCollection = en
            .EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE)
            .with_context(|| format!("EnumAudioEndpoints({flow:?}) failed"))?;
        let count = col
            .GetCount()
            .context("IMMDeviceCollection::GetCount failed")?;
        let mut out = Vec::with_capacity(usize::try_from(count).unwrap_or(0));
        for i in 0..count {
            let dev = col
                .Item(i)
                .with_context(|| format!("IMMDeviceCollection::Item({i}) failed"))?;
            out.push(
                Endpoint::from_device(dev, flow)
                    .with_context(|| format!("reading endpoint metadata failed (index {i})"))?,
            );
        }
        Ok(out)
    }
}

/// The default endpoint of `flow`: the communications role first (that is
/// what meeting apps actually use), falling back to the console role.
pub fn default_endpoint(flow: EDataFlow) -> Result<Endpoint> {
    let en = enumerator()?;
    let mut comms_err = None;
    let mut console_err = None;
    for (role, slot) in [(eCommunications, 0), (eConsole, 1)] {
        // SAFETY: plain COM query; the device pointer is owned via Result.
        match unsafe { en.GetDefaultAudioEndpoint(flow, role) } {
            Ok(dev) => return Endpoint::from_device(dev, flow),
            Err(e) => {
                if slot == 0 {
                    comms_err = Some(e);
                } else {
                    console_err = Some(e);
                }
            }
        }
    }
    Err(anyhow!(
        "no default {flow:?} endpoint (eCommunications: {comms_err:?}; eConsole: {console_err:?})"
    ))
}

/// First active endpoint of `flow` whose friendly name contains `hint`
/// (case-insensitive). The error lists what *is* installed, so a typo in the
/// `--input` / `--vdev` value is a one-look fix.
pub fn find_by_name(flow: EDataFlow, hint: &str) -> Result<Endpoint> {
    let hint_lc = hint.to_lowercase();
    let mut installed = Vec::new();
    for ep in list_endpoints(flow)? {
        if ep.info.name.to_lowercase().contains(&hint_lc) {
            return Ok(ep);
        }
        installed.push(ep.info.name);
    }
    bail!("no {flow:?} endpoint name contains {hint:?}; installed: {installed:?}")
}

/// Pick the physical microphone: the default communications capture endpoint
/// when no hint is given, otherwise the capture endpoint matching the name.
pub fn select_capture(hint: Option<&str>) -> Result<Endpoint> {
    match hint {
        None => default_endpoint(eCapture),
        Some(hint) => find_by_name(eCapture, hint),
    }
}

/// Pick the injection target: the render endpoint of the vdev-audio-win
/// virtual device, matched by name substring (the driver names its endpoints
/// with a `vdev` prefix, so the default hint just works).
pub fn select_vdev_render(hint: &str) -> Result<Endpoint> {
    find_by_name(eRender, hint)
}

// ===========================================================================
// AudioClient
// ===========================================================================

/// Which side of the endpoint this client streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamKind {
    /// We read from this endpoint (the physical microphone; also the vdev
    /// loopback capture, which is a capture client pinned to a render
    /// endpoint via `AUDCLNT_STREAMFLAGS_LOOPBACK`).
    Capture,
    /// We write to this endpoint (the vdev virtual device's render pin).
    Render,
}

/// Shared-mode `IAudioClient` on one endpoint, initialised with the engine's
/// mix format.
///
/// Drop order matters and is declaration order: `format` (a plain
/// allocation) and `buffer_frames` carry no COM references, and `client`
/// releases its last reference when the struct is dropped; the `ComGuard`
/// lives in `run`, declared *before* this client, so the apartment outlives
/// every client it created.
#[derive(Debug)]
pub struct AudioClient {
    client: IAudioClient,
    format: MixFormat,
    buffer_frames: u32,
    kind: StreamKind,
}

impl AudioClient {
    /// Open `device` in shared mode with the device mix format and the
    /// engine-default buffer. `kind` must match the endpoint direction.
    pub fn open_shared(device: &IMMDevice, kind: StreamKind) -> Result<Self> {
        Self::open_shared_flags(device, kind, 0, 0)
    }

    /// [`AudioClient::open_shared`] with explicit stream flags and buffer
    /// duration (100 ns units; 0 = engine default). The loopback capture
    /// client passes `AUDCLNT_STREAMFLAGS_LOOPBACK`; periodicity stays 0,
    /// which is required in shared mode.
    pub fn open_shared_flags(
        device: &IMMDevice,
        kind: StreamKind,
        stream_flags: u32,
        buffer_hns: i64,
    ) -> Result<Self> {
        // SAFETY: live device pointer; T's IID is attached by the generic
        // Activate plumbing. Requires a live ComGuard on this thread.
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }
            .context("IMMDevice::Activate(IAudioClient) failed")?;
        // SAFETY: live client; on success GetMixFormat hands us a
        // CoTaskMemAlloc'ed format (possibly WAVEFORMATEXTENSIBLE) that
        // MixFormat owns and frees on drop.
        let fmt_ptr =
            unsafe { client.GetMixFormat() }.context("IAudioClient::GetMixFormat failed")?;
        if fmt_ptr.is_null() {
            return Err(anyhow!("IAudioClient::GetMixFormat returned a null format"));
        }
        let format = MixFormat { ptr: fmt_ptr };
        // SAFETY: `format` is valid for the call (owned above, freed in Drop).
        // Shared mode with a zero periodicity asks WASAPI for its defaults;
        // `None` selects the process's default audio session.
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                stream_flags,
                buffer_hns,
                0,
                format.as_ptr(),
                None,
            )
        }
        .with_context(|| {
            format!(
                "IAudioClient::Initialize(shared, {kind:?}, flags={stream_flags:#x}) failed -- \
                 is the endpoint already held in exclusive mode?"
            )
        })?;
        // SAFETY: initialised client; the call only writes an integer.
        let buffer_frames =
            unsafe { client.GetBufferSize() }.context("IAudioClient::GetBufferSize failed")?;
        Ok(Self {
            client,
            format,
            buffer_frames,
            kind,
        })
    }

    /// The mix format the engine chose (`channels` / `sample_rate` /
    /// `bits_per_sample` on it describe the shared-mode engine format).
    #[must_use]
    pub fn format(&self) -> &MixFormat {
        &self.format
    }

    /// Total buffer size in frames (`IAudioClient::GetBufferSize`).
    #[must_use]
    pub fn buffer_frames(&self) -> u32 {
        self.buffer_frames
    }

    #[must_use]
    pub fn kind(&self) -> StreamKind {
        self.kind
    }

    /// Frames currently queued in the endpoint's buffer (render: how much is
    /// still to play; capture: usually 0 -- reads drain immediately).
    pub fn current_padding(&self) -> Result<u32> {
        // SAFETY: live client; the call only writes an integer.
        unsafe { self.client.GetCurrentPadding() }.context("IAudioClient::GetCurrentPadding failed")
    }

    /// Start the stream.
    pub fn start(&self) -> Result<()> {
        // SAFETY: live initialised client.
        unsafe { self.client.Start() }.context("IAudioClient::Start failed")
    }

    /// Stop the stream (queued capture data stays readable).
    pub fn stop(&self) -> Result<()> {
        // SAFETY: live initialised client.
        unsafe { self.client.Stop() }.context("IAudioClient::Stop failed")
    }

    /// The capture-side buffer reader.
    pub fn capture_client(&self) -> Result<IAudioCaptureClient> {
        if self.kind != StreamKind::Capture {
            bail!("capture_client() called on a {:?} client", self.kind);
        }
        // SAFETY: live initialised client; T's IID is attached by GetService.
        unsafe { self.client.GetService::<IAudioCaptureClient>() }
            .context("IAudioClient::GetService(IAudioCaptureClient) failed")
    }

    /// The render-side buffer writer.
    pub fn render_client(&self) -> Result<IAudioRenderClient> {
        if self.kind != StreamKind::Render {
            bail!("render_client() called on a {:?} client", self.kind);
        }
        // SAFETY: live initialised client; T's IID is attached by GetService.
        unsafe { self.client.GetService::<IAudioRenderClient>() }
            .context("IAudioClient::GetService(IAudioRenderClient) failed")
    }
}

/// Owned `GetMixFormat` allocation.
///
/// Kept as a raw pointer (not a boxed `WAVEFORMATEX`) because the shared-mode
/// engine format is frequently a `WAVEFORMATEXTENSIBLE`, which is larger than
/// `WAVEFORMATEX`; the allocation must go back to `CoTaskMemFree`.
#[derive(Debug)]
pub struct MixFormat {
    /// `GetMixFormat` allocation; non-null (checked at construction), freed
    /// exactly once in `Drop`.
    ptr: *mut WAVEFORMATEX,
}

impl MixFormat {
    /// Interleaved channel count of the engine format.
    #[must_use]
    pub fn channels(&self) -> u16 {
        // SAFETY: ptr came from GetMixFormat, is non-null, and outlives this
        // read (Drop runs after all accessors).
        unsafe { (*self.ptr).nChannels }
    }

    /// Engine sample rate in Hz.
    #[must_use]
    pub fn sample_rate(&self) -> u32 {
        // SAFETY: see `channels`.
        unsafe { (*self.ptr).nSamplesPerSec }
    }

    /// Bits per sample of the container (`wBitsPerSample`).
    #[must_use]
    pub fn bits_per_sample(&self) -> u16 {
        // SAFETY: see `channels`.
        unsafe { (*self.ptr).wBitsPerSample }
    }

    /// Sample encoding tag (`wFormatTag`): 1 = PCM 整型，3 = IEEE float，
    /// 0xFFFE = extensible（真实编码在 SubFormat）。
    #[must_use]
    pub fn format_tag(&self) -> u16 {
        // SAFETY: see `channels`.
        unsafe { (*self.ptr).wFormatTag }
    }

    /// Extensible 格式的 SubFormat.Data1（KSDATAFORMAT_SUBTYPE_* 的首 u32，
    /// IEEE float 为 3）。仅 `format_tag() == 0xFFFE` 时有意义，其余返回 None。
    #[must_use]
    pub fn extensible_subtype_data1(&self) -> Option<u32> {
        if self.format_tag() != 0xFFFE {
            return None;
        }
        // WAVEFORMATEXTENSIBLE 布局：WAVEFORMATEX（18 字节）+ wValidBitsPerSample
        // （2 字节）+ dwChannelMask（4 字节）+ SubFormat GUID（16 字节，偏移 24，
        // Data1 为其首 u32）。按字节指针 read_unaligned 读取，不依赖绑定侧
        // 结构体对齐假设。GetMixFormat 在 extensible 时返回完整的
        // WAVEFORMATEXTENSIBLE 缓冲（cbSize >= 22），偏移 24..28 在界内。
        // SAFETY: 见上；ptr 在 self 生命周期内有效。
        Some(unsafe { self.ptr.cast::<u8>().add(24).cast::<u32>().read_unaligned() })
    }

    /// The raw format pointer, for `Initialize` / format matches.
    #[must_use]
    pub fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.ptr
    }
}

impl Drop for MixFormat {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            // SAFETY: the pointer came from GetMixFormat (CoTaskMemAlloc) and
            // has not been freed anywhere else; this is the single release.
            unsafe { CoTaskMemFree(Some(self.ptr.cast_const().cast())) };
        }
    }
}

// Send-boundary note: everything above is deliberately `!Send` (raw pointers
// and COM apartment affinity). The live loop runs the whole session on one
// thread, which is also how the macOS backend treats its CoreAudio
// callbacks.

// ===========================================================================
// Live loop: the pipeline core (macOS-identical)
// ===========================================================================

/// int16 满幅值：[-1,1] float 与 int16 标度（RNNoise/wavio/metrics 口径）之间的换算系数。
const INT16_SCALE: f32 = 32768.0;

/// Per-frame pipeline state. A field-for-field mirror of the macOS `Core` so
/// both backends are one `on_frame` away from identical output.
struct Core {
    adaptive: bool,
    mix: f32,

    den: Denoiser,
    mixer: AdaptiveMixer,
    dry: DelayLine,
    out_ring: Arc<SpscRing>,

    /// One 480-sample frame, the buffer that goes to the virtual mic.
    /// 内容物为 **int16 标度**（±32768，RNNoise / wavio / metrics 的统一口径），
    /// 出口经 `out_float` 缩回 [-1,1] 再上环（审查 M-g）。
    frame: Vec<f32>,
    /// Dry frame, delayed in place before blending.
    dry_frame: Vec<f32>,
    /// WASAPI [-1,1] 输入放大到 int16 标度的每帧暂存。
    in_scaled: Vec<f32>,
    /// int16 标度的 `frame` 缩回 [-1,1] 的每帧暂存（上环前）。
    out_float: Vec<f32>,

    rec_in: Vec<f32>,
    rec_out: Vec<f32>,
    timing: Vec<f64>,

    frames: u64,
    vad_sum: f64,
    wet_sum: f64,
    wet_min: f64,
    wet_max: f64,
}

impl Core {
    fn new(engine: &Engine, cfg: &LiveConfig, out_ring: Arc<SpscRing>) -> Result<Self> {
        let den = engine.denoiser()?;
        let adaptive = cfg.adaptive && cfg.probe.is_none();
        // RNNoise's lookahead is 2 frames = 20 ms; `delay_line_len` decides
        // whether this run owes it (pure bypass does not, the acoustic probe does).
        let lag = super::delay_line_len(cfg, engine.frame_size);
        let mix = if cfg.probe == Some(ProbeMode::Acoustic) {
            // The acoustic probe puts a chirp through the whole chain; the model
            // would treat it as noise and gate it away. Bypassing the dry path
            // *with the lookahead delay* keeps the measured timing identical
            // (the delay line takes the place of the model's 20 ms) while the
            // marker survives intact.
            0.0
        } else {
            cfg.mix
        };
        let mut rec_in = Vec::with_capacity(RECORD_SAMPLES);
        rec_in.resize(RECORD_SAMPLES, 0.0);
        rec_in.clear();
        let mut rec_out = Vec::with_capacity(RECORD_SAMPLES);
        rec_out.resize(RECORD_SAMPLES, 0.0);
        rec_out.clear();
        Ok(Self {
            adaptive,
            mix,
            den,
            mixer: AdaptiveMixer::new(adaptive),
            dry: DelayLine::new(lag),
            out_ring,
            frame: vec![0.0; engine.frame_size],
            dry_frame: vec![0.0; engine.frame_size],
            in_scaled: vec![0.0; engine.frame_size],
            out_float: vec![0.0; engine.frame_size],
            rec_in,
            rec_out,
            timing: Vec::with_capacity(60 * 1000), // 60 s of 10 ms frames
            frames: 0,
            vad_sum: 0.0,
            wet_sum: 0.0,
            wet_min: f64::MAX,
            wet_max: f64::MIN,
        })
    }

    /// One 480-sample frame: model, blend, ship. Mirrors `agent::process`'s
    /// inner loop (and the macOS `Core::on_frame`) exactly, so the offline
    /// numbers stay valid.
    ///
    /// 标度约定（审查 M-g）：WASAPI 共享模式交付/消费 [-1,1] float，而
    /// RNNoise 与 crate 内全部离线口径（wavio 读写、metrics 的 dBFS、
    /// `AdaptiveMixer` 的 noise_floor）都是 **int16 标度 ±32768**。入口先把
    /// 输入放大到 int16 标度（模型、延迟线、混音、A/B 录音全在该标度），
    /// 出口再缩回 [-1,1] 上环。修复前直接把 [-1,1] 喂给模型——信号比
    /// 训练分布低约 90 dB，降噪/VAD 形同失效，混音噪声门限单位同样错乱。
    #[inline]
    fn on_frame(&mut self, input: &[f32]) {
        // 0) [-1,1] -> int16 标度（input 恒为 FRAME 长，见 den.process 的既有假设）
        for (d, &s) in self.in_scaled.iter_mut().zip(input) {
            *d = s * INT16_SCALE;
        }

        // 1) raw capture, for the A/B file（int16 标度，与 wavio::write 的约定一致）
        if self.rec_in.len() + FRAME <= RECORD_SAMPLES {
            self.rec_in.extend_from_slice(&self.in_scaled);
        }

        let t0 = Instant::now();
        let (vad, wet_frame) = self.den.process(&self.in_scaled);
        let dt_ms = t0.elapsed().as_secs_f64() * 1000.0;
        if self.timing.len() < self.timing.capacity() {
            self.timing.push(dt_ms);
        }

        // 2) dry path, delayed to the model's lookahead (see frames::DelayLine)
        self.dry_frame.copy_from_slice(&self.in_scaled);
        self.dry.process(&mut self.dry_frame);

        // 3) blend（mixer 拿到 int16 标度输入，noise_floor_dbfs 才是真实 dBFS）
        let w = if self.adaptive {
            self.mixer.update(&self.in_scaled, vad)
        } else {
            self.mix as f64
        };
        if w >= 1.0 {
            self.frame.copy_from_slice(wet_frame);
        } else if w <= 0.0 {
            self.frame.copy_from_slice(&self.dry_frame);
        } else {
            // frame/dry_frame are exactly `FRAME` long (`Engine::load` bails
            // unless frame_size == FRAME) and `wet_frame` likewise, so this
            // zips over the same samples the old `0..FRAME` loop indexed.
            for ((out, &wet), &dry) in self.frame.iter_mut().zip(wet_frame).zip(&self.dry_frame) {
                *out = (w * wet as f64 + (1.0 - w) * dry as f64) as f32;
            }
        }

        if self.rec_out.len() + FRAME <= RECORD_SAMPLES {
            self.rec_out.extend_from_slice(&self.frame);
        }

        // 4) ship it. Dropping on a full ring is the right failure mode: the
        // render side is starving, and stale audio would be worse than a gap.
        // 上环前缩回 [-1,1]——渲染侧（fill_render_ring）按 WASAPI float 写出。
        for (d, &s) in self.out_float.iter_mut().zip(&self.frame) {
            *d = s / INT16_SCALE;
        }
        self.out_ring.push_or_drop(&self.out_float);

        self.frames += 1;
        self.vad_sum += vad as f64;
        self.wet_sum += w;
        self.wet_min = self.wet_min.min(w);
        self.wet_max = self.wet_max.max(w);
    }
}

// ===========================================================================
// Live loop: marker sources (digital + acoustic probes)
// ===========================================================================

/// A sample source whose "a fresh marker starts here" transitions carry the
/// round trip's zero. Implemented by the digital probe's emitter (into the
/// vdev render) and the acoustic probe's speaker source.
trait EventSource {
    /// The next sample to hand the device, and whether a fresh marker starts
    /// here.
    fn next(&mut self) -> (f32, bool);
}

/// The digital probe's marker source: a sample-by-sample state machine the
/// poll loop drives through the render fill, so the marker leaves straight
/// from our write path with no extra ring or thread in between.
///
/// The vdev-audio-win driver loops this stream back into its own capture
/// side, which is what the loopback capture client observes.
struct MarkerEmitter {
    marker: Vec<f32>,
    /// Position inside the marker; `>= marker.len()` means "idle".
    pos: usize,
    /// Frames left before the next marker starts.
    until_next: u64,
    /// Marker-to-marker spacing; never shorter than the marker itself.
    period_frames: u64,
}

impl MarkerEmitter {
    fn new(period_frames: u64) -> Self {
        let m = marker();
        let n = m.len();
        Self {
            marker: m,
            pos: n,
            until_next: 0,
            period_frames: period_frames.max(MARKER_LEN as u64),
        }
    }
}

impl EventSource for MarkerEmitter {
    #[inline]
    fn next(&mut self) -> (f32, bool) {
        if self.pos >= self.marker.len() {
            if self.until_next == 0 {
                self.pos = 0;
            } else {
                self.until_next -= 1;
                return (0.0, false);
            }
        }
        let v = self.marker[self.pos];
        let started = self.pos == 0;
        self.pos += 1;
        if self.pos == self.marker.len() {
            self.until_next = self.period_frames;
        }
        (v, started)
    }
}

/// The acoustic probe's marker source for the physical speaker. Same machine
/// as [`MarkerEmitter`]: play the marker once, then keep `period_frames` of
/// silence before the next one.
struct SpeakerSource {
    marker: Vec<f32>,
    pos: usize,
    until_next: u64,
    period_frames: u64,
}

impl SpeakerSource {
    fn new(period_frames: u64) -> Self {
        Self {
            marker: marker(),
            pos: 0,
            until_next: 0,
            // 同款钳制：静音期不得短于标记本身
            period_frames: period_frames.max(MARKER_LEN as u64),
        }
    }
}

impl EventSource for SpeakerSource {
    #[inline]
    fn next(&mut self) -> (f32, bool) {
        if self.pos >= self.marker.len() {
            if self.until_next == 0 {
                self.pos = 0;
            } else {
                self.until_next -= 1;
                return (0.0, false);
            }
        }
        let started = self.pos == 0;
        let v = self.marker[self.pos];
        self.pos += 1;
        if self.pos == self.marker.len() {
            self.until_next = self.period_frames;
        }
        (v, started)
    }
}

// ===========================================================================
// Live loop: probe detection (WASAPI loopback capture)
// ===========================================================================

/// Rolling marker detector over the loopback capture stream. The buffer and
/// the correlation scratch are preallocated/capped so a stalled render side
/// cannot grow them without bound (`SEARCH_CAP` = 1 s).
struct ProbeSearch {
    buf: Vec<f32>,
    /// NCC prefix-sum scratch for `detect_with` (cleared and reused, so the
    /// hit path never has to grow a fresh allocation).
    prefix: Vec<f64>,
    needle: Vec<f32>,
    /// Correlation threshold: a digital loopback is bit-exact, an acoustic
    /// one is not (same numbers as the macOS backend).
    threshold: f32,
    latency: LatencyProbe,
}

impl ProbeSearch {
    fn new(acoustic: bool) -> Self {
        Self {
            buf: Vec::with_capacity(SEARCH_CAP + MARKER_LEN * 2),
            prefix: Vec::new(),
            needle: marker(),
            threshold: if acoustic { 0.35 } else { 0.60 },
            latency: LatencyProbe::new(),
        }
    }

    /// Consume one mono packet. On a hit, pairs the earliest unmatched
    /// `pending` timestamp with now; an unmatched hit counts as rejected.
    fn feed(&mut self, mono: &[f32], pending: &mut VecDeque<Instant>) {
        if self.buf.len() + mono.len() > SEARCH_CAP {
            let drop = self.buf.len() + mono.len() - SEARCH_CAP;
            self.buf.drain(..drop);
        }
        self.buf.extend_from_slice(mono);
        if self.buf.len() < MARKER_LEN {
            return;
        }
        self.prefix.clear();
        if let Some(d) = detect_with(&self.buf, &self.needle, self.threshold, &mut self.prefix) {
            match pending.pop_front() {
                Some(sent) => self.latency.record(sent, Instant::now()),
                None => self.latency.record_rejected(),
            }
            // do not re-report the same occurrence
            let consumed = (d.start + MARKER_LEN).min(self.buf.len());
            self.buf.drain(..consumed);
        }
    }

    fn stats(&self) -> LatencyStats {
        self.latency.stats()
    }
}

// ===========================================================================
// Live loop: WASAPI buffer plumbing
// ===========================================================================

/// Drain every pending packet of a capture client, hand each one to `emit`
/// as mono f32. Shared by the physical microphone and the loopback capture
/// (same `IAudioCaptureClient` contract). `staging` is reused across calls;
/// it may grow to the largest packet seen and stays there.
fn drain_capture(
    cc: &IAudioCaptureClient,
    staging: &mut Vec<f32>,
    channels: usize,
    emit: &mut dyn FnMut(&[f32]),
) -> Result<()> {
    loop {
        // SAFETY: live capture client; the call only writes an integer.
        let packet = unsafe { cc.GetNextPacketSize() }.with_context(|| {
            "IAudioCaptureClient::GetNextPacketSize failed -- was the endpoint \
             unplugged or its format changed?"
        })?;
        if packet == 0 {
            return Ok(());
        }
        let mut data: *mut u8 = std::ptr::null_mut();
        let mut frames: u32 = 0;
        let mut flags: u32 = 0;
        // SAFETY: all out-pointers are valid locals; the two optional device-
        // position outputs are deliberately None (the probe clock is the wall
        // clock, same as the macOS backend).
        unsafe { cc.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }
            .context("IAudioCaptureClient::GetBuffer failed")?;
        let n = frames as usize;
        staging.clear();
        staging.resize(n * channels, 0.0);
        // SILENT means the buffer contents are not valid: leave the zeros.
        if n > 0 && (flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) == 0 {
            // SAFETY: WASAPI guarantees `frames` frames of the mix format
            // behind `data`; the mix format was checked to be 32-bit float
            // at open (check_mix_format), interleaved over `channels`, so
            // reading n * channels f32 values is in bounds. `data` is only
            // valid until ReleaseBuffer below.
            let src = unsafe { std::slice::from_raw_parts(data as *const f32, n * channels) };
            staging.copy_from_slice(src);
        }
        // SAFETY: releases exactly the buffer/frames obtained above, once.
        unsafe { cc.ReleaseBuffer(frames) }.context("IAudioCaptureClient::ReleaseBuffer failed")?;
        if n > 0 {
            emit(&to_mono(staging, channels));
        }
    }
}

/// Top up the render buffer with `frames` frames from the ring (the live /
/// acoustic injection path). The mono signal is duplicated across all `nch`
/// channels so every consumer channel map hears it -- same choice as the
/// macOS inject IOProc.
fn fill_render_ring(
    rc: &IAudioRenderClient,
    frames: u32,
    nch: usize,
    ring: &SpscRing,
    mono: &mut [f32],
) -> Result<()> {
    let frames = usize::try_from(frames).map_err(|e| anyhow!("render fill size: {e}"))?;
    if frames == 0 || frames > mono.len() {
        bail!(
            "render fill: {frames} frames but scratch holds {}",
            mono.len()
        );
    }
    // Dropping on a short pop is the ring's own contract (pop_or_silence
    // pads with silence): a starving render side gets a gap, not stale audio.
    // Shortfalls are counted in the ring's starved_samples.
    ring.pop_or_silence(&mut mono[..frames]);
    // SAFETY: GetBuffer hands us a render buffer of `frames` frames of the
    // mix format (4-byte float samples, `nch` interleaved channels), valid
    // until ReleaseBuffer; we write exactly frames * nch f32 values.
    let ptr =
        unsafe { rc.GetBuffer(frames as u32) }.context("IAudioRenderClient::GetBuffer failed")?;
    // SAFETY: see above -- in bounds for frames * nch floats, unaliased,
    // WASAPI-owned for the duration of the fill. GetBuffer types the block
    // as bytes; the mix format (checked at open) is IEEE float32.
    let dst = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<f32>(), frames * nch) };
    for (f, &v) in mono[..frames].iter().enumerate() {
        let base = f * nch;
        for slot in &mut dst[base..base + nch] {
            *slot = v;
        }
    }
    // SAFETY: exactly the frames written into the GetBuffer block above.
    unsafe { rc.ReleaseBuffer(frames as u32, 0) }
        .context("IAudioRenderClient::ReleaseBuffer failed")?;
    Ok(())
}

/// Top up the render buffer from an [`EventSource`] (digital probe emitter /
/// acoustic speaker source). Every fresh marker start is timestamped at
/// `now + (queued + k) / 48 kHz` -- the write instant plus the queue depth
/// measured just before the fill plus the marker's own offset `k` inside
/// this block, i.e. the estimated instant this sample actually plays
/// (macOS timestamps inside the render callback, where the queue depth is
/// zero by construction). Returns how many markers started.
fn fill_render_events(
    rc: &IAudioRenderClient,
    frames: u32,
    nch: usize,
    src: &mut dyn EventSource,
    pending: &mut VecDeque<Instant>,
    queued_frames: u32,
) -> Result<u32> {
    let mut started_count = 0u32;
    // SAFETY: same GetBuffer contract as fill_render_ring.
    let ptr = unsafe { rc.GetBuffer(frames) }.context("IAudioRenderClient::GetBuffer failed")?;
    // SAFETY: in bounds for frames * nch floats, unaliased, WASAPI-owned.
    // GetBuffer types the block as bytes; the mix format is IEEE float32.
    let dst = unsafe { std::slice::from_raw_parts_mut(ptr.cast::<f32>(), frames as usize * nch) };
    for (k, slot) in dst.chunks_exact_mut(nch).enumerate() {
        let (v, started) = src.next();
        if started {
            let delay =
                Duration::from_secs_f64((queued_frames as f64 + k as f64) / PIPELINE_HZ as f64);
            pending.push_back(Instant::now() + delay);
            started_count += 1;
        }
        for s in slot.iter_mut() {
            *s = v;
        }
    }
    // SAFETY: exactly the frames written into the GetBuffer block above.
    unsafe { rc.ReleaseBuffer(frames, 0) }.context("IAudioRenderClient::ReleaseBuffer failed")?;
    Ok(started_count)
}

/// Shared-mode format gate. The WASAPI shared engine always mixes to 32-bit
/// IEEE float, but it does **not** resample: the mix format rate follows the
/// endpoint's default device format, and the pipeline is fixed at 48 kHz.
/// Rather than sneaking in an unvalidated resampler, refuse with the exact
/// fix (this is the "format not supported" path).
fn check_mix_format(fmt: &MixFormat, ep: &EndpointInfo, side: &str) -> Result<()> {
    if fmt.sample_rate() != PIPELINE_HZ {
        bail!(
            "{side} endpoint {:?} runs its shared mix format at {} Hz, but the denoise \
             pipeline is fixed at 48 kHz and WASAPI shared mode does not resample.\n\
             Fix: Control Panel > Sound > {side} device properties > Advanced > set the \
             default format to a 48000 Hz setting -- or pick another endpoint via \
             --input / --vdev.",
            ep.name,
            fmt.sample_rate()
        );
    }
    if fmt.bits_per_sample() != 32 {
        bail!(
            "{side} endpoint {:?} reports a {}-bit mix format; the WASAPI shared engine is \
             documented to mix to 32-bit float, so refusing to guess the sample layout.",
            ep.name,
            fmt.bits_per_sample()
        );
    }
    // 审查 L9：32 bit 容器不等于 float——还可能是 32 bit PCM 整型。整型样本
    // 被当 float 读/写会得到满幅噪声，必须按 wFormatTag 拒绝。
    // 接受：WAVE_FORMAT_IEEE_FLOAT(3)，或 extensible 且 SubFormat.Data1 == 3。
    let tag = fmt.format_tag();
    let is_float = tag == 3 || (tag == 0xFFFE && fmt.extensible_subtype_data1() == Some(3));
    if !is_float {
        bail!(
            "{side} endpoint {:?} reports mix format tag {tag:#06x} (subtype {:?}); the \
             pipeline reads/writes samples as IEEE float32, so refusing to guess the encoding.",
            ep.name,
            fmt.extensible_subtype_data1()
        );
    }
    Ok(())
}

/// Stop a client on the teardown path without letting a failure mask the
/// run's own result. Releasing the client would stop the stream anyway, but
/// an explicit Stop is the documented quiesce.
fn quiet_stop(label: &str, client: &AudioClient) {
    if let Err(e) = client.stop() {
        eprintln!("vdev-mic-agent: stop({label}) failed: {e:#}");
    }
}

// ===========================================================================
// Report
// ===========================================================================

#[derive(Debug, Serialize)]
struct EndpointOut {
    id: String,
    name: String,
}

#[derive(Debug, Serialize)]
struct MixOut {
    sample_rate: u32,
    channels: u16,
    bits_per_sample: u16,
}

fn mix_out(fmt: &MixFormat) -> MixOut {
    MixOut {
        sample_rate: fmt.sample_rate(),
        channels: fmt.channels(),
        bits_per_sample: fmt.bits_per_sample(),
    }
}

/// Top-level key names match the macOS `RunReport` wherever the concepts
/// exist, so the same summary tooling reads both backends; the nested device
/// objects differ in shape (macOS `DeviceInfo` vs `EndpointOut` here), so a
/// schema-typed parser needs a per-backend branch. Windows-specific
/// additions (`*_mix`, endpoint ids) describe the WASAPI mix formats.
#[derive(Debug, Serialize)]
struct WinReport {
    mode: String,
    /// Same meaning as the macOS report: 960 when the dry path is aligned to
    /// the model's lookahead, 0 for a pure bypass (`--mix 0`).
    dry_delay_samples: usize,
    seconds_requested: f64,
    sample_rate: u32,
    frame_samples: usize,
    frame_ms: f64,
    engine: Option<String>,
    engine_state_bytes: Option<usize>,
    adaptive: bool,
    mix: f32,
    vdev: EndpointOut,
    vdev_mix: MixOut,
    capture: Option<EndpointOut>,
    capture_mix: Option<MixOut>,
    speaker: Option<EndpointOut>,
    /// Frames the model actually processed.
    frames: u64,
    /// Audio seconds the model produced (frames * 10 ms).
    audio_seconds: f64,
    /// Wall seconds the graph was running (fills the gap between the two).
    wall_seconds: f64,
    vad_mean: f64,
    wet_ratio_mean: f64,
    wet_ratio_min: f64,
    wet_ratio_max: f64,
    /// Per-frame model time, p50/p95/p99/max, in ms.
    frame_ms_p50: f64,
    frame_ms_p95: f64,
    frame_ms_p99: f64,
    frame_ms_max: f64,
    /// Samples the ring dropped (model faster than the render side) ...
    ring_dropped_samples: usize,
    /// ... and samples the render side had to pad with silence (model late).
    ring_starved_samples: usize,
    cpu_seconds: f64,
    cpu_percent_of_one_core: f64,
    latency: Option<LatencyStatsOut>,
}

#[derive(Debug, Serialize)]
struct LatencyStatsOut {
    count: usize,
    injected: u64,
    detected: u64,
    rejected: u64,
    min_ms: f64,
    mean_ms: f64,
    p50_ms: f64,
    p95_ms: f64,
    max_ms: f64,
    stdev_ms: f64,
    /// `digital`: add the model's frame fill (0-10 ms) and its 20 ms lookahead
    /// to get the model total.
    /// `acoustic`: already end to end, including the model's delay line.
    interpretation: String,
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    sorted[((sorted.len() as f64 - 1.0) * q).round() as usize]
}

fn print_report(r: &WinReport) {
    println!("\nmode        : {}", r.mode);
    println!(
        "vdev        : \"{}\"  {} Hz, {} ch, {} bit",
        r.vdev.name, r.vdev_mix.sample_rate, r.vdev_mix.channels, r.vdev_mix.bits_per_sample
    );
    if let (Some(c), Some(m)) = (&r.capture, &r.capture_mix) {
        println!(
            "capture     : \"{}\"  {} Hz, {} ch",
            c.name, m.sample_rate, m.channels
        );
    }
    let dry_delay_note = if r.frames == 0 {
        // digital probe: no Core at all, so there is no dry path to talk about
        "  -- n/a (this probe runs no model and no dry path)".to_string()
    } else if r.dry_delay_samples == 0 {
        "  -- no dry path to align (pure bypass or pure wet)".to_string()
    } else {
        "  -- dry path aligned to the model's lookahead".to_string()
    };
    println!(
        "dry delay   : {} samples ({:.1} ms){}",
        r.dry_delay_samples,
        r.dry_delay_samples as f64 / 48.0,
        dry_delay_note
    );
    println!(
        "frames      : {}  ({:.3} s of audio)",
        r.frames, r.audio_seconds
    );
    println!(
        "wall/cpu    : {:.2} s / {:.2} s  ({:.1}% of one core)",
        r.wall_seconds, r.cpu_seconds, r.cpu_percent_of_one_core
    );
    println!(
        "frame time  : p50 {:.4}  p95 {:.4}  p99 {:.4}  max {:.4} ms",
        r.frame_ms_p50, r.frame_ms_p95, r.frame_ms_p99, r.frame_ms_max
    );
    println!(
        "mix         : adaptive={}  wet mean {:.3}  min {:.3}  max {:.3}",
        r.adaptive, r.wet_ratio_mean, r.wet_ratio_min, r.wet_ratio_max
    );
    println!(
        "ring        : dropped {}  starved {}",
        r.ring_dropped_samples, r.ring_starved_samples
    );
    if let Some(lat) = &r.latency {
        println!(
            "latency     : {} round trips (injected {}, rejected {})",
            lat.count, lat.injected, lat.rejected
        );
        println!(
            "  min {:.2}  mean {:.2}  p50 {:.2}  p95 {:.2}  max {:.2}  stdev {:.2} ms",
            lat.min_ms, lat.mean_ms, lat.p50_ms, lat.p95_ms, lat.max_ms, lat.stdev_ms
        );
        println!("  {}", lat.interpretation);
    }
}

// ===========================================================================
// Tests (run on a Windows host; compiled by the cross-checks on macOS)
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// M-g 回归：int16 标度换算系数必须与 crate 口径（wavio/metrics/mixer）
    /// 一致——1.0 ↔ 32768，且 on_frame 的入/出换算互为逆运算。
    #[test]
    fn int16_scale_roundtrips() {
        assert_eq!(INT16_SCALE, 32768.0);
        for &v in &[0.0f32, 0.5, -0.5, 1.0, -1.0] {
            let scaled = v * INT16_SCALE;
            assert!((scaled / INT16_SCALE - v).abs() < 1e-7);
        }
        assert_eq!(1.0f32 * INT16_SCALE, 32768.0);
    }

    #[test]
    fn marker_emitter_starts_immediately_then_every_period() {
        let mut em = MarkerEmitter::new(1_000);
        let mut starts = Vec::new();
        for i in 0..(MARKER_LEN + 1_500) {
            let (v, started) = em.next();
            assert!(v.abs() <= 0.5 + 1e-6);
            if started {
                starts.push(i);
            }
        }
        // First marker starts at once; the second after `period_frames` of
        // silence following the end of the first.
        assert_eq!(starts, vec![0, MARKER_LEN + 1_000]);
    }

    #[test]
    fn marker_emitter_period_never_shorter_than_marker() {
        let mut em = MarkerEmitter::new(1); // clamped up to MARKER_LEN
        let mut starts = 0;
        for _ in 0..(MARKER_LEN * 4) {
            if em.next().1 {
                starts += 1;
            }
        }
        // start, len silence (= marker len), start, len silence: 2 starts in
        // 4 * MARKER_LEN samples with the clamp in effect.
        assert_eq!(starts, 2);
    }

    #[test]
    fn probe_search_pairs_hits_with_pending() {
        let mut s = ProbeSearch::new(false);
        let needle = marker();
        let mut pending: VecDeque<Instant> = VecDeque::new();
        pending.push_back(Instant::now());

        // Silence first: the amplitude gate must not fire.
        s.feed(&vec![0.0; MARKER_LEN * 2], &mut pending);
        assert_eq!(s.stats().count, 0);

        // The exact marker: a bit-exact digital hit, pending consumed.
        s.feed(&needle, &mut pending);
        let st = s.stats();
        assert_eq!(st.count, 1);
        assert_eq!(st.detected, 1);
        assert_eq!(st.rejected, 0);
        assert!(pending.is_empty());

        // Another hit with nothing pending counts as rejected, not recorded.
        s.feed(&needle, &mut pending);
        let st = s.stats();
        assert_eq!(st.count, 1);
        assert_eq!(st.rejected, 1);
    }

    #[test]
    fn probe_search_window_is_capped() {
        let mut s = ProbeSearch::new(false);
        let mut pending: VecDeque<Instant> = VecDeque::new();
        for _ in 0..8 {
            s.feed(&vec![0.25; SEARCH_CAP / 4], &mut pending);
        }
        assert!(s.buf.len() <= SEARCH_CAP);
        // Never grew unbounded either.
        assert!(s.buf.capacity() <= SEARCH_CAP + MARKER_LEN * 2 + 64);
    }

    #[test]
    fn speaker_source_matches_emitter_semantics() {
        let mut spk = SpeakerSource::new(2_000);
        let first = spk.next();
        assert!(first.1, "the acoustic marker starts on the first sample");
        let mut starts = 0;
        for _ in 0..(MARKER_LEN + 2_000) {
            if spk.next().1 {
                starts += 1;
            }
        }
        assert!(starts >= 1);
    }
}
