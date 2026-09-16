//! 全 Rust `CMIOExtension` spike：最小虚拟摄像头（只出彩条）。
//! 手写 `CMIOExtension` `ObjC` 绑定（cmio.rs），帧管线用 `CoreVideo`/`CoreMedia` C FFI。

// ObjC 方法实现必须照抄 selector 命名（connectClient:error: 等），保留原命名
#![allow(non_snake_case)]

mod cmio;
mod filters;
mod frame_channel;

use cmio::{
    property_set, CMIOExtensionDeviceSource, CMIOExtensionProviderSource, CMIOExtensionStreamSource,
};
use dispatch2::{DispatchObject, DispatchQueue};
use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::ClassType;
use objc2_core_media::CMTime;
use objc2_foundation::NSObjectProtocol;
use objc2_foundation::{
    NSArray, NSDictionary, NSMutableDictionary, NSNumber, NSObject, NSSet, NSString, NSUUID,
};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

static LOG_BUF: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// 全局累计日志行数（含已被 `LOG_BUF` 从头丢弃的行）。`log_server` 以它计算
/// 每连接的续读位置——此前按连接只记 sent 行数、直接 skip(sent) 于当前 buf，
/// 连接发满 500 行后 skip 超出 buf 长度，日志流永久停滞。
static LOG_TOTAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 计算本连接应从 `LOG_BUF` 头部跳过的行数（纯函数，供单测覆盖丢弃场景）。
/// `sent` 为本连接已发送的全局累计行数；buf 只保留最近 `buf_len` 行，
/// 对应全局序号 `[total-buf_len, total)`；已发序号落在已丢弃区间时从头部续发。
fn log_skip(sent: u64, buf_len: usize, total: u64) -> usize {
    let covered = total.saturating_sub(buf_len as u64);
    // 目标平台 64 位 macOS，usize 与 u64 同宽，截断不可能
    #[allow(clippy::cast_possible_truncation)]
    let skip = sent.saturating_sub(covered) as usize;
    skip
}

fn log_server() {
    std::thread::spawn(move || {
        let listener = match std::net::TcpListener::bind("127.0.0.1:27891") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("log_server bind 失败: {e}");
                return;
            }
        };
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            std::thread::spawn(move || {
                let mut stream = stream;
                // 本连接已发送的全局累计行数
                let mut sent = 0u64;
                loop {
                    let lines: Vec<String> = {
                        let buf = LOG_BUF
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let total = LOG_TOTAL.load(Ordering::Relaxed);
                        buf.iter()
                            .skip(log_skip(sent, buf.len(), total))
                            .cloned()
                            .collect()
                    };
                    for l in lines {
                        let _ = std::io::Write::write_all(&mut stream, l.as_bytes());
                        sent += 1;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(200));
                }
            });
        }
    });
}

/// 供 `frame_channel` 复用同一日志口（模块间只走这一条日志路径）。
pub(crate) fn elog_for_channel(msg: impl AsRef<str>) {
    elog(msg);
}

fn elog(msg: impl AsRef<str>) {
    use std::io::Write;
    let line = format!(
        "[unix={}] {}\n",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
        msg.as_ref()
    );
    {
        let mut buf = LOG_BUF
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        buf.push(line.clone());
        // 与 buf.push 同锁递增：读侧（log_server）在锁内读到的 (buf, total) 一致
        LOG_TOTAL.fetch_add(1, Ordering::Relaxed);
        let over = buf.len().saturating_sub(500);
        if over > 0 {
            buf.drain(0..over);
        }
    }
    let mut paths = Vec::new();
    // HOME 派生路径（App Group 共享容器最可能可写——扩展有 app-group 权限）：
    // HOME 缺失时整组跳过（不硬编码用户目录），由末尾 /tmp、/var/tmp 兜底
    if let Ok(home) = std::env::var("HOME") {
        paths.push(format!(
            "{home}/Library/Group Containers/XFXU84HVK3.com.vdev.camera/vdev-camera-ext.log"
        ));
        paths.push(format!("{home}/vdev-camera-ext.log"));
        paths.push(format!(
            "{home}/Library/Containers/com.vdev.camera.ext.spike/Data/vdev-camera-ext.log"
        ));
    }
    paths.push("/tmp/vdev-camera-ext.log".to_string());
    paths.push("/var/tmp/vdev-camera-ext.log".to_string());
    for path in paths {
        if std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| f.write_all(line.as_bytes()))
            .is_ok()
        {
            break;
        }
    }
    eprintln!("{}", line.trim_end());
}

const WIDTH: i32 = 1920;
const HEIGHT: i32 = 1080;
const FPS: i64 = 60;
/// cmtime 的 timescale 用（FPS 的 i32 视图；60 远小于 i32 上限，截断不可能）
#[allow(clippy::cast_possible_truncation)]
const FPS_TS: i32 = FPS as i32;
const BGR_A: u32 = 0x42_47_52_41; // kCVPixelFormatType_32BGRA / kCMVideoCodecType_32BGRA

// ---------------- CoreVideo / CoreMedia / CoreFoundation C FFI ----------------
#[repr(C)]
#[derive(Clone, Copy)]
struct CMFormatDescription(*mut c_void);
unsafe impl Send for CMFormatDescription {}
#[repr(C)]
#[derive(Clone, Copy)]
struct CVPixelBuffer(*mut c_void);
unsafe impl Send for CVPixelBuffer {}
#[repr(C)]
#[derive(Clone, Copy)]
struct CVPixelBufferPool(*mut c_void);
unsafe impl Send for CVPixelBufferPool {}
#[repr(C)]
#[derive(Clone, Copy)]
struct CMSampleBuffer(*mut c_void);
unsafe impl Send for CMSampleBuffer {}

#[repr(C)]
#[derive(Clone, Copy)]
struct CMSampleTimingInfo {
    duration: CMTime,
    presentation_time_stamp: CMTime,
    decode_time_stamp: CMTime,
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferCreate(
        allocator: *const c_void,
        width: usize,
        height: usize,
        pixel_format: u32,
        attrs: *const c_void,
        out: *mut CVPixelBuffer,
    ) -> i32;
    fn CVPixelBufferPoolCreate(
        allocator: *const c_void,
        pool_attrs: *const c_void,
        pixel_buffer_attrs: *const c_void,
        out: *mut CVPixelBufferPool,
    ) -> i32;
    fn CVPixelBufferPoolCreatePixelBuffer(
        allocator: *const c_void,
        pool: CVPixelBufferPool,
        out: *mut CVPixelBuffer,
    ) -> i32;
    fn CVPixelBufferLockBaseAddress(buf: CVPixelBuffer, opts: u64) -> i32;
    fn CVPixelBufferUnlockBaseAddress(buf: CVPixelBuffer, opts: u64) -> i32;
    fn CVPixelBufferGetBaseAddress(buf: CVPixelBuffer) -> *mut c_void;
    fn CVPixelBufferGetBytesPerRow(buf: CVPixelBuffer) -> usize;
}

