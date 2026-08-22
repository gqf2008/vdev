//! vdev 虚拟声卡（PortCls 内核驱动）骨架占位。
//!
//! 下一轮接入 WDK 绑定后实现 `DriverEntry`（`WdfDriverCreate` + `PcInitializeAdapterDriver`）
//! 与 `AdapterCommon`（`WaveRT` 环回 miniport + `Topology`）。
//!
//! 已知障碍（记录在案，避免重复踩）：
//! - `wdk-sys` 0.5.1 生成的绑定在 Rust 1.97 下 const-eval 溢出（其 `bindgen` 0.71 不兼容
//!   `libclang` 22，需 0.72+）；
//! - `portcls.h` 内 include `windef.h`，与 `wdm.h` 的内联类型冲突，需绕开头文件。

/// 占位函数，后续替换为真实驱动入口。
#[must_use]
pub fn placeholder() -> u32 {
    0
}
