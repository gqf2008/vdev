//! vdev-bridge 内置 FFmpeg 解码器。
//!
//! 原先复用兄弟仓库的 `aerodesk-codec`（裸 path 依赖 `../../../aerodesk`，
//! 上游一动就编不过，且连带拉入 encode/hw_decode/mux + openh264/x264）。
//! 这里把 vdev-bridge 真正用到的那两个解码头内联进来，只依赖 `ffmpeg-next`：
//!
//! - [`VideoDecoder`]：H.264 / H.265 / VP9 / AV1 → 紧凑 RGBA（sws 转 RGBA）
//! - [`OpusDecoder`]：Opus → 单声道 i16 PCM（48kHz）
//!
//! 移植自 aerodesk-codec 的 `decode::FfmpegDecoder` / `audio::OpusDecoder`
//! （两者的关键坑：sws stride 填充需逐行打包、EAGAIN 的 errno 跨平台差异）。

use ffmpeg_next as ffmpeg;
use ffmpeg_next::codec::packet::Packet;
use ffmpeg_next::format::sample::Type;
use ffmpeg_next::format::{Pixel, Sample};
use ffmpeg_next::frame::Audio as AudioFrame;
use ffmpeg_next::frame::Video;
use ffmpeg_next::software::scaling::{flag::Flags as ScalingFlags, Context as ScalingContext};

use aerodesk_core::platform::Codec;

/// FFmpeg 全局初始化（幂等）。
fn init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let _ = ffmpeg::init();
    });
}

fn codec_id(codec: Codec) -> ffmpeg::codec::Id {
    match codec {
        Codec::H264 => ffmpeg::codec::Id::H264,
        Codec::Hevc => ffmpeg::codec::Id::HEVC,
        Codec::Vp9 => ffmpeg::codec::Id::VP9,
        Codec::Av1 => ffmpeg::codec::Id::AV1,
        other => panic!("ffmpeg decoder unsupported codec: {other:?}"),
    }
}

/// sws 输出的 RGBA 帧按 stride 逐行打包成紧凑 `w*h*4`。
///
/// sws 的输出行宽会按对齐补齐（如宽 1470 → stride 5888 ≠ 5880）；连续拷
/// `w*h*4` 会无视填充、逐行错位，累积成斜向剪切（aerodesk #487 真屏实测）。
fn pack_rgba(src: &[u8], stride: usize, width: usize, height: usize) -> Vec<u8> {
    let row = width * 4;
    let mut raw = vec![0u8; row * height];
    if stride == row {
        raw.copy_from_slice(&src[..row * height]);
    } else {
        for y in 0..height {
            let s = y * stride;
            raw[y * row..(y + 1) * row].copy_from_slice(&src[s..s + row]);
        }
    }
    raw
}

/// 解出的一帧 RGBA（紧凑 `width*height*4`）。
pub struct DecodedFrame {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// FFmpeg 视频解码器（包进 → RGBA 帧出）。
pub struct VideoDecoder {
    decoder: ffmpeg::decoder::Video,
    scaler: Option<ScalingContext>,
    codec: Codec,
}

impl VideoDecoder {
    pub fn new(codec: Codec) -> Result<Self, String> {
        init();
        let id = codec_id(codec);
        let ffmpeg_codec =
            ffmpeg::decoder::find(id).ok_or_else(|| format!("decoder not found: {id:?}"))?;
        // new_with_codec + decoder().video() 会自动打开解码器；SPS/PPS 随首个
        // 关键帧到达，用于配置宽高与像素格式。
        let decoder = ffmpeg::codec::context::Context::new_with_codec(ffmpeg_codec)
            .decoder()
            .video()
            .map_err(|e| format!("decoder open: {e}"))?;
        Ok(Self {
            decoder,
            scaler: None,
            codec,
        })
    }

    pub fn codec(&self) -> Codec {
        self.codec
    }