#[link(name = "CoreMedia", kind = "framework")]
extern "C" {
    fn CMVideoFormatDescriptionCreate(
        allocator: *const c_void,
        codec_type: u32,
        width: i32,
        height: i32,
        extensions: *const c_void,
        out: *mut CMFormatDescription,
    ) -> i32;
    fn CMSampleBufferCreateForImageBuffer(
        allocator: *const c_void,
        image_buffer: CVPixelBuffer,
        data_ready: bool,
        make_data_ready_callback: *const c_void,
        refcon: *const c_void,
        format_description: CMFormatDescription,
        sample_timing: *const CMSampleTimingInfo,
        out: *mut CMSampleBuffer,
    ) -> i32;
    fn CMClockGetHostTimeClock() -> *mut c_void;
    fn CMClockGetTime(clock: *mut c_void) -> CMTime;
    fn CMTimeCopyAsDictionary(time: CMTime, allocator: *const c_void) -> *mut c_void;
    fn CFAbsoluteTimeGetCurrent() -> f64;
    fn CFRelease(obj: *const c_void);
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    // 以下两项本文件暂未用到（spike 用阻塞 CFRunLoopRun），保留作分模式轮询备用
    #[allow(dead_code)]
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRun();
    #[allow(dead_code)]
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: bool,
    ) -> i32;
}

// 保留：动态加载框架/查符号的调试入口（本文件暂未调用，cmio.rs 另有自己的声明并使用）
#[allow(dead_code)]
extern "C" {
    fn dlsym(
        handle: *mut std::ffi::c_void,
        symbol: *const std::ffi::c_char,
    ) -> *mut std::ffi::c_void;
    fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut std::ffi::c_void;
}

// ---------------- 共享状态（进程生命周期） ----------------
static STREAM: Mutex<Option<usize>> = Mutex::new(None);
static STREAM_FORMAT: Mutex<Option<usize>> = Mutex::new(None);
static FORMAT_DESC: Mutex<Option<CMFormatDescription>> = Mutex::new(None);
/// 已连接客户端数，由 provider 的 `connectClient:` / `disconnectClient:` 维护。
/// **只用于日志，不参与产帧判定**：真机实测这两个回调并不总是配对——构建 22 的
/// 扩展启动时那次 `connectClient` 之后再没有对应的 `disconnectClient`（进程早已
/// 退出），计数会永久停在 1。产帧判定改用 cmiod 的权威列表
/// `CMIOExtensionStream.streamingClients`（见 `streaming_client_count`）。
static CLIENTS: AtomicUsize = AtomicUsize::new(0);
static SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// 上一次 `sendSampleBuffer` 里程碑（时刻, 累计帧数），用于打印窗口实测帧率。
static LAST_MARK: Mutex<Option<(std::time::Instant, u64)>> = Mutex::new(None);
/// 像素缓冲池缓存：`(宽, 高, 池)`。帧循环是唯一使用者，尺寸可能被推流方改来改去。
static POOLS: Mutex<Vec<(u32, u32, CVPixelBufferPool)>> = Mutex::new(Vec::new());
/// 池缓存上限。**必须有上限**：池按尺寸持有 IOSurface，尺寸是推流方给的，
/// 无上限时反复换尺寸会把扩展内存顶爆（单个 8K 池就上百 MB）。
const MAX_POOLS: usize = 4;
/// 单个池最多同时持有的缓冲数（`CVPixelBufferPoolAllocationThreshold`）。
/// 超出时 `CreatePixelBuffer` 会失败 → 我们回落「每帧新建」，不会卡住出帧。
const POOL_ALLOC_THRESHOLD: i32 = 8;

/// 在既有池尺寸表里找匹配项（纯函数，便于单测）。
fn find_pool(keys: &[(u32, u32)], w: u32, h: u32) -> Option<usize> {
    keys.iter().position(|(pw, ph)| *pw == w && *ph == h)
}

/// 池数量到达上限时要淘汰的下标（FIFO，淘汰最早创建的）；未到上限返回 `None`。
fn evict_slot(len: usize, cap: usize) -> Option<usize> {
    if len >= cap {
        Some(0)
    } else {
        None
    }
}

/// 客户端接入：返回接入后的连接数。
fn client_connected(clients: &AtomicUsize) -> usize {
    clients.fetch_add(1, Ordering::SeqCst) + 1
}

/// 客户端断开：返回断开后的连接数，未配对的 disconnect 饱和在 0（不回绕）。
fn client_disconnected(clients: &AtomicUsize) -> usize {
    clients
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
            Some(n.saturating_sub(1))
        })
        .map_or(0, |prev| prev.saturating_sub(1))
}

/// 产帧判定：只要还有**真正在拉流**的客户端就产帧。
///
/// 旧实现看的是 `startStream` 置的 RUNNING：客户端强杀不补发 `stopStream` →
/// 永久空转烧 CPU，新客户端还只能从旧失效队列的缝里拿到残帧（真机 2.3fps）。
fn should_produce(streaming_clients: usize) -> bool {
    streaming_clients > 0
}

/// 当前真正在拉流的客户端数——**权威信号**，取自 `CMIOExtensionStream.streamingClients`。
///
/// 这是 cmiod 自己维护、也是它路由帧的依据（头文件标注 key-value observable），
/// 不依赖 `startStream`/`stopStream`/`connectClient:`/`disconnectClient:` 这些
/// 对端不保证会发的回调。
///
/// 属性是 `copy` 语义的 getter（返回 +0 / autorelease），按「getter 取出的对象
/// 绝不 release」的规则读走即用。
///
/// `ObjC` 异常包一层：这一路是帧循环唯一的门闩，异常逃逸会直接 abort 扩展
/// （而扩展崩溃会被 cmiod 记成坏状态，见文档第 5 节坑 2）。异常按「没有客户端
/// 在拉流」处理——最多让本轮不产帧，下一轮自恢复。
fn streaming_client_count(stream: *mut NSObject) -> usize {
    let probed = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
        let stream_ref: &NSObject = unsafe { &*stream };
        let arr: *const NSArray<NSObject> = unsafe { msg_send![stream_ref, streamingClients] };
        if arr.is_null() {
            return 0usize;
        }
        unsafe { msg_send![arr, count] }
    }));
    match probed {
        Ok(n) => n,
        Err(ex) => {
            elog(format!("streamingClients 异常: {ex:?}"));
            0
        }
    }
}

fn class(name: &str) -> &'static objc2::runtime::AnyClass {
    let cname = std::ffi::CString::new(name).unwrap();
    objc2::runtime::AnyClass::get(&cname).expect("ObjC class not found")
}

fn cmtime(value: i64, timescale: i32) -> CMTime {
    CMTime {
        value,
        timescale,
        flags: objc2_core_media::CMTimeFlags::Valid,
        epoch: 0,
    }
}

