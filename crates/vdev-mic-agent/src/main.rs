mod agent;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod frames;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod latency;
mod metrics;
mod mixer;
mod platform;
mod proctime;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod ring;
mod rnnoise;
mod stats;
mod wavio;

use agent::{RunConfig, RunOutcome};
use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use metrics::MetricsReport;
use rnnoise::Engine;
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "vdev-mic-agent",
    version,
    about = "End-side AI microphone front-end for vdev -- D1-D2 offline harness",
    long_about = "capture -> RNNoise denoise -> inject, with per-frame timing.\n\
                  D1-D2 wires capture and injection to WAV files. D3-D4 (`live`) drives\n\
                  the same core from real audio callbacks (CoreAudio on macOS, WASAPI\n\
                  polling on Windows), and measures the buffering a WAV harness\n\
                  cannot see (`live --probe`)."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Denoise one WAV file.
    Run(RunArgs),
    /// Run the whole D1-D2 matrix (four SNRs x {fixed, adaptive}) and emit JSON.
    Bench(BenchArgs),
    /// Compare two WAVs sample by sample (used to cross-check against the C and
    /// Python harnesses -- same DLL, same frames, so they should agree exactly).
    Diff(DiffArgs),
    /// Drive the same core from real audio devices: capture -> denoise ->
    /// inject, optionally in latency-probe mode. CoreAudio on macOS, WASAPI
    /// polling on Windows.
    Live(LiveArgs),
}

#[derive(Args)]
struct RunArgs {
    /// Input WAV (mono, 48 kHz, PCM16).
    input: PathBuf,
    /// Where to write the denoised WAV.
    #[arg(short, long)]
    out: Option<PathBuf>,
    /// Path to librnnoise-0.dll / librnnoise.dylib.
    #[arg(long)]
    dll: Option<PathBuf>,
    /// Fixed dry/wet ratio.
    #[arg(long, default_value_t = 1.0)]
    mix: f32,
    /// Derive the dry/wet ratio per frame from the local noise floor.
    #[arg(long)]
    adaptive: bool,
    /// Frames processed before timing starts (RNN state warm-up).
    #[arg(long, default_value_t = 100)]
    warmup: usize,
    /// Model lookahead in samples; the dry path is delayed by this much before
    /// blending, otherwise the blend comb-filters. RNNoise: 960 (2 frames).
    #[arg(long, default_value_t = 960)]
    lookahead_samples: usize,
    /// Clean reference WAV; enables SI-SDR / segSNR / noise-floor reporting.
    #[arg(long)]
    reference: Option<PathBuf>,
    /// Write the full report as JSON here.
    #[arg(long)]
    stats: Option<PathBuf>,
}

#[derive(Args)]
struct BenchArgs {
    /// Directory holding clean_ref.wav and noisy_snr{30,10,5,0}db.wav.
    #[arg(long, default_value = "audio")]
    dir: PathBuf,
    /// JSON report output path.
    #[arg(long, default_value = "results_rust.json")]
    out: PathBuf,
    #[arg(long)]
    dll: Option<PathBuf>,
    /// Max delay searched during alignment, in 10 ms frames.
    #[arg(long, default_value_t = 3)]
    max_lag_frames: usize,
}

#[derive(Args)]
struct DiffArgs {
    a: PathBuf,
    b: PathBuf,
}

/// Arguments for `live`. These mirror `platform::LiveConfig` field for field --
/// the CLI is the only place the D3-D4 knobs are exposed.
#[derive(Args)]
struct LiveArgs {
    /// Virtual device to inject into, by UID or name substring.
    #[arg(long, default_value = "vdev-audio-A-device")]
    vdev: String,
    /// Physical capture device, by UID or name substring. Default: system input.
    #[arg(long)]
    input: Option<String>,
    /// Path to librnnoise.dylib; defaults to the loader's own search path.
    #[arg(long)]
    dll: Option<PathBuf>,
    /// Fixed dry/wet ratio (ignored when --adaptive).
    #[arg(long, default_value_t = 1.0)]
    mix: f32,
    /// Derive the dry/wet ratio per frame from the local noise floor.
    #[arg(long)]
    adaptive: bool,
    /// Seconds to run before stopping cleanly.
    #[arg(long, default_value_t = 20.0)]
    seconds: f64,
    /// Measure latency instead of denoising live.
    #[arg(long, value_enum)]
    probe: Option<ProbeArg>,
    /// Target IO buffer size in frames, set on the vdev + capture device before
    /// starting IO (the HAL owns this property for plug-in devices; the
    /// plugin's own copy is never consulted). 128 @48k = 2.67 ms per hop.
    #[arg(long, default_value_t = 128)]
    buffer_frames: u32,
    /// Marker cadence for the probe, in milliseconds.
    #[arg(long, default_value_t = 300)]
    probe_interval_ms: u64,
    /// Also write what the microphone captured, as a WAV.
    #[arg(long)]
    record_in: Option<PathBuf>,
    /// Also write what was injected into the virtual microphone, as a WAV.
    #[arg(long)]
    record_out: Option<PathBuf>,
    /// Write the run report (device info + latency + per-frame timing) as JSON.
    #[arg(long)]
    report: Option<PathBuf>,
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ProbeArg {
    /// Inject -> HAL plugin ring -> virtual-mic capture. No microphone, no
    /// room; safe to run unattended. Add the model lookahead for the total.
    Digital,
    /// Physical speaker -> room -> physical microphone -> virtual microphone.
    /// This is the number the user actually feels.
    Acoustic,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run(a) => cmd_run(a),
        Cmd::Bench(a) => cmd_bench(a),
        Cmd::Diff(a) => cmd_diff(a),
        Cmd::Live(a) => cmd_live(a),
    }
}

