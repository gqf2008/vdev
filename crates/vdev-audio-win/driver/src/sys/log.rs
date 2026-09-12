//! 内核调试日志（minor i）：DbgPrint 薄包装，默认关闭。
//!
//! - 本模块仅在 `kernel` feature 下编译（sys/mod.rs 门控）；
//! - `kdbg!` 宏仅在 `kernel-log` feature（隐含 kernel）下真正展开为 DbgPrint
//!   调用，默认展开为空——内核日志 I/O 有成本且会刷爆调试输出，故默认静默。
//!
//! DISPATCH_LEVEL：DbgPrint 任意 IRQL 可调用，仅作调试用途。

/// 内核调试日志宏：仅接受字面量消息（不带格式化参数，规避 varargs ABI 风险）。
///
/// 用法：`crate::kdbg!("DriverEntry\n");`（消息自带换行）。
#[macro_export]
macro_rules! kdbg {
    ($msg:literal) => {{
        #[cfg(feature = "kernel-log")]
        {
            const KDBG_MSG: &[u8] = concat!("[vdev-audio] ", $msg, "\0").as_bytes();
            const KDBG_FMT: &[u8] = b"%s\0";
            // SAFETY: DbgPrint 为 ntoskrnl 导出；消息与格式串均以 NUL 结尾，
            // 任意 IRQL 可调用
            unsafe {
                unsafe extern "C" {
                    fn DbgPrint(Format: *const i8, ...) -> u32;
                }
                let _ = DbgPrint(
                    KDBG_FMT.as_ptr().cast::<i8>(),
                    KDBG_MSG.as_ptr().cast::<i8>(),
                );
            }
        }
    }};
}
