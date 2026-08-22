//! 推流线程：取共享帧通道最新帧 → 填 `IMediaSample` → 交给下游 `IMemInputPin::Receive`。
//!
//! 线程在 `IMediaFilter::Run` 时启动、`Stop` 时停止；期间若无新帧（生产者未推流），
//! 回退到 vdev-camera 的测试图案（棋盘格），与 macOS 版「无帧回退彩条」语义一致。

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Media::DirectShow::IMediaSample;

use super::filter::FilterInner;
use super::media_type::VideoFormat;
use super::pin::PinInner;
use crate::com::shm::SharedFrameChannel;
use crate::com::ComInit;

/// 推流线程句柄。
pub struct StreamThread {
    stop: Arc<AtomicBool>,
    tstart: Arc<AtomicI64>,
    join: Option<JoinHandle<()>>,
}

impl StreamThread {
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }

    /// 更新 Run 时间起点（图在 Pause 预滚后调用 Run 时设置）。
    pub fn set_tstart(&self, tstart: i64) {
        self.tstart.store(tstart, Ordering::Relaxed);
    }
}

/// 启动推流线程。
///
/// 注意：推流线程在 **Pause** 时启动（源过滤器需先送预滚帧，渲染器才能完成
/// Pause，图才会调用 Run），而不是在 Run 时启动。
pub fn start_stream(filter: Arc<FilterInner>, tstart: i64) -> StreamThread {
    let stop = Arc::new(AtomicBool::new(false));
    let tstart_atomic = Arc::new(AtomicI64::new(tstart));
    let stop2 = stop.clone();
    let tstart2 = tstart_atomic.clone();
    let pin = filter.pin.clone();
    let channel = filter.channel.clone();
    let join = std::thread::Builder::new()
        .name("vdev-camera-win-stream".into())
        .stack_size(1 << 20) // 1 MiB（DirectShow 调用栈较深）
        .spawn(move || stream_loop(pin, channel, stop2, tstart2))
        .expect("spawn stream thread");
    StreamThread {
        stop,
        tstart: tstart_atomic,
        join: Some(join),
    }
}

fn stream_loop(
    pin: Arc<PinInner>,
    channel: Arc<SharedFrameChannel>,
    stop: Arc<AtomicBool>,
    tstart: Arc<AtomicI64>,
) {
    // 线程要调用 COM 对象（allocator/IMemInputPin），先初始化 COM（MTA）。
    let _com = match ComInit::new() {
        Ok(c) => c,
        Err(e) => {
            log::error!("stream thread COM init failed: {e}");
            return;
        }
    };

    let started = Instant::now();
    let mut frame: Vec<u8> = Vec::new();
    let mut pattern_t: f64 = 0.0;
    let mut iters: u64 = 0;
    let mut last_log = Instant::now();
    log::debug!("stream thread started");

    while !stop.load(Ordering::Relaxed) {
        // 断开连接即停止。
        let conn = match pin.connected() {
            Some(c) => c,
            None => break,
        };

        // Flush 期间不投递，等 EndFlush。
        if pin.flushing.load(Ordering::Relaxed) {
            std::thread::sleep(std::time::Duration::from_millis(5));
            continue;
        }

        // 等待新帧（超时 = 一帧时长）；无生产者时按目标帧率用测试图案兜底。
        // 注意：输出必须始终是协商格式（conn.format）的尺寸——生产者帧尺寸
        // 不一致时做最近邻缩放，否则下游会因帧大小不匹配而解码失败。
        let out_w = conn.format.width as usize;
        let out_h = conn.format.height as usize;
        let out_need = out_w * out_h * 4;
        let _ = channel.wait_frame(1000 / conn.format.fps.max(1));
        match channel.latest(&mut frame) {
            Some((w, h)) if w as usize == out_w && h as usize == out_h => {}
            Some((w, h)) if (w as usize * h as usize * 4) == frame.len() => {
                let src = std::mem::take(&mut frame);
                scale_bgra_nearest(&src, w as usize, h as usize, &mut frame, out_w, out_h);
            }
            _ => {
                render_pattern(&mut frame, &conn.format, &mut pattern_t);
            }
        }

        if frame.len() < out_need {
            continue;
        }
        let buf: &[u8] = &frame[..out_need];

        // 取下游缓冲区（阻塞直到可用或失败）。
        let mut sample_slot: Option<IMediaSample> = None;
        // SAFETY: 分配器已 Commit；GetBuffer 输出 sample_slot 并持有引用。
        let sample = match unsafe { conn.allocator.GetBuffer(&mut sample_slot, None, None, 0) } {
            Ok(()) => match sample_slot.take() {
                Some(s) => s,
                None => continue,
            },
            Err(e) => {
                log::debug!("GetBuffer failed: {e:?}");
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                continue;
            }
        };

        // 填帧。
        let tstart_now = tstart.load(Ordering::Relaxed);
        if !deliver_sample(
            &sample,
            buf,
            out_w as u32,
            out_h as u32,
            &conn.format,
            tstart_now,
            &started,
        ) {
            continue;
        }
        pin.note_frame();

        // 交给下游；下游停止（S_FALSE 等）时继续等待。
        // SAFETY: 连接已建立，sample 来自已 Commit 的分配器。
        if let Err(e) = unsafe { conn.input.Receive(&sample) } {
            log::debug!("downstream Receive failed: {e:?}");
            if stop.load(Ordering::Relaxed) {
                break;
            }
        }
        iters += 1;
        if last_log.elapsed() >= Duration::from_secs(1) {
            log::debug!(
                "stream loop: {iters} frames in {:?} (out={out_w}x{out_h})",
                last_log.elapsed()
            );
            last_log = Instant::now();
        }
    }
    log::debug!("stream thread exited");
}

