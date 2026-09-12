use std::{
    mem::{self, MaybeUninit},
    ptr::NonNull,
};

use driver_ipc::Mode;
use log::error;
use wdf_umdf_sys::{
    __BindgenBitfieldUnit, DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1,
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1__bindgen_ty_1, DISPLAYCONFIG_2DREGION,
    DISPLAYCONFIG_RATIONAL, DISPLAYCONFIG_SCANLINE_ORDERING, DISPLAYCONFIG_TARGET_MODE,
    DISPLAYCONFIG_VIDEO_SIGNAL_INFO, IDARG_IN_ADAPTER_INIT_FINISHED, IDARG_IN_COMMITMODES,
    IDARG_IN_GETDEFAULTDESCRIPTIONMODES, IDARG_IN_PARSEMONITORDESCRIPTION,
    IDARG_IN_QUERYTARGETMODES, IDARG_IN_SETSWAPCHAIN, IDARG_OUT_GETDEFAULTDESCRIPTIONMODES,
    IDARG_OUT_PARSEMONITORDESCRIPTION, IDARG_OUT_QUERYTARGETMODES, IDDCX_ADAPTER__,
    IDDCX_MONITOR_MODE, IDDCX_MONITOR_MODE_ORIGIN, IDDCX_MONITOR__, IDDCX_TARGET_MODE, NTSTATUS,
    WDFDEVICE, WDF_POWER_DEVICE_STATE,
};

use crate::{
    context::{DeviceContext, MonitorContext},
    edid::Edid,
    ipc::{AdapterObject, FlattenModes, ADAPTER, MONITOR_MODES},
    panic::catch_ntstatus,
};

// M3：以下所有 `extern "C-unwind"` 回调均为 WDF/IddCx 框架的调用入口。
// 实现体抽为 `*_impl`，入口统一 `catch_ntstatus` 兜底 —— panic 不得穿越
// FFI 边界进入框架宿主，而是以 STATUS_DRIVER_INTERNAL_ERROR 返回。

pub extern "C-unwind" fn adapter_init_finished(
    adapter_object: *mut IDDCX_ADAPTER__,
    p_in_args: *const IDARG_IN_ADAPTER_INIT_FINISHED,
) -> NTSTATUS {
    catch_ntstatus(|| adapter_init_finished_impl(adapter_object, p_in_args))
}

fn adapter_init_finished_impl(
    adapter_object: *mut IDDCX_ADAPTER__,
    _p_in_args: *const IDARG_IN_ADAPTER_INIT_FINISHED,
) -> NTSTATUS {
    let Some(adapter_ptr) = NonNull::new(adapter_object) else {
        error!("Adapter ptr was null");
        return NTSTATUS::STATUS_INVALID_ADDRESS;
    };

    // store adapter object for listener to use
    if ADAPTER.set(AdapterObject(adapter_ptr)).is_err() {
        error!("Failed to set adapter");
        return NTSTATUS::STATUS_ADAPTER_HARDWARE_ERROR;
    }

    DeviceContext::finish_init();

    NTSTATUS::STATUS_SUCCESS
}

pub extern "C-unwind" fn device_d0_entry(
    device: WDFDEVICE,
    p_previous_state: WDF_POWER_DEVICE_STATE,
) -> NTSTATUS {
    catch_ntstatus(|| device_d0_entry_impl(device, p_previous_state))
}

fn device_d0_entry_impl(device: WDFDEVICE, _previous_state: WDF_POWER_DEVICE_STATE) -> NTSTATUS {
    let status: NTSTATUS = unsafe {
        DeviceContext::get_mut(device.cast(), |context| {
            if let Err(e) = context.init_adapter() {
                error!("Failed to init adapter: {e:?}");
            }
        })
        .into()
    };

    if !status.is_success() {
        return status;
    }

    NTSTATUS::STATUS_SUCCESS
}

