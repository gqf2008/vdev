use std::panic::{self, AssertUnwindSafe};

#[cfg(debug_assertions)]
use std::backtrace::Backtrace;

use log::error;
use wdf_umdf_sys::NTSTATUS;

pub fn set_hook() {
    panic::set_hook(Box::new(|v| {
        // debug mode, get full backtrace
        #[cfg(debug_assertions)]
        {
            let backtrace = Backtrace::force_capture();
            error!("{v}\n\nstack backtrace:\n{backtrace}");
        }

        // otherwise just print the panic since we don't have a backtrace
        #[cfg(not(debug_assertions))]
        error!("{v}");
    }));
}

/// M3：FFI 回调出口兜底。
///
/// 驱动的回调以 `extern "C-unwind"` 导出，panic 若不拦截会沿 unwind 进入
/// WDF/IddCx 宿主，行为未定义。用 `catch_unwind` 捕获（panic hook 已在
/// `set_hook` 中记录日志与回溯），统一收敛为 `STATUS_DRIVER_INTERNAL_ERROR`
/// 返回给框架。
pub fn catch_ntstatus(f: impl FnOnce() -> NTSTATUS) -> NTSTATUS {
    // AssertUnwindSafe：回调持有的裸指针/WDF 句柄不保证 panic 安全，
    // 但 panic 路径上我们不再触碰它们，直接返回错误状态
    panic::catch_unwind(AssertUnwindSafe(f)).unwrap_or(NTSTATUS::STATUS_DRIVER_INTERNAL_ERROR)
}

/// M3：同 [`catch_ntstatus`]，用于无返回值的清理回调（`EvtCleanupCallback`），
/// panic 同样被吞掉并记入日志（hook），清理流程按已尽力处理收尾。
pub fn catch_ignore(f: impl FnOnce() -> ()) {
    _ = panic::catch_unwind(AssertUnwindSafe(f));
}
