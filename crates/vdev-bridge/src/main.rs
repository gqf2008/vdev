//! vdev-bridge：远端 WebRTC 流 → 本地虚拟摄像头（+ 后续声卡）。
//! 复用 aerodesk 的 str0m 收流 + FFmpeg 解码，解码帧经滤镜后推给 vdev 虚拟摄像头。
//!
//! 用法：vdev-bridge <signal_server> <room> [auth]
//! 环境变量：
//!   VDEV_FILTER="brightness,contrast,saturation,green,sharpen"  滤镜参数
//!   VDEV_WIDTH / VDEV_HEIGHT  输出分辨率（默认 1920x1080）

use std::time::Instant;

use aerodesk_core::access_unit::AccessUnitAssembler;
use aerodesk_core::connect::connect_live_role;
use aerodesk_core::endpoint::ClientEvent;
use aerodesk_core::platform::{Codec, EncodedUnit};
use aerodesk_core::protocol::signal::Role;
use aerodesk_codec::decode::FfmpegDecoder;
use str0m::{Input, Output};

mod audio;
mod frame;


fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: vdev-bridge <signal_server> <room> [auth]");
        std::process::exit(1);
    }
    let server = &args[1];
    let room = &args[2];
    let auth = args.get(3).map(|s| s.as_str());

    // 1. 连接 SFU（viewer）
    let session = connect_live_role(server, room, Role::Viewer, auth)?;
    println!("已连接：{}", session.summary());
    let mut endpoint = session.endpoint;
    let mut socket = session.socket;
    // signal 后台线程保活（收流走 UDP，signal 只负责协商后忽略消息）
    std::thread::spawn(move || {
        let mut signal = session.signal;
        loop {
            if signal.recv().is_err() {
                break;
            }
        }
    });

    // 2. 摄像头推流客户端
    let mut frame_client = match frame::connect() {
        Ok(f) => { println!("已连接虚拟摄像头 FrameChannel"); Some(f) }
        Err(e) => { eprintln!("警告：{e}，仅解码不推流"); None }
    };

    // 3. 滤镜参数
    let filter = filter_params_from_env();
    let out_w: u32 = std::env::var("VDEV_WIDTH").ok().and_then(|s| s.parse().ok()).unwrap_or(1920);
    let out_h: u32 = std::env::var("VDEV_HEIGHT").ok().and_then(|s| s.parse().ok()).unwrap_or(1080);

    // 4. 解码器 + 组装器
    let mut decoder: Option<FfmpegDecoder> = None;
    let mut assembler = AccessUnitAssembler::new();

    // 5. 音频输出（声卡）+ Opus 解码
    let mut audio_sink = match audio::AudioSink::new() {
        Ok(s) => Some(s),
        Err(e) => { eprintln!("警告：{e}，跳过音频"); None }
    };
    let mut opus_decoder = audio_sink.as_ref().and_then(|_| aerodesk_codec::audio::OpusDecoder::new().ok());

    let mut frames = 0u64;
    let mut audio_frames = 0u64;
    let mut buf = [0u8; 2048];
    println!("收流中… Ctrl-C 退出");
    loop {
        // 收 UDP → endpoint
        if let Ok((n, source)) = socket.recv_from(&mut buf) {
            if let Ok(contents) = buf[..n].try_into() {
                let _ = endpoint.handle_input(Input::Receive(
                    Instant::now(),
                    str0m::net::Receive {
                        proto: str0m::net::Protocol::Udp,
                        source,
                        destination: socket.local_addr().map_err(|e| e.to_string())?,
                        contents,
                    },
                ));
            }
        }
        let _ = endpoint.handle_timeout(Instant::now());

        // endpoint 输出 → socket
        while let Some(output) = endpoint.poll_output() {
            match output {
                Output::Transmit(t) => {
                    let _ = socket.send_to(&t.contents, t.destination);
                }
                Output::Timeout(_) => break,
                Output::Event(_) => {}
            }
        }

        // 事件 → 媒体
        while let Some(ev) = endpoint.poll_event() {
            if let ClientEvent::Media(data) = ev {
                // 音频轨：Opus 解码 → 声卡
                if data.params.spec().codec == str0m::format::Codec::Opus {
                    if let (Some(sink), Some(dec)) = (&mut audio_sink, &mut opus_decoder) {
                        if let Ok(Some(pcm)) = dec.decode(&data.data) {
                            sink.push_mono_i16(&pcm);
                            audio_frames += 1;
                            if audio_frames % 100 == 0 {
                                println!("音频帧 {}（samples={}）", audio_frames, pcm.len());
                            }
                        }
                    }
                    continue;
                }
                if data.params.spec().codec == str0m::format::Codec::PCMU {
                    continue;
                }
                // 视频轨：识别 codec
                let codec = match data.params.spec().codec {
                    str0m::format::Codec::H264 => Some(Codec::H264),
                    str0m::format::Codec::H265 => Some(Codec::Hevc),
                    str0m::format::Codec::Vp9 => Some(Codec::Vp9),
                    str0m::format::Codec::Av1 => Some(Codec::Av1),
                    _ => None,
                };
                let Some(codec) = codec else { continue };

                // 解码器按 codec 重建
                if decoder.as_ref().map(|d| d.codec() != codec).unwrap_or(true) {
                    decoder = FfmpegDecoder::new(codec).ok();
                }

                // 组装完整访问单元
                let Some(au) = assembler.push(
                    data.data.as_ref(),
                    data.time.as_micros(),
                    data.is_keyframe(),
                ) else {
                    continue;
                };
                let unit = EncodedUnit {
                    data: au.data,
                    keyframe: au.keyframe,
                    pts_ms: 0,
                    rtp_timestamp: 0,
                };
                let Some(dec) = decoder.as_mut() else { continue };
                let Ok(Some(vf)) = dec.decode_unit(&unit) else { continue };
                let Some(rgba) = vf.raw else { continue };

                // RGBA → BGRA + 缩放（帧分辨率可能是源分辨率，需缩放到输出）
                let mut bgra = rgba_to_bgra_scaled(&rgba, vf.width, vf.height, out_w, out_h);

                // 滤镜
                vdev_filter::process_frame(&mut bgra, out_w, out_h, &filter);

                // 推摄像头
                if let Some(fc) = &mut frame_client {
                    let _ = fc.send_frame(&bgra, out_w, out_h, out_w * 4, host_time_ns());
                }
                frames += 1;
                if frames % 60 == 0 {
                    println!("已推 {frames} 帧（{}x{}）", out_w, out_h);
                }
            }
        }
    }
}

