//! 虚拟声卡驱动安装 / 卸载 / 状态（SetupAPI，Media 类，Root\vdev-audio）。

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use serde::Serialize;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CM_Query_And_Remove_SubTreeW,
    CR_SUCCESS, DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIIRFLAG_FORCE_INF, DiInstallDriverW,
    GUID_DEVCLASS_MEDIA, HDEVINFO, SETUP_DI_GET_CLASS_DEVS_FLAGS, SETUP_DI_REGISTRY_PROPERTY,
    SP_DEVINFO_DATA, SPDRP_DRIVER, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SetupDiCallClassInstaller,
    SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList,
    SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW, SetupDiOpenDeviceInfoW,
    SetupDiSetDeviceRegistryPropertyW,
};
use windows::core::PCWSTR;

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
    buf.as_chunks::<2>()
        .0
        .iter()
        .map(|&chunk| u16::from_le_bytes(chunk))
        .collect::<Vec<u16>>()
}

fn wide_contains(haystack: &[u16], needle: &str) -> bool {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

/// 读取设备信息元素的设备实例 ID（如 `ROOT\MEDIA\0000`）。
fn get_instance_id(devs: HDEVINFO, info: &SP_DEVINFO_DATA) -> Result<String> {
    let mut required: u32 = 0;
    let mut buf: Vec<u16> = Vec::new();
    loop {
        // SAFETY: devs 为调用方传入的有效 HDEVINFO、info 指向其中有效元素；
        // 缓冲区切片与所需大小出参均按 SetupAPI 语义传入（过小时按
        // HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER) 扩容重试）
        let ok = unsafe {
            SetupDiGetDeviceInstanceIdW(devs, info, Some(buf.as_mut_slice()), Some(&mut required))
        };
        if ok.is_ok() {
            break;
        }
        // 缓冲区太小：required 给出所需大小
        // HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)
        let err = windows::core::Error::from_win32();
        if err.code().0 as u32 != 0x8007_007A || required == 0 {
            return Err(err).context("SetupDiGetDeviceInstanceIdW failed");
        }
        buf.resize(required as usize, 0);
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Ok(String::from_utf16_lossy(&buf[..len]))
}

/// 在调用方自己的设备信息集合上按实例 ID 重新打开设备，
/// 返回配对该集合的 [`SP_DEVINFO_DATA`]（元素必须属于传入的集合，SetupAPI 契约）。
fn open_device(devs: HDEVINFO, instance_id: &str) -> Result<SP_DEVINFO_DATA> {
    let id_wide: Vec<u16> = instance_id
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let mut info = SP_DEVINFO_DATA {
        cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
        ..Default::default()
    };
    // SAFETY: devs 为调用方传入的有效 HDEVINFO；id_wide 以 NUL 结尾且在
    // 本调用期间存活；info.cbSize 已按结构体大小初始化（SetupAPI 要求）
    unsafe {
        SetupDiOpenDeviceInfoW(
            devs,
            windows::core::PCWSTR(id_wide.as_ptr()),
            None,
            0,
            Some(&mut info),
        )
    }
    .with_context(|| format!("SetupDiOpenDeviceInfoW({instance_id}) failed"))?;
    Ok(info)
}

/// 在 Media 类设备里按硬件 ID 找 vdev-audio（含非 present），返回其设备实例 ID。
///
/// 返回实例 ID 而非 [`SP_DEVINFO_DATA`]：元素只属于枚举它的那个设备信息集合，
/// 集合销毁后配到别的集合上用违反 SetupAPI 契约（元素属于传入集合）；
/// 调用方应建立自己的集合并用 [`open_device`] 重新打开。
fn find_device() -> Result<Option<String>> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_MEDIA),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;
    let found = (|| -> Result<Option<String>> {
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
                found = Some(get_instance_id(devs, &info)?);
                break;
            }
        }
        Ok(found)
    })();
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    found
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
        remove_subtree(info)?;
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

/// 卸载：移除 Root\vdev-audio 设备节点。返回是否找到并移除。
pub fn uninstall() -> Result<bool> {
    let Some(instance_id) = find_device()? else {
        println!("未找到 vdev 虚拟声卡设备");
        return Ok(false);
    };

    // 与 find_device 同域（flags=0，含非 present 残留），保证找到的实例一定能在此
    // 集合里重新打开；Windows 运行时行为（SetupAPI + DIF_REMOVE），无法在 macOS 交叉环境单测
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_MEDIA),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;

    let result = (|| -> Result<()> {
        let dev_info = open_device(devs, &instance_id)?;
        remove_subtree(&dev_info)?;
        Ok(())
    })();

    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result?;
    println!("已移除 vdev 虚拟声卡设备");
    Ok(true)
}

/// 用配置管理器（CfgMgr）移除设备子树。
///
/// 不用 `DIF_REMOVE`：`SetupDiSetDeviceInstallParamsW` 强转 `SP_REMOVEDEVICE_PARAMS`
/// 在本机必然报 `0x800706F8`（ERROR_INVALID_USER_BUFFER）——与 HID、显示器两侧
/// 实测同款结论（显示器侧已在 `fix(win-display)` 换成 CfgMgr）。这里沿用
/// neflib/nefcon 同款做法：`CM_Get_Device_IDW` → `CM_Locate_DevNodeW` →
/// `CM_Query_And_Remove_SubTreeW`。
fn remove_subtree(info: &SP_DEVINFO_DATA) -> Result<()> {
    let mut id_buf = [0u16; 512];
    // SAFETY: info.DevInst 来自 SetupDi* 枚举；id_buf 可写且有长度
    let cr = unsafe { CM_Get_Device_IDW(info.DevInst, &mut id_buf, 0) };
    if cr != CR_SUCCESS {
        bail!("CM_Get_Device_IDW failed: 0x{:08X}", cr.0);
    }
    let mut devinst = 0u32;
    // SAFETY: id_buf 以 NUL 结尾（CM_Get_Device_IDW 保证）
    let cr = unsafe {
        CM_Locate_DevNodeW(
            &mut devinst,
            PCWSTR(id_buf.as_ptr()),
            CM_LOCATE_DEVNODE_NORMAL,
        )
    };
    if cr != CR_SUCCESS {
        bail!("CM_Locate_DevNodeW failed: 0x{:08X}", cr.0);
    }
    // SAFETY: devinst 有效；不关心 veto 详情
    let cr = unsafe { CM_Query_And_Remove_SubTreeW(devinst, None, None, 0) };
    if cr != CR_SUCCESS {
        bail!("CM_Query_And_Remove_SubTreeW failed: 0x{:08X}", cr.0);
    }
    Ok(())
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
    let Some(instance_id) = find_device()? else {
        return Ok(DeviceStatus {
            present: false,
            driver: None,
            friendly_name: None,
        });
    };

    // 与 find_device 同域（flags=0），在自己集合上按实例 ID 重新打开后再读属性；
    // Windows 运行时行为（SetupAPI 注册表属性读取），无法在 macOS 交叉环境单测
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_MEDIA),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;

    let result = (|| -> Result<DeviceStatus> {
        let dev_info = open_device(devs, &instance_id)?;
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
        Ok(DeviceStatus {
            present: true,
            driver,
            friendly_name,
        })
    })();

    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result
}
