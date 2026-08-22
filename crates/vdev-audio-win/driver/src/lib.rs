#![no_std]
//! vdev 虚拟声卡（纯 `WDM`/`PortCls` 内核驱动）手写精简绑定路线。
//!
//! 选型：不用 `WDF`（跳过 `wdk-sys` 函数表机制与 `bindgen` 兼容问题），
//! 驱动入口直接调用 `PortCls` 的初始化/子设备注册函数。
//!
//! 构建说明：
//! - 默认（无 `kernel` feature）：普通 cdylib，供 `cargo test`/`clippy` 使用；
//! - `--features kernel`：产出真正的内核驱动 `.sys`（`DriverEntry` 入口、
//!   `Native` 子系统、链接 `portcls`/`ntoskrnl`、无用户态 CRT）。

pub mod sys;

#[cfg(feature = "kernel")]
use sys::portcls::PcInitializeAdapterDriver;
#[cfg(feature = "kernel")]
use sys::types::{NTSTATUS, PDEVICE_OBJECT, PDRIVER_OBJECT, PUNICODE_STRING, STATUS_SUCCESS};

/// 设备添加回调：创建适配器对象并注册 `WaveRT`/`Topology` 子设备
///
/// # Safety
/// 由内核 `PnP` 子系统调用，`driver`/`pdo` 必须为有效指针。
#[cfg(feature = "kernel")]
unsafe extern "system" fn add_device(_driver: PDRIVER_OBJECT, _pdo: PDEVICE_OBJECT) -> NTSTATUS {
    // TODO: PcAddAdapterDevice + PcNewMiniport/PcNewPort + PcRegisterSubdevice（WaveRT/Topology）
    STATUS_SUCCESS
}

/// panic 处理器：落日志 + 蓝屏（内核环境不允许 `unwind`）
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    // SAFETY: KeBugCheckEx 是内核 API
    unsafe {
        unsafe extern "system" {
            fn KeBugCheckEx(code: u32, a: usize, b: usize, c: usize, d: usize) -> !;
        }
        KeBugCheckEx(0xDEAD_DEAD, 0, 0, 0, 0);
    }
}

/// Windows 驱动入口点
///
/// # Safety
/// 由加载器调用，参数为内核传入的有效指针。
#[cfg(feature = "kernel")]
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver_object: PDRIVER_OBJECT,
    registry_path: PUNICODE_STRING,
) -> NTSTATUS {
    // SAFETY: PortCls 初始化驱动（注册 AddDevice 回调）
    unsafe { PcInitializeAdapterDriver(driver_object, registry_path, Some(add_device)) }
}
