//! FrameChannel：真实帧推流通道（TCP 127.0.0.1:27890）。
//! 协议对齐 host 端 crates/vdev-app/src/frame.rs 与旧 Swift FrameChannel.swift：
//! 36 字节小端头 [magic=0x56444652(VDFR), version=1, width u32, height u32,
//! stride u32, ptsNs u64, payloadLen u64] + BGRA payload。
//! 只保留最新一帧（与 Swift 的 injectFrame 语义一致）。

use std::io::Read;
use std::net::{TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct InjectedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pts_ns: u64,
    pub received_at: Instant,
}

static INJECTED: Mutex<Option<InjectedFrame>> = Mutex::new(None);

const HEADER_SIZE: usize = 36;
const MAGIC: u32 = 0x56444652;
const VERSION: u32 = 1;

/// 取最新注入帧；超过 max_age 视为过期（返回 None → 回落彩条）。
pub fn take_fresh(max_age: Duration) -> Option<(Vec<u8>, u32, u32, u32, u64)> {
    let mut g = INJECTED.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(f) = g.as_ref() {
        if f.received_at.elapsed() < max_age {
            return Some((f.data.clone(), f.width, f.height, f.stride, f.pts_ns));
        }
        // 过期帧清掉，避免一直残留
        *g = None;
    }
    None
}

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off], b[off + 1], b[off + 2], b[off + 3], b[off + 4], b[off + 5], b[off + 6], b[off + 7],
    ])
}

fn handle_conn(mut stream: TcpStream) {
    let mut pending: Vec<u8> = Vec::new();
    let mut reading_header = true;
    let mut expected = 0usize;
    let mut hdr: Option<(u32, u32, u32, u64, usize)> = None;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                pending.extend_from_slice(&chunk[..n]);
                loop {
                    if reading_header {
                        if pending.len() < HEADER_SIZE {
                            break;
                        }
                        let magic = u32le(&pending, 0);
                        let version = u32le(&pending, 4);
                        if magic != MAGIC || version != VERSION {
                            break; // 协议不符，断开
                        }
                        let w = u32le(&pending, 8);
                        let h = u32le(&pending, 12);
                        let stride = u32le(&pending, 16);
                        let pts = u64le(&pending, 20);
                        let len = u64le(&pending, 28) as usize;
                        if w == 0 || h == 0 || stride < w * 4 || len != stride as usize * h as usize {
                            break;
                        }
                        hdr = Some((w, h, stride, pts, len));
                        expected = len;
                        reading_header = false;
                        pending.drain(..HEADER_SIZE);
                    } else {
                        if pending.len() < expected {
                            break;
                        }
                        let payload = pending[..expected].to_vec();
                        pending.drain(..expected);
                        if let Some((w, h, stride, pts, _len)) = hdr.take() {
                            *INJECTED.lock().unwrap_or_else(|e| e.into_inner()) =
                                Some(InjectedFrame {
                                    data: payload,
                                    width: w,
                                    height: h,
                                    stride,
                                    pts_ns: pts,
                                    received_at: Instant::now(),
                                });
                        }
                        reading_header = true;
                        expected = 0;
                    }
                }
            }
            Err(_) => break,
        }
    }
}

/// 启动推流通道监听（后台线程）。
pub fn start() {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind("127.0.0.1:27890") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("vdev-camera-ext: FrameChannel bind 127.0.0.1:27890 失败: {}", e);
                return;
            }
        };
        eprintln!("vdev-camera-ext: FrameChannel 监听 127.0.0.1:27890");
        for conn in listener.incoming() {
            match conn {
                Ok(stream) => {
                    // 新客户端接入即接管（旧连接线程会在断开时自然退出）
                    std::thread::spawn(move || handle_conn(stream));
                }
                Err(_) => continue,
            }
        }
    });
}
