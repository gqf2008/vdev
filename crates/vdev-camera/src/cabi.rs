//! C ABI 出口：供 `CMIOExtension` Swift 薄壳调用 Rust 帧核心。
//!
//! 宿主 App 是 100% Rust；Swift 仅保留 Apple 强制要求的系统扩展薄壳，
//! 通过这里的 C 函数取帧（BGRA32）。

use crate::frame::{dims_valid, render, FramePattern};

/// 渲染一帧 BGRA32（每像素 4 字节：B,G,R,A），供 `CMIOExtension` `CVPixelBuffer` 直接填充。
///
/// 返回 0 成功；-1 图案未知；-2 尺寸非法（零尺寸/像素总数超上限）或缓冲区参数非法。
///
/// # Safety
///
/// `out` 必须指向至少 `expected`（width*height*4）字节可写内存；函数内部仅校验
/// `out` 非空与 `out_len` 下限，无法校验指针目标本身的可写性，故为 unsafe。
#[no_mangle]
pub unsafe extern "C" fn vdev_camera_render_bgra32(
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
    // 与 render 共用同一尺寸判定：零尺寸/像素总数超上限在入口拒绝（-2），
    // 不让会导致 render 内部回绕的尺寸进入渲染路径。
    if !dims_valid(width, height) {
        return -2;
    }
    // expected 乘法用 checked_mul 收口：像素上限内 64 位平台不可达，
    // 32 位平台若回绕则前置为返回码，绝不携带回绕值继续。
    let Some(expected) = (width as usize)
        .checked_mul(height as usize)
        .and_then(|px| px.checked_mul(4))
    else {
        return -2;
    };
    if out.is_null() || out_len < expected {
        return -2;
    }
    let frame = render(pattern, width, height, t);
    // SAFETY: 调用方保证 out 至少 expected 字节可写。
    unsafe {
        let dst = std::slice::from_raw_parts_mut(out, expected);
        for (i, px) in frame.data.as_chunks::<3>().0.iter().enumerate() {
            dst[i * 4] = px[2]; // B
            dst[i * 4 + 1] = px[1]; // G
            dst[i * 4 + 2] = px[0]; // R
            dst[i * 4 + 3] = 255; // A
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// C ABI 出口行为锚点：RGB24 → BGRA32 打包与错误码
    /// （cabi 重构 `as_chunks` 时防止输出语义漂移；真实 CMIO 薄壳无法单测）。
    #[test]
    fn render_bgra32_packing_and_error_codes() {
        // 图案未知 → -1
        assert_eq!(
            unsafe { vdev_camera_render_bgra32(99, 7, 10, 0.0, std::ptr::null_mut(), 0) },
            -1
        );
        // 缓冲区参数非法 → -2
        let mut buf = vec![0u8; 7 * 10 * 4];
        assert_eq!(
            unsafe { vdev_camera_render_bgra32(0, 7, 10, 0.0, std::ptr::null_mut(), buf.len()) },
            -2
        );
        assert_eq!(
            unsafe { vdev_camera_render_bgra32(0, 7, 10, 0.0, buf.as_mut_ptr(), buf.len() - 1) },
            -2
        );
        // 正常渲染：SMPTE 彩条 (7,10)，首像素为白条 (191,191,191)，底部 10% 反相
        let rc = unsafe { vdev_camera_render_bgra32(0, 7, 10, 0.0, buf.as_mut_ptr(), buf.len()) };
        assert_eq!(rc, 0);
        assert_eq!(&buf[0..4], &[191, 191, 191, 255]); // (0,0) B,G,R,A
        assert_eq!(&buf[(9 * 7) * 4..(9 * 7) * 4 + 4], &[64, 64, 64, 255]); // (0,9) 反相
    }

    /// C ABI 溢出防护回归：像素总数超上限/零尺寸一律 -2。断言传
    /// `out_len=usize::MAX` 使“缓冲区过短”不可能命中，-2 只能来自尺寸门；
    /// 该路径在解引用 `out` 之前返回，不触碰内存。
    #[test]
    fn render_rejects_oversize_and_zero_dims() {
        let mut buf = vec![0u8; 16];
        // 70000×70000：像素总数超上限（修复前 debug 溢出 panic / release 回绕）
        assert_eq!(
            unsafe {
                vdev_camera_render_bgra32(0, 70_000, 70_000, 0.0, buf.as_mut_ptr(), usize::MAX)
            },
            -2
        );
        // u32::MAX×1：u64 域像素计数拒绝，32 位平台也不会回绕
        assert_eq!(
            unsafe { vdev_camera_render_bgra32(0, u32::MAX, 1, 0.0, buf.as_mut_ptr(), usize::MAX) },
            -2
        );
        // 零尺寸仍拒绝（与既有行为一致）
        assert_eq!(
            unsafe { vdev_camera_render_bgra32(0, 0, 10, 0.0, buf.as_mut_ptr(), usize::MAX) },
            -2
        );
    }
}
