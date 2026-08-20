//! CoreGraphics 公开 C API 的最小 FFI：显示器枚举 + 镜像配置。

use anyhow::{anyhow, Result};
use objc2_core_foundation::CGSize;
use std::ffi::c_void;
use std::ptr;

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGGetOnlineDisplayList(max_displays: u32, displays: *mut u32, count: *mut u32) -> i32;
    fn CGDisplayIsBuiltin(display: u32) -> u8;
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
    check(unsafe { CGGetOnlineDisplayList(0, ptr::null_mut(), &mut count) })?;
    let mut ids = vec![0u32; count as usize];
    check(unsafe { CGGetOnlineDisplayList(count, ids.as_mut_ptr(), &mut count) })?;
    ids.truncate(count as usize);

    Ok(ids
        .into_iter()
        .map(|id| DisplayInfo {
            id,
            builtin: unsafe { CGDisplayIsBuiltin(id) } != 0,
            width: unsafe { CGDisplayPixelsWide(id) },
            height: unsafe { CGDisplayPixelsHigh(id) },
            width_mm: unsafe { CGDisplayScreenSize(id) }.width,
            height_mm: unsafe { CGDisplayScreenSize(id) }.height,
            vendor: unsafe { CGDisplayVendorNumber(id) },
            product: unsafe { CGDisplayModelNumber(id) },
        })
        .collect())
}

/// 让 `target` 镜像 `source`。
pub fn mirror(source: u32, target: u32) -> Result<()> {
    let mut config: *mut c_void = ptr::null_mut();
    check(unsafe { CGBeginDisplayConfiguration(&mut config) })?;
    unsafe {
        CGConfigureDisplayMirrorOfDisplay(config, target, source);
        CGConfigureDisplayOrigin(config, source, 0, 0);
    }
    check(unsafe { CGCompleteDisplayConfiguration(config, KCG_CONFIGURE_FOR_SESSION) })
}

/// 解除 `target` 的镜像。
pub fn unmirror(target: u32) -> Result<()> {
    let mut config: *mut c_void = ptr::null_mut();
    check(unsafe { CGBeginDisplayConfiguration(&mut config) })?;
    unsafe {
        CGConfigureDisplayMirrorOfDisplay(config, target, 0);
    }
    check(unsafe { CGCompleteDisplayConfiguration(config, KCG_CONFIGURE_FOR_SESSION) })
}
