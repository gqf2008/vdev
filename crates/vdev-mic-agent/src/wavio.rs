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
        bail!(
            "{}: expected {} Hz, got {}",
            path.display(),
            SR,
            spec.sample_rate
        );
    }
    let ch = spec.channels.max(1) as usize;

    let raw: Vec<f32> = match spec.sample_format {
        SampleFormat::Int => match spec.bits_per_sample {
            16 => r
                .samples::<i16>()
                .map(|s| s.map(f32::from))
                .collect::<Result<_, _>>()?,
            24 | 32 => {
                // Single shift to int16 scale: 24-bit full scale (8388607)
                // reads as 32767, 32-bit as 32767. Never scale twice.
                let bits = spec.bits_per_sample as i64;
                let scale = (1i64 << (bits - 16)) as f32; // 24-bit: 256, 32-bit: 65536
                r.samples::<i32>()
                    .map(|s| s.map(|v| v as f32 / scale))
                    .collect::<Result<_, _>>()?
            }
            b => bail!("unsupported PCM bit depth {}", b),
        },
        SampleFormat::Float => r
            .samples::<f32>()
            .map(|s| s.map(|v| v * 32768.0))
            .collect::<Result<_, _>>()?,
    };

    Ok(downmix(&raw, ch))
}

pub fn write(path: &Path, samples: &[f32]) -> Result<()> {
    let spec = WavSpec {
        channels: 1,
        sample_rate: SR,
        bits_per_sample: 16,
        sample_format: SampleFormat::Int,
    };
    let mut w =
        WavWriter::create(path, spec).with_context(|| format!("create {}", path.display()))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 24-bit PCM must land on int16 scale with a *single* shift: full scale
    /// 8388607 -> 32767, not the previously double-scaled 128.
    #[test]
    fn reads_24bit_full_scale_correctly() {
        let path =
            std::env::temp_dir().join(format!("vdev-mic-agent-wavio24-{}.wav", std::process::id()));
        let spec = WavSpec {
            channels: 1,
            sample_rate: SR,
            bits_per_sample: 24,
            sample_format: SampleFormat::Int,
        };
        {
            let mut w = WavWriter::create(&path, spec).unwrap();
            w.write_sample(8388607i32).unwrap();
            w.write_sample(-8388608i32).unwrap();
            w.write_sample(0i32).unwrap();
            w.finalize().unwrap();
        }
        let s = read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // (2^23 - 1) / 2^8 = 32767.996: a single shift lands within 1 LSB of
        // int16 full scale, never at the double-scaled 128.
        assert!(
            (s[0] - 32767.0).abs() <= 1.0,
            "positive full scale: {}",
            s[0]
        );
        assert!(
            (s[1] + 32768.0).abs() <= 1.0,
            "negative full scale: {}",
            s[1]
        );
        assert_eq!(s[2], 0.0);
    }

    /// 32-bit path: unchanged by the fix, still v / 2^16.
    #[test]
    fn reads_32bit_full_scale_correctly() {
        let path =
            std::env::temp_dir().join(format!("vdev-mic-agent-wavio32-{}.wav", std::process::id()));
        let spec = WavSpec {
            channels: 1,
            sample_rate: SR,
            bits_per_sample: 32,
            sample_format: SampleFormat::Int,
        };
        {
            let mut w = WavWriter::create(&path, spec).unwrap();
            w.write_sample(2147483647i32).unwrap();
            w.write_sample(-2147483648i32).unwrap();
            w.finalize().unwrap();
        }
        let s = read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        // 2^31-1 as f32 rounds up to 2^31, so positive full scale reads 32768
        // (pre-existing behaviour, unchanged by the fix); within 1 LSB.
        assert!((s[0] - 32767.0).abs() <= 1.0, "{}", s[0]);
        assert!((s[1] + 32768.0).abs() <= 0.01, "{}", s[1]);
    }
}
