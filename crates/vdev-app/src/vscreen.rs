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
    // 二次 create 先销毁旧实例：直接覆盖槽位指针会泄漏旧 VirtualDisplay
    destroy();
    let opts = CreateOptions {
        name: "vdev-demo".to_string(),
        ..Default::default()
    };
    let vd = create(opts)?;
    let id = vd.display_id;
    *slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Box::into_raw(Box::new(vd)) as usize;
    Ok(id)
}

pub fn destroy() {
    let mut guard = slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *guard != 0 {
        // SAFETY：槽位非零时指向 create_display 存入的 Box<VirtualDisplay>，
        // 取回销毁后立即清零防二次释放；全程持锁与 display_id/display 读取互斥
        unsafe {
            drop(Box::from_raw(*guard as *mut VirtualDisplay));
        }
        *guard = 0;
    }
}

pub fn display_id() -> Option<u32> {
    let guard = slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if *guard == 0 {
        None
    } else {
        let p = *guard as *const VirtualDisplay;
        // SAFETY：持锁解引用——destroy 的释放同样在锁内，不会并发释放后读
        unsafe { Some((*p).display_id) }
    }
}