fn display_info(width: u32, height: u32, refresh_rate: u32) -> DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
    // M3：clock_rate 改在 u64 域 checked 计算 —— 原 u32 域在大分辨率高刷新率下
    // 乘法回绕（release）/直接 panic（debug）。任何一步溢出则钳制到 u64::MAX。
    let height_total = u64::from(height) + 4;
    let clock_rate = u64::from(refresh_rate)
        .checked_mul(height_total)
        .and_then(|v| v.checked_mul(height_total))
        .and_then(|v| v.checked_add(1000))
        .unwrap_or(u64::MAX);

    // DISPLAYCONFIG_RATIONAL 的 Numerator/Denominator 是 u32：M2 输入校验
    // （refresh [1,1000]、height [64,16384]）内仍有组合会击穿 u32::MAX ≈ 4.295e9，
    // 例如 2160p（总高 2164）@ 1000Hz ≈ 4.68e9（≥918Hz 即击穿）、高度 ≥2069 @
    // 1000Hz —— 故钳制到 u32::MAX，而非「常规组合达不到钳制」。
    let clock_rate_u32 = u32::try_from(clock_rate).unwrap_or(u32::MAX);
    let height_total_u32 = u32::try_from(height_total).unwrap_or(u32::MAX);
    let height_total_sq_u32 = u32::try_from(height_total * height_total).unwrap_or(u32::MAX);

    DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
        pixelRate: clock_rate,
        hSyncFreq: DISPLAYCONFIG_RATIONAL {
            Numerator: clock_rate_u32,
            Denominator: height_total_u32,
        },
        vSyncFreq: DISPLAYCONFIG_RATIONAL {
            Numerator: clock_rate_u32,
            Denominator: height_total_sq_u32,
        },
        activeSize: DISPLAYCONFIG_2DREGION {
            cx: width,
            cy: height,
        },
        totalSize: DISPLAYCONFIG_2DREGION {
            cx: width + 4,
            cy: height + 4,
        },
        __bindgen_anon_1: DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1 {
            AdditionalSignalInfo: unsafe {
                mem::transmute::<
                    __BindgenBitfieldUnit<[u8; 4]>,
                    DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1__bindgen_ty_1,
                >(
                    DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                        255, 0, 0,
                    ),
                )
            },
        },
        scanLineOrdering:
            DISPLAYCONFIG_SCANLINE_ORDERING::DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE,
    }
}

pub extern "C-unwind" fn parse_monitor_description(
    p_in_args: *const IDARG_IN_PARSEMONITORDESCRIPTION,
    p_out_args: *mut IDARG_OUT_PARSEMONITORDESCRIPTION,
) -> NTSTATUS {
    catch_ntstatus(|| parse_monitor_description_impl(p_in_args, p_out_args))
}

fn parse_monitor_description_impl(
    p_in_args: *const IDARG_IN_PARSEMONITORDESCRIPTION,
    p_out_args: *mut IDARG_OUT_PARSEMONITORDESCRIPTION,
) -> NTSTATUS {
    let in_args = unsafe { &*p_in_args };
    let out_args = unsafe { &mut *p_out_args };

    let Ok(monitors) = MONITOR_MODES.lock() else {
        error!("MONITOR_MODES mutex poisoned");
        return NTSTATUS::STATUS_DRIVER_INTERNAL_ERROR;
    };

    let edid = unsafe {
        std::slice::from_raw_parts(
            in_args.MonitorDescription.pData as *const u8,
            in_args.MonitorDescription.DataSize as usize,
        )
    };

    let monitor_index = Edid::get_serial(edid);
    let Ok(monitor_index) = monitor_index else {
        error!(
            "We got an edid {} bytes long, but this is incorrect",
            edid.len()
        );
        return NTSTATUS::STATUS_INVALID_VIEW_SIZE;
    };

    let Some(monitor) = monitors.iter().find(|&m| m.data.id == monitor_index) else {
        error!("Failed to find monitor id {monitor_index}");
        return NTSTATUS::STATUS_DRIVER_INTERNAL_ERROR;
    };

    // M3：模式计数用 checked 累加，异常时返回错误状态而非 expect panic
    let Ok(number_of_modes) = count_modes(&monitor.data.modes) else {
        error!("Monitor {monitor_index} has too many modes to count in u32");
        return NTSTATUS::STATUS_DRIVER_INTERNAL_ERROR;
    };

    out_args.MonitorModeBufferOutputCount = number_of_modes;
    if in_args.MonitorModeBufferInputCount < number_of_modes {
        // Return success if there was no buffer, since the caller was only asking for a count of modes
        return if in_args.MonitorModeBufferInputCount > 0 {
            NTSTATUS::STATUS_BUFFER_TOO_SMALL
        } else {
            NTSTATUS::STATUS_SUCCESS
        };
    }

    // minor(b)：无模式可写时 pMonitorModes 可能为 null，而
    // slice::from_raw_parts_mut 要求指针非空（len == 0 亦然，形式 UB）——
    // 先短路返回；行为与「空 slice + 空 loop」一致
    if number_of_modes == 0 {
        out_args.PreferredMonitorModeIdx = 0;
        return NTSTATUS::STATUS_SUCCESS;
    }

    let monitor_modes = unsafe {
        std::slice::from_raw_parts_mut(
            in_args
                .pMonitorModes
                .cast::<MaybeUninit<IDDCX_MONITOR_MODE>>(),
            number_of_modes as usize,
        )
    };

    for (mode, out_mode) in monitor.data.modes.flatten().zip(monitor_modes.iter_mut()) {
        out_mode.write(IDDCX_MONITOR_MODE {
            #[allow(clippy::cast_possible_truncation)]
            Size: mem::size_of::<IDDCX_MONITOR_MODE>() as u32,
            Origin: IDDCX_MONITOR_MODE_ORIGIN::IDDCX_MONITOR_MODE_ORIGIN_MONITORDESCRIPTOR,
            MonitorVideoSignalInfo: display_info(mode.width, mode.height, mode.refresh_rate),
        });
    }

    // Set the preferred mode as represented in the EDID
    out_args.PreferredMonitorModeIdx = 0;

    NTSTATUS::STATUS_SUCCESS
}