/// 填一帧到 IMediaSample 并设置时间戳；失败返回 false。
fn deliver_sample(
    sample: &IMediaSample,
    buf: &[u8],
    width: u32,
    height: u32,
    _format: &VideoFormat,
    _tstart: i64,
    _started: &Instant,
) -> bool {
    // SAFETY: COM 方法调用。
    let ptr = match unsafe { sample.GetPointer() } {
        Ok(p) => p,
        Err(_) => return false,
    };
    // SAFETY: COM 方法调用，返回样本缓冲区大小。
    let size = unsafe { sample.GetSize() } as usize;
    let need = (width as usize) * (height as usize) * 4;
    if size < need {
        log::warn!("sample too small: {size} < {need}");
        return false;
    }
    // SAFETY: ptr 指向 size 字节可写缓冲区；need <= size。
    unsafe { std::ptr::copy_nonoverlapping(buf.as_ptr(), ptr, need) };
    // SAFETY: 设置实际数据长度。
    if unsafe { sample.SetActualDataLength(need as i32) }.is_err() {
        return false;
    }
    // 不设置样本时间戳：虚拟摄像头是推源，设置时间戳会让基于 CBaseRenderer 的
    // 下游（如 NullRenderer）按参考时钟等待，造成帧率被拖慢/卡死。
    // （无时间戳时渲染器立即渲染；需要 A/V 同步的场景后续再按时钟补。）
    // SAFETY: COM 方法调用，设置样本标志位。
    let _ = unsafe { sample.SetSyncPoint(true) };
    let _ = unsafe { sample.SetPreroll(false) };
    true
}

/// 渲染 vdev-camera 测试图案（RGB24）并转换为 BGRA。
pub fn render_pattern(out: &mut Vec<u8>, format: &VideoFormat, t: &mut f64) {
    let frame = vdev_camera::frame::render(
        vdev_camera::frame::FramePattern::Checker,
        format.width,
        format.height,
        *t,
    );
    *t += 1.0 / format.fps as f64;
    out.resize(format.frame_size(), 0);
    for (i, px) in frame.data.chunks_exact(3).enumerate() {
        out[i * 4] = px[2]; // B
        out[i * 4 + 1] = px[1]; // G
        out[i * 4 + 2] = px[0]; // R
        out[i * 4 + 3] = 255; // A
    }
}

/// 最近邻缩放 BGRA（4 字节/像素）帧到目标尺寸。
///
/// 虚拟摄像头输出必须始终是连接协商的格式；生产者帧尺寸不一致时用它适配，
/// 保证下游收到的每帧大小与协商一致（整数运算，无浮点）。
fn scale_bgra_nearest(src: &[u8], sw: usize, sh: usize, dst: &mut Vec<u8>, dw: usize, dh: usize) {
    dst.resize(dw * dh * 4, 0);
    for y in 0..dh {
        let sy = (y * sh) / dh;
        let src_row = sy * sw * 4;
        let dst_row = y * dw * 4;
        for x in 0..dw {
            let sx = (x * sw) / dw;
            let s = src_row + sx * 4;
            let d = dst_row + x * 4;
            dst[d..d + 4].copy_from_slice(&src[s..s + 4]);
        }
    }
}
