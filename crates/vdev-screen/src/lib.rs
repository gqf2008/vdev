//! vdev-screen — 虚拟屏幕：通过 CoreGraphics 私有 API `CGVirtualDisplay` 创建虚拟显示器。
//!
//! 仅供学习研究：私有 API 跨版本可能变化。

pub mod ffi;
mod private;

use anyhow::Result;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;

pub use ffi::DisplayInfo;

/// 创建虚拟显示器的参数。
#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub width: u32,
    pub height: u32,
    pub refresh_rate: f64,
    pub name: String,
    pub vendor_id: u32,
    pub product_id: u32,
    pub serial_number: u32,
    pub width_mm: f64,
    pub height_mm: f64,
    pub max_pixels_wide: u32,
    pub max_pixels_high: u32,
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            refresh_rate: 60.0,
            name: "vdev".to_string(),
            vendor_id: 0x05AC, // Apple
            product_id: 0x1111,
            serial_number: 1,
            width_mm: 597.0,
            height_mm: 336.0,
            max_pixels_wide: 3840,
            max_pixels_high: 2160,
        }
    }
}

/// 一个已创建的虚拟显示器。Drop 时自动销毁。
pub struct VirtualDisplay {
    pub display_id: u32,
    _mode: Retained<AnyObject>,
    _descriptor: Retained<AnyObject>,
    _settings: Retained<AnyObject>,
    _display: Retained<AnyObject>,
}

impl VirtualDisplay {
    /// 让指定的物理显示器镜像本虚拟显示器。
    pub fn mirror(&self, target: u32) -> Result<()> {
        ffi::mirror(self.display_id, target)
    }

    /// 解除镜像。
    pub fn unmirror(&self, target: u32) -> Result<()> {
        ffi::unmirror(target)
    }
}

/// 枚举在线显示器。
pub fn list_displays() -> Result<Vec<DisplayInfo>> {
    ffi::online_displays()
}

/// 创建一个虚拟显示器。
pub fn create(opts: CreateOptions) -> Result<VirtualDisplay> {
    let mode = private::create_mode(opts.width, opts.height, opts.refresh_rate)?;
    let descriptor = private::create_descriptor(&private::DescriptorOptions {
        vendor_id: opts.vendor_id,
        product_id: opts.product_id,
        serial_number: opts.serial_number,
        name: opts.name,
        width_mm: opts.width_mm,
        height_mm: opts.height_mm,
        max_pixels_wide: opts.max_pixels_wide,
        max_pixels_high: opts.max_pixels_high,
    })?;
    let settings = private::create_settings(&mode)?;
    let display = private::create_display(&descriptor, &settings)?;
    let display_id = display.display_id;
    Ok(VirtualDisplay {
        display_id,
        _mode: mode,
        _descriptor: descriptor,
        _settings: settings,
        _display: display.obj,
    })
}
