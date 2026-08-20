//! C ABI 出口：供 CMIOExtension Swift 薄壳调用 Rust 帧核心。
//!
//! 宿主 App 是 100% Rust；Swift 仅保留 Apple 强制要求的系统扩展薄壳，
//! 通过这里的 C 函数取帧（BGRA32）。

use crate::frame::{render, FramePattern};

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
