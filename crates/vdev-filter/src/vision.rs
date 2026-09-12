//! Vision 人像分割 + 背景模糊/替换（feature "vision"）。
//! 用 macOS Vision.framework 的 `VNGeneratePersonSegmentationRequest` 生成人像 mask，
//! 再按 mask 把背景做模糊/替换，实现 Zoom 式虚拟背景。

use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};

pub type CVPixelBufferRef = *mut std::ffi::c_void;

/// `CVPixelBufferRef` 的 `ObjC` type-encoding 包装（^{__`CVBuffer`=}）。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CvBufferRef(pub CVPixelBufferRef);
unsafe impl objc2::encode::Encode for CvBufferRef {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("__CVBuffer", &[]));
}

// 链接 Vision.framework（否则 AnyClass::get 找不到 Vision 的类）
#[link(name = "Vision", kind = "framework")]
extern "C" {
    fn _vdev_vision_link_anchor();
}

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

/// 对输入 BGRA `CVPixelBuffer` 生成人像分割 mask（8bit 灰度，255=人像）。
/// 内部完成 mask 的取用与释放；返回与输入同宽高的 mask Vec<u8>。
// 复用 VNGeneratePersonSegmentationRequest（stateful，避免每帧 alloc + 提升时序稳定）。
// M3：VNRequest 复用非线程安全（并发 performRequests 会竞争内部状态、互相替换
// results），故包一层 Mutex 串行化；segment_person 全程持锁（含 results/mask
// 读取——mask buffer 生命周期挂在 observation 上，results 被并发替换会 use-after-free）。
static REQ: std::sync::OnceLock<std::sync::Mutex<usize>> = std::sync::OnceLock::new();
// 上次分割 mask 缓存 + 帧计数（每 3 帧分割一次，中间复用，人像 mask 慢变化）
static LAST_MASK: std::sync::Mutex<Option<(Vec<u8>, Vec<u8>)>> = std::sync::Mutex::new(None); // (mask, blur_bg)
static FRAME_COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// 取全局 request 并持锁；锁随返回的 guard 存活，持有期间独占该 request。
/// 以 `usize` 存指针，避免裸指针进 static 的 `Send`/`Sync` 问题。
fn get_req() -> std::sync::MutexGuard<'static, usize> {
    REQ.get_or_init(|| {
        let req_cls = AnyClass::get(c"VNGeneratePersonSegmentationRequest").unwrap();
        let req: *mut AnyObject = unsafe { objc2::msg_send![req_cls, new] };
        // SAFETY：req 非空时指向 request 实例，引用提升后再发消息
        let req_ref: &AnyObject = unsafe { &*req };
        unsafe {
            let _: () = objc2::msg_send![req_ref, setQualityLevel: 2u64];
        }
        std::sync::Mutex::new(req as usize)
    })
    .lock()
    .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub fn segment_person(pixel_buffer: CVPixelBufferRef) -> Option<(Vec<u8>, usize, usize)> {
    // M3：guard 存活至函数结束——request 的使用（含 results/mask 读取）全程独占
    let req_guard = get_req();
    let req = *req_guard as *mut AnyObject;
    if req.is_null() {
        return None;
    }

    let handler_cls = AnyClass::get(c"VNImageRequestHandler")?;
    let opts_cls = AnyClass::get(c"NSDictionary");
    let opts: *mut AnyObject = if let Some(c) = opts_cls {
        unsafe { objc2::msg_send![c, dictionary] }
    } else {
        std::ptr::null_mut()
    };
    // M1：alloc 返回 +1，init 消耗 alloc 并返回 +1，交 `Retained` 接管——函数退出
    //（含下方各早退路径）时 Drop release。修复前 handler 从不 release，背景模糊
    // 开启时每 5 帧泄漏一个 ObjC 对象。泄漏/释放配平无法在单测中断言（需
    // Instruments/ObjC 运行时泄漏检测），由 `Retained` 的 Drop 语义保证正常路径成对。
    // 注：若 init 抛 ObjC 异常，alloc 所得对象无从释放（catch 恢复后指针不可再用），
    // 该极端路径保持原行为。
    let handler: Option<Retained<AnyObject>> =
        objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            let h: *mut AnyObject = unsafe { objc2::msg_send![handler_cls, alloc] };
            let handler: *mut AnyObject = unsafe {
                objc2::msg_send![
                    h,
                    initWithCVPixelBuffer: CvBufferRef(pixel_buffer),
                    options: opts
                ]
            };
            // SAFETY：init 返回 +1 对象（返回 nil 时得 None，不释放任何指针）
            unsafe { Retained::from_raw(handler) }
        }))
        .unwrap_or_default();
    let handler = handler?;

    let ns_arr_cls = AnyClass::get(c"NSArray")?;
    let arr: *mut AnyObject = unsafe { objc2::msg_send![ns_arr_cls, arrayWithObject: req] };
    let mut err: *mut AnyObject = std::ptr::null_mut();
    // SAFETY：arr 为有效 ObjC 对象，引用提升后再发消息（handler 已由 Retained 持有）
    let arr_ref: &AnyObject = unsafe { &*arr };
    let ok: bool = objc2::exception::catch(std::panic::AssertUnwindSafe(|| unsafe {
        objc2::msg_send![&*handler, performRequests: arr_ref, error: &mut err]
    }))
    .unwrap_or(false);
    if !ok {
        return None;
    }

    // SAFETY：req 指向有效 request 实例（REQ 锁内独占），引用提升后再发消息
    let req_ref: &AnyObject = unsafe { &*req };
    let results: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![req_ref, results] };
    if results.is_null() {
        return None;
    }
    let results_ref: &objc2::runtime::AnyObject = unsafe { &*results };
    let obs: *mut objc2::runtime::AnyObject = unsafe { objc2::msg_send![results_ref, firstObject] };
    if obs.is_null() {
        return None;
    }
    let obs_ref: &objc2::runtime::AnyObject = unsafe { &*obs };
    let mask: CvBufferRef = unsafe { objc2::msg_send![obs_ref, pixelBuffer] };
    let mask: CVPixelBufferRef = mask.0;
    if mask.is_null() {
        return None;
    }

    // 读取 mask 像素
    if unsafe { CVPixelBufferLockBaseAddress(mask, 1) } != 0 {
        return None;
    }
    let base = unsafe { CVPixelBufferGetBaseAddress(mask) };
    let stride = unsafe { CVPixelBufferGetBytesPerRow(mask) };
    let w = unsafe { CVPixelBufferGetWidth(mask) };
    let h = unsafe { CVPixelBufferGetHeight(mask) };
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        // SAFETY: y < h，行跨度来自 mask 自身描述，src/dst 均不越界
        let src = unsafe { (base as *const u8).add(y * stride) };
        let dst = unsafe { out.as_mut_ptr().add(y * w) };
        unsafe { std::ptr::copy_nonoverlapping(src, dst, w) };
    }
    unsafe { CVPixelBufferUnlockBaseAddress(mask, 1) };
    // 严禁 CFRelease(mask)：-[VNPixelBufferObservation pixelBuffer] 返回 +0
    // （非 owning），buffer 由 observation 独占持有；此处多释放一次，全局 REQ
    // 复用替换 results 时旧 observation dealloc 再释放 → over-release 崩溃
    // （VDEV_BG=blur 实证 SIGTRAP）。mask 数据已拷入 out，无需另行持有。
    Some((out, w, h))
}

