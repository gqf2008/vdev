//! vdev-audio-ctl：设置/读取 vdev-audio 虚拟声卡的 DSP 参数（EQ + 增益）。
//! 用法：
//!   vdev-audio-ctl                  # 显示当前参数
//!   vdev-audio-ctl set g l m h      # 设置 gain/low/mid/high（dB）
//!   vdev-audio-ctl reset            # 全部归零（直通）

// 档位对齐 vdev-audio-win：CoreAudio C 头镜像结构与宽度转换属 FFI 惯例。
#![allow(clippy::cast_possible_truncation)] // FFI 结构字段宽度转换
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::struct_field_names)] // CoreAudio C 头结构体 m 前缀字段按原样镜像

use std::ffi::c_void;

const OBJ_SYSTEM: u32 = 1;
const SEL_DEVICES: u32 = 0x6465_7623; // 'dev#' kAudioHardwarePropertyDevices
const SEL_NAME: u32 = 0x6c6e_616d; // 'lnam' kAudioObjectPropertyName
const SEL_VDSP: u32 = 0x7664_7370; // 'vdsp' 自定义 DSP 参数
const SEL_VRUT: u32 = 0x7672_7574; // 'vrut' 路由矩阵
const SCOPE_GLOBAL: u32 = 0x676c_6f62; // 'glob'
const ELEM_MAIN: u32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct AudioObjectPropertyAddress {
    m_selector: u32,
    m_scope: u32,
    m_element: u32,
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(obj: *const c_void);
    fn CFStringGetCString(
        s: *const c_void,
        buf: *mut std::os::raw::c_char,
        len: isize,
        encoding: u32,
    ) -> bool;
    fn CFStringCreateWithCString(
        alloc: *const c_void,
        cstr: *const std::os::raw::c_char,
        encoding: u32,
    ) -> *mut c_void;
}
const CF_UTF8: u32 = 0x0800_0100; // kCFStringEncodingUTF8

#[link(name = "CoreAudio", kind = "framework")]
extern "C" {
    fn AudioObjectGetPropertyDataSize(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qsize: u32,
        qdata: *const c_void,
        size: *mut u32,
    ) -> i32;
    fn AudioObjectGetPropertyData(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qsize: u32,
        qdata: *const c_void,
        size: *mut u32,
        data: *mut c_void,
    ) -> i32;
    fn AudioObjectSetPropertyData(
        object: u32,
        address: *const AudioObjectPropertyAddress,
        qsize: u32,
        qdata: *const c_void,
        size: u32,
        data: *const c_void,
    ) -> i32;
}

fn addr(sel: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        m_selector: sel,
        m_scope: SCOPE_GLOBAL,
        m_element: ELEM_MAIN,
    }
}

// 找 vdev-audio 设备 ID（名字包含 vdev-audio）
fn find_device() -> Option<u32> {
    let a = addr(SEL_DEVICES);
    let mut size = 0u32;
    // SAFETY：设备枚举/名称查询为只读调用，size/ids/name_ref 均为本函数持有的可写缓冲
    if unsafe {
        AudioObjectGetPropertyDataSize(OBJ_SYSTEM, &raw const a, 0, std::ptr::null(), &raw mut size)
    } != 0
    {
        return None;
    }
    let n = size as usize / std::mem::size_of::<u32>();
    let mut ids = vec![0u32; n];
    if unsafe {
        AudioObjectGetPropertyData(
            OBJ_SYSTEM,
            &raw const a,
            0,
            std::ptr::null(),
            &raw mut size,
            ids.as_mut_ptr().cast::<c_void>(),
        )
    } != 0
    {
        return None;
    }
    for id in ids {
        let na = addr(SEL_NAME);
        let mut nsize = std::mem::size_of::<*mut c_void>() as u32;
        let mut name_ref: *mut c_void = std::ptr::null_mut();
        if unsafe {
            AudioObjectGetPropertyData(
                id,
                &raw const na,
                0,
                std::ptr::null(),
                &raw mut nsize,
                (&raw mut name_ref).cast::<c_void>(),
            )
        } == 0
            && !name_ref.is_null()
        {
            let mut buf = [0 as std::os::raw::c_char; 256];
            let ok = unsafe { CFStringGetCString(name_ref, buf.as_mut_ptr(), 256, CF_UTF8) };
            // 释放 CFString（GetPropertyData 返回 +1）
            unsafe { CFRelease(name_ref) };
            if ok {
                let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
                    .to_string_lossy()
                    .to_string();
                if s.to_lowercase().contains("vdev-audio") {
                    return Some(id);
                }
            }
        }
    }
    None
}