fn cmd_run(a: RunArgs) -> Result<()> {
    let engine = Engine::load(a.dll.as_deref())?;
    println!("engine      : {}", engine.path.display());
    println!(
        "frame       : {} samples ({} ms), state {} B",
        engine.frame_size,
        engine.frame_size as f64 / 48.0,
        engine.state_bytes
    );

    let input = wavio::read(&a.input)?;
    let cfg = RunConfig {
        mix: a.mix,
        adaptive: a.adaptive,
        warmup_frames: a.warmup,
        lookahead_samples: a.lookahead_samples,
    };
    let RunOutcome { samples, report } = agent::process(&input, &engine, &cfg)?;

    if let Some(p) = &a.out {
        wavio::write(p, &samples)?;
        println!("output      : {}", p.display());
    }

    let mut json = serde_json::json!({ "run": report });
    if let Some(refp) = &a.reference {
        let reference = wavio::read(refp)?;
        let before = metrics::si_sdr(&reference, &input, 3 * metrics::FRAME).report();
        let after = metrics::si_sdr(&reference, &samples, 3 * metrics::FRAME).report();
        json["input_dbfs"] = serde_json::json!(metrics::dbfs(&input));
        json["before"] = serde_json::to_value(&before)?;
        json["after"] = serde_json::to_value(&after)?;
        println!("\n{:<12} {:>10} {:>10}", "", "before", "after");
        println!(
            "{:<12} {:>10.2} {:>10.2}  dB   SI-SDR",
            "SI-SDR", before.si_sdr_db, after.si_sdr_db
        );
        println!(
            "{:<12} {:>10.2} {:>10.2}  dB   segSNR",
            "segSNR", before.segsnr_db, after.segsnr_db
        );
        println!(
            "{:<12} {:>10.2} {:>10.2}  dBFS noise floor",
            "noise floor", before.noise_floor_dbfs, after.noise_floor_dbfs
        );
    }

    println!("\n--- denoise stage ---");
    print_timing(&report.timing);
    println!("VAD mean    : {:.3}", report.vad_mean);
    println!(
        "wet ratio   : mean {:.3}  min {:.3}  max {:.3}",
        report.wet_ratio_mean, report.wet_ratio_min, report.wet_ratio_max
    );
    println!(
        "noise floor : {:.1} dBFS (adaptive mixer estimate)",
        report.noise_floor_dbfs
    );
    println!(
        "long-term SNR: {:.1} dB  (adaptive gate decided on this)",
        report.long_term_snr_db
    );
    println!(
        "dry delay   : {} samples ({:.1} ms)",
        report.dry_delay_samples,
        report.dry_delay_samples as f64 / 48.0
    );

    if let Some(p) = &a.stats {
        std::fs::write(p, serde_json::to_string_pretty(&json)?)?;
        println!("report      : {}", p.display());
    }
    Ok(())
}

fn cmd_live(a: LiveArgs) -> Result<()> {
    let probe = a.probe.map(|p| match p {
        ProbeArg::Digital => platform::ProbeMode::Digital,
        ProbeArg::Acoustic => platform::ProbeMode::Acoustic,
    });
    let cfg = platform::LiveConfig {
        dll: a.dll,
        adaptive: a.adaptive,
        mix: a.mix,
        vdev: a.vdev,
        input: a.input,
        seconds: a.seconds,
        probe,
        buffer_frames: a.buffer_frames,
        probe_interval_ms: a.probe_interval_ms,
        record_in: a.record_in,
        record_out: a.record_out,
        report: a.report,
    };
    platform::run_live(cfg).map_err(|e| {
        // Engine::load fires deep inside the live path, and its bare
        // "could not find librnnoise" hides what still works without a
        // backend. Attach the escape hatch when -- and only when -- that
        // is the failure, so device errors are not muddied by it.
        let engine_load = e.chain().any(|c| {
            let s = c.to_string();
            s.contains("librnnoise") || s.starts_with("dlopen ")
        });
        if engine_load {
            e.context(
                "live denoising needs librnnoise (offline `run`/`bench` \
                        score against the denoised audio, so they need it too); \
                        only `live --probe digital` runs without a backend",
            )
        } else {
            e
        }
    })
}

