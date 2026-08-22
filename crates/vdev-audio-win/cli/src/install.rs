//! 虚拟声卡驱动安装 / 卸载 / 状态（SetupAPI，Media 类，Root\vdev-audio）。

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIF_REMOVE, DIIRFLAG_FORCE_INF, DiInstallDriverW,
    GUID_DEVCLASS_MEDIA, SETUP_DI_GET_CLASS_DEVS_FLAGS, SETUP_DI_REGISTRY_PROPERTY,
    SP_DEVINFO_DATA, SPDRP_DRIVER, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SetupDiCallClassInstaller,
    SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW,
    SetupDiGetINFClassW, SetupDiOpenDeviceInfoW, SetupDiSetDeviceRegistryPropertyW,
};

pub const HARDWARE_ID: &str = r"Root\vdev-audio";

/// 把设备信息里的 REG_MULTI_SZ 属性读成宽字符序列
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
    let mut wide = Vec::with_capacity(buf.len() / 2);
    for chunk in buf.chunks_exact(2) {
        wide.push(u16::from_le_bytes([chunk[0], chunk[1]]));
    }
    wide
}

fn wide_contains(haystack: &[u16], needle: &str) -> bool {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

/// 在 Media 类设备里按硬件 ID 找 vdev-audio（含非 present）
fn find_device() -> Result<Option<SP_DEVINFO_DATA>> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_MEDIA),
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

/// 移除所有 vdev-audio 设备节点
fn remove_all_nodes() -> Result<usize> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_MEDIA),
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
            ClassInstallHeader: windows::Win32::Devices::DeviceAndDriverInstallation::SP_CLASSINSTALL_HEADER {
                cbSize: std::mem::size_of::<windows::Win32::Devices::DeviceAndDriverInstallation::SP_REMOVEDEVICE_PARAMS>() as u32,
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
    let inf_path = inf_dir.join("vdev-audio.inf");
    if !inf_path.exists() {
        bail!("找不到 INF: {}", inf_path.display());
    }

    let removed = remove_all_nodes()?;
    if removed > 0 {
        println!("已清理 {removed} 个残留设备节点");
    }

    // 从 INF 提取类 GUID 与类名
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
            windows::core::PCWSTR(inf_wide.as_ptr()),
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
                windows::core::PCWSTR(class_name_cstr.as_ptr()),
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
                        windows::core::PCWSTR(class_name_cstr.as_ptr()),
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

        // DiInstallDriverW 装入驱动存储并安装
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
                windows::core::PCWSTR(inf_path_wide.as_ptr()),
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
        println!("未找到 vdev 虚拟声卡设备");
        return Ok(false);
    };
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_MEDIA),
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
    println!("已移除 vdev 虚拟声卡设备");
    Ok(true)
}

/// 设备状态
#[derive(Debug, Clone, Serialize)]
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
            Some(&GUID_DEVCLASS_MEDIA),
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
