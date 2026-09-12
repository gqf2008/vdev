//! `CGVirtualDisplay` 私有 API 的 `ObjC` 消息封装（`objc2`）。
//!
//! 这些类存在于 CoreGraphics.framework，但不在公开头文件里。
//! `DisplayLink` 等硬件厂商也在用，跨 macOS 版本相对稳定，仅供学习研究。

use anyhow::{anyhow, Result};
use objc2::rc::Retained;
use objc2::runtime::{AnyClass, AnyObject};
use objc2::{msg_send, sel};
use objc2_core_foundation::{CGPoint, CGSize};
use objc2_foundation::{NSArray, NSString};
use std::ffi::CString;

fn class(name: &str) -> Result<&'static AnyClass> {
    let cname = CString::new(name).expect("class name has no NUL");
    AnyClass::get(&cname).ok_or_else(|| anyhow!("private ObjC class not found: {name}"))
}

/// 把 `ObjC` 返回的 +1 指针包装成 `Retained`（init 返回 nil 时报错）。
unsafe fn retained(ptr: *mut AnyObject, what: &str) -> Result<Retained<AnyObject>> {
    // SAFETY: 调用方保证 ptr 是该方法返回的 +1 retained 对象。
    unsafe { Retained::from_raw(ptr) }.ok_or_else(|| anyhow!("{what} returned nil"))
}

