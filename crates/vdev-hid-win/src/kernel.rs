//! vdev-hid-win 内核路线（路线 B）：虚拟 HID 内核驱动的安装/状态与报告注入。
//! 安装/卸载/状态走 SetupAPI（HIDClass，Root\vdev-hid）；注入经 HID 接口
//! WriteFile 8 字节键盘报告（厂商输出管道），由驱动投递给 hidclass。

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    DIIRFLAG_FORCE_INF, DiInstallDriverW, GUID_DEVCLASS_HIDCLASS, SETUP_DI_GET_CLASS_DEVS_FLAGS,
    SETUP_DI_REGISTRY_PROPERTY, SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA, SPDRP_DRIVER,
    SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList,
    SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW, SetupDiOpenDeviceInfoW,
    SetupDiSetDeviceRegistryPropertyW,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HidD_GetAttributes, HidD_GetHidGuid,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, WriteFile,
};
use windows::core::PCWSTR;

pub const HARDWARE_ID: &str = r"Root\vdev-hid";
/// vdev 虚拟键盘 VID/PID（与驱动一致）
const VID: u16 = 0x5644;

/// 8 字节键盘报告：1 修饰键 + 1 保留 + 6 按键
const KEYBOARD_REPORT_SIZE: usize = 8;

// ---------------- 安装 / 卸载 / 状态（SetupAPI） ----------------

fn read_multi_sz(
    devs: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    info: &SP_DEVINFO_DATA,
    prop: SETUP_DI_REGISTRY_PROPERTY,
) -> Vec<u16> {
    let mut required: u32 = 0;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let slice: &mut [u8] = &mut buf;
        let ok = unsafe {
            SetupDiGetDeviceRegistryPropertyW(
                devs,
                info,
                prop,
                None,
                Some(slice),
                Some(&mut required),
            )
        };
        if ok.is_ok() {
            break;
        }
        let err = windows::core::Error::from_win32();
        if err.code().0 as u32 != 0x8007_007A {
            return Vec::new();
        }
        if required == 0 {
            return Vec::new();
        }
        buf.resize(required as usize, 0);
    }
    buf.chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect()
}

fn wide_contains(haystack: &[u16], needle: &str) -> bool {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

fn find_device() -> Result<Option<SP_DEVINFO_DATA>> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_HIDCLASS),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;
    let mut found = None;
    let mut index = 0u32;
    loop {
        let mut info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInfo(devs, index, &mut info) }.is_err() {
            break;
        }
        index += 1;
        let hwids = read_multi_sz(devs, &info, SPDRP_HARDWAREID);
        if wide_contains(&hwids, HARDWARE_ID) {
            found = Some(info);
            break;
        }
    }
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    Ok(found)
}

fn remove_all_nodes() -> Result<usize> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_HIDCLASS),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;
    let mut to_remove: Vec<SP_DEVINFO_DATA> = Vec::new();
    let mut index = 0u32;
    loop {
        let mut info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInfo(devs, index, &mut info) }.is_err() {
            break;
        }
        index += 1;
        let hwids = read_multi_sz(devs, &info, SPDRP_HARDWAREID);
        if wide_contains(&hwids, HARDWARE_ID) {
            to_remove.push(info);
        }
    }
    for info in &mut to_remove {
        let params = windows::Win32::Devices::DeviceAndDriverInstallation::SP_REMOVEDEVICE_PARAMS {
            ClassInstallHeader:
                windows::Win32::Devices::DeviceAndDriverInstallation::SP_CLASSINSTALL_HEADER {
                    cbSize: std::mem::size_of::<
                        windows::Win32::Devices::DeviceAndDriverInstallation::SP_REMOVEDEVICE_PARAMS,
                    >() as u32,
                    InstallFunction: DIF_REMOVE,
                },
            Scope: windows::Win32::Devices::DeviceAndDriverInstallation::DI_REMOVEDEVICE_GLOBAL,
            HwProfile: 0,
        };
        unsafe {
            windows::Win32::Devices::DeviceAndDriverInstallation::SetupDiSetDeviceInstallParamsW(
                devs,
                Some(info),
                (&params as *const windows::Win32::Devices::DeviceAndDriverInstallation::SP_REMOVEDEVICE_PARAMS)
                    .cast::<windows::Win32::Devices::DeviceAndDriverInstallation::SP_DEVINSTALL_PARAMS_W>(),
            )
        }
        .context("SetupDiSetDeviceInstallParamsW failed")?;
        unsafe { SetupDiCallClassInstaller(DIF_REMOVE, devs, Some(info)) }
            .with_context(|| "DIF_REMOVE failed")?;
    }
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    Ok(to_remove.len())
}

