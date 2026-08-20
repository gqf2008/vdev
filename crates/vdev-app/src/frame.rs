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

impl FrameClient {
    /// 发送一帧 BGRA32：36 字节小端头 + payload（协议见 extension/FrameChannel.swift）。
    pub fn send_frame(
        &mut self,
        data: &[u8],
        width: u32,
        height: u32,
        stride: u32,
        pts_ns: u64,
    ) -> Result<()> {
        let mut buf = Vec::with_capacity(36 + data.len());
        buf.extend_from_slice(&0x56444652u32.to_le_bytes()); // "VDFR"
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
