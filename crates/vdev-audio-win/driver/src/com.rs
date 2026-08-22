//! COM 接口 vtable 基础设施（PortCls 小端口是 COM 对象）
#![allow(non_snake_case, non_camel_case_types)]

use crate::sys::types::{GUID, NTSTATUS, PVOID, c_void};

/// IUnknown 三方法签名
pub type PFN_QUERYINTERFACE =
    unsafe extern "system" fn(PVOID, *const GUID, *mut *mut c_void) -> NTSTATUS;
pub type PFN_ADDREF = unsafe extern "system" fn(PVOID) -> u32;
pub type PFN_RELEASE = unsafe extern "system" fn(PVOID) -> u32;

/// 原子递增引用计数（编译为 x86 LOCK XADD，零外部依赖）
///
/// # Safety
/// 调用方保证指针有效且对齐。
pub unsafe fn interlocked_increment(p: *mut u32) -> u32 {
    let a = unsafe { &*(p as *const core::sync::atomic::AtomicU32) };
    a.fetch_add(1, core::sync::atomic::Ordering::SeqCst) + 1
}

/// 原子递减引用计数
///
/// # Safety
/// 调用方保证指针有效且对齐。
pub unsafe fn interlocked_decrement(p: *mut u32) -> u32 {
    let a = unsafe { &*(p as *const core::sync::atomic::AtomicU32) };
    a.fetch_sub(1, core::sync::atomic::Ordering::SeqCst) - 1
}

/// 调用任意 COM 对象的 Release（IUnknown vtable 第 3 项）
///
/// # Safety
/// unknown 必须为有效 IUnknown 指针。
pub unsafe fn release_unknown(unknown: PVOID) -> u32 {
    let v = *(unknown as *const *const [usize; 3]);
    let release: PFN_RELEASE = core::mem::transmute((*v)[2]);
    release(unknown)
}
