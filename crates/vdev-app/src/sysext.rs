//! SystemExtensions FFI：激活/停用摄像头扩展。
//! 使用 objc2 的 define_class! 实现 OSSystemExtensionRequestDelegate。
use anyhow::{anyhow, Result};
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
    let queue: *mut c_void = std::ptr::null_mut(); // nil = 主队列
    let req: *mut AnyObject = unsafe {
        if activation {
            msg_send![
                req_cls,
                activationRequestForExtensionWithIdentifier: &*id,
                queue: queue
            ]
        } else {
            msg_send![
                req_cls,
                deactivationRequestForExtensionWithIdentifier: &*id,
                queue: queue
            ]
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