pub extern "C-unwind" fn monitor_get_default_modes(
    monitor_object: *mut IDDCX_MONITOR__,
    p_in_args: *const IDARG_IN_GETDEFAULTDESCRIPTIONMODES,
    p_out_args: *mut IDARG_OUT_GETDEFAULTDESCRIPTIONMODES,
) -> NTSTATUS {
    catch_ntstatus(|| monitor_get_default_modes_impl(monitor_object, p_in_args, p_out_args))
}

fn monitor_get_default_modes_impl(
    _monitor_object: *mut IDDCX_MONITOR__,
    _p_in_args: *const IDARG_IN_GETDEFAULTDESCRIPTIONMODES,
    _p_out_args: *mut IDARG_OUT_GETDEFAULTDESCRIPTIONMODES,
) -> NTSTATUS {
    NTSTATUS::STATUS_NOT_IMPLEMENTED
}

/// M3：把各模式的刷新率数量 checked 累加成 u32，溢出/越界返回 None
fn count_modes(modes: &[Mode]) -> Result<u32, ()> {
    modes
        .iter()
        .try_fold(0u32, |acc, m| {
            let n = u32::try_from(m.refresh_rates.len()).map_err(|_| ())?;
            acc.checked_add(n).ok_or(())
        })
        .map_err(|_| ())
}

pub fn target_mode(width: u32, height: u32, refresh_rate: u32) -> IDDCX_TARGET_MODE {
    let total_size = DISPLAYCONFIG_2DREGION {
        cx: width,
        cy: height,
    };

    // M3：乘法全部改 saturating 域 —— 原 u64 域 `*` 在 debug 下大参数直接 panic；
    // u32 的 hSyncFreq.Numerator 同类隐患一并收敛（M2 校验下实际不会触顶）
    let pixel_rate = u64::from(refresh_rate)
        .saturating_mul(u64::from(width))
        .saturating_mul(u64::from(height));

    IDDCX_TARGET_MODE {
        #[allow(clippy::cast_possible_truncation)]
        Size: mem::size_of::<IDDCX_TARGET_MODE>() as u32,

        TargetVideoSignalInfo: DISPLAYCONFIG_TARGET_MODE {
            targetVideoSignalInfo: DISPLAYCONFIG_VIDEO_SIGNAL_INFO {
                pixelRate: pixel_rate,
                hSyncFreq: DISPLAYCONFIG_RATIONAL {
                    Numerator: refresh_rate.saturating_mul(height),
                    Denominator: 1,
                },
                vSyncFreq: DISPLAYCONFIG_RATIONAL {
                    Numerator: refresh_rate,
                    Denominator: 1,
                },
                totalSize: total_size,
                activeSize: total_size,
                scanLineOrdering:
                    DISPLAYCONFIG_SCANLINE_ORDERING::DISPLAYCONFIG_SCANLINE_ORDERING_PROGRESSIVE,
                __bindgen_anon_1: DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1 {
                    AdditionalSignalInfo: unsafe {
                        mem::transmute::<__BindgenBitfieldUnit<[u8; 4]>, DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1__bindgen_ty_1>(
                            DISPLAYCONFIG_VIDEO_SIGNAL_INFO__bindgen_ty_1__bindgen_ty_1::new_bitfield_1(
                                255, 1, 0,
                            ),
                        )
                    },
                },
            },
        },

        ..Default::default()
    }
}

pub extern "C-unwind" fn monitor_query_modes(
    monitor_object: *mut IDDCX_MONITOR__,
    p_in_args: *const IDARG_IN_QUERYTARGETMODES,
    p_out_args: *mut IDARG_OUT_QUERYTARGETMODES,
) -> NTSTATUS {
    catch_ntstatus(|| monitor_query_modes_impl(monitor_object, p_in_args, p_out_args))
}

