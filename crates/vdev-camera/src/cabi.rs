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
