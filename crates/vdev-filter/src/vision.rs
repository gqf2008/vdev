//! Vision 人像分割 + 背景模糊/替换（feature "vision"）。
//! 用 macOS Vision.framework 的 VNGeneratePersonSegmentationRequest 生成人像 mask，
//! 再按 mask 把背景做模糊/替换，实现 Zoom 式虚拟背景。

use objc2::runtime::AnyClass;

pub type CVPixelBufferRef = *mut std::ffi::c_void;

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferLockBaseAddress(pb: CVPixelBufferRef, flags: u32) -> i32;
    fn CVPixelBufferUnlockBaseAddress(pb: CVPixelBufferRef, flags: u32) -> i32;
    fn CVPixelBufferGetBaseAddress(pb: CVPixelBufferRef) -> *mut std::ffi::c_void;
    fn CVPixelBufferGetBytesPerRow(pb: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetWidth(pb: CVPixelBufferRef) -> usize;
    fn CVPixelBufferGetHeight(pb: CVPixelBufferRef) -> usize;
    fn CFRelease(obj: *const std::ffi::c_void);
}

/// 对输入 BGRA CVPixelBuffer 生成人像分割 mask（8bit 灰度，255=人像）。
/// 内部完成 mask 的取用与释放；返回与输入同宽高的 mask Vec<u8>。
pub fn segment_person(pixel_buffer: CVPixelBufferRef) -> Option<Vec<u8>> {
    unsafe {
        let req_cls = AnyClass::get(c"VNGeneratePersonSegmentationRequest")?;
        let req: *mut objc2::runtime::AnyObject = objc2::msg_send![req_cls, init];
        if req.is_null() { return None; }
        // fast（流式，Neural Engine）
        let _: () = objc2::msg_send![&*req, setQualityLevel: 2u64];

        let handler_cls = AnyClass::get(c"VNImageRequestHandler")?;
        let handler: *mut objc2::runtime::AnyObject = objc2::msg_send![
            handler_cls,
            initWithCVPixelBuffer: pixel_buffer,
            options: std::ptr::null::<objc2::runtime::AnyObject>()
        ];
        if handler.is_null() { return None; }

        let ns_arr_cls = AnyClass::get(c"NSArray")?;
        let arr: *mut objc2::runtime::AnyObject =
            objc2::msg_send![ns_arr_cls, arrayWithObject: req];
        let mut err: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
        let ok: bool = objc2::msg_send![&*handler, performRequests: &*arr, error: &mut err];
        if !ok { return None; }

        let results: *mut objc2::runtime::AnyObject = objc2::msg_send![&*req, results];
        if results.is_null() { return None; }
        let obs: *mut objc2::runtime::AnyObject = objc2::msg_send![&*results, firstObject];
        if obs.is_null() { return None; }
        let mask: CVPixelBufferRef = objc2::msg_send![&*obs, pixelBuffer];
        if mask.is_null() { return None; }

        // 读取 mask 像素
        if CVPixelBufferLockBaseAddress(mask, 1) != 0 { return None; }
        let base = CVPixelBufferGetBaseAddress(mask);
        let stride = CVPixelBufferGetBytesPerRow(mask);
        let w = CVPixelBufferGetWidth(mask);
        let h = CVPixelBufferGetHeight(mask);
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            let src = (base as *const u8).add(y * stride);
            std::ptr::copy_nonoverlapping(src, out.as_mut_ptr().add(y * w), w);
        }
        CVPixelBufferUnlockBaseAddress(mask, 1);
        CFRelease(mask as *const std::ffi::c_void);
        Some(out)
    }
}

/// 按 mask 做背景替换/模糊。
/// - `background`：背景图（BGRA，与前景同尺寸）；None 时用盒式模糊作为"背景模糊"
/// - `blur_radius`：背景模糊半径（background=None 时生效）
pub fn apply_background(
    bgra: &mut [u8],
    width: u32,
    height: u32,
    mask: &[u8],
    background: Option<&[u8]>,
    blur_radius: u32,
) {
    let n = (width * height) as usize;
    let mn = mask.len().min(n);
    let bg: Vec<u8> = match background {
        Some(b) => b.to_vec(),
        None => box_blur(bgra, width, height, blur_radius.max(1)),
    };
    for i in 0..mn {
        let m = mask[i];
        let o = i * 4;
        if m < 128 {
            // 背景：用背景图
            bgra[o] = bg[o];
            bgra[o + 1] = bg[o + 1];
            bgra[o + 2] = bg[o + 2];
        } else if m < 250 {
            // 边缘过渡：混合
            let alpha = (m - 128) as f32 / 122.0;
            for c in 0..3 {
                let f = bgra[o + c] as f32;
                let b = bg[o + c] as f32;
                bgra[o + c] = (b * (1.0 - alpha) + f * alpha) as u8;
            }
        }
        // m >= 250：前景人像，保留原像素
    }
}

/// 盒式模糊（对整帧；用于"背景模糊"模式）
fn box_blur(bgra: &[u8], width: u32, height: u32, radius: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let r = radius as usize;
    let mut out = bgra.to_vec();
    // 水平 + 垂直分离盒式模糊（O(n) 滑动窗口）
    let mut tmp = vec![0u8; bgra.len()];
    for y in 0..h {
        for ch in 0..4 {
            let row = y * w;
            for x in 0..w {
                let l = x.saturating_sub(r);
                let rr = (x + r).min(w - 1);
                let mut acc = 0u32;
                for xx in l..=rr {
                    acc += bgra[(row + xx) * 4 + ch] as u32;
                }
                let cnt = (rr - l + 1) as u32;
                tmp[(row + x) * 4 + ch] = (acc / cnt) as u8;
            }
        }
    }
    // 垂直
    for x in 0..w {
        for ch in 0..4 {
            for y in 0..h {
                let t = y.saturating_sub(r);
                let b = (y + r).min(h - 1);
                let mut acc = 0u32;
                for yy in t..=b {
                    acc += tmp[(yy * w + x) * 4 + ch] as u32;
                }
                let cnt = (b - t + 1) as u32;
                out[(y * w + x) * 4 + ch] = (acc / cnt) as u8;
            }
        }
    }
    out
}