/// 安装驱动：清理旧节点 + 创建设备节点 + DiInstallDriverW 装入驱动存储
pub fn install(inf_dir: &Path) -> Result<()> {
    let inf_path = inf_dir.join("vdev-hid.inf");
    if !inf_path.exists() {
        bail!("找不到 INF: {}", inf_path.display());
    }

    let removed = remove_all_nodes()?;
    if removed > 0 {
        println!("已清理 {removed} 个残留设备节点");
    }

    let inf_wide: Vec<u16> = inf_path
        .as_os_str()
        .to_str()
        .context("INF 路径不是合法 UTF-8")?
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut class_guid = windows::core::GUID::zeroed();
    let mut class_name = [0u16; 256];
    unsafe {
        SetupDiGetINFClassW(
            PCWSTR(inf_wide.as_ptr()),
            &mut class_guid,
            &mut class_name,
            None,
        )
    }
    .context("SetupDiGetINFClassW failed")?;

    let devs = unsafe { SetupDiCreateDeviceInfoList(Some(&class_guid), None) }
        .context("SetupDiCreateDeviceInfoList failed")?;

    let result = (|| -> Result<()> {
        let mut dev_info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        let class_name_cstr: Vec<u16> = class_name
            .iter()
            .take_while(|&&c| c != 0)
            .copied()
            .chain(std::iter::once(0))
            .collect();
        let create_res = unsafe {
            SetupDiCreateDeviceInfoW(
                devs,
                PCWSTR(class_name_cstr.as_ptr()),
                &class_guid,
                None,
                None,
                DICD_GENERATE_ID,
                Some(&mut dev_info),
            )
        };
        match create_res {
            Ok(()) => {}
            Err(e) if e.code().0 as u32 == 0xE0000207 => {
                unsafe {
                    SetupDiOpenDeviceInfoW(
                        devs,
                        PCWSTR(class_name_cstr.as_ptr()),
                        None,
                        0,
                        Some(&mut dev_info),
                    )
                }
                .with_context(|| "SetupDiOpenDeviceInfoW failed")?;
            }
            Err(e) => return Err(e).context("SetupDiCreateDeviceInfoW failed"),
        }
        let mut hwid: Vec<u16> = HARDWARE_ID.encode_utf16().collect();
        hwid.push(0);
        hwid.push(0);
        let bytes = hwid
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect::<Vec<u8>>();
        unsafe {
            SetupDiSetDeviceRegistryPropertyW(devs, &mut dev_info, SPDRP_HARDWAREID, Some(&bytes))
        }
        .context("SetupDiSetDeviceRegistryPropertyW(SPDRP_HARDWAREID) failed")?;
        unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, devs, Some(&dev_info)) }
            .with_context(|| "DIF_REGISTERDEVICE failed")?;

        let inf_path = std::fs::canonicalize(&inf_path).context("无法解析 INF 绝对路径")?;
        let inf_path_wide: Vec<u16> = inf_path
            .as_os_str()
            .to_str()
            .context("INF 路径不是合法 UTF-8")?
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut reboot = windows::Win32::Foundation::BOOL(0);
        unsafe {
            DiInstallDriverW(
                None,
                PCWSTR(inf_path_wide.as_ptr()),
                DIIRFLAG_FORCE_INF,
                Some(&mut reboot),
            )
        }
        .with_context(|| "DiInstallDriverW failed（内核驱动需测试签名或已签名证书）")?;
        if reboot.as_bool() {
            println!("系统提示需要重启以完成安装");
        }
        Ok(())
    })();

    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result
}

