//! 极简 WAV 读写（纯逻辑，无平台依赖 → 宿主单测覆盖）。
//!
//! 只支持虚拟声卡链路实际会用到的两种采样格式：
//! - 16bit PCM（`wFormatTag=1`，也是本驱动的设备格式）
//! - 32bit IEEE float（`wFormatTag=3`，Windows 共享模式混音格式）
//!
//! 读入统一转成 f32（范围 −1.0..1.0）交错样本；写出固定 16bit PCM。

use anyhow::{Context as _, Result, bail};

/// 解析出的 WAV 数据
pub struct Wav {
    pub sample_rate: u32,
    pub channels: u16,
    /// 交错样本（长度 = 帧数 × 声道数），归一化到 −1.0..1.0
    pub samples: Vec<f32>,
}

impl Wav {
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            return 0;
        }
        self.samples.len() / usize::from(self.channels)
    }

    /// 取第 `frame` 帧第 `ch` 个声道（越界回绕/补 0，便于声道数不一致时的简易上下混音）
    pub fn sample(&self, frame: usize, ch: usize) -> f32 {
        let n = usize::from(self.channels);
        if n == 0 {
            return 0.0;
        }
        let src_ch = if ch < n { ch } else { ch % n };
        self.samples.get(frame * n + src_ch).copied().unwrap_or(0.0)
    }
}

fn u16_at(b: &[u8], off: usize) -> Result<u16> {
    let s = b.get(off..off + 2).context("WAV 截断（u16）")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

fn u32_at(b: &[u8], off: usize) -> Result<u32> {
    let s = b.get(off..off + 4).context("WAV 截断（u32）")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// 解析 16bit PCM / 32bit float 的 WAV（RIFF 块按 chunk 遍历，容忍额外的元数据块）
pub fn parse_wav(bytes: &[u8]) -> Result<Wav> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("不是 RIFF/WAVE 文件");
    }
    let mut pos = 12usize;
    let mut fmt: Option<(u16, u16, u32, u16)> = None; // (tag, channels, rate, bits)
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32_at(bytes, pos + 4)? as usize;
        let body_start = pos + 8;
        let body_end = body_start.saturating_add(size).min(bytes.len());
        match id {
            b"fmt " => {
                let body = &bytes[body_start..body_end];
                let tag = u16_at(body, 0)?;
                let channels = u16_at(body, 2)?;
                let rate = u32_at(body, 4)?;
                let bits = u16_at(body, 14)?;
                // WAVE_FORMAT_EXTENSIBLE：SubFormat 才是真实标签（PCM / IEEE_FLOAT）
                let tag = if tag == 0xFFFE && body.len() >= 26 {
                    let sub = &body[24..40.min(body.len())];
                    if sub.starts_with(&[0x03, 0x00, 0x00, 0x00]) {
                        3
                    } else {
                        1
                    }
                } else {
                    tag
                };
                fmt = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        // chunk 按偶数字节对齐
        pos = body_start + size + (size & 1);
    }
    let (tag, channels, sample_rate, bits) = fmt.context("WAV 缺少 fmt 块")?;
    let data = data.context("WAV 缺少 data 块")?;
    if channels == 0 {
        bail!("WAV 声道数为 0");
    }
    // 用 `as_chunks`（1.88 起稳定）而不是 `chunks_exact(常量)`：
    // clippy 1.98 起 `chunks_exact_to_as_chunks` 是默认 lint，CI 的 `-D warnings` 会直接拦下。
    let samples: Vec<f32> = match (tag, bits) {
        (1, 16) => data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| f32::from(i16::from_le_bytes(*c)) / 32768.0)
            .collect(),
        (3, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
        (1, 32) => data
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| i32::from_le_bytes(*c) as f32 / 2_147_483_648.0)
            .collect(),
        _ => bail!("不支持的 WAV 格式：tag={tag} bits={bits}（支持 16bit PCM / 32bit float）"),
    };
    Ok(Wav {
        sample_rate,
        channels,
        samples,
    })
}

