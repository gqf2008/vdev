//! vImage 缩放（Accelerate C API），BGRA 保序。
use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct VImageBuffer {
    pub data: *mut c_void,
    pub height: usize,
    pub width: usize,
    pub row_bytes: usize,
}

#[link(name = "Accelerate", kind = "framework")]
extern "C" {
    fn vImageScale_ARGB8888(
        src: *const VImageBuffer,
        dest: *mut VImageBuffer,
        back_color: *const c_void,
        flags: u32,
    ) -> i32;
}

/// 把 BGRA32 缩放到目标尺寸，返回 (data, stride)。
pub fn scale_bgra(
    data: &[u8],
    src_w: usize,
    src_h: usize,
    src_stride: usize,
    dst_w: usize,
    dst_h: usize,
) -> Option<(Vec<u8>, usize)> {
    if src_w == dst_w && src_h == dst_h {
        return Some((data.to_vec(), src_stride));
    }
    let dst_stride = dst_w * 4;
    let mut dst = vec![0u8; dst_stride * dst_h];
    let src = VImageBuffer {
        data: data.as_ptr() as *mut c_void,
        height: src_h,
        width: src_w,
        row_bytes: src_stride,
    };
    let mut dst_buf = VImageBuffer {
        data: dst.as_mut_ptr() as *mut c_void,
        height: dst_h,
        width: dst_w,
        row_bytes: dst_stride,
    };
    let rc = unsafe { vImageScale_ARGB8888(&src, &mut dst_buf, std::ptr::null(), 0) };
    if rc != 0 {
        return None;
    }
    Some((dst, dst_stride))
}
