//! `CGVirtualDisplay` 私有 API 的 ObjC 消息封装（objc2）。
//!
//! 这些类存在于 CoreGraphics.framework，但不在公开头文件里。
//! DisplayLink 等硬件厂商也在用，跨 macOS 版本相对稳定，仅供学习研究。

use anyhow::{anyhow, Result};
use objc2::msg_send;
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_foundation::{NSArray, NSString};
use std::ffi::CString;

fn class(name: &str) -> Result<&'static AnyClass> {
    let cname = CString::new(name).expect("class name has no NUL");
    AnyClass::get(&cname).ok_or_else(|| anyhow!("private ObjC class not found: {name}"))
}

/// 把 ObjC 返回的 +1 指针包装成 `Retained`（init 返回 nil 时报错）。
unsafe fn retained(ptr: *mut AnyObject, what: &str) -> Result<Retained<AnyObject>> {
    // SAFETY: 调用方保证 ptr 是该方法返回的 +1 retained 对象。
    unsafe { Retained::from_raw(ptr) }.ok_or_else(|| anyhow!("{what} returned nil"))
}

/// `CGVirtualDisplayMode`：一个显示模式。
pub fn create_mode(width: u32, height: u32, refresh_rate: f64) -> Result<Retained<AnyObject>> {
    let cls = class("CGVirtualDisplayMode")?;
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    let mode: *mut AnyObject = unsafe {
        msg_send![alloc, initWithWidth: width, height: height, refreshRate: refresh_rate]
    };
    // SAFETY: init 返回 +1 对象。
    unsafe { retained(mode, "CGVirtualDisplayMode init") }
}

/// `CGVirtualDisplayDescriptor`：虚拟显示器的描述信息。
#[derive(Debug, Clone)]
pub struct DescriptorOptions {
    pub vendor_id: u32,
    pub product_id: u32,
    pub serial_number: u32,
    pub name: String,
    pub width_mm: f64,
    pub height_mm: f64,
    pub max_pixels_wide: u32,
    pub max_pixels_high: u32,
}

pub fn create_descriptor(opts: &DescriptorOptions) -> Result<Retained<AnyObject>> {
    let cls = class("CGVirtualDisplayDescriptor")?;
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    let desc: *mut AnyObject = unsafe { msg_send![alloc, init] };
    // SAFETY: init 返回 +1 对象。
    let desc = unsafe { retained(desc, "CGVirtualDisplayDescriptor init")? };
    let name = NSString::from_str(&opts.name);

    unsafe {
        let _: () = msg_send![&*desc, setVendorID: opts.vendor_id];
        let _: () = msg_send![&*desc, setProductID: opts.product_id];
        let _: () = msg_send![&*desc, setSerialNumber: opts.serial_number];
        let _: () = msg_send![&*desc, setName: &*name];
        let _: () = msg_send![
            &*desc,
            setSizeInMillimeters: CGSize { width: opts.width_mm, height: opts.height_mm }
        ];
        let _: () = msg_send![&*desc, setMaxPixelsWide: opts.max_pixels_wide];
        let _: () = msg_send![&*desc, setMaxPixelsHigh: opts.max_pixels_high];
        // Display P3 色域主色（默认值，与大多数现代 Mac 显示器一致）
        let _: () = msg_send![&*desc, setRedPrimary: CGPoint { x: 0.680, y: 0.320 }];
        let _: () = msg_send![&*desc, setGreenPrimary: CGPoint { x: 0.265, y: 0.690 }];
        let _: () = msg_send![&*desc, setBluePrimary: CGPoint { x: 0.150, y: 0.060 }];
        let _: () = msg_send![&*desc, setWhitePoint: CGPoint { x: 0.3127, y: 0.3290 }];
    }
    Ok(desc)
}

/// `CGVirtualDisplaySettings`：模式列表 + HiDPI。
pub fn create_settings(mode: &AnyObject) -> Result<Retained<AnyObject>> {
    let cls = class("CGVirtualDisplaySettings")?;
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    let settings: *mut AnyObject = unsafe { msg_send![alloc, init] };
    // SAFETY: init 返回 +1 对象。
    let settings = unsafe { retained(settings, "CGVirtualDisplaySettings init")? };
    let modes = NSArray::from_slice(&[mode]);
    unsafe {
        let _: () = msg_send![&*settings, setModes: &*modes];
        let _: () = msg_send![&*settings, setHiDPI: 1u32];
        let _: () = msg_send![&*settings, setRotation: 0u32];
    }
    Ok(settings)
}

/// 创建虚拟显示器本体，并应用设置。
pub struct VirtualDisplay {
    pub obj: Retained<AnyObject>,
    pub display_id: u32,
}

pub fn create_display(
    descriptor: &AnyObject,
    settings: &AnyObject,
) -> Result<VirtualDisplay> {
    let cls = class("CGVirtualDisplay")?;
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    let display: *mut AnyObject = unsafe {
        msg_send![alloc, initWithDescriptor: descriptor]
    };
    // SAFETY: init 返回 +1 对象。
    let display = unsafe { retained(display, "CGVirtualDisplay init")? };
    let applied: bool = unsafe { msg_send![&*display, applySettings: settings] };
    if !applied {
        return Err(anyhow!("CGVirtualDisplay applySettings failed"));
    }
    let display_id: u32 = unsafe { msg_send![&*display, displayID] };
    Ok(VirtualDisplay { obj: display, display_id })
}
