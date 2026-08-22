//! vdev-camera-win — Windows 虚拟摄像头 CLI。

//!

//! 子命令：install / uninstall / selftest / push。

use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

use clap::{Parser, Subcommand};

use windows::Win32::Media::DirectShow::{
    IBaseFilter, ICreateDevEnum, IGraphBuilder, IMediaControl,
};

use windows::Win32::Media::MediaFoundation::{
    CLSID_FilterGraph, CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory,
};

use windows::Win32::System::Com::{CoCreateInstance, IEnumMoniker, IErrorLog, CLSCTX_ALL};

use windows::Win32::System::Com::StructuredStorage::IPropertyBag;

use windows::Win32::System::Variant::VARIANT;

use windows_core::{Interface, GUID, PCWSTR};

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

/// 枚举视频捕获源（ICreateDevEnum + CLSID_VideoInputDeviceCategory）。
fn list_devices() -> Result<()> {
    let _com = vdev_camera_win::com::ComInit::new().context("CoInitializeEx")?;

    let dev_enum: ICreateDevEnum =
        unsafe { CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_ALL) }
            .context("create system device enum")?;

    let mut moniker_enum: Option<IEnumMoniker> = None;

    unsafe {
        dev_enum.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut moniker_enum, 0)
    }
    .context("CreateClassEnumerator")?;

    let Some(moniker_enum) = moniker_enum else {
        println!("（没有视频捕获源）");

        return Ok(());
    };

    loop {
        let mut monikers = [None];

        let hr = unsafe { moniker_enum.Next(&mut monikers, None) };

        if hr != windows::Win32::Foundation::S_OK {
            break;
        }

        let Some(moniker) = monikers[0].take() else {
            break;
        };

        // FriendlyName 存在属性袋（IPropertyBag）里。

        let bag: IPropertyBag =
            match unsafe { moniker.BindToStorage::<_, _, IPropertyBag>(None, None) } {
                Ok(b) => b,

                Err(_) => continue,
            };

        let name_wide: Vec<u16> = "FriendlyName"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        let mut var = VARIANT::default();

        let hr = unsafe {
            bag.Read(
                PCWSTR(name_wide.as_ptr()),
                &mut var,
                Option::<&IErrorLog>::None,
            )
        };

        if hr.is_ok() {
            // VT_BSTR：var.Anonymous.Anonymous.Anonymous.bstrVal

            let bstr = unsafe { &var.Anonymous.Anonymous.Anonymous.bstrVal };

            let name = bstr.to_string();
            println!("  {name}");
        }
    }

    Ok(())
}

/// 进程内 DirectShow 图自测：我们的源过滤器 → NullRenderer，验证连接/推流/帧计数。
fn selftest(seconds: u64) -> Result<()> {
    let _com = vdev_camera_win::com::ComInit::new().context("CoInitializeEx")?;

    let (filter, inner) = dshow::filter::create_filter_with_inner().context("create filter")?;

    let graph: IGraphBuilder = unsafe { CoCreateInstance(&CLSID_FilterGraph, None, CLSCTX_ALL) }
        .context("create filter graph")?;

    unsafe { graph.AddFilter(&filter, PCWSTR::null()) }.context("add source filter")?;

    // NullRenderer：丢弃样本，用于验证帧确实从源流出。

    let null_clsid = GUID::from_u128(0xc1f400a4_3f08_11d3_9f0b_006008039e37);

    let null_renderer: IBaseFilter = unsafe { CoCreateInstance(&null_clsid, None, CLSCTX_ALL) }
        .context("create null renderer")?;

    unsafe { graph.AddFilter(&null_renderer, PCWSTR::null()) }.context("add null renderer")?;

    let out_pin = first_pin(&filter)?.context("source has no output pin")?;

    let in_pin = first_pin(&null_renderer)?.context("null renderer has no input pin")?;

    unsafe { graph.ConnectDirect(&out_pin, &in_pin, None) }.context("ConnectDirect")?;

    log::info!("graph connected");

    let media_control: IMediaControl = graph.cast().context("QI IMediaControl")?;

    unsafe { media_control.Run() }.context("graph Run")?;

    log::info!("graph running for {seconds}s ...");

    let before = inner
        .frames_delivered
        .load(std::sync::atomic::Ordering::Relaxed);

    std::thread::sleep(Duration::from_secs(seconds));

    let after = inner
        .frames_delivered
        .load(std::sync::atomic::Ordering::Relaxed);

    unsafe { media_control.Stop() }.ok();

    let delivered = after - before;

    println!("自测完成：{seconds}s 内交付 {delivered} 帧");

    if delivered == 0 {
        bail!("没有帧被交付 —— 推流链路异常");
    }

    println!("✅ 帧从源过滤器成功流向 NullRenderer");

    Ok(())
}

/// 取过滤器第一个 Pin。
fn first_pin(filter: &IBaseFilter) -> Result<Option<windows::Win32::Media::DirectShow::IPin>> {
    let enum_pins = unsafe { filter.EnumPins() }.context("EnumPins")?;

    let mut pins = [None];

    let _ = unsafe { enum_pins.Next(&mut pins, None) };

    Ok(pins[0].clone())
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
