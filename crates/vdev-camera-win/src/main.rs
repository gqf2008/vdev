//! vdev-camera-win — Windows 虚拟摄像头 CLI。
//!
//! 子命令：install / uninstall / selftest / push / list。
//!
//! 本文件是纯业务层：所有 Windows 系统 API 都通过 `com` / `dshow` 的安全封装
//! 模块调用，不含 unsafe。

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};

use vdev_camera_win::dshow;
use vdev_camera_win::{CameraServer, VideoFormat};

#[derive(Parser)]
#[command(
    name = "vdev-camera-win",
    version,
    about = "Windows 虚拟摄像头（DirectShow）"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 注册虚拟摄像头到系统（推荐以管理员运行；失败自动回退到当前用户注册）
    Install,
    /// 列出系统视频捕获源（与 ffmpeg -f dshow -list_devices 同路径）
    List,
    /// 注销虚拟摄像头
    Uninstall,
    /// 自测：进程内构建 DirectShow 图（源 → NullRenderer），推流 N 秒验证帧流动
    Selftest {
        #[arg(long, default_value_t = 3)]
        seconds: u64,
    },
    /// 推送测试画面到虚拟摄像头（需先 install；另一个终端/进程运行）
    Push {
        #[arg(long, default_value_t = 1280)]
        width: u32,
        #[arg(long, default_value_t = 720)]
        height: u32,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long)]
        seconds: Option<u64>,
    },
}

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::List => list_devices(),
        Cmd::Install => {
            vdev_camera_win::register_filter()?;
            println!("已注册虚拟摄像头：vdev-camera（视频捕获源）");
            println!("下一步：在另一个终端运行 `vdev-camera-win push` 推流，然后在任意 App 摄像头列表选 vdev-camera");
            Ok(())
        }
        Cmd::Uninstall => {
            vdev_camera_win::unregister_filter()?;
            println!("已注销虚拟摄像头 vdev-camera");
            Ok(())
        }
        Cmd::Selftest { seconds } => selftest(seconds),
        Cmd::Push {
            width,
            height,
            fps,
            seconds,
        } => push(width, height, fps, seconds),
    }
}

/// 列出系统视频捕获源（走 `dshow::device` 安全封装）。
fn list_devices() -> Result<()> {
    let _com = vdev_camera_win::com::ComInit::new().context("CoInitializeEx")?;
    let names = dshow::device::list_video_capture_devices()?;
    if names.is_empty() {
        println!("（没有视频捕获源）");
        return Ok(());
    }
    for name in names {
        println!("  {name}");
    }
    Ok(())
}

/// 进程内 DirectShow 图自测（走 `dshow::selftest` 安全封装）。
fn selftest(seconds: u64) -> Result<()> {
    let _com = vdev_camera_win::com::ComInit::new().context("CoInitializeEx")?;
    let delivered = dshow::selftest::run(seconds).context("selftest graph")?;
    println!("自测完成：{seconds}s 内交付 {delivered} 帧");
    if delivered == 0 {
        bail!("没有帧被交付 —— 推流链路异常");
    }
    println!("✅ 帧从源过滤器成功流向 NullRenderer");
    Ok(())
}

/// 推流：按帧率渲染测试图案并发布到共享帧通道。
fn push(width: u32, height: u32, fps: u32, seconds: Option<u64>) -> Result<()> {
    if fps == 0 || fps > 120 {
        bail!("fps 需在 1..=120 之间");
    }
    let server = CameraServer::open().context("open camera server")?;
    let format = VideoFormat { width, height, fps };
    let mut buf = Vec::new();
    let mut t = 0.0f64;
    let interval = Duration::from_secs_f64(1.0 / fps as f64);
    let start = Instant::now();
    let mut count: u64 = 0;

    log::info!("开始推流 {width}x{height}@{fps}fps（棋盘格图案）");
    loop {
        dshow::streaming::render_pattern(&mut buf, &format, &mut t);
        server.push_frame(width, height, &buf)?;
        count += 1;
        if count.is_multiple_of(fps as u64) {
            log::info!("已推 {count} 帧 @ {fps}fps");
        }
        if let Some(s) = seconds {
            if count >= s * fps as u64 {
                break;
            }
        }
        let next = start + interval * (count as u32);
        if let Some(remain) = next.checked_duration_since(Instant::now()) {
            std::thread::sleep(remain);
        }
    }
    log::info!("推流结束，共 {count} 帧");
    Ok(())
}