// ---------------- ProviderSource ----------------
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VdevRustProviderSource"]
    #[ivars = ()]
    struct ProviderSource;

    unsafe impl NSObjectProtocol for ProviderSource {}

    unsafe impl CMIOExtensionProviderSource for ProviderSource {
        #[unsafe(method(connectClient:error:))]
        unsafe fn connectClient_error(
            &self,
            _client: &NSObject,
            _out_error: *mut *mut NSObject,
        ) -> bool {
            let n = client_connected(&CLIENTS);
            elog(format!("provider: connectClient clients={n}"));
            true
        }

        #[unsafe(method(disconnectClient:))]
        unsafe fn disconnectClient(&self, _client: &NSObject) {
            // 仅日志：停帧与否完全由 streamingClients 决定，这里只记录回调时序
            // （这两个回调不保证配对，计数只作参考，不能当状态用）。
            let n = client_disconnected(&CLIENTS);
            elog(format!("provider: disconnectClient clients={n}"));
        }

        #[unsafe(method_id(availableProperties))]
        unsafe fn availableProperties(&self) -> Retained<NSSet<NSObject>> {
            property_set(&[
                "CMIOExtensionPropertyProviderName",
                "CMIOExtensionPropertyProviderManufacturer",
            ])
        }

        #[unsafe(method_id(providerPropertiesForProperties:error:))]
        unsafe fn providerPropertiesForProperties_error(
            &self,
            _properties: &NSSet<NSObject>,
            _out_error: *mut *mut NSObject,
        ) -> Option<Retained<NSObject>> {
            let cls = class("CMIOExtensionProviderProperties");
            let empty: Retained<NSDictionary<NSObject, NSObject>> = NSDictionary::new();
            let p: *mut NSObject = msg_send![cls, providerPropertiesWithDictionary: &*empty];
            let p_ref: &NSObject = unsafe { &*p };
            let _: () = msg_send![p_ref, setName: &*NSString::from_str("vdev-camera")];
            let _: () = msg_send![p_ref, setManufacturer: &*NSString::from_str("vdev")];
            Some(unsafe { Retained::retain(p).unwrap() })
        }

        #[unsafe(method(setProviderProperties:error:))]
        unsafe fn setProviderProperties_error(
            &self,
            _provider_properties: &NSObject,
            _out_error: *mut *mut NSObject,
        ) -> bool {
            true
        }
    }
);

// ---------------- DeviceSource ----------------
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VdevRustDeviceSource"]
    #[ivars = ()]
    struct DeviceSource;

    unsafe impl NSObjectProtocol for DeviceSource {}

    unsafe impl CMIOExtensionDeviceSource for DeviceSource {
        #[unsafe(method_id(availableProperties))]
        unsafe fn availableProperties(&self) -> Retained<NSSet<NSObject>> {
            property_set(&[
                "CMIOExtensionPropertyDeviceTransportType",
                "CMIOExtensionPropertyDeviceModel",
            ])
        }

        #[unsafe(method_id(devicePropertiesForProperties:error:))]
        unsafe fn devicePropertiesForProperties_error(
            &self,
            _properties: &NSSet<NSObject>,
            _out_error: *mut *mut NSObject,
        ) -> Option<Retained<NSObject>> {
            let cls = class("CMIOExtensionDeviceProperties");
            let empty: Retained<NSDictionary<NSObject, NSObject>> = NSDictionary::new();
            let p: *mut NSObject = msg_send![cls, devicePropertiesWithDictionary: &*empty];
            let p_ref: &NSObject = unsafe { &*p };
            let _: () =
                msg_send![p_ref, setTransportType: &*NSNumber::numberWithUnsignedInt(0x7674_726e)];
            let _: () = msg_send![p_ref, setModel: &*NSString::from_str("vdev-camera")];
            Some(unsafe { Retained::retain(p).unwrap() })
        }

        #[unsafe(method(setDeviceProperties:error:))]
        unsafe fn setDeviceProperties_error(
            &self,
            _device_properties: &NSObject,
            _out_error: *mut *mut NSObject,
        ) -> bool {
            true
        }
    }
);

// ---------------- StreamSource ----------------
define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VdevRustStreamSource"]
    #[ivars = ()]
    struct StreamSource;

    unsafe impl NSObjectProtocol for StreamSource {}

    unsafe impl CMIOExtensionStreamSource for StreamSource {
        #[unsafe(method_id(formats))]
        unsafe fn formats(&self) -> Retained<NSArray<NSObject>> {
            let fmt = STREAM_FORMAT
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unwrap() as *mut NSObject;
            let obj = unsafe { Retained::retain(fmt).unwrap() };
            NSArray::from_retained_slice(&[obj])
        }

        #[unsafe(method_id(availableProperties))]
        unsafe fn availableProperties(&self) -> Retained<NSSet<NSObject>> {
            property_set(&[
                "CMIOExtensionPropertyStreamActiveFormatIndex",
                "CMIOExtensionPropertyStreamFrameDuration",
            ])
        }

        #[unsafe(method_id(streamPropertiesForProperties:error:))]
        unsafe fn streamPropertiesForProperties_error(
            &self,
            _properties: &NSSet<NSObject>,
            _out_error: *mut *mut NSObject,
        ) -> Option<Retained<NSObject>> {
            let cls = class("CMIOExtensionStreamProperties");
            let empty: Retained<NSDictionary<NSObject, NSObject>> = NSDictionary::new();
            let p: *mut NSObject = msg_send![cls, streamPropertiesWithDictionary: &*empty];
            let p_ref: &NSObject = unsafe { &*p };
            let _: () = msg_send![p_ref, setActiveFormatIndex: &*NSNumber::numberWithInt(0)];
            let dur = cmtime(1, FPS_TS);
            let dict = unsafe { CMTimeCopyAsDictionary(dur, ptr::null()) };
            let dur_dict: &NSObject = unsafe { &*(dict.cast::<NSObject>()) };
            let _: () = msg_send![p_ref, setFrameDuration: dur_dict];
            // SAFETY: CMTimeCopyAsDictionary 按 Copy 规则返回 +1 的 CFDictionary，
            // setFrameDuration 内部已 retain，这里释放调用方自身持有，避免每次读取
            // 流属性泄漏一个字典；dict 非空才释放（CFRelease(NULL) 会崩溃）。
            if !dict.is_null() {
                unsafe { CFRelease(dict.cast_const()) };
            }
            Some(unsafe { Retained::retain(p).unwrap() })
        }

        #[unsafe(method(setStreamProperties:error:))]
        unsafe fn setStreamProperties_error(
            &self,
            _stream_properties: &NSObject,
            _out_error: *mut *mut NSObject,
        ) -> bool {
            true
        }

        #[unsafe(method(authorizedToStartStreamForClient:))]
        unsafe fn authorizedToStartStreamForClient(&self, _client: &NSObject) -> bool {
            true
        }

        #[unsafe(method(startStreamAndReturnError:))]
        unsafe fn startStreamAndReturnError(&self, _out_error: *mut *mut NSObject) -> bool {
            elog("stream: startStream");
            true
        }

        #[unsafe(method(stopStreamAndReturnError:))]
        unsafe fn stopStreamAndReturnError(&self, _out_error: *mut *mut NSObject) -> bool {
            elog("stream: stopStream");
            true
        }
    }
);

