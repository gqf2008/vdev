//! 内部工具（CoTaskMem 字符串分配）。

use windows::Win32::System::Com::CoTaskMemAlloc;
use windows_core::PWSTR;

/// 分配一个 CoTaskMem 字符串并返回 `PWSTR`（调用方负责 `CoTaskMemFree`）。
///
/// # Safety
/// 调用方必须保证在返回的 `PWSTR` 被 `CoTaskMemFree` 释放前，字符串内存有效。
pub unsafe fn alloc_pwstr(s: &str) -> PWSTR {
    let wide: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: 调用方负责释放返回值。
    let p = unsafe { CoTaskMemAlloc(wide.len() * 2) }.cast::<u16>();
    if p.is_null() {
        return PWSTR::null();
    }
    // SAFETY: p 指向至少 wide.len()*2 字节的可写内存。
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len()) };
    PWSTR(p)
}
