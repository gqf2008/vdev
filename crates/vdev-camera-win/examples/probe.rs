//! 32 位诊断探针：通过 CoCreateInstance 加载真实 DLL，逐步执行 VLC/ffmpeg
//! 打开 dshow 设备的核心调用，崩溃即定位到具体接口/步骤。
//!
//! 用法（32 位）：cargo run --release --target i686-pc-windows-msvc --example probe

use windows::Win32::Media::DirectShow::{IAMStreamConfig, IBaseFilter, IEnumPins, PIN_INFO};
use windows::Win32::Media::KernelStreaming::IKsPropertySet;
use windows::Win32::Media::MediaFoundation::{
    AMPROPSETID_Pin, PIN_CATEGORY_CAPTURE, VIDEOINFOHEADER,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows_core::{Interface, GUID};

fn main() {
    let _com = vdev_camera_win::com::ComInit::new().expect("CoInit");
    println!("[1] COM init OK");

    // CoCreateInstance 走 32 位 DLL（WOW6432Node 注册）。
    let filter: IBaseFilter = unsafe {
        CoCreateInstance(
            &vdev_camera_win::CLSID_VirtualCameraFilter,
            None,
            CLSCTX_ALL,
        )
    }
    .expect("CoCreateInstance filter");
    println!("[2] CoCreateInstance filter OK");

    let enum_pins: IEnumPins = unsafe { filter.EnumPins() }.expect("EnumPins");
    println!("[3] EnumPins OK");
    let mut pins = [None];
    let hr = unsafe { enum_pins.Next(&mut pins, None) };
    println!("[4] EnumPins::Next hr={hr:?} got={}", pins[0].is_some());
    let Some(pin) = pins[0].take() else {
        println!("[5] no pins");
        return;
    };

    let mut info = PIN_INFO::default();
    let _ = unsafe { pin.QueryPinInfo(&mut info) };
    println!(
        "[5] QueryPinInfo dir={:?} pFilter={}",
        info.dir,
        info.pFilter.is_some()
    );

    match pin.cast::<IAMStreamConfig>() {
        Ok(cfg) => {
            println!("[7] QI IAMStreamConfig OK");
            let (mut n, mut size) = (0, 0);
            unsafe { cfg.GetNumberOfCapabilities(&mut n, &mut size) }.expect("GetNumber");
            println!("[8] GetNumberOfCapabilities n={n} size={size}");
            for i in 0..n {
                let mut pmt = std::ptr::null_mut();
                let mut caps = vec![0u8; size as usize];
                unsafe { cfg.GetStreamCaps(i, &mut pmt, caps.as_mut_ptr().cast()) }
                    .expect("GetStreamCaps");
                if !pmt.is_null() {
                    // SAFETY: pmt 为 CoTaskMem 分配的 AM_MEDIA_TYPE。
                    let mt = unsafe { &*pmt };
                    let fmt = if mt.pbFormat.is_null() {
                        "no-format".to_string()
                    } else {
                        // SAFETY: pbFormat 指向 VIDEOINFOHEADER。
                        let vih = unsafe { &*(mt.pbFormat.cast::<VIDEOINFOHEADER>()) };
                        format!(
                            "{}x{} fps={}",
                            vih.bmiHeader.biWidth,
                            vih.bmiHeader.biHeight.abs(),
                            10_000_000 / vih.AvgTimePerFrame.max(1)
                        )
                    };
                    println!(
                        "[9] GetStreamCaps[{i}] major={:?} sub={:?} fmt={:?} lSample={} cbFmt={} pbFmt={} {fmt}",
                        mt.majortype, mt.subtype, mt.formattype, mt.lSampleSize, mt.cbFormat, mt.pbFormat.is_null()
                    );
                    // SAFETY: 释放 CoTaskMem 媒体类型。
                    unsafe { vdev_camera_win::dshow::media_type::free_media_type_ptr(pmt) };
                }
            }
        }
        Err(e) => println!("[7] QI IAMStreamConfig FAIL {e:?}"),
    }

    match pin.cast::<IKsPropertySet>() {
        Ok(ks) => {
            println!("[10] QI IKsPropertySet OK");
            let mut cat = GUID::zeroed();
            let mut returned = 0u32;
            let hr = unsafe {
                ks.Get(
                    &AMPROPSETID_Pin,
                    0, // AMPROPERTY_PIN_CATEGORY
                    std::ptr::null(),
                    0,
                    (&mut cat as *mut GUID).cast(),
                    std::mem::size_of::<GUID>() as u32,
                    &mut returned,
                )
            };
            println!(
                "[11] PinCategory hr={hr:?} returned={returned} is_capture={}",
                hr.is_ok() && cat == PIN_CATEGORY_CAPTURE
            );
        }
        Err(e) => println!("[10] QI IKsPropertySet FAIL {e:?}"),
    }

    println!("[12] probe done");
}