/// 按行拷贝 BGRA：只拷每行前 `w*4` 有效字节（padded 尾部不拷），行内长度再与
/// `min(stride, dst_stride)` 取 min——此前按整行（`src.len()==stride`）拷贝，
/// 推流方发 padded stride 而 CV 分配 `dst_stride=w*4` 时，最后一行
/// `row*dst_stride+stride` 越界 panic（每帧黑屏 + 日志刷屏）。
fn copy_bgra_rows(dst: &mut [u8], src: &[u8], w: u32, h: u32, stride: u32, dst_stride: usize) {
    let valid = w as usize * 4;
    if valid == 0 {
        return;
    }
    let row_bytes = valid.min(stride as usize).min(dst_stride);
    for row in 0..h as usize {
        let s = row * stride as usize;
        let d = row * dst_stride;
        dst[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }
}

// ---------------- 像素缓冲池 ----------------
/// 构造「IOSurface 支撑 + BGRA + w×h」的像素缓冲属性字典（池与回退路径共用口径）。
///
/// 键名用 `CoreVideo` 的字符串常量值（`kCVPixelBuffer*Key` 的字符串形态），
/// 避免为了几个常量再拉一层绑定。
#[allow(clippy::cast_possible_wrap)]
fn pixel_buffer_attrs(w: u32, h: u32) -> Retained<NSDictionary<NSObject, NSObject>> {
    let dict: Retained<NSMutableDictionary<NSObject, NSObject>> = NSMutableDictionary::new();
    let iosurf: Retained<NSDictionary<NSObject, NSObject>> = NSDictionary::new();
    unsafe {
        let set = |key: &str, value: &NSObject| {
            let k = NSString::from_str(key);
            let _: () = msg_send![&*dict, setObject: value, forKey: &*k];
        };
        let fmt = NSNumber::numberWithUnsignedInt(BGR_A);
        let ww = NSNumber::numberWithUnsignedInt(w);
        let hh = NSNumber::numberWithUnsignedInt(h);
        set("PixelFormatType", &fmt);
        set("Width", &ww);
        set("Height", &hh);
        set("IOSurfaceProperties", &iosurf);
    }
    // NSMutableDictionary → NSDictionary 是同一实例（子类关系），直接转换所有权
    unsafe { Retained::cast_unchecked(dict) }
}

/// 建一个新池（池属性只带并发上限，缓冲属性决定 `IOSurface` 与尺寸）。
fn create_pixel_buffer_pool(w: u32, h: u32) -> Option<CVPixelBufferPool> {
    let attrs = pixel_buffer_attrs(w, h);
    let pool_attrs: Retained<NSMutableDictionary<NSObject, NSObject>> = NSMutableDictionary::new();
    let k = NSString::from_str("AllocationThreshold");
    let v = NSNumber::numberWithInt(POOL_ALLOC_THRESHOLD);
    unsafe {
        let _: () = msg_send![&*pool_attrs, setObject: &*v, forKey: &*k];
    }
    let mut pool = CVPixelBufferPool(ptr::null_mut());
    let st = unsafe {
        CVPixelBufferPoolCreate(
            ptr::null(),
            std::ptr::from_ref(&*pool_attrs).cast::<c_void>(),
            std::ptr::from_ref(&*attrs).cast::<c_void>(),
            std::ptr::from_mut(&mut pool),
        )
    };
    if st != 0 || pool.0.is_null() {
        elog(format!("CVPixelBufferPoolCreate 失败 st={st} {w}x{h}"));
        return None;
    }
    Some(pool)
}

/// 取（必要时创建）指定尺寸的池。命中缓存时不重复建池。
fn pool_for(w: u32, h: u32) -> Option<CVPixelBufferPool> {
    let mut pools = POOLS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let keys: Vec<(u32, u32)> = pools.iter().map(|(pw, ph, _)| (*pw, *ph)).collect();
    if let Some(i) = find_pool(&keys, w, h) {
        return Some(pools[i].2);
    }
    let pool = create_pixel_buffer_pool(w, h)?;
    if let Some(slot) = evict_slot(pools.len(), MAX_POOLS) {
        // 真 FIFO：淘汰队头（最旧）并把新池追加到队尾，保证"最近用过的尺寸"
        // 不会被后来的尺寸反复顶掉。
        let (_, _, old) = pools.remove(slot);
        unsafe { CFRelease(old.0.cast_const()) };
    }
    pools.push((w, h, pool));
    Some(pool)
}

/// 每帧新建像素缓冲（池不可用时的回退路径，等价于优化前的行为）。
#[allow(clippy::cast_possible_wrap)]
fn create_pixel_buffer_direct(w: u32, h: u32) -> Option<CVPixelBuffer> {
    let attrs = pixel_buffer_attrs(w, h);
    let mut pb = CVPixelBuffer(ptr::null_mut());
    let st = unsafe {
        CVPixelBufferCreate(
            ptr::null(),
            w as usize,
            h as usize,
            BGR_A,
            std::ptr::from_ref(&*attrs).cast::<c_void>(),
            std::ptr::from_mut(&mut pb),
        )
    };
    if st != 0 || pb.0.is_null() {
        elog(format!("CVPixelBufferCreate 失败 st={st} {w}x{h}"));
        return None;
    }
    Some(pb)
}

/// 取一个可写的、IOSurface 支撑的像素缓冲：优先复用池里的（免掉每帧
/// `IOSurface` 分配 + 清零，这是 60fps 下主要固定成本），池不可用则回退新建。
///
/// 复用安全性：`CMSampleBufferCreateForImageBuffer` 会 retain 该缓冲，客户端
/// 拿着 sample buffer 时缓冲的引用计数不为 0，池不会把它再发出来——不存在
/// 「回收了还在用的缓冲」。
fn acquire_pixel_buffer(w: u32, h: u32) -> Option<CVPixelBuffer> {
    if let Some(pool) = pool_for(w, h) {
        let mut pb = CVPixelBuffer(ptr::null_mut());
        let st = unsafe {
            CVPixelBufferPoolCreatePixelBuffer(ptr::null(), pool, ptr::from_mut(&mut pb))
        };
        if st == 0 && !pb.0.is_null() {
            return Some(pb);
        }
        elog(format!(
            "CVPixelBufferPoolCreatePixelBuffer 失败 st={st} {w}x{h}，回落每帧新建"
        ));
    }
    create_pixel_buffer_direct(w, h)
}

// ---------------- 帧循环 ----------------
/// 把一帧 BGRA 数据包成 `CMSampleBuffer` 发给流。
// 单个 unsafe 块按流水线顺序组合一组 C FFI / ObjC 调用（创建像素缓冲 → 拷贝 →
// 打包 → 发送 → 释放），拆成逐操作小块只会稀释安全边界语义，故整块放行。
#[allow(clippy::multiple_unsafe_ops_per_block, clippy::cast_possible_wrap)]
fn send_bgra(
    stream: *mut NSObject,
    fmt: CMFormatDescription,
    data: &[u8],
    w: u32,
    h: u32,
    stride: u32,
    pts_ns: u64,
) {
    // 取缓冲（池复用优先，失败回落每帧新建）；拿不到就这帧不发，下帧再来
    let Some(pb) = acquire_pixel_buffer(w, h) else {
        return;
    };
    unsafe {
        CVPixelBufferLockBaseAddress(pb, 0);
        let base = CVPixelBufferGetBaseAddress(pb);
        let dst_stride = CVPixelBufferGetBytesPerRow(pb);
        if !base.is_null() && data.len() >= (stride as usize) * (h as usize) {
            let dst = std::slice::from_raw_parts_mut(base.cast::<u8>(), dst_stride * h as usize);
            if stride as usize == dst_stride {
                dst[..data.len()].copy_from_slice(&data[..(stride as usize) * (h as usize)]);
            } else {
                copy_bgra_rows(dst, data, w, h, stride, dst_stride);
            }
        }
        CVPixelBufferUnlockBaseAddress(pb, 0);

        let timing = CMSampleTimingInfo {
            duration: cmtime(1, FPS_TS),
            presentation_time_stamp: CMTime {
                value: pts_ns as i64,
                timescale: 1_000_000_000,
                flags: objc2_core_media::CMTimeFlags::Valid,
                epoch: 0,
            },
            decode_time_stamp: CMTime {
                value: -1,
                timescale: 1,
                flags: objc2_core_media::CMTimeFlags::empty(),
                epoch: 0,
            },
        };
        let mut sb = CMSampleBuffer(ptr::null_mut());
        let ss = CMSampleBufferCreateForImageBuffer(
            ptr::null(),
            pb,
            true,
            ptr::null(),
            ptr::null(),
            fmt,
            std::ptr::from_ref(&timing),
            std::ptr::from_mut(&mut sb),
        );
        if ss == 0 && !sb.0.is_null() {
            // ObjC 异常包住：sendSampleBuffer 抛异常时记录而不是 abort
            let send_res = objc2::exception::catch(std::panic::AssertUnwindSafe(|| {
                let _: () = msg_send![
                    &*stream,
                    sendSampleBuffer: sb.0,
                    discontinuity: 0u64,
                    hostTimeInNanoseconds: pts_ns
                ];
            }));
            if let Err(ex) = send_res {
                elog(format!("sendSampleBuffer 异常: {ex:?}"));
            }
            SENT.fetch_add(1, Ordering::SeqCst);
            let n = SENT.load(Ordering::SeqCst);
            if n.is_multiple_of(300) || n == 1 {
                let now = std::time::Instant::now();
                let mut last = LAST_MARK
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let fps = last.map_or(0.0, |(at, m)| window_fps(at, m, now, n));
                *last = Some((now, n));
                drop(last);
                elog(format!("sendSampleBuffer #{n} 窗口实测 fps={fps:.1}"));
            }
            CFRelease(sb.0.cast_const());
        } else {
            elog(format!("CMSampleBufferCreateForImageBuffer 失败 st={ss}"));
        }
        CFRelease(pb.0.cast_const());
    }
}

/// 两个里程碑之间的窗口实测帧率（`n` 为累计帧数；非法输入返回 0）。
#[allow(clippy::cast_precision_loss)]
fn window_fps(prev_at: std::time::Instant, prev_n: u64, now: std::time::Instant, n: u64) -> f64 {
    let dt = now.saturating_duration_since(prev_at).as_secs_f64();
    if dt <= 0.0 || n <= prev_n {
        return 0.0;
    }
    (n - prev_n) as f64 / dt
}

/// 目标帧周期（60fps）。
const FRAME_PERIOD: std::time::Duration = std::time::Duration::from_nanos(16_666_667);
/// 过载时每轮的最小让出时长（不忙等）。
const MIN_YIELD: std::time::Duration = std::time::Duration::from_micros(500);

/// 固定节拍器：按**绝对 deadline** 推进，睡眠只补足到下一个 tick。
///
/// 旧实现是「先产帧、再固定睡满 16.666ms」，睡眠**加在**帧生成耗时之上：
/// 实测周期 ≈ 帧耗时(≈14ms) + 16.666ms ≈ 30.7ms ≈ 32.5fps（issue #51）。
/// 这里改为推进 deadline，帧生成耗时被 tick 吸收而不是累加。
struct Pacer {
    period: std::time::Duration,
    next: std::time::Instant,
}

impl Pacer {
    fn new(now: std::time::Instant, period: std::time::Duration) -> Self {
        Self {
            period,
            next: now + period,
        }
    }

    /// 推进一个 tick，返回本轮应休眠的时长。
    ///
    /// 稳态（帧耗时 < period）：帧起始间隔严格 = period。
    /// 过载（帧耗时 ≥ period）：deadline 重锚到 `now + period`，本轮只让出
    /// `MIN_YIELD`——不追赶、不补帧风暴；速率退化为「帧耗时 + 让出」，
    /// 而不是掉到「帧耗时 + period」的半个帧率。
    ///
    /// 注意 `MIN_YIELD` 只挂在**重锚分支**上：帧耗时恰好等于 period 时
    /// `wait == 0` 属于「踩线」而非「落后」，此时应立即进入下一轮（周期仍严格
    /// = period），不该平白多加 0.5ms（那会把边界帧率压到 ~58fps）。
    fn tick(&mut self, now: std::time::Instant) -> std::time::Duration {
        let wait = self.next.saturating_duration_since(now);
        self.next += self.period;
        if self.next <= now {
            self.next = now + self.period;
            return MIN_YIELD;
        }
        wait
    }
}

fn frame_loop() {
    let fmt = FORMAT_DESC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .unwrap();
    // 彩条缓冲复用，避免每帧 8.3MB 分配
    let mut bars_buf = vec![0u8; (WIDTH as usize) * (HEIGHT as usize) * 4];
    let mut pacer = Pacer::new(std::time::Instant::now(), FRAME_PERIOD);
    // 上一轮是否在产帧：仅用于在状态翻转时打一条带 streamingClients 条数的日志，
    // 这是「产帧判定确实跟着客户端存亡走」的现场证据（强杀客户端时应当立刻看到
    // `暂停产帧 streamingClients=0`）。
    let mut producing = false;
    loop {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let stream = STREAM
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unwrap() as *mut NSObject;
            // 每轮都问一次权威列表：没有客户端在拉流时既不发帧也不烧 CPU，
            // 新客户端接入后下一轮就会自动恢复产帧。
            let streaming = streaming_client_count(stream);
            if should_produce(streaming) != producing {
                producing = !producing;
                elog(format!(
                    "产帧{} streamingClients={streaming}",
                    if producing { "开始" } else { "暂停" }
                ));
            }
            if should_produce(streaming) {
                // 优先注入帧（2s 新鲜窗口），否则回落 Rust 彩条
                let injected = frame_channel::take_fresh(std::time::Duration::from_secs(2));
                if let Some((data, w, h, stride, pts)) = injected {
                    // 设备侧滤镜（美颜/背景替换）；没配滤镜就走原来的直通
                    if filters::enabled() {
                        let mut buf = filters::repack(&data, w, h, stride);
                        filters::apply(&mut buf, w, h);
                        send_bgra(stream, fmt, &buf, w, h, w * 4, pts);
                    } else {
                        send_bgra(stream, fmt, &data, w, h, stride, pts);
                    }
                } else {
                    let now = unsafe { CFAbsoluteTimeGetCurrent() };
                    // SAFETY: bars_buf 长度恰为 WIDTH*HEIGHT*4，满足 C ABI 对 out 缓冲区的要求
                    let rc = unsafe {
                        vdev_camera::cabi::vdev_camera_render_bgra32(
                            0,
                            WIDTH as u32,
                            HEIGHT as u32,
                            now,
                            bars_buf.as_mut_ptr(),
                            bars_buf.len(),
                        )
                    };
                    if rc == 0 {
                        let pts = host_now_ns();
                        send_bgra(
                            stream,
                            fmt,
                            &bars_buf,
                            WIDTH as u32,
                            HEIGHT as u32,
                            WIDTH as u32 * 4,
                            pts,
                        );
                    }
                }
            }
        }));
        // 60fps 固定节拍：只补足到下一个 tick，不把帧耗时再叠加一个 period
        std::thread::sleep(pacer.tick(std::time::Instant::now()));
    }
}

