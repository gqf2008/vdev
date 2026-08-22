//! vdev-camera-win — Windows 虚拟摄像头（DirectShow 源过滤器，100% Rust）。
//!
//! 架构原则：**安全封装优先**。所有 Windows 系统 API（COM/注册表/共享内存/
//! DirectShow）先收敛到带 `SAFETY` 注释的安全封装模块，业务逻辑只调安全接口。
//!
//! ```text
//! vdev-camera-win (cdylib = DirectShow filter DLL / rlib = 宿主 API)
//!   com/
//!     mod.rs        COM 初始化 / 类工厂 / DLL 导出（安全封装）
//!     registry.rs   注册表安全封装（安装/卸载 DirectShow 过滤器）
//!     shm.rs        跨进程共享帧通道安全封装（SHM + 命名事件，无锁双缓冲）
//!   dshow/
//!     media_type.rs   AM_MEDIA_TYPE / VIDEOINFOHEADER 安全封装
//!     filter.rs       VirtualCameraFilter（IBaseFilter/IMediaFilter/IPersist/IAMFilterMiscFlags）
//!     pin.rs          OutputPin（IPin/IAMStreamConfig/IKsPropertySet）
//!     device.rs       视频捕获源枚举安全封装（ICreateDevEnum + IPropertyBag）
//!     selftest.rs     进程内自测图安全封装（源 → NullRenderer）
//!     enum_pins.rs / enum_media_types.rs / streaming.rs / util.rs
//!   camera.rs       面向宿主的高层安全 API（install/uninstall/push_frame）
//!   main.rs         CLI：install / uninstall / selftest / push / list（纯业务层，零 unsafe）
//! ```
//!
//! 本 crate 是 Windows 系统互操作白名单 crate（需要 `unsafe` 实现 COM 接口与
//! 系统调用），`unsafe` 全部收敛到上述封装模块并带 `SAFETY` 注释。

// COM `_Impl` trait 方法签名固定为 `&self` + 裸指针（无法改 unsafe），
// 内部解引用均已在 SAFETY 注释的 unsafe 块内完成。
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod camera;
pub mod com;
pub mod dshow;

pub use camera::CameraServer;
pub use camera::{register_filter, unregister_filter};
pub use com::shm::SharedFrameChannel;
pub use dshow::filter::CLSID_VirtualCameraFilter;
pub use dshow::media_type::VideoFormat;

// ─── DirectShow COM 服务器标准导出 ───
// DllGetClassObject / DllCanUnloadNow / DllRegisterServer / DllUnregisterServer
// 由 regsvr32、CoCreateInstance 等加载。

use std::ffi::c_void;
use windows_core::GUID;
use windows_core::HRESULT;

#[no_mangle]
pub extern "system" fn DllGetClassObject(
    rclsid: *const GUID,
    riid: *const GUID,
    ppv: *mut *mut c_void,
) -> HRESULT {
    com::dll::get_class_object(rclsid, riid, ppv)
}

#[no_mangle]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    com::dll::can_unload_now()
}

#[no_mangle]
pub extern "system" fn DllRegisterServer() -> HRESULT {
    com::dll::register_server()
}

#[no_mangle]
pub extern "system" fn DllUnregisterServer() -> HRESULT {
    com::dll::unregister_server()
}
