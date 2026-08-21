//! 视频文件推流：NSOpenPanel 选文件 + AVAssetReader 解码 + vImage 缩放。
use anyhow::{anyhow, Result};
use std::sync::Mutex;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::ClassType;
use objc2_app_kit::{NSModalResponseOK, NSOpenPanel};
use objc2_av_foundation::{
    AVAssetReader, AVAssetReaderTrackOutput, AVMediaTypeVideo, AVURLAsset,
};
use objc2_foundation::{NSArray, NSDictionary, NSString, NSURL};

mod ffi {
    use std::ffi::c_void;
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CMTime {
        pub value: i64,
        pub timescale: i32,
        pub flags: u32,
        pub epoch: i64,
    }
    #[link(name = "CoreMedia", kind = "framework")]
    extern "C" {
        pub fn CMClockGetHostTimeClock() -> *mut c_void;
        pub fn CMClockGetTime(clock: *mut c_void) -> CMTime;
        pub fn CMSampleBufferGetImageBuffer(sb: *mut c_void) -> *mut c_void;
        pub fn CMSampleBufferGetPresentationTimeStamp(sb: *mut c_void) -> CMTime;
    }
    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        pub fn CFRelease(obj: *const c_void);
    }
    #[link(name = "CoreVideo", kind = "framework")]
    extern "C" {
        pub fn CVPixelBufferLockBaseAddress(pb: *mut c_void, opts: u64);
        pub fn CVPixelBufferUnlockBaseAddress(pb: *mut c_void, opts: u64);
        pub fn CVPixelBufferGetBaseAddress(pb: *mut c_void) -> *mut c_void;
        pub fn CVPixelBufferGetBytesPerRow(pb: *mut c_void) -> usize;
        pub fn CVPixelBufferGetWidth(pb: *mut c_void) -> usize;
        pub fn CVPixelBufferGetHeight(pb: *mut c_void) -> usize;
    }
}

fn error_description(e: &AnyObject) -> String {
    unsafe {
        let reason: Retained<NSString> = msg_send![e, reason];
        reason.to_string()
    }
}

pub fn host_time_ns() -> u64 {
    unsafe {
        let t = ffi::CMClockGetTime(ffi::CMClockGetHostTimeClock());
        (t.value as f64 / t.timescale as f64 * 1e9) as u64
    }
}

/// 确保 NSApplication 已初始化（否则 NSOpenPanel 返回 NULL）。
pub fn ensure_nsapp() {
    unsafe {
        let Some(cls) = objc2::runtime::AnyClass::get(c"NSApplication") else { return };
        let app: *mut objc2::runtime::AnyObject = objc2::msg_send![cls, sharedApplication];
        if !app.is_null() {
            let _: () = objc2::msg_send![&*app, setActivationPolicy: 0]; // Regular
            let _: bool = objc2::msg_send![&*app, activateIgnoringOtherApps: true];
        }
    }
}

/// 沙盒内读取用户所选文件的访问令牌：持有 NSOpenPanel 返回的 NSURL，
/// 并保持 security-scoped 访问，直到推流结束（Drop 自动释放）。
pub struct FileAccess {
    url: Option<Retained<NSURL>>,
    started: bool,
}

// NSURL 是不可变对象，跨线程只读使用是安全的。
unsafe impl Send for FileAccess {}

impl FileAccess {
    /// 非沙盒场景（dev 自测二进制）不需要访问令牌。
    pub fn none() -> Self {
        Self { url: None, started: false }
    }

    fn start(url: Retained<NSURL>) -> Self {
        let started = unsafe {
            let ok: bool = msg_send![&*url, startAccessingSecurityScopedResource];
            ok
        };
        Self { url: Some(url), started }
    }
}

impl Drop for FileAccess {
    fn drop(&mut self) {
        if self.started {
            if let Some(url) = &self.url {
                unsafe {
                    let _: () = msg_send![&**url, stopAccessingSecurityScopedResource];
                }
            }
        }
    }
}

