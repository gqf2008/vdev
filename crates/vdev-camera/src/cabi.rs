//! C ABI 出口：供 DAL 插件薄壳（ObjC++）调用 Rust 帧核心。
//!
//! 未来 `dal/` 下的 CMIOHardwarePlugIn 通过这里拿帧，做到
//! 「插件壳是 ObjC++，逻辑全在 Rust」。

use crate::frame::{render, FramePattern};

/// 渲染一帧到 `out`（RGB24，长度必须 >= width*height*3）。
///
/// 返回 0 成功；-1 图案未知；-2 缓冲区参数非法。
#[no_mangle]
pub extern "C" fn vdev_camera_render_frame(
    pattern: i32,
    width: u32,
    height: u32,
    t: f64,
    out: *mut u8,
    out_len: usize,
) -> i32 {
    let Some(pattern) = FramePattern::from_i32(pattern) else {
        return -1;
    };
    if width == 0 || height == 0 {
        return -2;
    }
    let expected = (width as usize) * (height as usize) * 3;
    if out.is_null() || out_len < expected {
        return -2;
    }
    let frame = render(pattern, width, height, t);
    // SAFETY: 调用方保证 out 至少 expected 字节可写。
    unsafe {
        std::ptr::copy_nonoverlapping(frame.data.as_ptr(), out, expected);
    }
    0
}

/// 渲染一帧 ARGB32（每像素 4 字节：A,R,G,B），供 DAL 插件 CVPixelBuffer 直接填充。
///
/// 返回 0 成功；-1 图案未知；-2 缓冲区参数非法。
#[no_mangle]
pub extern "C" fn vdev_camera_render_argb32(
    pattern: i32,
    width: u32,
    height: u32,
    t: f64,
    out: *mut u8,
    out_len: usize,
) -> i32 {
    let Some(pattern) = FramePattern::from_i32(pattern) else {
        return -1;
    };
    if width == 0 || height == 0 {
        return -2;
    }
    let expected = (width as usize) * (height as usize) * 4;
    if out.is_null() || out_len < expected {
        return -2;
    }
    let frame = render(pattern, width, height, t);
    // SAFETY: 调用方保证 out 至少 expected 字节可写。
    unsafe {
        let dst = std::slice::from_raw_parts_mut(out, expected);
        for (i, px) in frame.data.chunks_exact(3).enumerate() {
            dst[i * 4] = 255; // A
            dst[i * 4 + 1] = px[0]; // R
            dst[i * 4 + 2] = px[1]; // G
            dst[i * 4 + 3] = px[2]; // B
        }
    }
    0
}

/// 渲染一帧 BGRA32（每像素 4 字节：B,G,R,A），供 CMIOExtension CVPixelBuffer 直接填充。
///
/// 返回 0 成功；-1 图案未知；-2 缓冲区参数非法。
#[no_mangle]
pub extern "C" fn vdev_camera_render_bgra32(
    pattern: i32,
    width: u32,
    height: u32,
    t: f64,
    out: *mut u8,
    out_len: usize,
) -> i32 {
    let Some(pattern) = FramePattern::from_i32(pattern) else {
        return -1;
    };
    if width == 0 || height == 0 {
        return -2;
    }
    let expected = (width as usize) * (height as usize) * 4;
    if out.is_null() || out_len < expected {
        return -2;
    }
    let frame = render(pattern, width, height, t);
    // SAFETY: 调用方保证 out 至少 expected 字节可写。
    unsafe {
        let dst = std::slice::from_raw_parts_mut(out, expected);
        for (i, px) in frame.data.chunks_exact(3).enumerate() {
            dst[i * 4] = px[2]; // B
            dst[i * 4 + 1] = px[1]; // G
            dst[i * 4 + 2] = px[0]; // R
            dst[i * 4 + 3] = 255; // A
        }
    }
    0
}
