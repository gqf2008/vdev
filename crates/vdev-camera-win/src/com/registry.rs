//! 注册表安全封装（DirectShow 过滤器注册/注销）。
//!
//! DirectShow 过滤器以 COM 组件形式注册在 `HKCR\CLSID\<clsid>`，
//! 要出现在「视频捕获源」（Video Capture Sources，摄像头列表）还需在
//! `CLSID_VideoInputDeviceCategory` 的 `Instance` 键下登记。

use std::io;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegSetValueExW, HKEY, KEY_ALL_ACCESS, REG_BINARY,
    REG_OPTION_NON_VOLATILE, REG_SZ,
};
use windows_core::{GUID, PCWSTR};

/// 把 `&str` 转成带结尾 NUL 的 UTF-16。
fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 已打开的注册表键（RAII：析构时关闭）。
pub struct RegKey(HKEY);

impl RegKey {
    /// 打开或创建非易失键。
    pub fn create(root: HKEY, path: &str) -> io::Result<Self> {
        let wide = to_wide(path);
        let mut key = HKEY::default();
        // SAFETY: wide 在调用期间存活；其余参数为空/默认值。
        let status = unsafe {
            RegCreateKeyExW(
                root,
                PCWSTR(wide.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_ALL_ACCESS,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.0 as i32));
        }
        Ok(Self(key))
    }

    /// 设置一个 `REG_SZ` 字符串值。
    pub fn set_string(&self, name: &str, value: &str) -> io::Result<()> {
        let name_wide = to_wide(name);
        let value_wide = to_wide(value);
        // REG_SZ 要求包含结尾 NUL：value_wide 已含 NUL，按字节传。
        let bytes = unsafe {
            std::slice::from_raw_parts(value_wide.as_ptr() as *const u8, value_wide.len() * 2)
        };
        // SAFETY: name_wide 在调用期间存活；bytes 指向 value_wide 的完整字节（含 NUL）。
        let status = unsafe {
            RegSetValueExW(
                self.0,
                PCWSTR(name_wide.as_ptr()),
                None,
                REG_SZ,
                Some(bytes),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.0 as i32));
        }
        Ok(())
    }

    /// 设置一个 REG_BINARY 值。
    pub fn set_binary(&self, name: &str, value: &[u8]) -> io::Result<()> {
        let name_wide = to_wide(name);
        // SAFETY: name_wide 在调用期间存活；value 指向的切片由调用方保证存活。
        let status = unsafe {
            RegSetValueExW(
                self.0,
                PCWSTR(name_wide.as_ptr()),
                None,
                REG_BINARY,
                Some(value),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(io::Error::from_raw_os_error(status.0 as i32));
        }
        Ok(())
    }
}

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: 句柄在 Drop 前有效且未被其他路径关闭。
        unsafe {
            let _ = RegCloseKey(self.0);
        };
    }
}

/// 删除一个键及其子树；键不存在视为成功。
pub fn delete_tree(root: HKEY, path: &str) -> io::Result<()> {
    let wide = to_wide(path);
    // SAFETY: wide 在调用期间存活。
    let status = unsafe { RegDeleteTreeW(root, PCWSTR(wide.as_ptr())) };
    if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND || status == ERROR_PATH_NOT_FOUND {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.0 as i32))
    }
}

/// 当前模块（DLL/EXE）的完整路径。
pub fn module_file_path() -> io::Result<String> {
    let mut buf = vec![0u16; 2048];
    // SAFETY: buf 足够大，GetModuleFileNameW 写入后返回实际长度。
    let len = unsafe { GetModuleFileNameW(None, &mut buf) };
    if len == 0 {
        return Err(io::Error::last_os_error());
    }
    buf.truncate(len as usize);
    Ok(String::from_utf16_lossy(&buf))
}

/// 把 GUID 格式化为注册表路径用的 `{XXXXXXXX-...}` 字符串。
pub fn guid_string(g: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}
