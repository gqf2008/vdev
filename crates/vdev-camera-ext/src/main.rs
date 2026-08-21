//! 全 Rust CMIOExtension spike：最小虚拟摄像头（只出彩条）。
//! 手写 CMIOExtension ObjC 绑定（cmio.rs），帧管线用 CoreVideo/CoreMedia C FFI。

mod cmio;
mod frame_channel;

use cmio::{
    property_set, CMIOExtensionDeviceSource, CMIOExtensionProviderSource,
    CMIOExtensionStreamSource,
};
use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2_foundation::NSObjectProtocol;
use objc2::ClassType;
use objc2_core_media::CMTime;
use dispatch2::{DispatchObject, DispatchQueue};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSSet, NSString, NSUUID};
use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static LOG_BUF: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn log_server() {
    std::thread::spawn(move || {
        let listener = match std::net::TcpListener::bind("127.0.0.1:27891") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("log_server bind 失败: {}", e);
                return;
            }
        };
        for conn in listener.incoming() {
            let Ok(stream) = conn else { continue };
            std::thread::spawn(move || {
            let mut stream = stream;
            let mut sent = 0usize;
            loop {
                let lines: Vec<String> = {
                    let buf = LOG_BUF.lock().unwrap_or_else(|e| e.into_inner());
                    buf.iter().skip(sent).cloned().collect()
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
    let line = format!("[unix={}] {}\n", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0), msg.as_ref());
    {
        let mut buf = LOG_BUF.lock().unwrap_or_else(|e| e.into_inner());
        buf.push(line.clone());
        let over = buf.len().saturating_sub(500);
        if over > 0 { buf.drain(0..over); }
    }
    let mut paths = Vec::new();
    // App Group 共享容器（扩展有 app-group 权限，最可能可写）
    paths.push(format!(
        "{}/Library/Group Containers/XFXU84HVK3.com.vdev.camera/vdev-camera-ext.log",
        std::env::var("HOME").unwrap_or_else(|_| "/Users/sqb".to_string())
    ));
    if let Ok(h) = std::env::var("HOME") {
        paths.push(format!("{}/vdev-camera-ext.log", h));
    }
    paths.push(format!(
        "{}/Library/Containers/com.vdev.camera.ext.spike/Data/vdev-camera-ext.log",
        std::env::var("USER").map(|_| "/Users/sqb".to_string()).unwrap_or_else(|_| String::new())
    ));
    paths.push("/tmp/vdev-camera-ext.log".to_string());
    paths.push("/var/tmp/vdev-camera-ext.log".to_string());
    for path in paths {
        if std::fs::OpenOptions::new().create(true).append(true).open(&path)
            .and_then(|mut f| f.write_all(line.as_bytes())).is_ok()
        {
            break;
        }
    }
    eprintln!("{}", line.trim_end());
}

const WIDTH: i32 = 1920;
const HEIGHT: i32 = 1080;
const FPS: i64 = 60;
const BGR_A: u32 = 0x42475241; // kCVPixelFormatType_32BGRA / kCMVideoCodecType_32BGRA

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
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRun();
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: bool) -> i32;
}

extern "C" {
    fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
    fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut std::ffi::c_void;
}

// ---------------- 共享状态（进程生命周期） ----------------
static STREAM: Mutex<Option<usize>> = Mutex::new(None);
static STREAM_FORMAT: Mutex<Option<usize>> = Mutex::new(None);
static FORMAT_DESC: Mutex<Option<CMFormatDescription>> = Mutex::new(None);
static RUNNING: AtomicBool = AtomicBool::new(false);
static SENT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
            elog("provider: connectClient");
            true
        }

        #[unsafe(method(disconnectClient:))]
        unsafe fn disconnectClient(&self, _client: &NSObject) {}

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
            let empty: Retained<NSDictionary<NSObject, NSObject>> =
                NSDictionary::new();
            let p: *mut NSObject = msg_send![cls, providerPropertiesWithDictionary: &*empty];
            let _: () = msg_send![&*p, setName: &*NSString::from_str("vdev-camera")];
            let _: () = msg_send![&*p, setManufacturer: &*NSString::from_str("vdev")];
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
            let _: () = msg_send![&*p, setTransportType: &*NSNumber::numberWithUnsignedInt(0x7674726e)];
            let _: () = msg_send![&*p, setModel: &*NSString::from_str("vdev-camera")];
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
            let fmt = STREAM_FORMAT.lock().unwrap_or_else(|e| e.into_inner()).unwrap() as *mut NSObject;
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
            let _: () = msg_send![&*p, setActiveFormatIndex: &*NSNumber::numberWithInt(0)];
            let dur = cmtime(1, FPS as i32);
            let dict = CMTimeCopyAsDictionary(dur, ptr::null());
            let _: () = msg_send![&*p, setFrameDuration: &*(dict as *const NSObject)];
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
            RUNNING.store(true, Ordering::SeqCst);
            true
        }

        #[unsafe(method(stopStreamAndReturnError:))]
        unsafe fn stopStreamAndReturnError(&self, _out_error: *mut *mut NSObject) -> bool {
            elog("stream: stopStream");
            RUNNING.store(false, Ordering::SeqCst);
            true
        }
    }
);