fn get_cf_string(dev: u32, sel: u32) -> Option<String> {
    let a = addr(sel);
    let mut cf: *mut c_void = std::ptr::null_mut();
    let mut size = std::mem::size_of::<*mut c_void>() as u32;
    let rc = unsafe {
        AudioObjectGetPropertyData(
            dev,
            &raw const a,
            0,
            std::ptr::null(),
            &raw mut size,
            (&raw mut cf).cast::<c_void>(),
        )
    };
    if rc != 0 || cf.is_null() {
        return None;
    }
    let mut buf = [0 as std::os::raw::c_char; 128];
    let ok = unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), 128, CF_UTF8) };
    unsafe { CFRelease(cf) };
    if ok {
        unsafe {
            Some(
                std::ffi::CStr::from_ptr(buf.as_ptr())
                    .to_string_lossy()
                    .to_string(),
            )
        }
    } else {
        None
    }
}
fn set_cf_string(dev: u32, sel: u32, s: &str) -> bool {
    let a = addr(sel);
    let c = std::ffi::CString::new(s).unwrap();
    let cf = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_UTF8) };
    if cf.is_null() {
        return false;
    }
    let rc = unsafe {
        AudioObjectSetPropertyData(
            dev,
            &raw const a,
            0,
            std::ptr::null(),
            std::mem::size_of::<*mut c_void>() as u32,
            (&raw const cf).cast::<c_void>(),
        )
    };
    unsafe { CFRelease(cf) };
    rc == 0
}

fn get_params(dev: u32) -> Option<[f32; 4]> {
    let a = addr(SEL_VDSP);
    let mut cf: *mut c_void = std::ptr::null_mut();
    let mut size = std::mem::size_of::<*mut c_void>() as u32;
    let rc = unsafe {
        AudioObjectGetPropertyData(
            dev,
            &raw const a,
            0,
            std::ptr::null(),
            &raw mut size,
            (&raw mut cf).cast::<c_void>(),
        )
    };
    if rc != 0 || cf.is_null() {
        eprintln!("GetPropertyData(vdsp) rc={rc}");
        return None;
    }
    let mut buf = [0 as std::os::raw::c_char; 128];
    if !unsafe { CFStringGetCString(cf, buf.as_mut_ptr(), 128, CF_UTF8) } {
        unsafe { CFRelease(cf) };
        return None;
    }
    unsafe { CFRelease(cf) };
    let s = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }
        .to_string_lossy()
        .to_string();
    let v: Vec<f32> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    if v.len() == 4 {
        Some([v[0], v[1], v[2], v[3]])
    } else {
        None
    }
}

fn set_params(dev: u32, p: &[f32; 4]) -> bool {
    let a = addr(SEL_VDSP);
    let s = format!("{:.1},{:.1},{:.1},{:.1}", p[0], p[1], p[2], p[3]);
    let c = std::ffi::CString::new(s).unwrap();
    let cf = unsafe { CFStringCreateWithCString(std::ptr::null(), c.as_ptr(), CF_UTF8) };
    if cf.is_null() {
        return false;
    }
    let rc = unsafe {
        AudioObjectSetPropertyData(
            dev,
            &raw const a,
            0,
            std::ptr::null(),
            std::mem::size_of::<*mut c_void>() as u32,
            (&raw const cf).cast::<c_void>(),
        )
    };
    unsafe { CFRelease(cf) };
    rc == 0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(dev) = find_device() else {
        eprintln!("未找到 vdev-audio 设备，请先 make install 安装驱动");
        std::process::exit(1);
    };

    if args.len() >= 2 && args[1] == "set" && args.len() == 6 {
        let v: Vec<f32> = args[2..6]
            .iter()
            .map(|s| s.parse().unwrap_or(0.0))
            .collect();
        let p = [v[0], v[1], v[2], v[3]];
        if set_params(dev, &p) {
            println!(
                "已设置 DSP 参数 gain={} low={} mid={} high={} (dB)",
                p[0], p[1], p[2], p[3]
            );
        } else {
            eprintln!("设置失败");
            std::process::exit(1);
        }
    } else if args.len() >= 2 && args[1] == "reset" {
        if set_params(dev, &[0.0, 0.0, 0.0, 0.0]) {
            println!("DSP 已重置为直通");
        } else {
            eprintln!("重置失败");
            std::process::exit(1);
        }
    } else if args.len() == 1 {
        if let Some(p) = get_params(dev) {
            println!("gain={} low={} mid={} high={} (dB)", p[0], p[1], p[2], p[3]);
        } else {
            eprintln!("读取失败");
            std::process::exit(1);
        }
    } else if args.len() >= 2 && args[1] == "route" && args.len() == 6 {
        let v: Vec<f32> = args[2..6]
            .iter()
            .map(|s| s.parse().unwrap_or(0.0))
            .collect();
        let s = format!("{:.1},{:.1},{:.1},{:.1}", v[0], v[1], v[2], v[3]);
        if set_cf_string(dev, SEL_VRUT, &s) {
            println!(
                "已设置路由矩阵 [[{:.1},{:.1}],[{:.1},{:.1}]]",
                v[0], v[1], v[2], v[3]
            );
        } else {
            eprintln!("设置失败");
            std::process::exit(1);
        }
    } else if args.len() >= 2 && args[1] == "route" && args.len() == 2 {
        if let Some(s) = get_cf_string(dev, SEL_VRUT) {
            println!("route={s}");
        } else {
            eprintln!("读取失败");
            std::process::exit(1);
        }
    } else {
        eprintln!("用法: vdev-audio-ctl [set g l m h | reset | route [r00 r01 r10 r11]]");
        std::process::exit(1);
    }
}