/// 用户选择的视频：路径 + 沙盒访问令牌（必须存活到推流结束）。
pub struct PickedVideo {
    pub path: String,
    pub access: FileAccess,
}

fn panic_detail(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| e.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "未知异常".to_string())
}

/// 主线程弹原生文件选择器（NSOpenPanel，非 rfd——需要保留 NSURL 供沙盒访问）。
/// Ok(Some) 已选视频；Ok(None) 用户取消；Err 系统面板异常（如沙盒缺权限）。
pub fn pick_video() -> Result<Option<PickedVideo>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Option<PickedVideo>, String> {
        let mtm = objc2::MainThreadMarker::new()
            .ok_or_else(|| "文件选择器必须在主线程打开".to_string())?;
        #[allow(deprecated)]
        let panel = NSOpenPanel::openPanel(mtm); // 沙盒缺 user-selected 权限时返回 NULL → panic
        unsafe {
            let _: () = msg_send![&*panel, setCanChooseFiles: true];
            let _: () = msg_send![&*panel, setCanChooseDirectories: false];
            let _: () = msg_send![&*panel, setAllowsMultipleSelection: false];
            let _: () = msg_send![&*panel, setTitle: &*NSString::from_str("选择要推流的视频")];
            let exts: Vec<Retained<NSString>> = ["mp4", "mov", "m4v", "mkv"]
                .iter()
                .map(|e| NSString::from_str(e))
                .collect();
            let array = NSArray::from_retained_slice(&exts);
            #[allow(deprecated)]
            let _: () = msg_send![&*panel, setAllowedFileTypes: Some(&*array)];
        }
        let resp = panel.runModal();
        if resp == NSModalResponseOK {
            let url = panel
                .URL()
                .ok_or_else(|| "未获取到所选文件的 URL".to_string())?;
            let path = url
                .path()
                .map(|p| p.to_string())
                .ok_or_else(|| "所选文件路径为空".to_string())?;
            let access = FileAccess::start(url);
            Ok(Some(PickedVideo { path, access }))
        } else {
            Ok(None)
        }
    }))
    .map_err(|e| format!("系统文件选择器异常: {}", panic_detail(e)))?
}

/// 只验证 NSOpenPanel 能否创建（不弹窗、不阻塞），用于沙盒权限自测。
pub fn openpanel_selftest() -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mtm = objc2::MainThreadMarker::new()
            .ok_or_else(|| "文件选择器必须在主线程打开".to_string())?;
        let _panel = NSOpenPanel::openPanel(mtm);
        Ok::<(), String>(())
    }))
    .map_err(|e| format!("NSOpenPanel 创建失败: {}", panic_detail(e)))?
}

