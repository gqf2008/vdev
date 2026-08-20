//! 虚拟屏幕：复用 vdev-screen（CGVirtualDisplay 私有 API）。
use anyhow::Result;
use std::sync::{Mutex, OnceLock};
use vdev_screen::{create, CreateOptions, VirtualDisplay};

static VD: OnceLock<Mutex<usize>> = OnceLock::new();

fn slot() -> &'static Mutex<usize> {
    VD.get_or_init(|| Mutex::new(0))
}

/// 创建虚拟屏幕并保持存活（App 退出即销毁）。
pub fn create_display() -> Result<u32> {
    let opts = CreateOptions {
        name: "vdev-demo".to_string(),
        ..Default::default()
    };
    let vd = create(opts)?;
    let id = vd.display_id;
    *slot().lock().unwrap_or_else(|e| e.into_inner()) = Box::into_raw(Box::new(vd)) as usize;
    Ok(id)
}

pub fn destroy() {
    let p = *slot().lock().unwrap_or_else(|e| e.into_inner());
    if p != 0 {
        unsafe {
            drop(Box::from_raw(p as *mut VirtualDisplay));
        }
        *slot().lock().unwrap_or_else(|e| e.into_inner()) = 0;
    }
}

pub fn display_id() -> Option<u32> {
    let p = *slot().lock().unwrap_or_else(|e| e.into_inner());
    if p == 0 {
        None
    } else {
        unsafe { Some((*(p as *const VirtualDisplay)).display_id) }
    }
}
