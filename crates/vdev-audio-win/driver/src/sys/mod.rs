//! 手写 WDM/PortCls 绑定
/// 内核调试日志（minor i；kernel-log feature 控制，默认静默）
#[cfg(feature = "kernel")]
pub mod log;
pub mod mem;
pub mod portcls;
pub mod types;