/// 网络/外置卷（/Volumes/*）的视频先复制到本地缓存再解码：
/// AVAssetReader 直接从 ossfs/FUSE 读会卡 30s+（读索引），复制到本地后读取快且稳定。
fn cache_dir() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    let dir = std::path::Path::new(&home).join("Library/Caches/vdev");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 若路径在 /Volumes 下（外置/网络卷），复制到本地缓存并返回本地路径。
/// 已缓存则直接复用。返回 (本地路径, 是否需要复制过)。
pub fn prepare_local_path(path: &str) -> Result<String> {
    if !path.starts_with("/Volumes/") {
        return Ok(path.to_string());
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    // 保留原扩展名：AVAssetReader 对无扩展名文件创建会失败（AV1 实测）
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("mp4");
    let name = format!("{:016x}.{}", h.finish(), ext);
    let dest = cache_dir().join(&name);
    if !dest.exists() {
        eprintln!("视频推流: 复制 {} -> {}", path, dest.display());
        std::fs::copy(path, &dest)?;
        eprintln!("视频推流: 复制完成");
    }
    Ok(dest.to_string_lossy().to_string())
}

/// 解码并推流视频（后台线程）。on_frame 回调已缩放 BGRA32；
/// 结束时（自然播完或被 stop）调用 on_done。
///
/// 保活机制：AVAssetReader 读网络挂载（ossfs 等）或解码复杂编码（AV1/HEVC）时
/// 可能停顿 >2s，扩展端 2s 没收到新注入帧就回落到彩条，造成「彩条/视频交替」。
/// 这里用保活线程：最后发送超过 500ms 就重发最后一帧（带新时间戳），
/// 让扩展永远等不到 2s 超时；推流真正结束后 done=true，保活退出，扩展正常回落彩条。
pub fn push_video(
    path: &str,
    access: FileAccess,
    width: u32,
    height: u32,
    fps: u32,
    on_frame: impl FnMut(Vec<u8>, u32, u32, u32) + Send + 'static,
    on_done: impl FnOnce() + Send + 'static,
) -> Result<()> {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    let path = path.to_string();
    let cb: Arc<Mutex<Box<dyn FnMut(Vec<u8>, u32, u32, u32) + Send>>> =
        Arc::new(Mutex::new(Box::new(on_frame)));
    let last_frame: Arc<Mutex<Option<(Vec<u8>, u32, u32, u32)>>> = Arc::new(Mutex::new(None));
    let last_sent: Arc<Mutex<std::time::Instant>> =
        Arc::new(Mutex::new(std::time::Instant::now()));
    let done: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));

    // 解码线程
    let cb_d = cb.clone();
    let frame_d = last_frame.clone();
    let sent_d = last_sent.clone();
    let done_d = done.clone();
    std::thread::spawn(move || {
        // access 随线程存活：沙盒内 security-scoped 权限保持到推流结束
        let _access = access;
        // 网络/外置卷先复制到本地缓存（在后台线程做，不阻塞 UI）
        let local = match prepare_local_path(&path) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("视频推流: 复制到本地失败: {}", e);
                path.clone()
            }
        };
        let r = run(&local, width, height, fps, move |buf, w, h, stride| {
            let mut guard = cb_d.lock().unwrap_or_else(|e| e.into_inner());
            let c = guard.as_mut();
            c(buf.clone(), w, h, stride);
            drop(guard);
            *frame_d.lock().unwrap_or_else(|e| e.into_inner()) = Some((buf, w, h, stride));
            *sent_d.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
        });
        if let Err(e) = &r {
            eprintln!("视频推流失败: {}", e);
        }
        done_d.store(true, std::sync::atomic::Ordering::SeqCst);
        on_done();
    });

    // 保活线程：解码停顿 >500ms 时重发最后一帧，避免扩展回落彩条
    let cb_k = cb.clone();
    let frame_k = last_frame.clone();
    let sent_k = last_sent.clone();
    let done_k = done.clone();
    std::thread::spawn(move || {
        while !done_k.load(std::sync::atomic::Ordering::SeqCst) {
            let stale = sent_k
                .lock()
                .map(|s| s.elapsed() >= std::time::Duration::from_millis(500))
                .unwrap_or(false);
            if stale {
                let frame = frame_k.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if let Some((buf, w, h, stride)) = frame {
                    let mut guard = cb_k.lock().unwrap_or_else(|e| e.into_inner());
                    let c = guard.as_mut();
                    c(buf, w, h, stride);
                    drop(guard);
                    *sent_k.lock().unwrap_or_else(|e| e.into_inner()) = std::time::Instant::now();
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
    Ok(())
}

#[allow(deprecated)]
fn run(
    path: &str,
    width: u32,
    height: u32,
    _fps: u32,
    mut on_frame: impl FnMut(Vec<u8>, u32, u32, u32),
) -> Result<()> {
    unsafe {
        let url = NSURL::fileURLWithPath_isDirectory_relativeToURL(&NSString::from_str(path), false, None);
        let asset = AVURLAsset::URLAssetWithURL_options(&url, None);
        let reader = AVAssetReader::assetReaderWithAsset_error(&asset)
            .ok()
            .ok_or_else(|| anyhow!("AVAssetReader 创建失败"))?;
        let Some(video_type) = AVMediaTypeVideo else {
            return Err(anyhow!("AVMediaTypeVideo 不可用"));
        };
        let tracks = asset.tracksWithMediaType(video_type);
        let track = tracks
            .firstObject()
            .ok_or_else(|| anyhow!("没有视频轨"))?;

        // outputSettings = { kCVPixelBufferPixelFormatTypeKey: kCVPixelFormatType_32BGRA }
        let key = NSString::from_str("PixelFormatType");
        let val: Retained<AnyObject> = msg_send![
            objc2_foundation::NSNumber::class(),
            numberWithUnsignedInt: 0x42475241u32
        ];
        let dict: Retained<NSDictionary<NSString>> =
            msg_send![<NSDictionary<NSString>>::class(), dictionaryWithObject: &*val, forKey: &*key];
        let output = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
            AVAssetReaderTrackOutput::assetReaderTrackOutputWithTrack_outputSettings(
                &*track,
                Some(&*dict),
            )
        }))
        .map_err(|e| {
            let desc = e
                .as_deref()
                .map(|ex| error_description(ex))
                .unwrap_or_else(|| "未知异常".to_string());
            anyhow!("assetReaderTrackOutput 抛异常: {}", desc)
        })?;
        reader.addOutput(&output);
        if !reader.startReading() {
            return Err(anyhow!("视频解码启动失败"));
        }

        let start_ns = host_time_ns();
        // 按源视频 PTS 时间戳推流（不再固定 60fps 掐帧）：
        // 24fps 视频就 24fps 推，VFR 也精确；解码慢于实时时自然追帧（不丢帧）
        let mut base_pts_ns: Option<i64> = None;

        loop {
            if crate::VIDEO_STOP.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            let sample: *mut std::ffi::c_void = msg_send![&*output, copyNextSampleBuffer];
            if sample.is_null() {
                break;
            }
            let pb = ffi::CMSampleBufferGetImageBuffer(sample);
            if pb.is_null() {
                ffi::CFRelease(sample as *const std::ffi::c_void);
                continue;
            }
            let pts = ffi::CMSampleBufferGetPresentationTimeStamp(sample);
            let pts_ns = if pts.timescale > 0 {
                (pts.value as f64 / pts.timescale as f64 * 1e9) as i64
            } else {
                0
            };
            let base = *base_pts_ns.get_or_insert(pts_ns);
            let target_ns = start_ns.saturating_add((pts_ns - base).max(0) as u64);
            ffi::CVPixelBufferLockBaseAddress(pb, 1); // read-only
            let src_w = ffi::CVPixelBufferGetWidth(pb);
            let src_h = ffi::CVPixelBufferGetHeight(pb);
            let src_stride = ffi::CVPixelBufferGetBytesPerRow(pb);
            let base = ffi::CVPixelBufferGetBaseAddress(pb);
            let raw = if base.is_null() {
                Vec::new()
            } else {
                std::slice::from_raw_parts(base as *const u8, src_stride * src_h).to_vec()
            };
            ffi::CVPixelBufferUnlockBaseAddress(pb, 1);

            let scaled = crate::vimage::scale_bgra(
                &raw,
                src_w,
                src_h,
                src_stride,
                width as usize,
                height as usize,
            )
            .unwrap_or((raw, src_stride));
            let now = host_time_ns();
            if target_ns > now {
                std::thread::sleep(std::time::Duration::from_nanos(target_ns - now));
            }
            on_frame(scaled.0, width, height, scaled.1 as u32);
            ffi::CFRelease(sample as *const std::ffi::c_void);
        }
        Ok(())
    }
}
