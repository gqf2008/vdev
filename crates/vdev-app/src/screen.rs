//! 屏幕推流：CGDisplayStream（C API + CFRunLoop + block2），无 ObjC delegate。
use anyhow::{anyhow, Result};
use block2::RcBlock;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

type OnFrame = Box<dyn FnMut(Vec<u8>, u32, u32, u32) + Send>;

static CB: OnceLock<Mutex<Option<OnFrame>>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);
static RUNLOOP: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
#[derive(Clone, Copy)]
struct CGDisplayStreamRef(*mut c_void);
// CFTypeRef 由系统管理生命周期，跨线程传递指针安全（配合 CFRelease 收尾）
unsafe impl Send for CGDisplayStreamRef {}
#[repr(C)]
#[derive(Clone, Copy)]
struct CFRunLoopRef(*mut c_void);
unsafe impl Send for CFRunLoopRef {}
#[repr(C)]
#[derive(Clone, Copy)]
struct CFRunLoopSourceRef(*mut c_void);
unsafe impl Send for CFRunLoopSourceRef {}

const KCV_PIXEL_FORMAT_BGRA: i32 = 0x42475241; // 'BGRA'
const KCG_FRAME_COMPLETE: i32 = 0;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGDisplayStreamCreate(
        display: u32,
        w: usize,
        h: usize,
        fmt: i32,
        props: *const c_void,
        handler: *const c_void,
    ) -> CGDisplayStreamRef;
    fn CGDisplayStreamStart(s: CGDisplayStreamRef) -> i32;
    fn CGDisplayStreamStop(s: CGDisplayStreamRef) -> i32;
    fn CGDisplayStreamGetRunLoopSource(s: CGDisplayStreamRef) -> CFRunLoopSourceRef;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGMainDisplayID() -> u32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGRequestScreenCaptureAccess() -> bool;
    fn CFRelease(obj: *const c_void);
}

#[link(name = "IOSurface", kind = "framework")]
extern "C" {
    fn IOSurfaceLock(s: *const c_void, opts: u32, seed: *mut u32) -> i32;
    fn IOSurfaceUnlock(s: *const c_void, opts: u32, seed: *mut u32) -> i32;
    fn IOSurfaceGetBaseAddress(s: *const c_void) -> *mut c_void;
    fn IOSurfaceGetBytesPerRow(s: *const c_void) -> usize;
    fn IOSurfaceGetWidth(s: *const c_void) -> usize;
    fn IOSurfaceGetHeight(s: *const c_void) -> usize;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopGetCurrent() -> CFRunLoopRef;
    fn CFRunLoopAddSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: *const c_void);
    fn CFRunLoopRemoveSource(rl: CFRunLoopRef, src: CFRunLoopSourceRef, mode: *const c_void);
    fn CFRunLoopRun();
    fn CFRunLoopStop(rl: CFRunLoopRef);
}

fn copy_surface(surface: *const c_void) -> Option<(Vec<u8>, u32, u32, u32)> {
    unsafe {
        if IOSurfaceLock(surface, 1 /* kIOSurfaceLockReadOnly */, std::ptr::null_mut()) != 0 {
            return None;
        }
        let w = IOSurfaceGetWidth(surface) as u32;
        let h = IOSurfaceGetHeight(surface) as u32;
        let stride = IOSurfaceGetBytesPerRow(surface) as u32;
        let base = IOSurfaceGetBaseAddress(surface);
        let mut buf = Vec::with_capacity(stride as usize * h as usize);
        if !base.is_null() {
            std::slice::from_raw_parts(base as *const u8, stride as usize * h as usize)
                .clone_into(&mut buf);
        }
        IOSurfaceUnlock(surface, 1, std::ptr::null_mut());
        Some((buf, w, h, stride))
    }
}

/// 开始推流指定显示器。on_frame 在采集线程回调（BGRA32）。
pub fn start(display_id: u32, on_frame: OnFrame) -> Result<()> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err(anyhow!("屏幕推流已在运行"));
    }
    *CB.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(on_frame);

    if !unsafe { CGPreflightScreenCaptureAccess() } {
        unsafe { CGRequestScreenCaptureAccess() };
        return Err(anyhow!("需要屏幕录制权限：系统设置 → 隐私与安全性 → 屏幕录制"));
    }

    let handler = RcBlock::new(move |status: i32, _t: u64, surface: *const c_void, _u: *const c_void| {
        if status == KCG_FRAME_COMPLETE && !surface.is_null() {
            if let Some((buf, w, h, stride)) = copy_surface(surface) {
                // 统一缩放到摄像头主格式 1920x1080
                let scaled = crate::vimage::scale_bgra(&buf, w as usize, h as usize, stride as usize, 1920, 1080)
                    .unwrap_or((buf, stride as usize));
                if let Some(cb) = CB.get_or_init(|| Mutex::new(None)).lock().unwrap().as_mut() {
                    cb(scaled.0, 1920, 1080, scaled.1 as u32);
                }
            }
        }
    });

    let sw = unsafe { CGDisplayPixelsWide(display_id) };
    let sh = unsafe { CGDisplayPixelsHigh(display_id) };
    let stream = unsafe {
        CGDisplayStreamCreate(
            display_id,
            sw,
            sh,
            KCV_PIXEL_FORMAT_BGRA,
            std::ptr::null(),
            &*handler as *const _ as *const c_void,
        )
    };
    if stream.0.is_null() {
        RUNNING.store(false, Ordering::SeqCst);
        return Err(anyhow!("CGDisplayStreamCreate 失败（显示器 {:#x}）", display_id));
    }
    unsafe {
        let rc = CGDisplayStreamStart(stream);
        if rc != 0 {
            CFRelease(stream.0);
            RUNNING.store(false, Ordering::SeqCst);
            return Err(anyhow!("CGDisplayStreamStart 失败 rc={}", rc));
        }
    }

    // 独立线程跑 CFRunLoop，驱动 block 回调
    std::thread::spawn(move || unsafe {
        let rl = CFRunLoopGetCurrent();
        RUNLOOP.store(rl.0 as usize, Ordering::SeqCst);
        let src = CGDisplayStreamGetRunLoopSource(stream);
        CFRunLoopAddSource(rl, src, kCFRunLoopDefaultMode);
        CFRunLoopRun();
        CFRunLoopRemoveSource(rl, src, kCFRunLoopDefaultMode);
        RUNLOOP.store(0, Ordering::SeqCst);
        CGDisplayStreamStop(stream);
        CFRelease(stream.0);
        RUNNING.store(false, Ordering::SeqCst);
    });
    Ok(())
}

pub fn stop() {
    let rl = RUNLOOP.load(Ordering::SeqCst);
    if rl != 0 {
        unsafe { CFRunLoopStop(CFRunLoopRef(rl as *mut c_void)) };
    }
}

pub fn main_display_id() -> u32 {
    unsafe { CGMainDisplayID() }
}

#[allow(dead_code)]
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}