/// 按 mask 做背景替换/模糊。
/// - `background`：背景图（BGRA，与前景同尺寸）；None 时用盒式模糊作为"背景模糊"
/// - `blur_radius`：背景模糊半径（background=None 时生效）
///
/// M2：`background` 过短（`< width * height * 4`）时索引会越界，入口校验后
/// 保持原帧直接返回（等价"无法应用背景"），不再 panic。
pub fn apply_background(
    bgra: &mut [u8],
    width: u32,
    height: u32,
    mask: &[u8],
    background: Option<&[u8]>,
    blur_radius: u32,
) {
    // 尺寸算术第一操作数即转 usize 域，杜绝 u32 乘法回绕（debug 下会 panic）
    let n = width as usize * height as usize;
    let mn = mask.len().min(n);
    let bg: Vec<u8> = match background {
        Some(b) => {
            // M2 回归守护：循环内 bg[o..o+2] 索引上限为 n*4-1，背景过短时
            // 修复前 panic；此处校验不足则不动帧（mask/帧有效性由调用方保证）。
            // 用除法改写 `b.len() < n * 4`，避免极端尺寸下 usize 域 n*4 再回绕。
            if n > b.len() / 4 {
                return;
            }
            b.to_vec()
        }
        None => box_blur(bgra, width, height, blur_radius.max(1)),
    };
    for (i, &m) in mask.iter().enumerate().take(mn) {
        let o = i * 4;
        if m < 128 {
            // 背景：用背景图
            bgra[o] = bg[o];
            bgra[o + 1] = bg[o + 1];
            bgra[o + 2] = bg[o + 2];
        } else if m < 250 {
            // 边缘过渡：混合
            let alpha = f32::from(m - 128) / 122.0;
            for c in 0..3 {
                let f = f32::from(bgra[o + c]);
                let b = f32::from(bg[o + c]);
                bgra[o + c] = (b * (1.0 - alpha) + f * alpha) as u8;
            }
        }
        // m >= 250：前景人像，保留原像素
    }
}