// ---------------- 帧循环 ----------------
/// 把一帧 BGRA 数据包成 CMSampleBuffer 发给流。
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
        let attrs: Retained<NSDictionary<NSObject, NSObject>> = unsafe {
            msg_send![
                <NSDictionary<NSObject, NSObject>>::class(),
                dictionaryWithObject: &*empty_dict,
                forKey: &*iosurf_key
            ]
        };
        let mut pb = CVPixelBuffer(ptr::null_mut());
        let st = CVPixelBufferCreate(
            ptr::null(),
            w as usize,
            h as usize,
            BGR_A,
            &*attrs as *const _ as *const c_void,
            &mut pb,
        );
        if st != 0 || pb.0.is_null() {
            elog(format!("CVPixelBufferCreate 失败 st={} {}x{}", st, w, h));
            return;
        }
        CVPixelBufferLockBaseAddress(pb, 0);
        let base = CVPixelBufferGetBaseAddress(pb);
        let dst_stride = CVPixelBufferGetBytesPerRow(pb);
        if !base.is_null() && data.len() >= (stride as usize) * (h as usize) {
            let dst = std::slice::from_raw_parts_mut(base as *mut u8, dst_stride * h as usize);
            if stride as usize == dst_stride {
                dst[..data.len()].copy_from_slice(&data[..(stride as usize) * (h as usize)]);
            } else {
                for row in 0..h as usize {
                    let src = &data[row * stride as usize..(row + 1) * stride as usize];
                    dst[row * dst_stride..row * dst_stride + src.len()].copy_from_slice(src);
                }
            }
        }
        CVPixelBufferUnlockBaseAddress(pb, 0);

        let mut timing = CMSampleTimingInfo {
            duration: cmtime(1, FPS as i32),
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
            &mut timing,
            &mut sb,
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
                elog(format!("sendSampleBuffer 异常: {:?}", ex));
            }
            SENT.fetch_add(1, Ordering::SeqCst);
            let n = SENT.load(Ordering::SeqCst);
            if n % 300 == 0 || n == 1 {
                elog(format!("sendSampleBuffer #{}", n));
            }
            CFRelease(sb.0 as *const c_void);
        } else {
            elog(format!("CMSampleBufferCreateForImageBuffer 失败 st={}", ss));
        }
        CFRelease(pb.0 as *const c_void);
    }
}