fn monitor_query_modes_impl(
    monitor_object: *mut IDDCX_MONITOR__,
    p_in_args: *const IDARG_IN_QUERYTARGETMODES,
    p_out_args: *mut IDARG_OUT_QUERYTARGETMODES,
) -> NTSTATUS {
    // find out which monitor this belongs too

    let Ok(monitors) = MONITOR_MODES.lock() else {
        error!("MONITOR_MODES mutex poisoned");
        return NTSTATUS::STATUS_DRIVER_INTERNAL_ERROR;
    };

    // we have stored the monitor object per id, so we should be able to compare pointers
    let Some(monitor) = monitors
        .iter()
        .find(|&m| m.object.is_some_and(|p| p.as_ptr() == monitor_object))
    else {
        error!("Failed to find monitor object in cache for {monitor_object:?}");
        return NTSTATUS::STATUS_DRIVER_INTERNAL_ERROR;
    };

    // M3：模式计数用 checked 累加，异常时返回错误状态而非 expect panic
    let Ok(number_of_modes) = count_modes(&monitor.data.modes) else {
        error!("Cached monitor has too many modes to count in u32");
        return NTSTATUS::STATUS_DRIVER_INTERNAL_ERROR;
    };

    // Create a set of modes supported for frame processing and scan-out. These are typically not based on the
    // monitor's descriptor and instead are based on the static processing capability of the device. The OS will
    // report the available set of modes for a given output as the intersection of monitor modes with target modes.

    let out_args = unsafe { &mut *p_out_args };
    out_args.TargetModeBufferOutputCount = number_of_modes;

    let in_args = unsafe { &*p_in_args };

    // minor(d)：缓冲不足时与 parse_monitor_description 保持一致返回
    // STATUS_BUFFER_TOO_SMALL（缓冲为 0 仍返回 SUCCESS —— 调用方只是在询问数量），
    // 原实现静默返回 SUCCESS 会让 OS 以为目标模式已写入而读到未初始化缓冲
    if in_args.TargetModeBufferInputCount < number_of_modes {
        return if in_args.TargetModeBufferInputCount > 0 {
            NTSTATUS::STATUS_BUFFER_TOO_SMALL
        } else {
            NTSTATUS::STATUS_SUCCESS
        };
    }

    // minor(b)：无模式可写时 pTargetModes 可能为 null，而
    // slice::from_raw_parts_mut 要求指针非空（len == 0 亦然，形式 UB）——
    // 先短路返回；行为与「空 slice + 空 loop」一致
    if number_of_modes == 0 {
        return NTSTATUS::STATUS_SUCCESS;
    }

    let out_target_modes = unsafe {
        std::slice::from_raw_parts_mut(
            in_args
                .pTargetModes
                .cast::<MaybeUninit<IDDCX_TARGET_MODE>>(),
            number_of_modes as usize,
        )
    };

    for (mode, out_target) in monitor
        .data
        .modes
        .flatten()
        .zip(out_target_modes.iter_mut())
    {
        let target_mode = target_mode(mode.width, mode.height, mode.refresh_rate);

        out_target.write(target_mode);
    }

    NTSTATUS::STATUS_SUCCESS
}

pub extern "C-unwind" fn adapter_commit_modes(
    adapter_object: *mut IDDCX_ADAPTER__,
    p_in_args: *const IDARG_IN_COMMITMODES,
) -> NTSTATUS {
    catch_ntstatus(|| adapter_commit_modes_impl(adapter_object, p_in_args))
}

fn adapter_commit_modes_impl(
    _adapter_object: *mut IDDCX_ADAPTER__,
    _p_in_args: *const IDARG_IN_COMMITMODES,
) -> NTSTATUS {
    NTSTATUS::STATUS_SUCCESS
}

pub extern "C-unwind" fn assign_swap_chain(
    monitor_object: *mut IDDCX_MONITOR__,
    p_in_args: *const IDARG_IN_SETSWAPCHAIN,
) -> NTSTATUS {
    catch_ntstatus(|| assign_swap_chain_impl(monitor_object, p_in_args))
}

fn assign_swap_chain_impl(
    monitor_object: *mut IDDCX_MONITOR__,
    p_in_args: *const IDARG_IN_SETSWAPCHAIN,
) -> NTSTATUS {
    let p_in_args = unsafe { &*p_in_args };

    unsafe {
        MonitorContext::get_mut(monitor_object.cast(), |context| {
            context.assign_swap_chain(
                p_in_args.hSwapChain,
                p_in_args.RenderAdapterLuid,
                p_in_args.hNextSurfaceAvailable,
            );
        })
        .into()
    }
}

pub extern "C-unwind" fn unassign_swap_chain(monitor_object: *mut IDDCX_MONITOR__) -> NTSTATUS {
    catch_ntstatus(|| unassign_swap_chain_impl(monitor_object))
}

fn unassign_swap_chain_impl(monitor_object: *mut IDDCX_MONITOR__) -> NTSTATUS {
    unsafe {
        MonitorContext::get_mut(monitor_object.cast(), |context| {
            context.unassign_swap_chain();
        })
        .into()
    }
}
