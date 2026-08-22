//! `AM_MEDIA_TYPE` / `VIDEOINFOHEADER` 安全封装。
//!
//! DirectShow 约定：`AM_MEDIA_TYPE` 结构体与其 `pbFormat` 都通过 `CoTaskMemAlloc`
//! 分配，由接收方用 `CoTaskMemFree` 释放（C++ 侧 `DeleteMediaType` / `FreeMediaType`）。
//! 本模块把这些分配/释放收敛到 `unsafe` 边界内，上层只操作 [`VideoFormat`]。

use std::ffi::c_void;
use std::mem::{size_of, ManuallyDrop};
use std::ptr;

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::BITMAPINFOHEADER;
use windows::Win32::Media::MediaFoundation::{
    FORMAT_VideoInfo, MEDIATYPE_Video, AM_MEDIA_TYPE, VIDEOINFOHEADER,
};
use windows::Win32::System::Com::{CoTaskMemAlloc, CoTaskMemFree};
use windows_core::GUID;

/// YUY2（YUV 4:2:2 打包）媒体子类型。
///
/// 选择 YUY2 而非 RGB32：DirectShow 摄像头生态以 YUV 为主，VLC 3.0 等消费方
/// 无法从 RGB32（BI_RGB）媒体类型提取 fourcc（报 unsupported format），
/// 而 YUY2 是标准摄像头格式、兼容性最好（OBS Virtual Camera 用 NV12 同理）。
pub const MEDIASUBTYPE_YUY2: GUID = GUID::from_u128(0x3259_5559_0000_0010_8000_00aa_0038_9b71);

/// 一种支持输出格式：YUY2 @ 指定分辨率/帧率。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

impl VideoFormat {
    /// YUY2 输出帧字节数（每像素 2 字节）。
    pub fn frame_size(&self) -> usize {
        self.width as usize * self.height as usize * 2
    }

    /// 构建 `VIDEOINFOHEADER`（顶到下，biHeight 为负，与 BGRA 内存布局一致）。
    pub fn to_video_info_header(&self) -> VIDEOINFOHEADER {
        let w = self.width as i32;
        let h = self.height as i32;
        VIDEOINFOHEADER {
            rcSource: RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            },
            rcTarget: RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            },
            dwBitRate: self.width * self.height * 16 * self.fps,
            dwBitErrorRate: 0,
            AvgTimePerFrame: 10_000_000 / self.fps as i64,
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: w,
                // 正 biHeight：OBS/libdshowcapture 同款。VLC/ffmpeg 对 YUV 直接按
                // 数据顺序显示（不因 biHeight 翻转），负值反而会让 VLC 的
                // fmt.video.i_height（unsigned）溢出为 4294966216 导致无 ES。
                biHeight: h,
                biPlanes: 1,
                biBitCount: 16,
                biCompression: 0x3259_5559, // MAKEFOURCC('Y','U','Y','2')
                biSizeImage: self.frame_size() as u32,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
        }
    }

    /// 构建完整 `AM_MEDIA_TYPE`（`pbFormat` 为 CoTaskMem 分配；用后须 [`free_format`] 释放）。
    pub fn to_media_type(&self) -> AM_MEDIA_TYPE {
        let vih = self.to_video_info_header();
        let cb_format = size_of::<VIDEOINFOHEADER>();
        // SAFETY: CoTaskMemAlloc 返回有效内存；ptr::write 初始化之。
        let pb_format = unsafe { CoTaskMemAlloc(cb_format) }.cast::<VIDEOINFOHEADER>();
        if !pb_format.is_null() {
            // SAFETY: pb_format 指向至少 cb_format 字节的可写内存。
            unsafe { ptr::write(pb_format, vih) };
        }
        AM_MEDIA_TYPE {
            majortype: MEDIATYPE_Video,
            subtype: MEDIASUBTYPE_YUY2,
            bFixedSizeSamples: true.into(),
            bTemporalCompression: false.into(),
            lSampleSize: self.frame_size() as u32,
            formattype: FORMAT_VideoInfo,
            pUnk: ManuallyDrop::new(None),
            cbFormat: cb_format as u32,
            pbFormat: pb_format.cast::<u8>(),
        }
    }
}