// 主机时钟秒→纳秒换算：值域远小于 f64 精度边界与 u64 上限，饱和截断足够
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
fn host_now_ns() -> u64 {
    let clock = unsafe { CMClockGetHostTimeClock() };
    let t = unsafe { CMClockGetTime(clock) };
    (t.value as f64 / f64::from(t.timescale) * 1e9) as u64
}

// spike 的初始化序列（格式描述 → 流 → 三个 source → stream/device/provider →
// 接线 → 启动）长而线性，步骤注释按 1)~8) 编号连贯阅读，不拆函数。
#[allow(clippy::too_many_lines)]
fn main() {
    eprintln!("vdev-camera-ext: 启动（全 Rust spike）");
    // 日志服务器放最前：进程一启动就能读，任何提前退出都能看到
    log_server();
    elog("=== 启动 ===");
    std::panic::set_hook(Box::new(|info| {
        elog(format!("PANIC: {info}"));
    }));

    // 1) 创建流格式描述（1920x1080 BGRA @60）
    let mut fmt = CMFormatDescription(ptr::null_mut());
    let fst = unsafe {
        CMVideoFormatDescriptionCreate(
            ptr::null(),
            BGR_A,
            WIDTH,
            HEIGHT,
            ptr::null(),
            std::ptr::from_mut(&mut fmt),
        )
    };
    if fst != 0 || fmt.0.is_null() {
        eprintln!("vdev-camera-ext: CMVideoFormatDescriptionCreate 失败 {fst}");
        elog(format!("CMVideoFormatDescriptionCreate 失败 {fst}"));
        std::process::exit(1);
    }
    elog(format!("CMVideoFormatDescription OK fmt={:p}", fmt.0));
    *FORMAT_DESC
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(fmt);

    // 2) 流格式对象
    let sf_cls = class("CMIOExtensionStreamFormat");
    let dur = cmtime(1, FPS_TS);
    let sf: *mut NSObject = unsafe {
        msg_send![
            sf_cls,
            streamFormatWithFormatDescription: fmt.0,
            maxFrameDuration: dur,
            minFrameDuration: dur,
            validFrameDurations: ptr::null::<objc2_foundation::NSArray<NSObject>>()
        ]
    };
    let sf_ref: &NSObject = unsafe { &*sf };
    let sf: *mut NSObject = unsafe { msg_send![sf_ref, retain] };
    *STREAM_FORMAT
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sf as usize);

    // 3) 三个 source（进程生命周期持有）
    let provider_source_obj: Retained<ProviderSource> =
        unsafe { msg_send![ProviderSource::class(), new] };
    let device_source_obj: Retained<DeviceSource> =
        unsafe { msg_send![DeviceSource::class(), new] };
    let stream_source_obj: Retained<StreamSource> =
        unsafe { msg_send![StreamSource::class(), new] };
    let provider_source = std::ptr::from_ref(&*provider_source_obj)
        .cast::<NSObject>()
        .cast_mut();
    let device_source = std::ptr::from_ref(&*device_source_obj)
        .cast::<NSObject>()
        .cast_mut();
    let stream_source = std::ptr::from_ref(&*stream_source_obj)
        .cast::<NSObject>()
        .cast_mut();
    std::mem::forget(provider_source_obj);
    std::mem::forget(device_source_obj);
    std::mem::forget(stream_source_obj);

    // 4) stream
    let stream_cls = class("CMIOExtensionStream");
    let stream_id = NSUUID::new();
    let stream: *mut NSObject = unsafe {
        msg_send![
            stream_cls,
            streamWithLocalizedName: &*NSString::from_str("vdev-camera"),
            streamID: &*stream_id,
            direction: 0i64,
            clockType: 0i64,
            source: stream_source
        ]
    };
    let stream_ref: &NSObject = unsafe { &*stream };
    let stream: *mut NSObject = unsafe { msg_send![stream_ref, retain] };
    *STREAM
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(stream as usize);

    // 5) device
    let dev_cls = class("CMIOExtensionDevice");
    let device_id = NSUUID::new();
    let device: *mut NSObject = unsafe {
        msg_send![
            dev_cls,
            deviceWithLocalizedName: &*NSString::from_str("vdev-camera"),
            deviceID: &*device_id,
            legacyDeviceID: &*device_id.UUIDString(),
            source: device_source
        ]
    };
    let device_ref: &NSObject = unsafe { &*device };
    let device: *mut NSObject = unsafe { msg_send![device_ref, retain] };

    // 6) provider（clientQueue 用真实 dispatch queue，NULL 可能注册不上 XPC）
    let provider_cls = class("CMIOExtensionProvider");
    let queue = DispatchQueue::new("com.vdev.camera.ext.spike.provider", None);
    let queue_raw = queue.as_raw().as_ptr();
    std::mem::forget(queue);
    elog(format!("provider clientQueue={queue_raw:p}"));
    let provider: *mut NSObject = unsafe {
        msg_send![
            provider_cls,
            providerWithSource: provider_source,
            clientQueue: queue_raw
        ]
    };
    let provider_ref: &NSObject = unsafe { &*provider };
    let provider: *mut NSObject = unsafe { msg_send![provider_ref, retain] };

    // 7) 接线：addStream 先于 addDevice（macOS 26 顺序要求）
    let ok_add_stream: bool =
        unsafe { msg_send![device, addStream: stream, error: ptr::null_mut::<NSObject>()] };
    elog(format!("addStream -> {ok_add_stream}"));
    if !ok_add_stream {
        eprintln!("vdev-camera-ext: addStream 失败");
    }
    let ok_add_dev: bool =
        unsafe { msg_send![provider, addDevice: device, error: ptr::null_mut::<NSObject>()] };
    elog(format!("addDevice -> {ok_add_dev}"));
    if !ok_add_dev {
        eprintln!("vdev-camera-ext: addDevice 失败");
    }

    // 8) 帧线程 + 启动服务
    std::thread::spawn(frame_loop);
    let _: () = unsafe { msg_send![provider_cls, startServiceWithProvider: provider] };
    // 启动真实帧推流通道（宿主 App / 外部桥连 127.0.0.1:27890 推帧）
    frame_channel::start();
    // 配置通道（27892）：真机上扩展读不到用户写的任何文件，配置只能走网络
    frame_channel::start_control();
    // 设备侧滤镜状态进日志口：真机验收要看得到「启用/未配置」与配置来源
    if let Some(desc) = filters::describe() {
        eprintln!("vdev-camera-ext: 设备侧滤镜已启用（{desc}）");
        elog(format!("设备侧滤镜已启用（{desc}）"));
    } else {
        let line = format!("设备侧滤镜未配置（直通，来源={}）", filters::source());
        eprintln!("vdev-camera-ext: {line}");
        elog(line);
    }
    elog(format!("滤镜配置探测: {}", filters::probe_report()));
    eprintln!("vdev-camera-ext: 服务已启动，进入 runloop");
    elog("startService 完成，进入 runloop");
    unsafe { CFRunLoopRun() };
}

