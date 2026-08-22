//! 设备枚举安全封装：列出系统「视频捕获源」（DirectShow 摄像头列表）。
//!
//! 把 `ICreateDevEnum` + `IMoniker` + `IPropertyBag` 的全部裸指针交互收敛到这里，
//! 上层（CLI）只调用 [`list_video_capture_devices`]，不直接写 unsafe。
//! 调用方需先 `ComInit`。

use anyhow::{Context, Result};
use windows::Win32::Foundation::S_OK;
use windows::Win32::Media::DirectShow::ICreateDevEnum;
use windows::Win32::Media::MediaFoundation::{
    CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory,
};
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{CoCreateInstance, IEnumMoniker, IErrorLog, CLSCTX_ALL};
use windows::Win32::System::Variant::{VariantClear, VARIANT};
use windows_core::PCWSTR;

/// 枚举视频捕获源，返回每个设备的显示名（`FriendlyName`）。
pub fn list_video_capture_devices() -> Result<Vec<String>> {
    // SAFETY: CoCreateInstance 返回 ICreateDevEnum 并持有引用；参数为合法常量。
    let dev_enum: ICreateDevEnum =
        unsafe { CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_ALL) }
            .context("create system device enum")?;

    let mut moniker_enum: Option<IEnumMoniker> = None;
    // SAFETY: CreateClassEnumerator 通过 Option 输出枚举器；失败时内部已释放。
    unsafe {
        dev_enum.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut moniker_enum, 0)
    }
    .context("CreateClassEnumerator")?;

    let Some(moniker_enum) = moniker_enum else {
        return Ok(Vec::new());
    };

    let mut names = Vec::new();
    loop {
        let mut monikers = [None];
        // SAFETY: Next 写入 monikers 槽位（引用计数由调用方接管）；返回非 S_OK 即枚举结束。
        let hr = unsafe { moniker_enum.Next(&mut monikers, None) };
        if hr != S_OK {
            break;
        }
        let Some(moniker) = monikers[0].take() else {
            break;
        };
        // SAFETY: BindToStorage 输出 IPropertyBag 并持有引用。
        let bag: IPropertyBag =
            match unsafe { moniker.BindToStorage::<_, _, IPropertyBag>(None, None) } {
                Ok(b) => b,
                Err(_) => continue,
            };

        let name_wide: Vec<u16> = "FriendlyName"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut var = VARIANT::default();
        // SAFETY: name_wide 在调用期间存活；var 由本函数用 VariantClear 释放。
        let hr = unsafe {
            bag.Read(
                PCWSTR(name_wide.as_ptr()),
                &mut var,
                Option::<&IErrorLog>::None,
            )
        };
        if hr.is_ok() {
            // SAFETY: 读 VT_BSTR 联合字段；BSTR 由 VariantClear 统一释放。
            let bstr = unsafe { &var.Anonymous.Anonymous.Anonymous.bstrVal };
            names.push(bstr.to_string());
        }
        // SAFETY: VariantClear 释放 VARIANT 内联 BSTR。
        unsafe { VariantClear(&mut var) }.ok();
    }
    Ok(names)
}
