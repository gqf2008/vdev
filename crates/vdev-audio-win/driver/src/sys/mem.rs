//! 非分页池分配/释放封装
#![allow(non_snake_case)]

/// 非分页池（NX）分配
///
/// # Safety
/// 内核 API；size 为所需字节数。
pub unsafe fn ExAllocatePool2_np(flags: u64, size: u64, tag: u32) -> *mut core::ffi::c_void {
    // SAFETY: 内核 API
    unsafe {
        unsafe extern "system" {
            fn ExAllocatePool2(
                PoolFlags: u64,
                NumberOfBytes: u64,
                Tag: u32,
            ) -> *mut core::ffi::c_void;
        }
        ExAllocatePool2(flags, size, tag)
    }
}

/// 释放池内存
///
/// # Safety
/// ptr 必须来自 ExAllocatePool2 且未被释放。
pub unsafe fn ExFreePoolWithTag_np(ptr: *mut core::ffi::c_void, tag: u32) {
    // SAFETY: 内核 API
    unsafe {
        unsafe extern "system" {
            fn ExFreePoolWithTag(P: *mut core::ffi::c_void, Tag: u32);
        }
        ExFreePoolWithTag(ptr, tag);
    }
}