#[cfg(test)]
mod tests {
    use super::{
        client_connected, client_disconnected, copy_bgra_rows, evict_slot, find_pool, log_skip,
        should_produce, window_fps, Pacer, FRAME_PERIOD, MAX_POOLS, MIN_YIELD,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    #[test]
    fn bgra_rows_padded_src_stride_copies_only_valid_bytes() {
        // w=2 → 每行有效 8 字节；推流方 padded stride=12（每行 4 字节 pad 不拷）
        let src = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xAA, 0xAA, 0xAA, // row 0 + pad
            9, 10, 11, 12, 13, 14, 15, 16, 0xBB, 0xBB, 0xBB, 0xBB, // row 1 + pad
        ];
        let mut dst = vec![0u8; 8 * 2];
        copy_bgra_rows(&mut dst, &src, 2, 2, 12, 8);
        assert_eq!(
            dst,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]
        );
    }

    #[test]
    fn bgra_rows_dst_stride_narrower_than_src_no_panic() {
        // 修复的 panic 场景：stride=12 > dst_stride=8，旧实现最后一行越界 panic
        let src = [
            1, 2, 3, 4, 5, 6, 7, 8, 0xAA, 0xAA, 0xAA, 0xAA, //
            9, 10, 11, 12, 13, 14, 15, 16, 0xBB, 0xBB, 0xBB, 0xBB,
        ];
        let mut dst = vec![0u8; 8 * 2];
        copy_bgra_rows(&mut dst, &src, 2, 2, 12, 8); // 只拷有效 8 字节，不越界
        assert_eq!(dst[8..], [9, 10, 11, 12, 13, 14, 15, 16]);
    }

    #[test]
    fn bgra_rows_dst_stride_larger_lands_rows_at_dst_stride() {
        // CV 分配更大行距（dst_stride=16 > stride=8）：行落在 row*dst_stride，
        // 行间 pad 保持原值不写
        let src: Vec<u8> = (1..=16).collect(); // w=2,h=2,stride=8
        let mut dst = vec![0u8; 16 * 2];
        copy_bgra_rows(&mut dst, &src, 2, 2, 8, 16);
        assert_eq!(
            dst,
            [
                1, 2, 3, 4, 5, 6, 7, 8, 0, 0, 0, 0, 0, 0, 0, 0, //
                9, 10, 11, 12, 13, 14, 15, 16, 0, 0, 0, 0, 0, 0, 0, 0,
            ]
        );
    }

    #[test]
    fn bgra_rows_equal_strides_full_copy() {
        let src: Vec<u8> = (0..32).collect(); // w=2,h=4,stride=8
        let mut dst = vec![0u8; 32];
        copy_bgra_rows(&mut dst, &src, 2, 4, 8, 8);
        assert_eq!(dst, src);
    }

    #[test]
    fn log_skip_current_connection_continues_after_drain() {
        // 全局已产 600 行、buf 只留最近 500 行（对应全局 [100,600)）：
        // 连接已发 590 行 → 从 buf[490]（全局 590）续发，不再停滞
        assert_eq!(log_skip(590, 500, 600), 490);
    }

    #[test]
    fn log_skip_new_connection_gets_tail_from_head() {
        assert_eq!(log_skip(0, 500, 600), 0);
    }

    #[test]
    fn log_skip_lagging_connection_saturates_at_buffer_head() {
        // 已发行数落在已 drain 区间（10 < 100）：从 buf 头部续发（尽力而为）
        assert_eq!(log_skip(10, 500, 600), 0);
    }

    #[test]
    fn log_skip_no_drain_yet_reads_from_start() {
        assert_eq!(log_skip(0, 10, 10), 0);
        assert_eq!(log_skip(3, 10, 10), 3);
    }

    // ---- 60fps 节拍（issue #51）----

    #[test]
    fn pacer_steady_state_holds_period_despite_frame_work_time() {
        // 帧生成耗时 14ms：真机扩展流式期间 CPU 实测 ~12–14ms/帧
        let work = Duration::from_millis(14);
        let t0 = Instant::now();
        let mut pacer = Pacer::new(t0, FRAME_PERIOD);
        let mut now = t0;
        let mut starts = Vec::new();
        for _ in 0..120 {
            starts.push(now);
            now += work; // 产一帧
            now += pacer.tick(now); // 睡到下一个 tick
        }
        let span = starts[starts.len() - 1].saturating_duration_since(starts[0]);
        #[allow(clippy::cast_possible_truncation)]
        let avg = span / (starts.len() as u32 - 1);
        assert!(
            avg.abs_diff(FRAME_PERIOD) <= Duration::from_micros(1),
            "平均帧间隔 {avg:?} 应严格等于 {FRAME_PERIOD:?}（60fps）；\n\
             若得到 ~30.7ms 说明 sleep 是「帧耗时之上再加一个 period」（旧实现）"
        );
        // 顺带钉住量级：60fps 与 32fps 必须能被区分
        assert!(avg < Duration::from_millis(20), "平均帧间隔 {avg:?} 仍偏大");
    }

    #[test]
    fn pacer_at_exact_period_work_keeps_full_rate() {
        // 边界：帧耗时恰好 == period。若把「踩线」（wait==0）也当成落后而加
        // MIN_YIELD，周期会被抬到 17.166ms ≈ 58.25fps；正确语义下应立即进入
        // 下一轮，周期仍严格 = period。
        let t0 = Instant::now();
        let mut pacer = Pacer::new(t0, FRAME_PERIOD);
        let mut now = t0;
        let mut starts = Vec::new();
        for _ in 0..60 {
            starts.push(now);
            now += FRAME_PERIOD; // 产一帧，耗时恰好一个周期
            now += pacer.tick(now);
        }
        let span = starts[starts.len() - 1].saturating_duration_since(starts[0]);
        #[allow(clippy::cast_possible_truncation)]
        let avg = span / (starts.len() as u32 - 1);
        assert!(
            avg.abs_diff(FRAME_PERIOD) <= Duration::from_micros(1),
            "平均帧间隔 {avg:?} 应等于 {FRAME_PERIOD:?}（边界处不应多付 {MIN_YIELD:?}）"
        );
    }

    #[test]
    fn pacer_overload_degrades_gracefully_without_catch_up_burst() {
        // 帧生成 25ms > 16.67ms：不追赶、不补帧，退化为「帧耗时 + 最小让出」
        let work = Duration::from_millis(25);
        let t0 = Instant::now();
        let mut pacer = Pacer::new(t0, FRAME_PERIOD);
        let mut now = t0;
        let mut starts = Vec::new();
        for _ in 0..8 {
            starts.push(now);
            now += work;
            now += pacer.tick(now);
        }
        for pair in starts.windows(2) {
            let d = pair[1].saturating_duration_since(pair[0]);
            assert!(d >= work, "帧间隔 {d:?} 不应短于帧生成耗时");
            assert!(
                d <= work + Duration::from_millis(1),
                "帧间隔 {d:?} 被额外 sleep / 补帧风暴放大（应约 {work:?} + {MIN_YIELD:?}）"
            );
        }
    }

    // ---- 产帧生命周期（issue #52）----

    // ---- 像素缓冲池（issue #54-1）----

    #[test]
    fn find_pool_matches_exact_size_only() {
        let keys = [(1920u32, 1080u32), (640, 480)];
        assert_eq!(find_pool(&keys, 1920, 1080), Some(0));
        assert_eq!(find_pool(&keys, 640, 480), Some(1));
        assert_eq!(find_pool(&keys, 1280, 720), None);
        // 宽高颠倒不算命中：否则会把 1080x1920 的帧塞进 1920x1080 的池
        assert_eq!(find_pool(&keys, 1080, 1920), None);
    }

    #[test]
    fn pool_cache_is_fifo_and_stays_bounded() {
        // 模拟 pool_for 的缓存策略：命中复用、未满追加、满则淘汰队头。
        // 尺寸是推流方给的，若不封顶，反复换尺寸会让池数量（与 IOSurface 内存）
        // 无限增长——这条断言就是那个上限。
        let mut keys: Vec<(u32, u32)> = Vec::new();
        for i in 0..12u32 {
            let (w, h) = (640 + i, 480 + i);
            if find_pool(&keys, w, h).is_none() {
                if let Some(slot) = evict_slot(keys.len(), MAX_POOLS) {
                    keys.remove(slot);
                }
                keys.push((w, h));
            }
            assert!(
                keys.len() <= MAX_POOLS,
                "池数量 {} 超过上限 {MAX_POOLS}（内存会随尺寸爆炸）",
                keys.len()
            );
            assert!(find_pool(&keys, w, h).is_some(), "刚放入的尺寸必须命中");
        }
        assert_eq!(keys.len(), MAX_POOLS);
        // FIFO：最早插入的尺寸已被淘汰，最近的还在
        assert_eq!(find_pool(&keys, 640, 480), None, "队头应被淘汰");
        assert!(find_pool(&keys, 641, 481).is_none(), "队头之后一个也应淘汰");
        assert!(find_pool(&keys, 651, 491).is_some(), "最新尺寸必须在缓存里");
    }

    #[test]
    fn evict_slot_only_when_full() {
        assert_eq!(evict_slot(0, MAX_POOLS), None);
        assert_eq!(evict_slot(MAX_POOLS - 1, MAX_POOLS), None);
        assert_eq!(evict_slot(MAX_POOLS, MAX_POOLS), Some(0));
        assert_eq!(evict_slot(MAX_POOLS + 3, MAX_POOLS), Some(0));
    }

    #[test]
    fn produce_only_when_a_client_is_streaming() {
        // 判据是权威列表 `streamingClients` 的条数：0 → 停帧，≥1 → 产帧。
        // 旧实现看的是 startStream 置的 RUNNING——强杀后它永久为真，于是
        // 「没有客户端还在拉流」也会满速产帧（真机 32fps 空转 + 41.5% CPU）。
        assert!(!should_produce(0), "没有客户端在拉流时必须停帧");
        assert!(should_produce(1));
        assert!(should_produce(2));
    }

    #[test]
    fn client_counter_is_logged_only_and_saturates() {
        // CLIENTS 只用于日志（真机实测回调不总是配对），但计数器本身不能回绕：
        // 未配对的 disconnect 必须饱和在 0，否则日志会打印天文数字。
        let clients = AtomicUsize::new(0);
        assert_eq!(client_connected(&clients), 1);
        assert_eq!(client_connected(&clients), 2);
        assert_eq!(client_disconnected(&clients), 1);
        assert_eq!(client_disconnected(&clients), 0);
        assert_eq!(client_disconnected(&clients), 0);
        assert_eq!(client_disconnected(&clients), 0);
        assert_eq!(clients.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn window_fps_reports_window_rate_and_rejects_bad_input() {
        let t0 = Instant::now();
        // 300 帧 / 5s = 60fps
        let fps = window_fps(t0, 0, t0 + Duration::from_secs(5), 300);
        assert!((fps - 60.0).abs() < 0.01, "fps={fps}");
        // 非法输入：零窗口 / 非递增计数 → 0（不产生 inf/NaN 污染日志）
        let is_zero = |fps: f64| fps.abs() < f64::EPSILON;
        assert!(is_zero(window_fps(t0, 0, t0, 300)));
        assert!(is_zero(window_fps(
            t0,
            300,
            t0 + Duration::from_secs(1),
            300
        )));
        assert!(is_zero(window_fps(
            t0,
            300,
            t0 + Duration::from_secs(1),
            100
        )));
    }
}
