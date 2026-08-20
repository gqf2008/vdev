//! 视频文件推流：NSOpenPanel 选文件 + AVAssetReader 解码 + vImage 缩放。
use anyhow::{anyhow, Result};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::ClassType;
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderTrackOutput, AVMediaTypeVideo, AVURLAsset,
};
use objc2_foundation::{NSDictionary, NSString, NSURL};

mod ffi {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CMTime {
        pub value: i64,
        pub timescale: i32,
        pub flags: u32,
        pub epoch: i64,
    }
    #[link(name = "CoreMedia", kind = "framework")]
    extern "C" {
        pub fn CMClockGetHostTimeClock() -> *mut c_void;
        pub fn CMClockGetTime(clock: *mut c_void) -> CMTime;
        pub fn CMSampleBufferGetImageBuffer(sb: *mut c_void) -> *mut c_void;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(obj: *mut c_void);
    }
    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        pub fn CVPixelBufferLockBaseAddress(pb: *mut c_void, opts: u64);
        pub fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, opts: u64);
        pub fn CVPixelBufferGetBaseAddress(pb: *mut c_void) -> *mut c_void;
        pub fn CVPixelBufferGetBytesPerRow(pb: *mut c_void) -> usize;
        pub fn CVPixelBufferGetWidth(pb: *mut c_void) -> usize;
        pub fn CVPixelBufferGetHeight(pb: *mut c_void) -> usize;
    }
}

pub fn host_time_ns() -> u64 {
    unsafe {
        let t = ffi::CMClockGetTime(ffi::CMClockGetHostTimeClock());
        (t.value as f64 / t.timescale as f64 * 1e9) as u64
    }
}

/// 主线程弹文件选择器，返回选中视频路径（取消返回 None）。
pub fn pick_video_url() -> Option<String> {
    unsafe {
        let mtm = objc2::MainThreadMarker::new()?;
        let panel = NSOpenPanel::openPanel(mtm);
        let _: () = msg_send![&*panel, setCanChooseDirectories: false];
        let _: () = msg_send![&*panel, setAllowsMultipleSelection: false];
        let resp = panel.runModal();
        if resp == NSModalResponseOK {
            panel
                .URLs()
                .firstObject()
                .and_then(|u| u.path().map(|p| p.to_string()))
                .or_else(|| Some(String::new()))
        } else {
            None
        }
    }
}

/// 解码并推流视频（后台线程）。on_frame 回调已缩放 BGRA32。
pub fn push_video(
    path: &str,
    width: u32,
    height: u32,
    fps: u32,
    on_frame: impl FnMut(Vec<u8>, u32, u32, u32) + Send + 'static,
) -> Result<()> {
    let path = path.to_string();
    std::thread::spawn(move || {
        if let Err(e) = run(&path, width, height, fps, on_frame) {
            eprintln!("视频推流失败: {}", e);
        }
    });
    Ok(())
}

fn run(
    path: &str,
    width: u32,
    height: u32,
    fps: u32,
    mut on_frame: impl FnMut(Vec<u8>, u32, u32, u32),
) -> Result<()> {
    unsafe {
        let url = NSURL::fileURLWithPath_isDirectory_relativeToURL(&NSString::from_str(path), false, None);
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
            .ok()
            .ok_or_else(|| anyhow!("AVAssetReader 创建失败"))?;
        let track = asset
            .tracksWithMediaType(AVMediaTypeVideo.unwrap())
            .firstObject()
            .ok_or_else(|| anyhow!("没有视频轨"))?;

        // outputSettings = { kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA }
        let key = NSString::from_str("kCVPixelBufferPixelFormatTypeKey");
        let val: Retained<AnyObject> = msg_send![
            objc2_foundation::NSNumber::class(),
            numberWithUnsignedInt: 0x42475241u32
        ];
        let dict: Retained<NSDictionary<NSString>> =
            msg_send![<NSDictionary<NSString>>::class(), dictionaryWithObject: &*val, forKey: &*key];
        let output = AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
            &track,
            Some(&*dict),
        );
        reader.addOutput(&output);
        if !reader.startReading() {
            return Err(anyhow!("视频解码启动失败"));
        }

        let interval_ns = 1_000_000_000u64 / fps as u64;
        let start_ns = host_time_ns();
        let mut index: u64 = 0;

        loop {
            if crate::VIDEO_STOP.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let sample: *mut std::ffi::c_void = msg_send![&*output, copyNextSampleBuffer];
            if sample.is_null() {
                break;
            }
            let pb = ffi::CMSampleBufferGetImageBuffer(sample);
            if pb.is_null() {
                continue;
            }
            ffi::CVPixelBufferLockBaseAddress(pb, 1); // read-only
            let src_w = ffi::CVPixelBufferGetWidth(pb);
            let src_h = ffi::CVPixelBufferGetHeight(pb);
            let src_stride = ffi::CVPixelBufferGetBytesPerRow(pb);
            let base = ffi::CVPixelBufferGetBaseAddress(pb);
            let raw = if base.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(base as *const u8, src_stride * src_h).to_vec()
            };
            ffi::CVPixelBufferUnlockBaseAddress(pb, 1);

            let scaled = crate::vimage::scale_bgra(
                &raw,
                src_w,
                src_h,
                src_stride,
                width as usize,
                height as usize,
            )
            .unwrap_or((raw, src_stride));
            let target_ns = start_ns + index * interval_ns;
            index += 1;
            let now = host_time_ns();
            if target_ns > now {
                std::thread::sleep(std::time::Duration::from_nanos(target_ns - now));
            }
            on_frame(scaled.0, width, height, scaled.1 as u32);
            ffi::CFRelease(sample);
        }
        Ok(())
    }
}
