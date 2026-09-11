//! Minimal mono / 48 kHz / PCM16 WAV I/O.
//!
//! Signals are kept in *int16 scale* (`-32768.0 ..= 32767.0`) everywhere in this
//! crate, because that is the scale RNNoise expects and the scale the Python
//! reference harness used -- which lets us diff the two outputs sample by sample.

use anyhow::{bail, Context, Result};
use hound::{SampleFormat, WavReader, WavSpec, WavWriter};
use std::path::Path;

pub const SR: u32 = 48_000;

pub fn read(path: &Path) -> Result<Vec<f32>> {
    let mut r = WavReader::open(path).with_context(|| format!("open {}", path.display()))?;
    let spec = r.spec();
    if spec.sample_rate != SR {
        bail!("{}: expected {} Hz, got {}", path.display(), SR, spec.sample_rate);
    }
    let ch = spec.channels.max(1) as usize;

    let raw: Vec<f32> = match spec.sample_format {
        SampleFormat::Int => match spec.bits_per_sample {
            16 => r.samples::<i16>().map(|s| s.map(f32::from)).collect::<Result<_, _>>()?,
            24 | 32 => {
                let scale = if spec.bits_per_sample == 24 { 256.0f32 } else { 1.0 };
                let bits = spec.bits_per_sample as i32;
                r.samples::<i32>()
                    .map(|s| s.map(|v| (v as f32 / scale) / (1i64 << (bits - 16)) as f32))
                    .collect::<Result<_, _>>()?
            }
            b => bail!("unsupported PCM bit depth {}", b),
        },
        SampleFormat::Float => r.samples::<f32>().map(|s| s.map(|v| v * 32768.0)).collect::<Result<_, _>>()?,
    };

    Ok(downmix(&raw, ch))
}

pub fn write(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = WavSpec { channels: 1, sample_rate: SR, bits_per_sample: 16, sample_format: SampleFormat::Int };
    let mut w = WavWriter::create(path, spec).with_context(|| format!("create {}", path.display()))?;
    for &v in samples {
        w.write_sample(v.clamp(-32768.0, 32767.0).round() as i16)?;
    }
    w.finalize()?;
    Ok(())
}

fn downmix(interleaved: &[f32], channels: usize) -> Vec<f32> {
    if channels == 1 {
        return interleaved.to_vec();
    }
    interleaved
        .chunks_exact(channels)
        .map(|f| f.iter().sum::<f32>() / channels as f32)
        .collect()
}
