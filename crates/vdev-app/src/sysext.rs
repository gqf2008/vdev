//! SystemExtensions FFI：激活/停用摄像头扩展。
//! 使用 objc2 的 define_class! 实现 OSSystemExtensionRequestDelegate。
use anyhow::{anyhow, Result};
use dispatch2::{DispatchObject, DispatchQueue};
use objc2::define_class;
use objc2::msg_send;
use objc2::ClassType;
use objc2::runtime::{AnyClass, AnyObject, NSObject};
use objc2_foundation::{NSInteger, NSString};
use std::ffi::{c_void, CString};
use std::sync::{Mutex, OnceLock};

pub enum SysextEvent {
    NeedsApproval,
    Finished(u32),
    Failed(String),
}

type Cb = Box<dyn FnMut(SysextEvent) + Send>;

static CALLBACK: OnceLock<Mutex<Option<Cb>>> = OnceLock::new();
// 委托对象与请求对象：进程生命周期内持有（泄露即可），存裸指针避免 Send 约束
static DELEGATE_PTR: Mutex<usize> = Mutex::new(0);
static REQUEST_PTR: Mutex<usize> = Mutex::new(0);

// 确保 SystemExtensions 框架被 dyld 加载（否则 ObjC 类未注册）
#[link(name = "SystemExtensions", kind = "framework")]
extern "C" {}

fn class(name: &str) -> Result<&'static AnyClass> {
    let cname = CString::new(name).expect("class name has no NUL");
    AnyClass::get(&cname).ok_or_else(|| anyhow!("ObjC class not found: {name}"))
}

fn fire(ev: SysextEvent) {
    if let Some(cb) = CALLBACK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap()
        .as_mut()
    {
        cb(ev);
    }
}

fn error_description(err: &AnyObject) -> String {
    unsafe {
        let s: *mut NSString = msg_send![err, localizedDescription];
        if s.is_null() {
            "未知错误".to_string()
        } else {
            let retained = objc2::rc::Retained::from_raw(s).unwrap();
            retained.to_string()
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "VDCameraSysextDelegate"]
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
            fire(SysextEvent::NeedsApproval);
        }

        #[unsafe(method(request:didFinishWithResult:))]
        fn did_finish(&self, _req: &AnyObject, result: NSInteger) {
            fire(SysextEvent::Finished(result as u32));
        }

        #[unsafe(method(request:didFailWithError:))]
        fn did_fail(&self, _req: &AnyObject, error: &AnyObject) {
            fire(SysextEvent::Failed(error_description(error)));
        }
    }
);

static SYSEXT_QUEUE: OnceLock<usize> = OnceLock::new();

/// 激活/停用请求的 delegate 回调队列（串行，进程生命周期内持有）。
fn sysext_queue() -> *mut c_void {
    let p = SYSEXT_QUEUE.get_or_init(|| {
        let q = DispatchQueue::new("com.vdev.camera.sysext", None);
        let raw = q.as_raw().as_ptr() as usize;
        std::mem::forget(q); // 保持队列存活
        raw
    });
    *p as *mut c_void
}

fn ensure_delegate() -> *mut AnyObject {
    let mut p = DELEGATE_PTR.lock().unwrap();
    if *p == 0 {
        let obj: *mut AnyObject = unsafe { msg_send![SysextDelegate::class(), new] };
        *p = obj as usize;
    }
    *p as *mut AnyObject
}

/// 提交激活/停用请求。bundle_id 例如 "com.vdev.camera.host.extension"。
pub fn submit(bundle_id: &str, activation: bool, cb: Cb) -> Result<()> {
    *CALLBACK
        .get_or_init(|| Mutex::new(None))
        .lock()
        .unwrap() = Some(cb);

    let delegate = ensure_delegate();

    let manager = class("OSSystemExtensionManager")?;
    let shared: *mut AnyObject = unsafe { msg_send![manager, sharedManager] };
    if shared.is_null() {
        return Err(anyhow!("OSSystemExtensionManager 不可用"));
    }

    let req_cls = class("OSSystemExtensionRequest")?;
    let id = NSString::from_str(bundle_id);
    let queue = sysext_queue();
    let req: *mut AnyObject = unsafe {
        if activation {
            msg_send![req_cls, activationRequestForExtension: &*id, queue: queue]
        } else {
            msg_send![req_cls, deactivationRequestForExtension: &*id, queue: queue]
        }
    };
    if req.is_null() {
        return Err(anyhow!("创建 OSSystemExtensionRequest 失败"));
    }
    // 工厂方法返回 autoreleased，先 retain 为自己持有
    let req: *mut AnyObject = unsafe { msg_send![&*req, retain] };
    unsafe {
        let _: () = msg_send![&*req, setDelegate: &*delegate];
        let _: () = msg_send![&*shared, submitRequest: &*req];
    }
    *REQUEST_PTR.lock().unwrap() = req as usize;
    Ok(())
}

// 服务主队列事件（自测用）：在指定秒数内转主 runloop，让 sysext 回调得到分发。
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: bool) -> i32;
}

pub fn service_main_queue(seconds: f64) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds);
    while std::time::Instant::now() < deadline {
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 0.2, true);
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