fn filter_params_from_env() -> vdev_filter::FilterParams {
    let mut p = vdev_filter::FilterParams::default();
    if let Ok(v) = std::env::var("VDEV_FILTER") {
        let parts: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if parts.len() >= 1 { p.brightness = parts[0]; }
        if parts.len() >= 2 { p.contrast = parts[1]; }
        if parts.len() >= 3 { p.saturation = parts[2]; }
        if parts.len() >= 4 { p.green_screen_threshold = parts[3].clamp(0.0, 255.0) as u8; }
        if parts.len() >= 5 { p.sharpen = parts[4]; }
    }
    p
}

/// RGBA → BGRA，并缩放到目标尺寸（简单双线性，够 MVP 用）
fn rgba_to_bgra_scaled(rgba: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = vec![0u8; (dw * dh * 4) as usize];
    for y in 0..dh {
        let sy = (y * sh / dh.max(1)) as usize;
        for x in 0..dw {
            let sx = (x * sw / dw.max(1)) as usize;
            let si = (sy * sw as usize + sx) * 4;
            let di = (y * dw + x) as usize * 4;
            out[di] = rgba[si + 2];     // B ← R
            out[di + 1] = rgba[si + 1]; // G
            out[di + 2] = rgba[si];     // R ← B
            out[di + 3] = rgba[si + 3]; // A
        }
    }
    out
}

fn host_time_ns() -> u64 {
    unsafe extern "C" { fn mach_absolute_time() -> u64; }
    // 近似：mach ticks 直接当纳秒（摄像头侧只看单调性，不要求绝对精度）
    unsafe { mach_absolute_time() }
}
