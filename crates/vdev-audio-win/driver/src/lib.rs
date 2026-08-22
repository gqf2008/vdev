#![cfg_attr(feature = "kernel", no_std)]
// 内核 FFI 回调体内操作全部是 unsafe（PortCls/内核 API），逐块包裹属机械噪音；
// 每个函数均带 // SAFETY: 说明不变式。
#![allow(unsafe_op_in_unsafe_fn)]
// 内核 FFI 驱动的 pedantic 噪声豁免（均有正当理由）：
#![allow(clippy::multiple_unsafe_ops_per_block)] // 内核回调体内多处 unsafe 操作是常态
#![allow(clippy::wildcard_imports)] // FFI 绑定层通配导入是惯例
#![allow(clippy::doc_markdown)] // 中文文档夹杂英文标识符
#![allow(clippy::must_use_candidate)] // FFI 回调/入口返回值不便强制 must_use
#![allow(clippy::upper_case_acronyms)] // Windows GUID/CLSID 常量命名惯例
#![allow(clippy::not_unsafe_ptr_arg_deref)] // 自包含 SPSC 环形缓冲的公开安全方法
#![allow(clippy::ptr_as_ptr)] // FFI 裸指针互转用 as 更直观
#![allow(clippy::items_after_statements)] // 内核回调内联 extern 声明
#![allow(clippy::borrow_as_ptr)] // FFI 参数 &x -> *const 惯用写法
#![allow(non_upper_case_globals)] // Windows GUID/CLSID 常量命名惯例（CLSID_PortWaveRT 等）
#![allow(clippy::cast_possible_truncation)] // FFI 结构字段宽度转换
#![allow(clippy::cast_ptr_alignment)] // KS 结构紧跟布局的强转
#![allow(clippy::if_same_then_else)] // 对称分支
#![allow(clippy::missing_safety_doc)] // 绑定层已逐函数注释
#![allow(clippy::ptr_cast_constness)] // FFI 指针 constness 转换
#![allow(clippy::ref_as_ptr)] // FFI 参数引用转裸指针
#![allow(clippy::unnecessary_cast)] // FFI 指针显式转换
//! vdev 虚拟声卡（纯 `WDM`/`PortCls` 内核驱动）手写精简绑定路线。
//!
//! 选型：不用 `WDF`（跳过 `wdk-sys` 函数表机制与 `bindgen` 兼容问题），
//! 驱动入口直接调用 `PortCls` 的初始化/子设备注册函数。
//!
//! 构建说明：
//! - 默认（无 `kernel` feature）：普通 cdylib，供 `cargo test`/`clippy` 使用；
//! - `--features kernel`：产出真正的内核驱动 `.sys`（`DriverEntry` 入口、
//!   `Native` 子系统、链接 `portcls`/`ntoskrnl`、无用户态 CRT）。

#[cfg(feature = "kernel")]
pub mod adapter;
pub mod com;
#[cfg(feature = "kernel")]
pub mod miniport;
pub mod ringbuffer;
pub mod sys;

#[cfg(feature = "kernel")]
use adapter::install_virtual_cable;
#[cfg(feature = "kernel")]
use sys::portcls::{PcAddAdapterDevice, PcInitializeAdapterDriver};
#[cfg(feature = "kernel")]
use sys::types::{
    NTSTATUS, PDEVICE_OBJECT, PDRIVER_OBJECT, PIRP, PRESOURCELIST, PUNICODE_STRING,
    STATUS_INSUFFICIENT_RESOURCES, STATUS_SUCCESS,
};

/// 设备添加回调：PcAddAdapterDevice 注册 StartDevice
///
/// # Safety
/// 由内核 `PnP` 子系统调用，`driver`/`pdo` 必须为有效指针。
#[cfg(feature = "kernel")]
unsafe extern "system" fn add_device(driver: PDRIVER_OBJECT, pdo: PDEVICE_OBJECT) -> NTSTATUS {
    // SAFETY: PortCls 为设备创建 FDO 并绑定 StartDevice 回调
    unsafe { PcAddAdapterDevice(driver, pdo, Some(start_device), 64, core::ptr::null_mut()) }
}

/// StartDevice：创建设备启动时的适配器与虚拟声卡
///
/// # Safety
/// 由内核在设备启动时调用，参数为有效指针。
#[cfg(feature = "kernel")]
unsafe extern "system" fn start_device(
    device: PDEVICE_OBJECT,
    _irp: PIRP,
    _resource_list: PRESOURCELIST,
) -> NTSTATUS {
    // SAFETY: 创建设备适配器（单例）
    let adapter = unsafe { adapter::create(device) };
    if adapter.is_null() {
        return STATUS_INSUFFICIENT_RESOURCES;
    }
    // SAFETY: 安装虚拟扬声器 + 麦克风并配对环回
    let st = unsafe { install_virtual_cable(adapter) };
    if st < 0 {
        // 失败则释放适配器
        // SAFETY: 释放引用
        unsafe { crate::com::release_unknown(adapter.cast()) };
        return st;
    }
    STATUS_SUCCESS
}

/// panic 处理器：落日志 + 蓝屏（内核环境不允许 `unwind`）
#[cfg(all(feature = "kernel", not(test)))]
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

/// MSVC C++ 异常处理器 stub：内核驱动无 C++ 异常，仅满足链接器对 core 的引用
#[cfg(feature = "kernel")]
#[unsafe(no_mangle)]
pub extern "C" fn __CxxFrameHandler3() -> i32 {
    0
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