fn frame_loop() {
    let fmt = FORMAT_DESC.lock().unwrap_or_else(|e| e.into_inner()).unwrap();
    let mut last_sent: std::time::Instant = std::time::Instant::now();
    // 彩条缓冲复用，避免每帧 8.3MB 分配
    let mut bars_buf = vec![0u8; (WIDTH as usize) * (HEIGHT as usize) * 4];
    loop {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if RUNNING.load(Ordering::SeqCst) {
                let stream = STREAM.lock().unwrap_or_else(|e| e.into_inner()).unwrap() as *mut NSObject;
                // 优先注入帧（2s 新鲜窗口），否则回落 Rust 彩条
                let injected = frame_channel::take_fresh(std::time::Duration::from_secs(2));
                if let Some((data, w, h, stride, pts)) = injected {
                    send_bgra(stream, fmt, &data, w, h, stride, pts);
                } else {
                    let rc = vdev_camera::cabi::vdev_camera_render_bgra32(
                        0,
                        WIDTH as u32,
                        HEIGHT as u32,
                        unsafe { CFAbsoluteTimeGetCurrent() },
                        bars_buf.as_mut_ptr(),
                        bars_buf.len(),
                    );
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
        last_sent = std::time::Instant::now();
        // 60fps 节拍
        let elapsed = last_sent.elapsed();
        let target = std::time::Duration::from_micros(16_666);
        if elapsed < target {
            std::thread::sleep(target - elapsed);
        } else {
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
    }
}

fn host_now_ns() -> u64 {
    unsafe {
        let t = CMClockGetTime(CMClockGetHostTimeClock());
        (t.value as f64 / t.timescale as f64 * 1e9) as u64
    }
}

fn main() {
    eprintln!("vdev-camera-ext: 启动（全 Rust spike）");
    // 日志服务器放最前：进程一启动就能读，任何提前退出都能看到
    log_server();
    elog("=== 启动 ===");
    std::panic::set_hook(Box::new(|info| {
        elog(format!("PANIC: {}", info));
    }));

    // 1) 创建流格式描述（1920x1080 BGRA @60）
    let mut fmt = CMFormatDescription(ptr::null_mut());
    let fst = unsafe {
        CMVideoFormatDescriptionCreate(ptr::null(), BGR_A, WIDTH, HEIGHT, ptr::null(), &mut fmt)
    };
    if fst != 0 || fmt.0.is_null() {
        eprintln!("vdev-camera-ext: CMVideoFormatDescriptionCreate 失败 {fst}");
        elog(format!("CMVideoFormatDescriptionCreate 失败 {fst}"));
        std::process::exit(1);
    }
    elog(format!("CMVideoFormatDescription OK fmt={:p}", fmt.0));
    *FORMAT_DESC.lock().unwrap_or_else(|e| e.into_inner()) = Some(fmt);

    // 2) 流格式对象
    let sf_cls = class("CMIOExtensionStreamFormat");
    let dur = cmtime(1, FPS as i32);
    let sf: *mut NSObject = unsafe {
        msg_send![
            sf_cls,
            streamFormatWithFormatDescription: fmt.0,
            maxFrameDuration: dur,
            minFrameDuration: dur,
            validFrameDurations: ptr::null::<objc2_foundation::NSArray<NSObject>>()
        ]
    };
    let sf: *mut NSObject = unsafe { msg_send![&*sf, retain] };
    *STREAM_FORMAT.lock().unwrap_or_else(|e| e.into_inner()) = Some(sf as usize);

    // 3) 三个 source（进程生命周期持有）
    let provider_source_obj: Retained<ProviderSource> = unsafe { msg_send![ProviderSource::class(), new] };
    let device_source_obj: Retained<DeviceSource> = unsafe { msg_send![DeviceSource::class(), new] };
    let stream_source_obj: Retained<StreamSource> = unsafe { msg_send![StreamSource::class(), new] };
    let provider_source = &*provider_source_obj as *const ProviderSource as *mut NSObject;
    let device_source = &*device_source_obj as *const DeviceSource as *mut NSObject;
    let stream_source = &*stream_source_obj as *const StreamSource as *mut NSObject;
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
    let stream: *mut NSObject = unsafe { msg_send![&*stream, retain] };
    *STREAM.lock().unwrap_or_else(|e| e.into_inner()) = Some(stream as usize);

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
    let device: *mut NSObject = unsafe { msg_send![&*device, retain] };

    // 6) provider（clientQueue 用真实 dispatch queue，NULL 可能注册不上 XPC）
    let provider_cls = class("CMIOExtensionProvider");
    let queue = DispatchQueue::new("com.vdev.camera.ext.spike.provider", None);
    let queue_raw = queue.as_raw().as_ptr();
    std::mem::forget(queue);
    elog(format!("provider clientQueue={:p}", queue_raw));
    let provider: *mut NSObject = unsafe {
        msg_send![
            provider_cls,
            providerWithSource: provider_source,
            clientQueue: queue_raw
        ]
    };
    let provider: *mut NSObject = unsafe { msg_send![&*provider, retain] };

    // 7) 接线：addStream 先于 addDevice（macOS 26 顺序要求）
    let ok_add_stream: bool = unsafe { msg_send![&*device, addStream: &*stream, error: ptr::null_mut::<NSObject>()] };
    elog(format!("addStream -> {}", ok_add_stream));
    if !ok_add_stream {
        eprintln!("vdev-camera-ext: addStream 失败");
    }
    let ok_add_dev: bool = unsafe { msg_send![&*provider, addDevice: &*device, error: ptr::null_mut::<NSObject>()] };
    elog(format!("addDevice -> {}", ok_add_dev));
    if !ok_add_dev {
        eprintln!("vdev-camera-ext: addDevice 失败");
    }

    // 8) 帧线程 + 启动服务
    std::thread::spawn(frame_loop);
    unsafe {
        let _: () = msg_send![provider_cls, startServiceWithProvider: &*provider];
    }
    // 启动真实帧推流通道（宿主 App 连 127.0.0.1:27890 推帧）
    frame_channel::start();
    eprintln!("vdev-camera-ext: 服务已启动，进入 runloop");
    elog("startService 完成，进入 runloop");
    unsafe { CFRunLoopRun() };
}