/// 盒式模糊（分离 + 前缀和 O(1)/像素；用于"背景模糊"模式）
fn box_blur(bgra: &[u8], width: u32, height: u32, radius: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let r = radius as usize;
    let mut tmp = vec![0u8; bgra.len()];
    // 水平前缀和
    for y in 0..h {
        for ch in 0..4 {
            let mut prefix = vec![0u32; w + 1];
            for x in 0..w {
                prefix[x + 1] = prefix[x] + u32::from(bgra[(y * w + x) * 4 + ch]);
            }
            for x in 0..w {
                let l = x.saturating_sub(r);
                let rr = (x + r).min(w - 1);
                let sum = prefix[rr + 1] - prefix[l];
                tmp[(y * w + x) * 4 + ch] = (sum / (rr - l + 1) as u32) as u8;
            }
        }
    }
    // 垂直前缀和
    let mut out = vec![0u8; bgra.len()];
    for x in 0..w {
        for ch in 0..4 {
            let mut prefix = vec![0u32; h + 1];
            for y in 0..h {
                prefix[y + 1] = prefix[y] + u32::from(tmp[(y * w + x) * 4 + ch]);
            }
            for y in 0..h {
                let t = y.saturating_sub(r);
                let b = (y + r).min(h - 1);
                let sum = prefix[b + 1] - prefix[t];
                out[(y * w + x) * 4 + ch] = (sum / (b - t + 1) as u32) as u8;
            }
        }
    }
    out
}

/// 创建 BGRA CVPixelBuffer（测试/集成用）
#[must_use]
pub fn create_bgra_buffer(width: usize, height: usize) -> CVPixelBufferRef {
    unsafe extern "C" {
        fn CVPixelBufferCreate(
            allocator: *const std::ffi::c_void,
            width: usize,
            height: usize,
            pixel_format: u32,
            attrs: *const std::ffi::c_void,
            out: *mut CVPixelBufferRef,
        ) -> i32;
    }
    const BGRA: u32 = 0x4247_5241; // kCVPixelFormatType_32BGRA
    let mut pb: CVPixelBufferRef = std::ptr::null_mut();
    unsafe {
        let rc = CVPixelBufferCreate(
            std::ptr::null(),
            width,
            height,
            BGRA,
            std::ptr::null(),
            &raw mut pb,
        );
        if rc != 0 {
            return std::ptr::null_mut();
        }
    }
    pb
}

/// 把 BGRA 帧写入 CVPixelBuffer（供 Vision 输入）
#[must_use]
pub fn bgra_to_cvpixelbuffer(bgra: &[u8], width: usize, height: usize) -> Option<CVPixelBufferRef> {
    let pb = create_bgra_buffer(width, height);
    if pb.is_null() {
        return None;
    }
    if unsafe { CVPixelBufferLockBaseAddress(pb, 0) } != 0 {
        unsafe { CFRelease(pb.cast_const()) };
        return None;
    }
    let base = unsafe { CVPixelBufferGetBaseAddress(pb) }.cast::<u8>();
    let stride = unsafe { CVPixelBufferGetBytesPerRow(pb) };
    for y in 0..height {
        // SAFETY: 行数/跨度来自 CVPixelBuffer 自身描述，源按帧宽取址，均不越界
        let dst = unsafe { base.add(y * stride) };
        let src = unsafe { bgra.as_ptr().add(y * width * 4) };
        unsafe { std::ptr::copy_nonoverlapping(src, dst, width * 4) };
    }
    unsafe { CVPixelBufferUnlockBaseAddress(pb, 0) };
    Some(pb)
}

