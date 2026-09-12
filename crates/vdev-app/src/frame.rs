//! 真实帧推流客户端：连扩展 FrameChannel（127.0.0.1:27890）。
use anyhow::{anyhow, Result};
use std::io::Write;
use std::net::TcpStream;
use std::time::Duration;

pub struct FrameClient {
    stream: TcpStream,
}

pub fn connect() -> Result<FrameClient> {
    let addr = "127.0.0.1:27890".parse()?;
    for _ in 0..30 {
        if let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
            return Ok(FrameClient { stream });
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    Err(anyhow!("连接扩展 FrameChannel 失败（扩展未运行？）"))
}

/// 发帧能力的最小抽象：让 main.rs 的「发送失败→重置→下一帧重连」共享路径
/// （`send_or_reconnect`）可以注入 `FakeClient` 做离线单测，真实连接则由 `FrameClient` 实现。
pub trait FrameSender {
    fn send_frame(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        pts_ns: u64,
    ) -> Result<()>;
}

impl FrameSender for FrameClient {
    /// 发送一帧 BGRA32：36 字节小端头 + payload（协议见 crates/vdev-camera-ext/src/frame_channel.rs）。
    fn send_frame(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        pts_ns: u64,
    ) -> Result<()> {
        let mut buf = Vec::with_capacity(36 + data.len());
        buf.extend_from_slice(&0x5644_4652_u32.to_le_bytes()); // "VDFR"
        buf.extend_from_slice(&1u32.to_le_bytes()); // version
        buf.extend_from_slice(&width.to_le_bytes());
        buf.extend_from_slice(&height.to_le_bytes());
        buf.extend_from_slice(&stride.to_le_bytes());
        buf.extend_from_slice(&pts_ns.to_le_bytes());
        buf.extend_from_slice(&(data.len() as u64).to_le_bytes());
        buf.extend_from_slice(data);
        self.stream.write_all(&buf)?;
        Ok(())
    }
}