/// 生成 16bit PCM WAV 字节（交错样本，自动钳位）
pub fn write_wav16(sample_rate: u32, channels: u16, samples: &[f32]) -> Vec<u8> {
    let ch = usize::from(channels.max(1));
    let frames = samples.len() / ch;
    let data_len = (frames * ch * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt 块长度
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&channels.max(1).to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    let block_align = (ch * 2) as u16;
    out.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples.iter().take(frames * ch) {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i32;
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

/// 交错样本的 RMS（线性）与峰值绝对值
pub fn rms_peak(samples: &[f32]) -> (f64, f64) {
    if samples.is_empty() {
        return (0.0, 0.0);
    }
    let mut sum_sq = 0.0f64;
    let mut peak = 0.0f64;
    for &s in samples {
        let v = f64::from(s);
        sum_sq += v * v;
        let a = v.abs();
        if a > peak {
            peak = a;
        }
    }
    ((sum_sq / samples.len() as f64).sqrt(), peak)
}

/// 线性幅度 → dBFS（0 与负数回 −100，避免 −inf 难以阅读）
pub fn to_dbfs(linear: f64) -> f64 {
    if linear <= 1e-10 {
        -100.0
    } else {
        20.0 * linear.log10()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 16bit PCM 写→读回环：数值在 1 LSB 内一致
    #[test]
    fn wav16_roundtrip() {
        let src = vec![0.0f32, 0.5, -0.5, 0.999, -0.999];
        let bytes = write_wav16(48_000, 1, &src);
        let w = parse_wav(&bytes).expect("parse");
        assert_eq!(w.sample_rate, 48_000);
        assert_eq!(w.channels, 1);
        assert_eq!(w.frames(), src.len());
        // 写用 32767（正满幅映射）、读用 32768（i16 → f32 的标准比例），
        // 两个比例相差约 1 LSB，故容差取 2 LSB。
        for (a, b) in src.iter().zip(w.samples.iter()) {
            assert!((a - b).abs() < 2.0 / 32768.0, "{a} vs {b}");
        }
    }

    /// 32bit float WAV（WAVE_FORMAT_IEEE_FLOAT）也能读，且幅度约为 1.0
    #[test]
    fn parse_float32_wav() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36u32 + 8).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&48_000u32.to_le_bytes());
        bytes.extend_from_slice(&192_000u32.to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&32u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&1.0f32.to_le_bytes());
        bytes.extend_from_slice(&(-0.25f32).to_le_bytes());
        let w = parse_wav(&bytes).expect("parse float wav");
        assert_eq!(w.samples, vec![1.0, -0.25]);
    }

    /// 非 WAV 输入给出明确错误
    #[test]
    fn reject_non_wav() {
        assert!(parse_wav(b"not a wav at all").is_err());
    }

    /// RMS/峰值/dBFS：满幅正弦 RMS ≈ −3.01 dBFS
    #[test]
    fn rms_peak_of_sine() {
        let rate = 48_000usize;
        let s: Vec<f32> = (0..rate)
            .map(|i| (2.0 * std::f64::consts::PI * 1000.0 * i as f64 / rate as f64).sin() as f32)
            .collect();
        let (rms, peak) = rms_peak(&s);
        assert!(
            (to_dbfs(rms) + 3.01).abs() < 0.1,
            "rms dBFS={}",
            to_dbfs(rms)
        );
        assert!((peak - 1.0).abs() < 0.01);
        assert_eq!(to_dbfs(0.0), -100.0);
    }

    /// 声道数不一致时 sample() 回绕取模（简易上下混音的基础）
    #[test]
    fn sample_wraps_channel_index() {
        let w = Wav {
            sample_rate: 48_000,
            channels: 1,
            samples: vec![0.25, 0.5],
        };
        assert_eq!(w.sample(0, 1), 0.25);
        assert_eq!(w.sample(1, 5), 0.5);
        assert_eq!(w.sample(9, 0), 0.0);
    }
}