/// `CGVirtualDisplayMode`：一个显示模式。
pub fn create_mode(width: u32, height: u32, refresh_rate: f64) -> Result<Retained<AnyObject>> {
    let cls = class("CGVirtualDisplayMode")?;
    // SAFETY: alloc 是类方法，返回 +1 未初始化对象，随后作为 init 的 receiver。
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    // DeskPad 私有头 dump 显示 initWithWidth:height:refreshRate: 的 width/height
    // 是 NSUInteger（64 位）；按 u32 传参时寄存器高位不确定，必须转成 u64。
    // 本消息需真实 CGVirtualDisplayMode 实例才能回归验证，无法单测。
    // SAFETY: alloc 的对象作为 receiver 走 init 初始化路径；失败返回 nil，
    // 由 retained 统一报错。
    let mode: *mut AnyObject = unsafe {
        msg_send![
            alloc,
            initWithWidth: u64::from(width),
            height: u64::from(height),
            refreshRate: refresh_rate
        ]
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
    // SAFETY: alloc 返回 +1 未初始化对象，随后作为 init 的 receiver。
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    // SAFETY: alloc 的对象作为 receiver 走 init 初始化路径；失败返回 nil，
    // 由 retained 统一报错。
    let desc: *mut AnyObject = unsafe { msg_send![alloc, init] };
    // SAFETY: init 返回 +1 对象。
    let desc = unsafe { retained(desc, "CGVirtualDisplayDescriptor init")? };
    let name = NSString::from_str(&opts.name);

    // SAFETY: desc 是刚 init 的有效对象；标量 setter，不转移所有权。
    unsafe {
        let _: () = msg_send![&*desc, setVendorID: opts.vendor_id];
    }
    // SAFETY: 同上：有效 receiver，标量 setter。
    unsafe {
        let _: () = msg_send![&*desc, setProductID: opts.product_id];
    }
    // SAFETY: 同上：有效 receiver，标量 setter。
    unsafe {
        let _: () = msg_send![&*desc, setSerialNumber: opts.serial_number];
    }
    // SAFETY: 有效 receiver；setName 借出 NSString 指针，name 活到消息返回。
    unsafe {
        let _: () = msg_send![&*desc, setName: &*name];
    }
    // SAFETY: 有效 receiver；CGSize 按值传入。
    unsafe {
        let _: () = msg_send![
            &*desc,
            setSizeInMillimeters: CGSize { width: opts.width_mm, height: opts.height_mm }
        ];
    }
    // SAFETY: 同上：有效 receiver，标量 setter。
    unsafe {
        let _: () = msg_send![&*desc, setMaxPixelsWide: opts.max_pixels_wide];
    }
    // SAFETY: 同上：有效 receiver，标量 setter。
    unsafe {
        let _: () = msg_send![&*desc, setMaxPixelsHigh: opts.max_pixels_high];
    }
    // Display P3 色域主色（默认值，与大多数现代 Mac 显示器一致）
    // SAFETY: 有效 receiver；CGPoint 按值传入。
    unsafe {
        let _: () = msg_send![&*desc, setRedPrimary: CGPoint { x: 0.680, y: 0.320 }];
    }
    // SAFETY: 同上：有效 receiver；CGPoint 按值传入。
    unsafe {
        let _: () = msg_send![&*desc, setGreenPrimary: CGPoint { x: 0.265, y: 0.690 }];
    }
    // SAFETY: 同上：有效 receiver；CGPoint 按值传入。
    unsafe {
        let _: () = msg_send![&*desc, setBluePrimary: CGPoint { x: 0.150, y: 0.060 }];
    }
    // SAFETY: 同上：有效 receiver；CGPoint 按值传入。
    unsafe {
        let _: () = msg_send![&*desc, setWhitePoint: CGPoint { x: 0.3127, y: 0.3290 }];
    }
    Ok(desc)
}

/// `CGVirtualDisplaySettings`：模式列表 + `HiDPI`。
pub fn create_settings(mode: &AnyObject) -> Result<Retained<AnyObject>> {
    let cls = class("CGVirtualDisplaySettings")?;
    // SAFETY: alloc 返回 +1 未初始化对象，随后作为 init 的 receiver。
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    // SAFETY: alloc 的对象作为 receiver 走 init 初始化路径；失败返回 nil，
    // 由 retained 统一报错。
    let settings: *mut AnyObject = unsafe { msg_send![alloc, init] };
    // SAFETY: init 返回 +1 对象。
    let settings = unsafe { retained(settings, "CGVirtualDisplaySettings init")? };
    let modes = NSArray::from_slice(&[mode]);
    // SAFETY: settings 是刚 init 的有效对象；setModes 借出数组指针，不转移所有权。
    unsafe {
        let _: () = msg_send![&*settings, setModes: &*modes];
    }
    // SAFETY: 同上：有效 receiver，标量 setter。
    unsafe {
        let _: () = msg_send![&*settings, setHiDPI: 1u32];
    }
    // setRotation: 不在 DeskPad 私有头 dump 的 CGVirtualDisplaySettings 属性
    // 列表里，某些 macOS 版本没有该 selector，无条件发消息会抛
    // NSInvalidArgumentException 直接 abort；必须先 respondsToSelector: 探测，
    // 存在才调。实参类型出处未定（0u32 沿用 DeskPad 原始写法），
    // macOS 26.5 实测可用。防护逻辑需真实 ObjC 运行时才能回归验证，无法单测。
    // SAFETY: 有效 receiver；respondsToSelector: 只读查询，SEL 为编译期常量，
    // BOOL 按值返回。
    let has_rotation: bool =
        unsafe { msg_send![&*settings, respondsToSelector: sel!(setRotation:)] };
    if has_rotation {
        // SAFETY: has_rotation 为真保证 selector 存在；有效 receiver，标量 setter。
        unsafe {
            let _: () = msg_send![&*settings, setRotation: 0u32];
        }
    }
    Ok(settings)
}

/// 创建虚拟显示器本体，并应用设置。
pub struct VirtualDisplay {
    pub obj: Retained<AnyObject>,
    pub display_id: u32,
}

pub fn create_display(descriptor: &AnyObject, settings: &AnyObject) -> Result<VirtualDisplay> {
    let cls = class("CGVirtualDisplay")?;
    // SAFETY: alloc 返回 +1 未初始化对象，随后作为 init 的 receiver。
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    // SAFETY: alloc 的对象作为 receiver 走 init 初始化路径；失败返回 nil，
    // 由 retained 统一报错。
    let display: *mut AnyObject = unsafe { msg_send![alloc, initWithDescriptor: descriptor] };
    // SAFETY: init 返回 +1 对象。
    let display = unsafe { retained(display, "CGVirtualDisplay init")? };
    // SAFETY: display 是刚 init 的有效对象；settings 借用传入，返回 bool 按值拷出。
    let applied: bool = unsafe { msg_send![&*display, applySettings: settings] };
    if !applied {
        return Err(anyhow!("CGVirtualDisplay applySettings failed"));
    }
    // SAFETY: 纯 getter；display 有效，u32 按值返回。
    let display_id: u32 = unsafe { msg_send![&*display, displayID] };
    Ok(VirtualDisplay {
        obj: display,
        display_id,
    })
}