/// BGRA 双线性缩放（供降分辨率分割用）
#[must_use]
pub fn resize_bgra(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh * 4];
    if sw == dw && sh == dh {
        out.copy_from_slice(&src[..dw * dh * 4]);
        return out;
    }
    for y in 0..dh {
        // 源坐标（中心对齐）
        let sy = ((y as f64 + 0.5) * sh as f64 / dh as f64 - 0.5).clamp(0.0, sh as f64 - 1.0);
        let sy0 = sy.floor() as usize;
        let sy1 = (sy0 + 1).min(sh - 1);
        let fy = sy - sy0 as f64;
        for x in 0..dw {
            let sx = ((x as f64 + 0.5) * sw as f64 / dw as f64 - 0.5).clamp(0.0, sw as f64 - 1.0);
            let sx0 = sx.floor() as usize;
            let sx1 = (sx0 + 1).min(sw - 1);
            let fx = sx - sx0 as f64;
            let di = (y * dw + x) * 4;
            for c in 0..4 {
                let p00 = f64::from(src[(sy0 * sw + sx0) * 4 + c]);
                let p10 = f64::from(src[(sy0 * sw + sx1) * 4 + c]);
                let p01 = f64::from(src[(sy1 * sw + sx0) * 4 + c]);
                let p11 = f64::from(src[(sy1 * sw + sx1) * 4 + c]);
                let top = p00 * (1.0 - fx) + p10 * fx;
                let bot = p01 * (1.0 - fx) + p11 * fx;
                out[di + c] = (top * (1.0 - fy) + bot * fy).round() as u8;
            }
        }
    }
    out
}

/// mask 最近邻放大（灰度 mask，最近邻足够）
#[must_use]
pub fn resize_mask(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh];
    for y in 0..dh {
        let sy = (y * sh / dh).min(sh - 1);
        for x in 0..dw {
            let sx = (x * sw / dw).min(sw - 1);
            out[y * dw + x] = src[sy * sw + sx];
        }
    }
    out
}

/// 一步封装：BGRA 帧 → 人像分割 → 背景替换/模糊。
/// `background` 为 None 时用盒式模糊作为"背景模糊"。
/// 需要 feature "vision"。返回处理后的 BGRA（原地修改 bgra）。
pub fn segment_and_replace(
    bgra: &mut [u8],
    width: u32,
    height: u32,
    background: Option<&[u8]>,
    blur_radius: u32,
) -> bool {
    // 降分辨率分割：最长边压到 512，mask 再放大回原尺寸（帧率从 ~10 提到 30+）
    let max_dim = width.max(height);
    let (sw, sh) = if max_dim > 512 {
        let scale = 512.0 / f64::from(max_dim);
        (
            (f64::from(width) * scale) as u32,
            (f64::from(height) * scale) as u32,
        )
    } else {
        (width, height)
    };
    // 节流：每 5 帧真正跑一次 Vision 分割，中间复用上一帧 mask
    let n = FRAME_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if n.is_multiple_of(5) {
        let small = resize_bgra(bgra, width, height, sw, sh);
        let Some(pb) = bgra_to_cvpixelbuffer(&small, sw as usize, sh as usize) else {
            return false;
        };
        let Some((mask, mw, mh)) = segment_person(pb) else {
            unsafe {
                CFRelease(pb.cast_const());
            }
            return false;
        };
        unsafe {
            CFRelease(pb.cast_const());
        }
        let full_mask = resize_mask(&mask, mw as u32, mh as u32, width, height);
        // 同时缓存模糊背景（每 5 帧算一次，中间复用）
        let blur_bg = match background {
            Some(b) => b.to_vec(),
            None => box_blur(bgra, width, height, blur_radius.max(1)),
        };
        *LAST_MASK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some((full_mask, blur_bg));
    }
    let guard = LAST_MASK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some((m, bg)) = guard.as_ref() {
        apply_background(bgra, width, height, m, Some(bg), blur_radius);
        true
    } else {
        false
    }
}

#[cfg(all(test, feature = "vision"))]
mod tests {
    use super::*;

