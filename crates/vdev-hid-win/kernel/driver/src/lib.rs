#![no_std]
//! vdev 虚拟 HID（路线 B）：KMDF 内核 HID minidriver（参考 Microsoft vhidmini2）

use core::mem::size_of;

use wdk_sys::{
    DRIVER_OBJECT, NTSTATUS, PCUNICODE_STRING, PDRIVER_OBJECT, ULONG, WDF_DRIVER_CONFIG,
    WDF_NO_HANDLE, WDF_NO_OBJECT_ATTRIBUTES, WDFDEVICE, WDFDEVICE_INIT, WDFDRIVER,
    call_unsafe_wdf_function_binding,
};

/// 设备添加回调：创建设备并挂 HID 队列
///
/// # Safety
/// 由 WDF 调用，`driver`/`init` 为有效句柄。
unsafe extern "C" fn evt_device_add(_driver: WDFDRIVER, mut init: *mut WDFDEVICE_INIT) -> NTSTATUS {
    // TODO: WdfDeviceInitSetPnpPowerEventCallbacks + HID 注册（HidRegisterMinidriver）
    let mut attributes = wdk_sys::WDF_OBJECT_ATTRIBUTES {
        Size: size_of::<wdk_sys::WDF_OBJECT_ATTRIBUTES>() as ULONG,
        ..wdk_sys::WDF_OBJECT_ATTRIBUTES::default()
    };
    let mut device: WDFDEVICE = core::ptr::null_mut();
    // SAFETY: init/attributes/device 均为有效指针
    unsafe {
        call_unsafe_wdf_function_binding!(WdfDeviceCreate, &mut init, &mut attributes, &mut device)
    }
}

/// Windows 驱动入口点
///
/// # Safety
/// 由加载器调用，参数为内核传入的有效指针。
#[unsafe(export_name = "DriverEntry")]
pub unsafe extern "system" fn driver_entry(
    driver: *mut DRIVER_OBJECT,
    registry_path: PCUNICODE_STRING,
) -> NTSTATUS {
    let mut config = WDF_DRIVER_CONFIG {
        Size: size_of::<WDF_DRIVER_CONFIG>() as ULONG,
        EvtDriverDeviceAdd: Some(evt_device_add),
        ..WDF_DRIVER_CONFIG::default()
    };
    // SAFETY: driver/registry_path 由 DriverEntry 提供且有效；config 有效；handle 输出可空
    unsafe {
        call_unsafe_wdf_function_binding!(
            WdfDriverCreate,
            driver as PDRIVER_OBJECT,
            registry_path,
            WDF_NO_OBJECT_ATTRIBUTES,
            &mut config,
            WDF_NO_HANDLE.cast::<WDFDRIVER>(),
        )
    }
}

/// panic 处理器：蓝屏（内核不允许 unwind）
#[cfg(not(test))]
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    loop {}
}