/// 支持的输出格式列表（`IAMStreamConfig` 能力枚举）。
pub const FORMATS: [VideoFormat; 3] = [
    VideoFormat {
        width: 1920,
        height: 1080,
        fps: 30,
    },
    VideoFormat {
        width: 1280,
        height: 720,
        fps: 30,
    },
    VideoFormat {
        width: 640,
        height: 480,
        fps: 30,
    },
];

/// 校验 `AM_MEDIA_TYPE` 是否为受支持的 YUY2 视频格式，返回对应 [`VideoFormat`]。
pub fn media_type_matches(mt: &AM_MEDIA_TYPE) -> Option<VideoFormat> {
    if mt.majortype != MEDIATYPE_Video || mt.subtype != MEDIASUBTYPE_YUY2 {
        return None;
    }
    if mt.formattype != FORMAT_VideoInfo
        || mt.pbFormat.is_null()
        || mt.cbFormat < size_of::<VIDEOINFOHEADER>() as u32
    {
        return None;
    }
    // SAFETY: cbFormat 已确认不小于 VIDEOINFOHEADER，pbFormat 非空。
    let vih = unsafe { &*(mt.pbFormat.cast::<VIDEOINFOHEADER>()) };
    let w = vih.bmiHeader.biWidth;
    let h = vih.bmiHeader.biHeight.abs();
    if w <= 0 || h <= 0 {
        return None;
    }
    let fps = if vih.AvgTimePerFrame > 0 {
        (10_000_000 / vih.AvgTimePerFrame) as u32
    } else {
        30
    };
    let f = VideoFormat {
        width: w as u32,
        height: h as u32,
        fps: fps.clamp(1, 120),
    };
    FORMATS.iter().find(|x| **x == f).copied()
}

/// 释放 `AM_MEDIA_TYPE.pbFormat`（不释放结构体本身；栈上的 `AM_MEDIA_TYPE` 用它）。
pub fn free_format(mt: &AM_MEDIA_TYPE) {
    if !mt.pbFormat.is_null() {
        // SAFETY: pbFormat 由 CoTaskMemAlloc 分配。
        unsafe { CoTaskMemFree(Some(mt.pbFormat.cast::<c_void>())) };
    }
}

/// 深拷贝 `AM_MEDIA_TYPE` 到 CoTaskMem 内存并返回指针。
///
/// 调用方用 [`free_media_type_ptr`]（C++ 语义 `DeleteMediaType`）释放。
///
/// # Safety
/// 调用方必须保证返回指针最终被 [`free_media_type_ptr`] 释放，且 `mt` 有效。
pub unsafe fn alloc_media_type_copy(mt: &AM_MEDIA_TYPE) -> *mut AM_MEDIA_TYPE {
    // SAFETY: 由调用方保证返回值被正确释放。
    let p = unsafe { CoTaskMemAlloc(size_of::<AM_MEDIA_TYPE>()) }.cast::<AM_MEDIA_TYPE>();
    if p.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: p 指向已分配内存。
    unsafe {
        ptr::write(
            p,
            AM_MEDIA_TYPE {
                majortype: mt.majortype,
                subtype: mt.subtype,
                bFixedSizeSamples: mt.bFixedSizeSamples,
                bTemporalCompression: mt.bTemporalCompression,
                lSampleSize: mt.lSampleSize,
                formattype: mt.formattype,
                pUnk: ManuallyDrop::new(None),
                cbFormat: mt.cbFormat,
                pbFormat: ptr::null_mut(),
            },
        );
        if !mt.pbFormat.is_null() && mt.cbFormat > 0 {
            let pb = CoTaskMemAlloc(mt.cbFormat as usize).cast::<u8>();
            if !pb.is_null() {
                ptr::copy_nonoverlapping(mt.pbFormat, pb, mt.cbFormat as usize);
                (*p).pbFormat = pb;
            }
        }
    }
    p
}

/// 释放 [`alloc_media_type_copy`] 返回的 `AM_MEDIA_TYPE`（结构体 + pbFormat）。
///
/// # Safety
/// `p` 必须是由 [`alloc_media_type_copy`] 分配的指针（或 null）。
pub unsafe fn free_media_type_ptr(p: *mut AM_MEDIA_TYPE) {
    if p.is_null() {
        return;
    }
    // SAFETY: 调用方保证 p 是 CoTaskMem 分配的 AM_MEDIA_TYPE。
    unsafe {
        free_format(&*p);
        CoTaskMemFree(Some(p.cast::<c_void>()));
    }
}
