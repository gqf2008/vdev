//! 屏幕推流：CGDisplayStream（C API + `CFRunLoop` + block2），无 `ObjC` delegate。
use anyhow::{anyhow, Result};
use block2::RcBlock;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

type OnFrame = Box<dyn FnMut(Vec<u8>, u32, u32, u32) + Send>;
/// 最近一帧缓存（保活线程重发用）
type LastFrame = Arc<Mutex<Option<(Vec<u8>, u32, u32, u32)>>>;

static CB: OnceLock<Mutex<Option<OnFrame>>> = OnceLock::new();
static RUNNING: AtomicBool = AtomicBool::new(false);
static RUNLOOP: AtomicUsize = AtomicUsize::new(0);
/// 会话代际：`start`/`stop` 各 +1。保活线程记录自己所属代际，不符即退出——
/// 否则 stop→start 在保活线程 200ms sleep 窗口内完成时，旧线程以全局 RUNNING
/// 为存活条件永远不退出，每 500ms 把上一会话的旧帧经新 CB 注入（周期性闪回）。
static SESSION: AtomicU64 = AtomicU64::new(0);
/// runloop 线程句柄：`stop()` 需同步 join，等待清理（DisplayStreamStop/CFRelease/
/// RUNNING=false）完成——之前 stop 只发 `CFRunLoopStop` 即返回，立即 start 会撞
/// 「已在运行」，保活线程也会短暂以旧会话状态存活。
static RUNLOOP_THREAD: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

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

const KCV_PIXEL_FORMAT_BGRA: i32 = 0x4247_5241; // 'BGRA'
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
    // SAFETY：surface 为系统回调提供的有效 IOSurfaceRef，以下均为只读查询/成对加解锁
    if unsafe {
        IOSurfaceLock(
            surface,
            1, /* kIOSurfaceLockReadOnly */
            std::ptr::null_mut(),
        )
    } != 0
    {
        return None;
    }
    let w = unsafe { IOSurfaceGetWidth(surface) } as u32;
    let h = unsafe { IOSurfaceGetHeight(surface) } as u32;
    let stride = unsafe { IOSurfaceGetBytesPerRow(surface) } as u32;
    let base = unsafe { IOSurfaceGetBaseAddress(surface) };
    let mut buf = Vec::with_capacity(stride as usize * h as usize);
    if !base.is_null() {
        // SAFETY：base 指向 lock 期间有效的 stride*h 字节像素缓冲
        unsafe { std::slice::from_raw_parts(base as *const u8, stride as usize * h as usize) }
            .clone_into(&mut buf);
    }
    unsafe { IOSurfaceUnlock(surface, 1, std::ptr::null_mut()) };
    Some((buf, w, h, stride))
}