    /// M5：重置进程级分割状态（`FRAME_COUNT`/`LAST_MASK`），测试自建初值、不再
    /// 依赖执行顺序（cargo test 默认并行跑）。持 M3 的 `REQ` 锁与进行中的分割
    /// 互斥，避免 reset 与并发分割交错；`REQ` → `LAST_MASK` 嵌套与生产路径（先
    /// 释放 `REQ` 再取 `LAST_MASK`）无锁环，不会死锁。与并行测试的完全隔离
    /// 做不到（进程级全局态），但 reset 后本测试自会重新分割并覆盖 `LAST_MASK`，
    /// 断言不依赖初值。
    fn reset_vision_state() {
        let _req_guard = get_req();
        FRAME_COUNT.store(0, std::sync::atomic::Ordering::Relaxed);
        *LAST_MASK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    #[test]
    fn test_segment_person_runs() {
        let pb = create_bgra_buffer(256, 256);
        assert!(!pb.is_null(), "CVPixelBuffer 创建失败");
        // 回归：连续两次调用，覆盖全局 REQ 复用路径（第二次 performRequests 替换
        // results → 旧 observation dealloc）。修复前对 +0 pixelBuffer 的误 CFRelease
        // 属 over-release，其 SIGTRAP 只在真实扩展进程管线实证（审查用
        // CFGetRetainCount 定位）；单测进程内 CoreVideo 内部引用余量会吸收多余
        // release，无法在修复前复现崩溃，故本测试守护复用路径行为不回归。
        for _ in 0..2 {
            let (mask, mw, mh) = segment_person(pb).expect("人像分割失败");
            assert_eq!(mask.len(), mw * mh, "mask 长度应与宽高一致");
            println!(
                "mask 尺寸 {}x{}，非零占比: {:.1}%",
                mw,
                mh,
                mask.iter().filter(|&&m| m > 128).count() as f32 * 100.0 / mask.len() as f32
            );
        }
        unsafe {
            CFRelease(pb.cast_const());
        }
    }

    #[test]
    fn test_segment_and_replace_runs() {
        // M5：不依赖进程级 FRAME_COUNT/LAST_MASK 初值，先重置再走完整分割路径
        reset_vision_state();
        let w = 128;
        let h = 128;
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            let o = i * 4;
            bgra[o] = 60;
            bgra[o + 1] = 120;
            bgra[o + 2] = 180;
            bgra[o + 3] = 255;
        }
        // 纯色无人像：segment 返回 mask（全背景），replace 后应仍有效（不崩溃）
        assert!(segment_and_replace(&mut bgra, w, h, None, 3));
    }

    #[test]
    fn test_apply_background_blur() {
        let w = 64;
        let h = 64;
        let mut bgra = vec![0u8; w * h * 4];
        for i in 0..w * h {
            let o = i * 4;
            bgra[o] = 100;
            bgra[o + 1] = 150;
            bgra[o + 2] = 200;
            bgra[o + 3] = 255;
        }
        // 全背景 mask（全 0）
        let mask = vec![0u8; w * h];
        let before = bgra.clone();
        apply_background(&mut bgra, w as u32, h as u32, &mask, None, 3);
        // 背景模糊后，像素应被平滑（纯色图模糊后仍接近原值，但不完全等于——验证不崩溃 + 仍有效）
        let p = crate::Pixel {
            b: bgra[0],
            g: bgra[1],
            r: bgra[2],
            a: bgra[3],
        };
        assert!(p.a == before[3], "alpha 应保留");
        assert!(p.r > 0, "模糊后仍有效");
    }

    #[test]
    fn test_apply_background_short_background_no_panic() {
        // M2 回归：背景切片过短（< w*h*4）修复前会索引越界 panic，修复后保持原帧
        let w = 4;
        let h = 4;
        let mut bgra = vec![7u8; (w * h * 4) as usize];
        let mask = vec![0u8; (w * h) as usize]; // 全背景 → 必走 bg 索引路径
        let before = bgra.clone();
        apply_background(&mut bgra, w, h, &mask, Some(&[1u8, 2, 3]), 1);
        assert_eq!(bgra, before, "背景过短时应保持原帧不变");

        // 对照：合法长度背景正常替换（Some 路径此前无覆盖）
        let full_bg = vec![200u8; (w * h * 4) as usize];
        apply_background(&mut bgra, w, h, &mask, Some(&full_bg), 1);
        assert_eq!(bgra[0], 200, "全背景 mask 下应替换为背景像素");
    }

    #[test]
    fn test_apply_background_overflow_dims_no_panic() {
        // 回归：n 原本在 u32 域算 width*height，u32::MAX 尺寸在 debug 下乘法
        // 回绕 panic（算 n 时即崩）；修复后 usize 域计算，空 mask + 过短背景
        // 走早退路径，不再 panic 也不做无效分配。
        let mut bgra: Vec<u8> = Vec::new();
        apply_background(&mut bgra, u32::MAX, u32::MAX, &[], Some(&[]), 1);
        assert!(bgra.is_empty(), "非法尺寸下应早退、原帧不变");
    }
}
