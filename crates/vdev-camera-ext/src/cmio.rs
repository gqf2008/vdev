//! CMIOExtension.framework 手写 ObjC 绑定（spike 最小集）。
//! Rust 侧 trait 名 == ObjC 协议名，方法签名照 CMIOExtension*.h。

use objc2::extern_protocol;
use objc2::rc::Retained;
use objc2_foundation::NSObjectProtocol;
use objc2_foundation::{NSArray, NSObject, NSString, NSSet};

// 确保 CoreMediaIO 被 dyld 加载（否则 ObjC 类/协议未注册）
#[link(name = "CoreMediaIO", kind = "framework")]
extern "C" {}

extern_protocol!(
    pub unsafe trait CMIOExtensionProviderSource: NSObjectProtocol {
        #[unsafe(method(connectClient:error:))]
        unsafe fn connectClient_error(
            &self,
            client: &NSObject,
            outError: *mut *mut NSObject,
        ) -> bool;

        #[unsafe(method(disconnectClient:))]
        unsafe fn disconnectClient(&self, client: &NSObject);

        #[unsafe(method(availableProperties))]
        unsafe fn availableProperties(&self) -> Retained<NSSet<NSObject>>;

        #[unsafe(method(providerPropertiesForProperties:error:))]
        unsafe fn providerPropertiesForProperties_error(
            &self,
            properties: &NSSet<NSObject>,
            outError: *mut *mut NSObject,
        ) -> Option<Retained<NSObject>>;

        #[unsafe(method(setProviderProperties:error:))]
        unsafe fn setProviderProperties_error(
            &self,
            providerProperties: &NSObject,
            outError: *mut *mut NSObject,
        ) -> bool;
    }
);

extern_protocol!(
    pub unsafe trait CMIOExtensionDeviceSource: NSObjectProtocol {
        #[unsafe(method(availableProperties))]
        unsafe fn availableProperties(&self) -> Retained<NSSet<NSObject>>;

        #[unsafe(method(devicePropertiesForProperties:error:))]
        unsafe fn devicePropertiesForProperties_error(
            &self,
            properties: &NSSet<NSObject>,
            outError: *mut *mut NSObject,
        ) -> Option<Retained<NSObject>>;

        #[unsafe(method(setDeviceProperties:error:))]
        unsafe fn setDeviceProperties_error(
            &self,
            deviceProperties: &NSObject,
            outError: *mut *mut NSObject,
        ) -> bool;
    }
);

extern_protocol!(
    pub unsafe trait CMIOExtensionStreamSource: NSObjectProtocol {
        #[unsafe(method(formats))]
        unsafe fn formats(&self) -> Retained<NSArray<NSObject>>;

        #[unsafe(method(availableProperties))]
        unsafe fn availableProperties(&self) -> Retained<NSSet<NSObject>>;

        #[unsafe(method(streamPropertiesForProperties:error:))]
        unsafe fn streamPropertiesForProperties_error(
            &self,
            properties: &NSSet<NSObject>,
            outError: *mut *mut NSObject,
        ) -> Option<Retained<NSObject>>;

        #[unsafe(method(setStreamProperties:error:))]
        unsafe fn setStreamProperties_error(
            &self,
            streamProperties: &NSObject,
            outError: *mut *mut NSObject,
        ) -> bool;

        #[unsafe(method(authorizedToStartStreamForClient:))]
        unsafe fn authorizedToStartStreamForClient(&self, client: &NSObject) -> bool;

        #[unsafe(method(startStreamAndReturnError:))]
        unsafe fn startStreamAndReturnError(&self, outError: *mut *mut NSObject) -> bool;

        #[unsafe(method(stopStreamAndReturnError:))]
        unsafe fn stopStreamAndReturnError(&self, outError: *mut *mut NSObject) -> bool;
    }
);

// dlsym 取 CoreMediaIO 的 NSString* 全局属性常量（避免猜字符串值）。
// 注意：RTLD_DEFAULT 在共享缓存场景下找不到，必须用 dlopen 句柄。
extern "C" {
    fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char)
        -> *mut std::ffi::c_void;
    fn dlopen(path: *const std::ffi::c_char, mode: i32) -> *mut std::ffi::c_void;
}

static CMIO_HANDLE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn cmio_handle() -> *mut std::ffi::c_void {
    let p = CMIO_HANDLE.get_or_init(|| unsafe {
        dlopen(
            c"/System/Library/Frameworks/CoreMediaIO.framework/CoreMediaIO".as_ptr(),
            2, // RTLD_NOW
        ) as usize
    });
    *p as *mut std::ffi::c_void
}

/// 运行时读取属性常量（如 "CMIOExtensionPropertyProviderName"）。
pub fn property_const(name: &str) -> Retained<NSString> {
    let sym = std::ffi::CString::new(name).expect("symbol no NUL");
    let sym_addr = unsafe { dlsym(cmio_handle(), sym.as_ptr()) };
    assert!(!sym_addr.is_null(), "dlsym 找不到 {name}");
    // dlsym 返回的是全局变量（NSString* const）的地址，要先解引用拿对象指针
    let obj_ptr = unsafe { *(sym_addr as *const *const std::ffi::c_void) };
    assert!(!obj_ptr.is_null(), "{name} 全局值为空");
    let ns = obj_ptr as *const NSString;
    // 全局常量对象，retain 一份保证 Rust 侧生命周期安全
    unsafe { Retained::retain(ns as *mut NSString).unwrap() }
}

/// 用属性常量名构造 NSSet（availableProperties 返回用）。
pub fn property_set(names: &[&str]) -> Retained<NSSet<NSObject>> {
    let objs: Vec<Retained<NSObject>> = names
        .iter()
        .map(|n| {
            let s = property_const(n);
            unsafe { Retained::retain(&*s as *const NSString as *mut NSObject).unwrap() }
        })
        .collect();
    let arr = NSArray::from_retained_slice(&objs);
    unsafe { NSSet::setWithArray(&arr) }
}
