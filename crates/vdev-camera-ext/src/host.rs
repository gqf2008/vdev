//! vdev-spike-host：激活 spike 扩展的最小宿主。
//! 只做一件事：对 com.vdev.camera.ext.spike 提交 `OSSystemExtensionRequest` 激活，
//! 然后转 runloop 等用户批准（120s）。

use dispatch2::{DispatchObject, DispatchQueue};
use objc2::define_class;
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2::ClassType;
use objc2_foundation::{NSInteger, NSString};
use std::ffi::{c_void, CString};
use std::sync::{Mutex, OnceLock};

const SPIKE_BUNDLE_ID: &str = "com.vdev.camera.ext.spike";

/// 拆出类型别名，避免 clippy `type_complexity` 警告。
type HostCallback = Box<dyn FnMut(String) + Send>;
static CALLBACK: OnceLock<Mutex<Option<HostCallback>>> = OnceLock::new();
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
    fn CFRunLoopRunInMode(
        mode: *const c_void,
        seconds: f64,
        return_after_source_handled: bool,
    ) -> i32;
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
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            fire(format!("完成 result={result}"));
        }

        #[unsafe(method(request:didFailWithError:))]
        fn did_fail(&self, _req: &AnyObject, error: &AnyObject) {
            fire(format!("失败: {}", error_description(error)));
        }
    }
);

fn ensure_delegate() -> *mut AnyObject {
    let mut p = DELEGATE_PTR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
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
    println!(
        "spike-host: 提交{}请求 {bundle_id}",
        if deactivate { "停用" } else { "激活" }
    );

    *CALLBACK.get_or_init(|| Mutex::new(None)).lock().unwrap() = Some(Box::new(|_| {}));

    let delegate = ensure_delegate();
    let manager = class("OSSystemExtensionManager");
    let shared: *mut AnyObject = unsafe { msg_send![manager, sharedManager] };
    if shared.is_null() {
        eprintln!("spike-host: OSSystemExtensionManager 不可用");
        std::process::exit(1);
    }

    let req_cls = class("OSSystemExtensionRequest");
    let id = NSString::from_str(&bundle_id);
    // SAFETY: req_cls/id 为合法 ObjC 类与字符串对象，queue 为进程生命周期持有的
    // dispatch queue；selector 按 Apple 文档二选一（激活/停用请求）。
    // 无法单测：OSSystemExtensionManager 请求依赖真实系统扩展框架与用户授权流程。
    let req: *mut AnyObject = if deactivate {
        // 停用必须走 deactivationRequestForExtension:queue:——此前无条件提交
        // 激活请求，--deactivate 只影响了打印文案，实际从未停用
        unsafe { msg_send![req_cls, deactivationRequestForExtension: &*id, queue: sysext_queue()] }
    } else {
        unsafe { msg_send![req_cls, activationRequestForExtension: &*id, queue: sysext_queue()] }
    };
    if req.is_null() {
        eprintln!("spike-host: 创建 OSSystemExtensionRequest 失败");
        std::process::exit(1);
    }
    let req: *mut AnyObject = unsafe { msg_send![req, retain] };
    // SAFETY: req/shared 为刚创建并 retain 的对象指针，ObjC 消息逐条发送
    let _: () = unsafe { msg_send![req, setDelegate: delegate] };
    let _: () = unsafe { msg_send![shared, submitRequest: req] };
    *REQUEST_PTR
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = req as usize;

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