/// 卸载
pub fn uninstall() -> Result<bool> {
    let Some(dev_info) = find_device()? else {
        println!("未找到 vdev 虚拟键盘设备");
        return Ok(false);
    };
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_HIDCLASS),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;
    let params = windows::Win32::Devices::DeviceAndDriverInstallation::SP_REMOVEDEVICE_PARAMS {
        ClassInstallHeader:
            windows::Win32::Devices::DeviceAndDriverInstallation::SP_CLASSINSTALL_HEADER {
                cbSize: std::mem::size_of::<
                    windows::Win32::Devices::DeviceAndDriverInstallation::SP_REMOVEDEVICE_PARAMS,
                >() as u32,
                InstallFunction: DIF_REMOVE,
            },
        Scope: windows::Win32::Devices::DeviceAndDriverInstallation::DI_REMOVEDEVICE_GLOBAL,
        HwProfile: 0,
    };
    unsafe {
        windows::Win32::Devices::DeviceAndDriverInstallation::SetupDiSetDeviceInstallParamsW(
            devs,
            Some(&dev_info),
            (&params as *const windows::Win32::Devices::DeviceAndDriverInstallation::SP_REMOVEDEVICE_PARAMS)
                .cast::<windows::Win32::Devices::DeviceAndDriverInstallation::SP_DEVINSTALL_PARAMS_W>(),
        )
    }
    .context("SetupDiSetDeviceInstallParamsW failed")?;
    unsafe { SetupDiCallClassInstaller(DIF_REMOVE, devs, Some(&dev_info)) }
        .with_context(|| "DIF_REMOVE failed")?;
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    println!("已移除 vdev 虚拟键盘设备");
    Ok(true)
}

/// 设备状态
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeviceStatus {
    pub present: bool,
    pub driver: Option<String>,
    pub friendly_name: Option<String>,
}

/// 查询设备状态
pub fn status() -> Result<DeviceStatus> {
    let Some(dev_info) = find_device()? else {
        return Ok(DeviceStatus {
            present: false,
            driver: None,
            friendly_name: None,
        });
    };
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_HIDCLASS),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;
    let driver = {
        let buf = read_multi_sz(devs, &dev_info, SPDRP_DRIVER);
        if buf.is_empty() {
            None
        } else {
            Some(
                String::from_utf16_lossy(&buf)
                    .trim_end_matches('\0')
                    .to_string(),
            )
        }
    };
    let friendly_name = {
        let buf = read_multi_sz(devs, &dev_info, SPDRP_FRIENDLYNAME);
        if buf.is_empty() {
            None
        } else {
            Some(
                String::from_utf16_lossy(&buf)
                    .trim_end_matches('\0')
                    .to_string(),
            )
        }
    };
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    Ok(DeviceStatus {
        present: true,
        driver,
        friendly_name,
    })
}

// ---------------- 报告注入（经 HID 接口 WriteFile） ----------------

/// 按 VID/PID 找到 vdev 虚拟键盘的 HID 设备路径
fn find_hid_path(pid: u16) -> Result<String> {
    let hid_guid = unsafe { HidD_GetHidGuid() };
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&hid_guid),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(DIGCF_PRESENT.0 | DIGCF_DEVICEINTERFACE.0),
        )
    }
    .context("SetupDiGetClassDevsW(hid) failed")?;
    let mut result = None;
    let mut index = 0u32;
    loop {
        let mut iface = SP_DEVICE_INTERFACE_DATA {
            cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
            ..Default::default()
        };
        if unsafe { SetupDiEnumDeviceInterfaces(devs, None, &hid_guid, index, &mut iface) }.is_err()
        {
            break;
        }
        index += 1;
        // 先取所需大小
        let mut required: u32 = 0;
        let _ = unsafe {
            SetupDiGetDeviceInterfaceDetailW(devs, &iface, None, 0, Some(&mut required), None)
        };
        let err = windows::core::Error::from_win32();
        if err.code().0 as u32 != 0x8007_007A {
            continue;
        }
        // required 含 SP_DEVICE_INTERFACE_DETAIL_DATA_W 头；分配足够空间
        let mut buf = vec![0u8; required as usize + 8];
        let detail = buf.as_mut_ptr().cast::<windows::Win32::Devices::DeviceAndDriverInstallation::SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
        unsafe {
            (*detail).cbSize = std::mem::size_of::<windows::Win32::Devices::DeviceAndDriverInstallation::SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
        }
        let mut size = required;
        let ok = unsafe {
            SetupDiGetDeviceInterfaceDetailW(
                devs,
                &iface,
                Some(&mut *detail),
                buf.len() as u32,
                Some(&mut size),
                None,
            )
        };
        if ok.is_err() {
            continue;
        }
        // 设备路径紧跟在 cbSize 之后（wchar_t 对齐）
        let path_ptr = unsafe { detail.cast::<u8>().add(std::mem::size_of::<u32>()) }.cast::<u16>();
        let mut path_w: Vec<u16> = Vec::new();
        let mut i = 0usize;
        unsafe {
            loop {
                let c = *path_ptr.add(i);
                if c == 0 {
                    break;
                }
                path_w.push(c);
                i += 1;
            }
        }
        let path = String::from_utf16_lossy(&path_w);
        // 打开并核对 VID/PID
        let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                GENERIC_READ.0 | GENERIC_WRITE.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            )
        }
        .ok();
        let Some(handle) = handle else { continue };
        let mut attrs = HIDD_ATTRIBUTES {
            Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
            ..Default::default()
        };
        let ok = unsafe { HidD_GetAttributes(handle, &mut attrs) };
        unsafe { CloseHandle(handle) }.ok();
        if ok.as_bool() && attrs.VendorID == VID && attrs.ProductID == pid {
            result = Some(path);
            break;
        }
    }
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result.context("未找到 vdev 虚拟键盘 HID 设备（先安装驱动）")
}