    /// 解一个完整访问单元；`Ok(None)` = 还需要更多输入（EAGAIN）。
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<DecodedFrame>, String> {
        let mut packet = Packet::new(data.len());
        if let Some(d) = packet.data_mut() {
            d.copy_from_slice(data);
        }
        self.decoder
            .send_packet(&packet)
            .map_err(|e| format!("send_packet: {e}"))?;
        let mut frame = Video::empty();
        match self.decoder.receive_frame(&mut frame) {
            Ok(()) => {
                let w = frame.width() as usize;
                let h = frame.height() as usize;
                if self.scaler.is_none() {
                    self.scaler = Some(
                        ScalingContext::get(
                            frame.format(),
                            w as u32,
                            h as u32,
                            Pixel::RGBA,
                            w as u32,
                            h as u32,
                            ScalingFlags::BILINEAR,
                        )
                        .map_err(|e| format!("scaler: {e}"))?,
                    );
                }
                let mut rgba = Video::empty();
                self.scaler
                    .as_mut()
                    .unwrap()
                    .run(&frame, &mut rgba)
                    .map_err(|e| format!("scale: {e}"))?;
                let raw = pack_rgba(rgba.data(0), rgba.stride(0) as usize, w, h);
                Ok(Some(DecodedFrame {
                    rgba: raw,
                    width: w as u32,
                    height: h as u32,
                }))
            }
            Err(e) => match e {
                ffmpeg::Error::Eof => Ok(None),
                // EAGAIN：解码器需要更多输入。errno 平台相关——Linux/Windows=11、
                // macOS/BSD=35；只认 11 会在 macOS 上把 B 帧缓冲误报成错误。
                ffmpeg::Error::Other { errno } if matches!(errno.abs(), 11 | 35) => Ok(None),
                e => Err(format!("receive_frame: {e:?}")),
            },
        }
    }
}

/// FFmpeg Opus 解码器（libopus → 单声道 i16，48kHz）。
pub struct OpusDecoder {
    decoder: ffmpeg::decoder::Audio,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, String> {
        init();
        let codec = ffmpeg::decoder::find(ffmpeg::codec::Id::OPUS)
            .ok_or_else(|| "opus decoder not found".to_string())?;
        let mut decoder = ffmpeg::codec::context::Context::new_with_codec(codec)
            .decoder()
            .audio()
            .map_err(|e| format!("opus decoder context: {e}"))?;
        // 统一请求 S16 交错输出，避免按解码器默认格式（FLTP）分平面处理。
        decoder.request_format(Sample::I16(Type::Packed));
        Ok(Self { decoder })
    }

    /// 解一个 Opus 包 → 单声道 i16 PCM（双声道取平均）。
    pub fn decode(&mut self, data: &[u8]) -> Result<Option<Vec<i16>>, String> {
        let mut packet = Packet::new(data.len());
        if let Some(d) = packet.data_mut() {
            d.copy_from_slice(data);
        }
        self.decoder
            .send_packet(&packet)
            .map_err(|e| format!("opus send_packet: {e}"))?;
        let mut frame = AudioFrame::empty();
        match self.decoder.receive_frame(&mut frame) {
            Ok(()) => {
                let samples = frame.samples();
                let ch = frame.channels().max(1) as usize;
                let bytes = frame.data(0);
                let mut out = Vec::with_capacity(samples);
                for i in 0..samples {
                    let mut sum = 0i32;
                    let mut cnt = 0usize;
                    for c in 0..ch {
                        let off = (i * ch + c) * 2;
                        if off + 2 <= bytes.len() {
                            sum += i16::from_le_bytes([bytes[off], bytes[off + 1]]) as i32;
                            cnt += 1;
                        }
                    }
                    out.push((sum / cnt.max(1) as i32) as i16);
                }
                Ok(Some(out))
            }
            Err(ffmpeg::Error::Eof) => Ok(None),
            Err(ffmpeg::Error::Other { errno }) if errno.abs() == 11 => Ok(None), // EAGAIN
            Err(e) => Err(format!("opus receive_frame: {e:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// stride 对齐时退化为整块拷贝，逐行打包同样正确（#487 镜像方向）。
    #[test]
    fn pack_rgba_aligned_stride() {
        let (w, h, stride) = (4usize, 3usize, 16usize); // row == stride
        let mut src = vec![0u8; stride * h];
        for y in 0..h {
            for x in 0..w {
                src[y * stride + x * 4] = (y * w + x) as u8;
            }
        }
        assert_eq!(pack_rgba(&src, stride, w, h), src);
    }

    /// stride 带填充时逐行错位是历史花屏根因，这里钉住行为。
    #[test]
    fn pack_rgba_padded_stride() {
        let (w, h, stride) = (3usize, 2usize, 16usize); // row=12 < stride=16
        let mut src = vec![0xAAu8; stride * h];
        for y in 0..h {
            for x in 0..w {
                src[y * stride + x * 4] = (y * 10 + x) as u8;
            }
        }
        let out = pack_rgba(&src, stride, w, h);
        assert_eq!(out.len(), w * h * 4);
        for y in 0..h {
            for x in 0..w {
                assert_eq!(
                    out[y * w * 4 + x * 4],
                    (y * 10 + x) as u8,
                    "row {y} pixel {x} 错位"
                );
            }
        }
    }
}