/// 保活线程：无新帧超过 500ms 就重发最后一帧（与视频推流同款）。
/// 退出条件用会话代际而非 RUNNING：stop→start 在 200ms sleep 窗口内完成时
/// RUNNING 又变回 true，旧线程永不退出并持有旧帧周期性注入（画面闪回）。
fn spawn_keep_alive(last: LastFrame, last_sent: Arc<Mutex<std::time::Instant>>, session: u64) {
    std::thread::spawn(move || {
        while SESSION.load(Ordering::SeqCst) == session {
            let stale = last_sent
                .lock()
                .is_ok_and(|s| s.elapsed() >= std::time::Duration::from_millis(500));
            if stale {
                let frame = last
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some((buf, w, h, stride)) = frame {
                    if let Some(cb) = CB
                        .get_or_init(|| Mutex::new(None))
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_mut()
                    {
                        cb(buf, w, h, stride);
                    }
                    *last_sent
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        std::time::Instant::now();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
}

/// `开始推流指定显示器。on_frame` 在采集线程回调（BGRA32）。
pub fn start(display_id: u32, on_frame: OnFrame) -> Result<()> {
    if RUNNING.swap(true, Ordering::SeqCst) {
        return Err(anyhow!("屏幕推流已在运行"));
    }
    // 本次会话代际（stop 会再 +1 使旧保活线程失效）
    let session = SESSION.fetch_add(1, Ordering::SeqCst) + 1;
    *CB.get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(on_frame);

    if !unsafe { CGPreflightScreenCaptureAccess() } {
        unsafe { CGRequestScreenCaptureAccess() };
        // 权限失败同样必须复位 RUNNING：否则用户授权后再 start 永远得到
        // 「屏幕推流已在运行」（与下方 Create/Start 失败分支同责）
        RUNNING.store(false, Ordering::SeqCst);
        return Err(anyhow!(
            "需要屏幕录制权限：系统设置 → 隐私与安全性 → 屏幕录制"
        ));
    }

    // 保存最后一帧 + 保活线程：静止画面 CGDisplayStream 几乎不发回调（COMPLETE/IDLE
    // 都停），不能依赖回调驱动重发；用独立线程每 500ms 重发最后一帧，避免摄像头回落彩条。
    let last: LastFrame = Arc::new(Mutex::new(None));
    let last_sent: Arc<Mutex<std::time::Instant>> = Arc::new(Mutex::new(std::time::Instant::now()));
    let last_cb = last.clone();
    let sent_cb = last_sent.clone();
    let handler = RcBlock::new(
        move |status: i32, _t: u64, surface: *const c_void, _u: *const c_void| {
            // 本闭包经 block2 trampoline 被 CoreGraphics 调用：panic 会跨 FFI
            // 边界展开 = 进程 abort。因此所有锁访问必须中毒安全（不能 unwrap）。
            if status == KCG_FRAME_COMPLETE && !surface.is_null() {
                if let Some((buf, w, h, stride)) = copy_surface(surface) {
                    let scaled = crate::vimage::scale_bgra(
                        &buf,
                        w as usize,
                        h as usize,
                        stride as usize,
                        1920,
                        1080,
                    )
                    .unwrap_or((buf, stride as usize));
                    let frame = (scaled.0, 1920u32, 1080u32, scaled.1 as u32);
                    if let Some(cb) = CB
                        .get_or_init(|| Mutex::new(None))
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .as_mut()
                    {
                        cb(frame.0.clone(), frame.1, frame.2, frame.3);
                    }
                    *last_cb
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(frame);
                    *sent_cb
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        std::time::Instant::now();
                }
            }
        },
    );

    let sw = unsafe { CGDisplayPixelsWide(display_id) };
    let sh = unsafe { CGDisplayPixelsHigh(display_id) };
    let stream = unsafe {
        CGDisplayStreamCreate(
            display_id,
            sw,
            sh,
            KCV_PIXEL_FORMAT_BGRA,
            std::ptr::null(),
            (&raw const *handler).cast::<c_void>(),
        )
    };
    if stream.0.is_null() {
        RUNNING.store(false, Ordering::SeqCst);
        return Err(anyhow!(
            "CGDisplayStreamCreate 失败（显示器 {display_id:#x}）"
        ));
    }
    let rc = unsafe { CGDisplayStreamStart(stream) };
    if rc != 0 {
        unsafe { CFRelease(stream.0) };
        RUNNING.store(false, Ordering::SeqCst);
        return Err(anyhow!("CGDisplayStreamStart 失败 rc={rc}"));
    }

    // 保活线程：无新帧超过 500ms 就重发最后一帧（与视频推流同款）
    spawn_keep_alive(last.clone(), last_sent.clone(), session);

    // 独立线程跑 CFRunLoop，驱动 block 回调；句柄存入 RUNLOOP_THREAD，
    // stop() join 它以同步等待清理完成
    let rl_thread = std::thread::spawn(move || {
        // SAFETY：stream 生命周期由本线程持有（停止后释放），rl 为当前线程 runloop
        let rl = unsafe { CFRunLoopGetCurrent() };
        RUNLOOP.store(rl.0 as usize, Ordering::SeqCst);
        let src = unsafe { CGDisplayStreamGetRunLoopSource(stream) };
        unsafe { CFRunLoopAddSource(rl, src, kCFRunLoopDefaultMode) };
        unsafe { CFRunLoopRun() };
        unsafe { CFRunLoopRemoveSource(rl, src, kCFRunLoopDefaultMode) };
        RUNLOOP.store(0, Ordering::SeqCst);
        unsafe { CGDisplayStreamStop(stream) };
        unsafe { CFRelease(stream.0) };
        RUNNING.store(false, Ordering::SeqCst);
    });
    *RUNLOOP_THREAD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(rl_thread);
    Ok(())
}

/// 停止推流：同步等待 runloop 线程清理完成（原实现只发 `CFRunLoopStop` 即返回，
/// stop 后立即 start 会撞「已在运行」，且旧保活线程在窗口期会注入旧会话帧）。
pub fn stop() {
    // 会话代际 +1：旧保活线程醒来发现代际不符即退出
    SESSION.fetch_add(1, Ordering::SeqCst);
    let Some(handle) = RUNLOOP_THREAD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take()
    else {
        return; // 从未 start，或已被并发 stop 取走
    };
    // start 刚返回就 stop 的竞态：runloop 线程可能尚未登记 RUNLOOP，稍等它登记
    while !handle.is_finished() && RUNLOOP.load(Ordering::SeqCst) == 0 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let rl = RUNLOOP.load(Ordering::SeqCst);
    if rl != 0 && !handle.is_finished() {
        // SAFETY：rl 非零且线程仍存活时是它登记的 CFRunLoopRef，用于停止其事件循环
        unsafe { CFRunLoopStop(CFRunLoopRef(rl as *mut c_void)) };
    }
    // join = 等清理（RemoveSource/DisplayStreamStop/CFRelease/RUNNING=false）完成
    let _ = handle.join();
}

pub fn main_display_id() -> u32 {
    unsafe { CGMainDisplayID() }
}

#[allow(dead_code)]
pub fn is_running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}