fn print_timing(t: &stats::TimingStats) {
    println!(
        "frames      : {}  ({:.3} s of audio)",
        t.frames, t.audio_seconds
    );
    println!(
        "frame time  : min {:.4}  avg {:.4}  p50 {:.4}  p95 {:.4}  p99 {:.4}  max {:.4} ms",
        t.frame_ms_min,
        t.frame_ms_avg,
        t.frame_ms_p50,
        t.frame_ms_p95,
        t.frame_ms_p99,
        t.frame_ms_max
    );
    println!(
        "wall        : {:.4} s   RTF {:.5}   frame budget used (p99) {:.2} %",
        t.wall_seconds, t.real_time_factor, t.realtime_budget_used_pct
    );
    // proctime::cpu_seconds() is Windows-only; elsewhere it is 0.0 and the
    // derived percentages would be garbage (streams/core divides 0 by 1e-9).
    // Say "n/a" instead of inventing numbers.
    if t.cpu_seconds > 0.0 {
        println!(
            "cpu         : {:.4} s   {:.2} % of one core to run realtime   -> {:.0} streams/core",
            t.cpu_seconds, t.cpu_percent_realtime, t.realtime_streams_per_core
        );
    } else {
        println!("cpu         : n/a  (process CPU time not measured on this platform)");
    }
}

#[derive(Serialize)]
struct VariantReport {
    label: String,
    mix: String,
    run: agent::RunReport,
    before: MetricsReport,
    after: MetricsReport,
    delta_si_sdr_db: f64,
    delta_segsnr_db: f64,
    delta_noise_floor_db: f64,
}

#[derive(Serialize)]
struct BenchReport {
    engine: String,
    engine_state_bytes: usize,
    frame_samples: usize,
    frame_ms: f64,
    sample_rate: u32,
    host: String,
    toolchain: String,
    note: String,
    cases: Vec<CaseReport>,
}

