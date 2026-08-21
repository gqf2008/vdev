//! vdev-spike-host：激活 spike 扩展的最小宿主。
//! 只做一件事：对 com.vdev.camera.ext.spike 提交 OSSystemExtensionRequest 激活，
//! 然后转 runloop 等用户批准（120s）。

use dispatch2::{DispatchObject, DispatchQueue};
use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::ClassType;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2_foundation::{NSInteger, NSString};
use std::ffi::{c_void, CString};
use std::sync::{Mutex, OnceLock};

const SPIKE_BUNDLE_ID: &str = "com.vdev.camera.ext.spike";

static CALLBACK: OnceLock<Mutex<Option<Box<dyn FnMut(String) + Send>>>> = OnceLock::new();
static DELEGATE_PTR: Mutex<usize> = Mutex::new(0);
static REQUEST_PTR: Mutex<usize> = Mutex::new(0);
static SYSEXT_QUEUE: OnceLock<usize> = OnceLock::new();

fn sysext_queue() -> *mut c_void {
    let p = SYSEXT_QUEUE.get_or_init(|| {
        let q = DispatchQueue::new("com.vdev.camera.spike", None);
        let raw = q.as_raw().as_ptr() as usize;
        std::mem::forget(q); // 进程生命周期持有
        raw
    });
    *p as *mut c_void
}

#[link(name = "SystemExtensions", kind = "framework")]
extern "C" {}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: bool)
        -> i32;
}

fn class(name: &str) -> &'static AnyClass {
    let cname = CString::new(name).unwrap();
    AnyClass::get(&cname).expect("ObjC class not found")
}

fn fire(msg: String) {
    println!("spike-host: {msg}");
    if let Some(cb) = CALLBACK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_mut()
    {
        cb(msg);
    }
}

fn error_description(err: &AnyObject) -> String {
    unsafe {
        let s: Retained<NSString> = msg_send![err, localizedDescription];
        s.to_string()
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VdevSpikeSysextDelegate"]
    #[ivars = ()]
    struct SysextDelegate;

    impl SysextDelegate {
        #[unsafe(method(request:actionForReplacingExtension:withExtension:))]
        fn action_for_replacing(
            &self,
            _req: &AnyObject,
            _existing: &AnyObject,
            _ext: &AnyObject,
        ) -> NSInteger {
            1 // OSSystemExtensionReplacementActionReplace
        }

        #[unsafe(method(requestNeedsUserApproval:))]
        fn needs_approval(&self, _req: &AnyObject) {
            fire("需要批准：系统设置 → 通用 → 登录项与扩展 → 扩展 → 按类别 → 相机扩展 → 打开 vdev-camera-spike".to_string());
        }

        #[unsafe(method(request:didFinishWithResult:))]
        fn did_finish(&self, _req: &AnyObject, result: NSInteger) {
            fire(format!("完成 result={}", result));
        }

        #[unsafe(method(request:didFailWithError:))]
        fn did_fail(&self, _req: &AnyObject, error: &AnyObject) {
            fire(format!("失败: {}", error_description(error)));
        }
    }
);

fn ensure_delegate() -> *mut AnyObject {
    let mut p = DELEGATE_PTR.lock().unwrap_or_else(|e| e.into_inner());
    if *p == 0 {
        let obj: *mut AnyObject = unsafe { msg_send![SysextDelegate::class(), new] };
        *p = obj as usize;
    }
    *p as *mut AnyObject
}

fn main() {
    // 支持命令行覆盖 bundle id（Swift 对照实验用）；--deactivate 则停用
    let args: Vec<String> = std::env::args().collect();
    let deactivate = args.iter().any(|a| a == "--deactivate");
    let bundle_id = args
        .iter()
        .find(|a| !a.starts_with('-') && a.as_str() != args[0].as_str())
        .cloned()
        .unwrap_or_else(|| SPIKE_BUNDLE_ID.to_string());
    println!("spike-host: 提交{}请求 {}", if deactivate { "停用" } else { "激活" }, bundle_id);

    *CALLBACK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some(Box::new(|_| {}));

    let delegate = ensure_delegate();
    let manager = class("OSSystemExtensionManager");
    let shared: *mut AnyObject = unsafe { msg_send![manager, sharedManager] };
    if shared.is_null() {
        eprintln!("spike-host: OSSystemExtensionManager 不可用");
        std::process::exit(1);
    }

    let req_cls = class("OSSystemExtensionRequest");
    let id = NSString::from_str(&bundle_id);
    let req: *mut AnyObject = unsafe {
        msg_send![req_cls, activationRequestForExtension: &*id, queue: sysext_queue()]
    };
    if req.is_null() {
        eprintln!("spike-host: 创建 OSSystemExtensionRequest 失败");
        std::process::exit(1);
    }
    let req: *mut AnyObject = unsafe { msg_send![&*req, retain] };
    unsafe {
        let _: () = msg_send![&*req, setDelegate: &*delegate];
        let _: () = msg_send![&*shared, submitRequest: &*req];
    }
    *REQUEST_PTR.lock().unwrap_or_else(|e| e.into_inner()) = req as usize;

    // 打开系统设置（相机扩展页）
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.ExtensionsPreferences")
        .spawn();

    println!("spike-host: 等待批准，转 runloop 120s…");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.2, true);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    println!("spike-host: 超时退出。可重跑安装器或检查 系统设置 → 相机扩展");
}
