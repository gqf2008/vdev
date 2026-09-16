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
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSSet, NSString, NSUUID};
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
fn streaming_client_count(stream: *mut NSObject) -> usize {
    let stream_ref: &NSObject = unsafe { &*stream };
    let arr: *const NSArray<NSObject> = unsafe { msg_send![stream_ref, streamingClients] };
    if arr.is_null() {
        return 0;
    }
    unsafe { msg_send![arr, count] }
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
            let n = client_disconnected(&CLIENTS);
            if n == 0 {
                // 客户端可能已被强杀（不会补发 stopStream）：此处是唯一的兜底停帧点
                elog("provider: disconnectClient clients=0 → 暂停产帧");
            } else {
                elog(format!("provider: disconnectClient clients={n}"));
            }
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
    unsafe {
        let iosurf_key = NSString::from_str("IOSurfaceProperties");
        let empty_dict: Retained<NSDictionary<NSObject, NSObject>> = NSDictionary::new();
        let attrs: Retained<NSDictionary<NSObject, NSObject>> = msg_send![
            <NSDictionary<NSObject, NSObject>>::class(),
            dictionaryWithObject: &*empty_dict,
            forKey: &*iosurf_key
        ];
        let mut pb = CVPixelBuffer(ptr::null_mut());
        let st = CVPixelBufferCreate(
            ptr::null(),
            w as usize,
            h as usize,
            BGR_A,
            std::ptr::from_ref(&*attrs).cast::<c_void>(),
            std::ptr::from_mut(&mut pb),
        );
        if st != 0 || pb.0.is_null() {
            elog(format!("CVPixelBufferCreate 失败 st={st} {w}x{h}"));
            return;
        }
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
    fn tick(&mut self, now: std::time::Instant) -> std::time::Duration {
        let wait = self.next.saturating_duration_since(now);
        self.next += self.period;
        if self.next <= now {
            self.next = now + self.period;
        }
        if wait.is_zero() {
            MIN_YIELD
        } else {
            wait
        }
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
    loop {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let stream = STREAM
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .unwrap() as *mut NSObject;
            // 每轮都问一次权威列表：没有客户端在拉流时既不发帧也不烧 CPU，
            // 新客户端接入后下一轮就会自动恢复产帧。
            if should_produce(streaming_client_count(stream)) {
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
    if let Some(desc) = filters::describe() {
        eprintln!("vdev-camera-ext: 设备侧滤镜已启用（{desc}）");
    }
    eprintln!("vdev-camera-ext: 服务已启动，进入 runloop");
    elog("startService 完成，进入 runloop");
    unsafe { CFRunLoopRun() };
}

#[cfg(test)]
mod tests {
    use super::{
        client_connected, client_disconnected, copy_bgra_rows, log_skip, should_produce,
        window_fps, Pacer, FRAME_PERIOD, MIN_YIELD,
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