#[derive(Serialize)]
struct CaseReport {
    case: String,
    input: String,
    input_dbfs: f64,
    variants: Vec<VariantReport>,
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn run_variant(
    label: &str,
    mix: String,
    input: &[f32],
    reference: &[f32],
    engine: &Engine,
    cfg: RunConfig,
    max_lag: usize,
) -> Result<(VariantReport, Vec<f32>)> {
    let RunOutcome { samples, report } = agent::process(input, engine, &cfg)?;
    let before = metrics::si_sdr(reference, input, max_lag);
    let after = metrics::si_sdr(reference, &samples, max_lag);
    let v = VariantReport {
        label: label.to_string(),
        mix,
        delta_si_sdr_db: round2(after.si_sdr_db - before.si_sdr_db),
        delta_segsnr_db: round2(after.segsnr_db - before.segsnr_db),
        delta_noise_floor_db: round2(after.noise_floor_dbfs - before.noise_floor_dbfs),
        before: before.report(),
        after: after.report(),
        run: report,
    };
    Ok((v, samples))
}

fn cmd_bench(a: BenchArgs) -> Result<()> {
    let engine = Engine::load(a.dll.as_deref())?;
    let max_lag = a.max_lag_frames * metrics::FRAME;

    let clean_path = a.dir.join("clean_ref.wav");
    let reference = wavio::read(&clean_path)
        .with_context(|| format!("reading clean reference {}", clean_path.display()))?;

    // keep the rendered audio next to the report, so the A/B bundle can be rebuilt
    let wav_dir = a.dir.clone();
    let variants = [
        (
            "full_wet",
            RunConfig {
                mix: 1.0,
                adaptive: false,
                warmup_frames: 100,
                lookahead_samples: 960,
            },
        ),
        (
            "adaptive",
            RunConfig {
                mix: 1.0,
                adaptive: true,
                warmup_frames: 100,
                lookahead_samples: 960,
            },
        ),
    ];

    let mut cases = Vec::new();

    // Case 0: push a perfectly clean recording through the model. This is the
    // row that motivates the adaptive gate.
    cases.push(bench_case(
        "clean_ref",
        "clean_ref.wav",
        &reference,
        &reference,
        &engine,
        &variants,
        max_lag,
        &wav_dir,
        None,
    )?);

    for snr in [30, 10, 5, 0] {
        let name = format!("noisy_snr{snr}db.wav");
        let path = a.dir.join(&name);
        let input = wavio::read(&path).with_context(|| format!("reading {}", path.display()))?;
        cases.push(bench_case(
            &format!("snr{snr}db"),
            &name,
            &input,
            &reference,
            &engine,
            &variants,
            max_lag,
            &wav_dir,
            Some(snr),
        )?);
    }

    let report = BenchReport {
        engine: engine.path.display().to_string(),
        engine_state_bytes: engine.state_bytes,
        frame_samples: engine.frame_size,
        frame_ms: engine.frame_size as f64 / 48.0,
        sample_rate: wavio::SR,
        host: host_description(),
        toolchain: toolchain_description(),
        note: "Rust twin of the C/Python harness: same DLL, same frame loop, same warm-up".into(),
        cases,
    };

    std::fs::write(&a.out, serde_json::to_string_pretty(&report)?)?;
    println!("\nreport      : {}", a.out.display());
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn bench_case(
    case: &str,
    input_name: &str,
    input: &[f32],
    reference: &[f32],
    engine: &Engine,
    variants: &[(&str, RunConfig)],
    max_lag: usize,
    wav_dir: &Path,
    snr: Option<i32>,
) -> Result<CaseReport> {
    let mut out_variants = Vec::new();
    for (label, cfg) in variants {
        let (v, samples) = run_variant(
            label,
            label.to_string(),
            input,
            reference,
            engine,
            *cfg,
            max_lag,
        )?;
        let out_name = match snr {
            Some(n) => format!("after_rust_{label}_snr{n}db.wav"),
            None => format!("after_rust_{label}_cleanref.wav"),
        };
        wavio::write(&wav_dir.join(&out_name), &samples)?;
        // Same n/a rule as print_timing: without a cpu_seconds measurement the
        // "% core realtime" figure would print a fabricated 0.00 %.
        let core_pct = if v.run.timing.cpu_seconds > 0.0 {
            format!("{:.2}%", v.run.timing.cpu_percent_realtime)
        } else {
            "n/a".to_string()
        };
        println!(
            "{:<10} {:<9} | SI-SDR {:>7.2} -> {:>7.2} dB | segSNR {:>6.2} -> {:>6.2} | \
             floor {:>7.2} -> {:>7.2} dBFS | p99 {:.4} ms | {} core realtime | wet {:.2}",
            case,
            label,
            v.before.si_sdr_db,
            v.after.si_sdr_db,
            v.before.segsnr_db,
            v.after.segsnr_db,
            v.before.noise_floor_dbfs,
            v.after.noise_floor_dbfs,
            v.run.timing.frame_ms_p99,
            core_pct,
            v.run.wet_ratio_mean
        );
        out_variants.push(v);
    }
    Ok(CaseReport {
        case: case.to_string(),
        input: input_name.to_string(),
        input_dbfs: round2(metrics::dbfs(input)),
        variants: out_variants,
    })
}

fn host_description() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn toolchain_description() -> String {
    // rustc has no compile-time version macro, and `rustc -V` at runtime is not
    // worth a subprocess; the lockfile + README pin the rest.
    format!(
        "Rust stable, {} release profile (LTO thin, panic=abort)",
        std::env::consts::ARCH
    )
}

fn cmd_diff(a: DiffArgs) -> Result<()> {
    let x = wavio::read(&a.a)?;
    let y = wavio::read(&a.b)?;
    if x.len() != y.len() {
        anyhow::bail!("length mismatch: {} vs {}", x.len(), y.len());
    }
    let mut exact = 0usize;
    let mut within1 = 0usize;
    let mut worst = 0.0f64;
    let mut sum_abs = 0.0f64;
    for (p, q) in x.iter().zip(&y) {
        let d = (*p as f64 - *q as f64).abs();
        if d == 0.0 {
            exact += 1;
        }
        if d <= 1.0 {
            within1 += 1;
        }
        sum_abs += d;
        worst = worst.max(d);
    }
    let n = x.len() as f64;
    println!("samples      : {}", x.len());
    println!("bit-exact    : {:.4} %", exact as f64 / n * 100.0);
    println!("within 1 LSB : {:.4} %", within1 as f64 / n * 100.0);
    println!("mean |diff|  : {:.6} LSB", sum_abs / n);
    println!("max  |diff|  : {:.1} LSB", worst);
    Ok(())
}
