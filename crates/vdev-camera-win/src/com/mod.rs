//! COM 基础设施安全封装。
//!
//! 原则（本 crate 全局）：所有系统 API 的使用必须先收敛到安全封装模块，
//! 业务逻辑（filter/pin/streaming）只调用安全接口，不直接写 unsafe。

use std::ffi::c_void;

use windows::Win32::Foundation::{E_NOTIMPL, E_POINTER, RPC_E_CHANGED_MODE, S_FALSE, S_OK};
use windows::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, IClassFactory, IClassFactory_Impl, COINIT_MULTITHREADED,
};
use windows_core::{implement, Interface, Ref, GUID, HRESULT};

pub mod registry;
pub mod shm;

/// COM 初始化守卫（RAII）：仅当本对象真正初始化了 COM 时才 `CoUninitialize`。
///
/// 兼容宿主线程已初始化 COM 的场景（如 GUI 主线程被 winit 初始化为 STA）：
/// `CoInitializeEx` 返回 `S_FALSE`（已初始化）或 `RPC_E_CHANGED_MODE`
/// （已用其他模式初始化）时复用现有 COM 且**不**卸载——Uninitialize 只对
/// 本对象发起的初始化（`S_OK`）执行，避免破坏宿主线程的 COM 状态。
pub struct ComInit {
    owns: bool,
}

impl ComInit {
    /// 以 MTA 模式初始化当前线程的 COM（兼容已初始化场景）。
    pub fn new() -> windows_core::Result<Self> {
        // SAFETY: 传入合法参数。
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr == S_OK {
            Ok(Self { owns: true })
        } else if hr == S_FALSE || hr == RPC_E_CHANGED_MODE {
            // 已初始化（S_FALSE）或已用其他模式初始化（RPC_E_CHANGED_MODE）：
            // 复用现有 COM，不拥有卸载权。
            Ok(Self { owns: false })
        } else {
            Err(windows_core::Error::from_hresult(hr))
        }
    }
}

impl Drop for ComInit {
    fn drop(&mut self) {
        if self.owns {
            // SAFETY: 仅对本次 CoInitializeEx(S_OK) 配对卸载。
            unsafe { CoUninitialize() };
        }
    }
}

/// DirectShow 过滤器类工厂：创建 [`crate::dshow::filter::VirtualCameraFilter`]。
///
/// 不支持聚合（`punkouter != null` 时返回 `E_NOTIMPL`）。
#[implement(IClassFactory)]
pub struct FilterClassFactory;

impl IClassFactory_Impl for FilterClassFactory_Impl {
    fn CreateInstance(
        &self,
        punkouter: Ref<windows_core::IUnknown>,
        riid: *const GUID,
        ppvobject: *mut *mut c_void,
    ) -> windows_core::Result<()> {
        if !punkouter.is_null() {
            return Err(windows_core::Error::from_hresult(E_NOTIMPL));
        }
        if ppvobject.is_null() || riid.is_null() {
            return Err(windows_core::Error::from_hresult(E_POINTER));
        }
        let filter = crate::dshow::filter::create_filter()?;
        // SAFETY: riid/ppvobject 已在上方校验非空；query 写入 ppvobject 并 AddRef。
        let hr = unsafe { filter.query(riid, ppvobject) };
        if hr.is_ok() {
            Ok(())
        } else {
            Err(windows_core::Error::from_hresult(hr))
        }
    }

    fn LockServer(&self, _flock: windows_core::BOOL) -> windows_core::Result<()> {
        Ok(())
    }
}

/// DLL 导出实现（`DllGetClassObject` / `DllCanUnloadNow` / `DllRegisterServer` / `DllUnregisterServer`）。
pub(crate) mod dll {
    use super::*;
    use crate::dshow::filter::CLSID_VirtualCameraFilter;
    use windows::Win32::Foundation::E_FAIL;

    pub(crate) fn get_class_object(
        rclsid: *const GUID,
        riid: *const GUID,
        ppv: *mut *mut c_void,
    ) -> HRESULT {
        if ppv.is_null() {
            return E_POINTER;
        }
        // SAFETY: ppv 已校验非空；COM 惯例：失败路径先置 null。
        unsafe { *ppv = std::ptr::null_mut() };
        if rclsid.is_null() || riid.is_null() {
            return E_POINTER;
        }
        // SAFETY: rclsid 已校验非空，指向调用方传入的 CLSID。
        let clsid = unsafe { *rclsid };
        if clsid != CLSID_VirtualCameraFilter {
            return E_NOTIMPL; // CLASS_E_CLASSNOTAVAILABLE 语义
        }
        let factory: IClassFactory = FilterClassFactory.into();
        // SAFETY: riid/ppv 已校验非空；query 写入 ppv 并 AddRef。
        unsafe { factory.query(riid, ppv) }
    }

    /// 始终返回 `S_FALSE`（不卸载）：简单且安全，避免 DLL 在使用中被卸载。
    pub(crate) fn can_unload_now() -> HRESULT {
        S_FALSE
    }

    pub(crate) fn register_server() -> HRESULT {
        match crate::camera::register_filter() {
            Ok(()) => S_OK,
            Err(e) => {
                log::error!("DllRegisterServer failed: {e:#}");
                E_FAIL
            }
        }
    }

    pub(crate) fn unregister_server() -> HRESULT {
        match crate::camera::unregister_filter() {
            Ok(()) => S_OK,
            Err(e) => {
                log::error!("DllUnregisterServer failed: {e:#}");
                E_FAIL
            }
        }
    }
}