/// 写入一个 8 字节键盘报告（按下/抬起）
pub fn write_report(pid: u16, report: &[u8]) -> Result<()> {
    let path = find_hid_path(pid)?;
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .context("打开 vdev 虚拟 HID 设备失败")?;
    if handle == INVALID_HANDLE_VALUE {
        bail!("打开 vdev 虚拟 HID 设备失败（设备可能未安装或已被占用）");
    }
    let mut written = 0u32;
    let ok = unsafe { WriteFile(handle, Some(report), Some(&mut written), None) };
    unsafe { CloseHandle(handle) }.ok();
    if ok.is_err() {
        bail!("写入 HID 报告失败：{}", windows::core::Error::from_win32());
    }
    Ok(())
}

/// 键名 → （修饰键位, HID 用法码）
pub fn key_to_hid(name: &str) -> Result<(u8, Option<u8>)> {
    let n = name.to_ascii_lowercase();
    // 修饰键：报告第 0 字节位（直接返回，不占按键槽）
    match n.as_str() {
        "ctrl" | "control" => return Ok((0x01, None)),
        "shift" => return Ok((0x02, None)),
        "alt" => return Ok((0x04, None)),
        "win" | "lwin" => return Ok((0x08, None)),
        _ => {}
    }
    // 单字母 a-z -> 0x04..
    if n.len() == 1
        && let Some(c) = n.chars().next()
    {
        if c.is_ascii_lowercase() {
            return Ok((0, Some(0x04 + (c as u8 - b'a'))));
        }
        if c.is_ascii_digit() {
            return Ok((0, Some(0x1E + (c as u8 - b'0'))));
        }
    }
    // F1-F24
    if let Some(f) = n.strip_prefix('f')
        && let Ok(num) = f.parse::<u16>()
        && (1..=24).contains(&num)
    {
        let usage = if num <= 12 {
            0x3A + num - 1
        } else {
            0x68 + num - 13
        };
        return Ok((0, Some(usage as u8)));
    }
    let map: &[(&str, u8)] = &[
        ("enter", 0x28),
        ("return", 0x28),
        ("esc", 0x29),
        ("escape", 0x29),
        ("backspace", 0x2A),
        ("tab", 0x2B),
        ("space", 0x2C),
        ("minus", 0x2D),
        ("equal", 0x2E),
        ("lbrace", 0x2F),
        ("rbrace", 0x30),
        ("backslash", 0x31),
        ("semicolon", 0x33),
        ("quote", 0x34),
        ("grave", 0x35),
        ("comma", 0x36),
        ("period", 0x37),
        ("slash", 0x38),
        ("capslock", 0x39),
        ("caps", 0x39),
        ("f1", 0x3A),
        ("f2", 0x3B),
        ("f3", 0x3C),
        ("f4", 0x3D),
        ("f5", 0x3E),
        ("f6", 0x3F),
        ("f7", 0x40),
        ("f8", 0x41),
        ("f9", 0x42),
        ("f10", 0x43),
        ("f11", 0x44),
        ("f12", 0x45),
        ("printscreen", 0x46),
        ("scrolllock", 0x47),
        ("pause", 0x48),
        ("insert", 0x49),
        ("ins", 0x49),
        ("home", 0x4A),
        ("pageup", 0x4B),
        ("delete", 0x4C),
        ("del", 0x4C),
        ("end", 0x4D),
        ("pagedown", 0x4E),
        ("right", 0x4F),
        ("left", 0x50),
        ("down", 0x51),
        ("up", 0x52),
        ("numlock", 0x53),
        ("numpad0", 0x62),
        ("numpad1", 0x59),
        ("numpad2", 0x5A),
        ("numpad3", 0x5B),
        ("numpad4", 0x5C),
        ("numpad5", 0x5D),
        ("numpad6", 0x5E),
        ("numpad7", 0x5F),
        ("numpad8", 0x60),
        ("numpad9", 0x61),
    ];
    if let Some((_, usage)) = map.iter().find(|(k, _)| *k == n.as_str()) {
        return Ok((0, Some(*usage)));
    }
    bail!(
        "未知键名：{name}（支持 a-z/0-9/F1-F24/enter/tab/space/esc/backspace/arrows/ctrl/alt/shift/win 等）"
    )
}

