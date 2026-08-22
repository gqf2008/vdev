//! 进程内自测图安全封装：源过滤器 → NullRenderer，验证连接/推流/帧计数。
//!
//! 把所有 DirectShow 图 API（CoCreateInstance / AddFilter / ConnectDirect /
//! Run / Stop / EnumPins）的裸指针交互收敛到这里，上层（CLI）只调用
//! [`run`]，不直接写 unsafe。调用方需先 `ComInit`。

use std::sync::atomic::Ordering;
use std::time::Duration;

use anyhow::{Context, Result};
use windows::Win32::Media::DirectShow::{IBaseFilter, IGraphBuilder, IMediaControl, IPin};
use windows::Win32::Media::MediaFoundation::CLSID_FilterGraph;
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows_core::{Interface, GUID, PCWSTR};

/// 跑一次进程内图自测，返回 `seconds` 秒内交付的帧数。
pub fn run(seconds: u64) -> Result<u64> {
    let (filter, inner) = super::filter::create_filter_with_inner().context("create filter")?;

    // SAFETY: CoCreateInstance 返回 IGraphBuilder 并持有引用。
    let graph: IGraphBuilder = unsafe { CoCreateInstance(&CLSID_FilterGraph, None, CLSCTX_ALL) }
        .context("create filter graph")?;
    // SAFETY: AddFilter 增加 filter 引用；PCWSTR::null() 表示不命名。
    unsafe { graph.AddFilter(&filter, PCWSTR::null()) }.context("add source filter")?;

    // NullRenderer：丢弃样本，用于验证帧确实从源流出。
    let null_clsid = GUID::from_u128(0xc1f400a4_3f08_11d3_9f0b_006008039e37);
    // SAFETY: CoCreateInstance 返回 IBaseFilter 并持有引用。
    let null_renderer: IBaseFilter = unsafe { CoCreateInstance(&null_clsid, None, CLSCTX_ALL) }
        .context("create null renderer")?;
    // SAFETY: AddFilter 增加 null_renderer 引用。
    unsafe { graph.AddFilter(&null_renderer, PCWSTR::null()) }.context("add null renderer")?;

    let out_pin = first_pin(&filter)?.context("source has no output pin")?;
    let in_pin = first_pin(&null_renderer)?.context("null renderer has no input pin")?;

    // SAFETY: ConnectDirect 连接两个已加入图的 pin。
    unsafe { graph.ConnectDirect(&out_pin, &in_pin, None) }.context("ConnectDirect")?;
    log::info!("graph connected");

    // SAFETY: cast 返回 IMediaControl 并持有引用。
    let media_control: IMediaControl = graph.cast().context("QI IMediaControl")?;
    // SAFETY: Run 启动图（启动前图内 pin 已连接）。
    unsafe { media_control.Run() }.context("graph Run")?;
    log::info!("graph running for {seconds}s ...");

    let before = inner.frames_delivered.load(Ordering::Relaxed);
    std::thread::sleep(Duration::from_secs(seconds));
    let after = inner.frames_delivered.load(Ordering::Relaxed);

    // SAFETY: Stop 停止图。
    unsafe { media_control.Stop() }.ok();

    Ok(after - before)
}

/// 取过滤器第一个 Pin（仅用于自测图，两个过滤器各只有一个 pin）。
fn first_pin(filter: &IBaseFilter) -> Result<Option<IPin>> {
    // SAFETY: EnumPins 输出 IEnumPins 并持有引用。
    let enum_pins = unsafe { filter.EnumPins() }.context("EnumPins")?;
    let mut pins = [None];
    // SAFETY: Next 写入 pins 槽位（引用计数由调用方接管）。
    let _ = unsafe { enum_pins.Next(&mut pins, None) };
    Ok(pins[0].clone())
}
