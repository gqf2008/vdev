//! CoreGraphics 公开 C API 的最小 FFI：显示器枚举 + 镜像配置。

use anyhow::{anyhow, Result};
use objc2_core_foundation::CGSize;
use std::ffi::c_void;
use std::ptr;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(max_displays: u32, displays: *mut u32, count: *mut u32) -> i32;
    // boolean_t 在 MacTypes.h 中是 4 字节 int，ABI 声明必须用 i32（此前误写为
    // u8，仅因小端读取低位而恰好可用）。宽度/防护类 FFI 声明需真实 CoreGraphics
    // 环境才能回归验证，无法单测。
    fn CGDisplayIsBuiltin(display: u32) -> i32;
    fn CGDisplayPixelsWide(display: u32) -> usize;
    fn CGDisplayPixelsHigh(display: u32) -> usize;
    fn CGDisplayVendorNumber(display: u32) -> u32;
    fn CGDisplayModelNumber(display: u32) -> u32;
    fn CGDisplayScreenSize(display: u32) -> CGSize;

    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayMirrorOfDisplay(config: *mut c_void, display: u32, mirror: u32) -> i32;
    fn CGConfigureDisplayOrigin(config: *mut c_void, display: u32, x: i32, y: i32) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: i32) -> i32;
}

const KCG_CONFIGURE_FOR_SESSION: i32 = 1;

fn check(err: i32) -> Result<()> {
    if err == 0 {
        Ok(())
    } else {
        Err(anyhow!("CoreGraphics error: {err}"))
    }
}

/// 在线显示器信息。
#[derive(Debug, Clone)]
pub struct DisplayInfo {
    pub id: u32,
    pub builtin: bool,
    pub width: usize,
    pub height: usize,
    pub width_mm: f64,
    pub height_mm: f64,
    pub vendor: u32,
    pub product: u32,
}

/// 枚举在线显示器。
pub fn online_displays() -> Result<Vec<DisplayInfo>> {
    let mut count: u32 = 0;
    // SAFETY: 上限传 0 且缓冲为空是文档约定的"只查数量"用法，函数仅回写
    // count；&raw mut count 是合法的可写 u32 位置。
    check(unsafe { CGGetOnlineDisplayList(0, ptr::null_mut(), &raw mut count) })?;
    let mut ids = vec![0u32; count as usize];
    // SAFETY: ids 容量恰为上一次调用回写的在线显示器数，CG 保证至多写入
    // max_displays 个元素，不会越界。
    check(unsafe { CGGetOnlineDisplayList(count, ids.as_mut_ptr(), &raw mut count) })?;
    ids.truncate(count as usize);

    Ok(ids
        .into_iter()
        .map(|id| DisplayInfo {
            id,
            // SAFETY: 纯查询函数，display id 按值传入，不涉及指针。
            builtin: unsafe { CGDisplayIsBuiltin(id) } != 0,
            // SAFETY: 纯查询函数，display id 按值传入，不涉及指针。
            width: unsafe { CGDisplayPixelsWide(id) },
            // SAFETY: 纯查询函数，display id 按值传入，不涉及指针。
            height: unsafe { CGDisplayPixelsHigh(id) },
            // SAFETY: 纯查询函数，display id 按值传入，返回值由值拷出。
            width_mm: unsafe { CGDisplayScreenSize(id) }.width,
            // SAFETY: 纯查询函数，display id 按值传入，返回值由值拷出。
            height_mm: unsafe { CGDisplayScreenSize(id) }.height,
            // SAFETY: 纯查询函数，display id 按值传入，不涉及指针。
            vendor: unsafe { CGDisplayVendorNumber(id) },
            // SAFETY: 纯查询函数，display id 按值传入，不涉及指针。
            product: unsafe { CGDisplayModelNumber(id) },
        })
        .collect())
}

/// 让 `target` 镜像 `source`。
pub fn mirror(source: u32, target: u32) -> Result<()> {
    let mut config: *mut c_void = ptr::null_mut();
    // SAFETY: config 是合法的可写 out 指针；成功后持有 CG 分配的配置句柄。
    check(unsafe { CGBeginDisplayConfiguration(&raw mut config) })?;
    // SAFETY: config 来自上一步成功的 CGBeginDisplayConfiguration，是有效
    // 句柄；仅修改待应用的配置，不转移所有权。
    // CGConfigureDisplayMirrorOfDisplay/CGConfigureDisplayOrigin 均返回
    // CGError（i32），必须逐个检查：任一失败都要提前返回，否则 CGComplete
    // 会把残缺配置应用出去。错误路径需真实显示器配置环境，无法单测
    // （check 的判定逻辑由下方纯函数单测覆盖）。
    check(unsafe { CGConfigureDisplayMirrorOfDisplay(config, target, source) })?;
    // SAFETY: 同上，config 为有效句柄；仅修改待应用的配置，不转移所有权。
    check(unsafe { CGConfigureDisplayOrigin(config, source, 0, 0) })?;
    // SAFETY: config 为有效句柄；CGComplete 应用并消费该配置，此后不复用。
    check(unsafe { CGCompleteDisplayConfiguration(config, KCG_CONFIGURE_FOR_SESSION) })
}

/// 解除 `target` 的镜像。
pub fn unmirror(target: u32) -> Result<()> {
    let mut config: *mut c_void = ptr::null_mut();
    // SAFETY: config 是合法的可写 out 指针；成功后持有 CG 分配的配置句柄。
    check(unsafe { CGBeginDisplayConfiguration(&raw mut config) })?;
    // SAFETY: config 来自上一步成功的 CGBeginDisplayConfiguration，是有效
    // 句柄；mirror 传 0 表示解除镜像，仅修改待应用的配置。
    // 同 mirror：configure 返回 CGError，失败必须提前返回，逐个检查。
    check(unsafe { CGConfigureDisplayMirrorOfDisplay(config, target, 0) })?;
    // SAFETY: config 为有效句柄；CGComplete 应用并消费该配置，此后不复用。
    check(unsafe { CGCompleteDisplayConfiguration(config, KCG_CONFIGURE_FOR_SESSION) })
}

#[cfg(test)]
mod tests {
    use super::check;

    #[test]
    fn check_zero_is_ok() {
        assert!(check(0).is_ok());
    }

    #[test]
    fn check_nonzero_is_err_with_code() {
        // kCGErrorFailure == 1
        assert_eq!(check(1).unwrap_err().to_string(), "CoreGraphics error: 1");
        assert_eq!(check(-5).unwrap_err().to_string(), "CoreGraphics error: -5");
    }
}