/// 构造键盘报告：mods 为修饰位，usage 为按键（None 表示纯修饰键）
pub fn make_report(mods: u8, usage: Option<u8>) -> [u8; KEYBOARD_REPORT_SIZE] {
    let mut r = [0u8; KEYBOARD_REPORT_SIZE];
    r[0] = mods;
    if let Some(u) = usage {
        r[2] = u;
    }
    r
}
/// 键盘 HID 设备 PID（"HI"）
pub const PID_KBD: u16 = 0x4849;

/// 鼠标 HID 设备 PID（"HM"）
pub const PID_MOUSE: u16 = 0x484D;
/// 鼠标报告长度：1 键位 + X + Y + 滚轮
pub const MOUSE_REPORT_SIZE: usize = 4;

/// 构造 4 字节鼠标报告（键位 + 相对 X/Y + 滚轮，带符号）
pub fn mouse_report(buttons: u8, dx: i8, dy: i8, wheel: i8) -> [u8; MOUSE_REPORT_SIZE] {
    [buttons, dx as u8, dy as u8, wheel as u8]
}

/// 鼠标按键位（HID：bit0 左 / bit1 右 / bit2 中）
pub fn mouse_button_bit(button: &str) -> Result<u8> {
    match button.to_ascii_lowercase().as_str() {
        "left" => Ok(0x01),
        "right" => Ok(0x02),
        "middle" => Ok(0x04),
        _ => bail!("未知鼠标按键：{button}（left/right/middle）"),
    }
}

/// 相对移动
pub fn mouse_move(dx: i32, dy: i32) -> Result<()> {
    let dx = dx.clamp(-127, 127) as i8;
    let dy = dy.clamp(-127, 127) as i8;
    let rep = mouse_report(0, dx, dy, 0);
    write_report(PID_MOUSE, &rep)?;
    Ok(())
}

/// 按键动作（down/up/click）
pub fn mouse_button(button: &str, action: &str) -> Result<()> {
    let bit = mouse_button_bit(button)?;
    match action {
        "down" => {
            let rep = mouse_report(bit, 0, 0, 0);
            write_report(PID_MOUSE, &rep)?;
        }
        "up" => {
            let rep = mouse_report(0, 0, 0, 0);
            write_report(PID_MOUSE, &rep)?;
        }
        "click" => {
            let rep = mouse_report(bit, 0, 0, 0);
            write_report(PID_MOUSE, &rep)?;
            std::thread::sleep(std::time::Duration::from_millis(10));
            let up = mouse_report(0, 0, 0, 0);
            write_report(PID_MOUSE, &up)?;
        }
        other => bail!("未知动作：{other}（down/up/click）"),
    }
    Ok(())
}

/// 滚轮（正=向上，负=向下；120 的倍数，clamp 到 ±127）
pub fn mouse_wheel(delta: i32) -> Result<()> {
    let wheel = (delta / 120).clamp(-127, 127) as i8;
    let rep = mouse_report(0, 0, 0, wheel);
    write_report(PID_MOUSE, &rep)?;
    Ok(())
}
