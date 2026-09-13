//! 虚拟显示器驱动安装 / 卸载 / 状态（SetupAPI）。
//!
//! 创建 Root 枚举设备节点（Display 类，硬件 ID `Root\vdev-display`），
//! 并从 INF 安装 UMDF 驱动；卸载时移除设备节点。

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::Serialize;
use windows::core::PCWSTR;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_Locate_DevNodeW, CM_Query_And_Remove_SubTreeW, DiInstallDriverW,
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
    SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW,
    SetupDiOpenDeviceInfoW, SetupDiSetDeviceRegistryPropertyW, CM_LOCATE_DEVNODE_NORMAL,
    CR_SUCCESS, DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIIRFLAG_FORCE_INF, GUID_DEVCLASS_DISPLAY,
    HDEVINFO, SETUP_DI_GET_CLASS_DEVS_FLAGS, SETUP_DI_REGISTRY_PROPERTY, SPDRP_DRIVER,
    SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SP_DEVINFO_DATA,
};

pub const HARDWARE_ID: &str = r"Root\vdev-display";

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
        // 缓冲区太小：required 给出所需大小
        let err = windows::core::Error::from_win32();
        if err.code().0 as u32 != 0x8007_007A {
            // HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)
            return Vec::new();
        }
        if required == 0 {
            return Vec::new();
        }
        buf.resize(required as usize, 0);
    }
    // 以 u16 对齐读取（slice::as_chunks 需 Rust 1.88+）
    let wide = buf
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&chunk| u16::from_le_bytes(chunk))
        .collect::<Vec<u16>>();
    wide
}

fn wide_contains(haystack: &[u16], needle: &str) -> bool {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    haystack
        .windows(needle.len())
        .any(|w| w == needle.as_slice())
}

/// 读取设备信息元素的设备实例 ID（如 `ROOT\DISPLAY\0000`）。
fn get_instance_id(devs: HDEVINFO, info: &SP_DEVINFO_DATA) -> Result<String> {
    let mut required: u32 = 0;
    let mut buf: Vec<u16> = Vec::new();
    loop {
        // SAFETY: `devs` 是调用方传入的有效 HDEVINFO 句柄，`info` 是已初始化
        // （cbSize 已设）且属于该集合的 SP_DEVINFO_DATA 引用；
        // `buf.as_mut_slice()`/`&mut required` 都是本次调用内有效的合法引用，
        // windows crate 仅在缓冲不足时返回错误并以 `required` 报告所需大小
        let ok = unsafe {
            SetupDiGetDeviceInstanceIdW(devs, info, Some(buf.as_mut_slice()), Some(&mut required))
        };
        if ok.is_ok() {
            break;
        }
        // 缓冲区太小：required 给出所需大小
        let err = windows::core::Error::from_win32();
        if err.code().0 as u32 != 0x8007_007A || required == 0 {
            // HRESULT_FROM_WIN32(ERROR_INSUFFICIENT_BUFFER)
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
    // SAFETY: `devs` 是调用方传入的有效 HDEVINFO 句柄；`id_wide` 在本次调用期间
    // 存活且以 NUL 结尾（PCWSTR 借用其指针）；`&mut info` 已初始化 cbSize，
    // 是本次调用的合法出参
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

/// 在 Display 类已枚举设备里按硬件 ID 找 vdev-display，返回其设备实例 ID。
///
/// 返回实例 ID 而非 [`SP_DEVINFO_DATA`]：元素只属于枚举它的那个设备信息集合，
/// 集合销毁后配到别的集合上用违反 SetupAPI 契约（元素属于传入集合）；
/// 调用方应建立自己的集合并用 [`open_device`] 重新打开。
fn find_device() -> Result<Option<String>> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_DISPLAY),
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

/// 安装驱动：创建设备节点 + 从 INF 装驱动。
/// `inf_dir` 必须包含 `vdev-display.inf` 与 `vdev_display.dll`。
pub fn install(inf_dir: &Path) -> Result<()> {
    let inf_path = inf_dir.join("vdev-display.inf");
    if !inf_path.exists() {
        bail!("找不到 INF: {}", inf_path.display());
    }

    // 从 INF 提取类 GUID 与类名（devcon 同款：用类名 + DICD_GENERATE_ID 创建设备信息）
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
    .context("SetupDiGetINFClassW failed（INF 缺少 Class/ClassGUID？）")?;

    // 先移除所有残留节点（含非 present），保证单设备节点（IddCx 驱动按单实例设计）
    let removed = remove_all_nodes()?;
    if removed > 0 {
        println!("已清理 {removed} 个残留设备节点");
    }

    // 1. 建设备信息集
    let devs = unsafe { SetupDiCreateDeviceInfoList(Some(&class_guid), None) }
        .context("SetupDiCreateDeviceInfoList failed")?;

    let result = (|| -> Result<()> {
        let mut dev_info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        // 2. 创建设备信息：类名 + DICD_GENERATE_ID（SetupAPI 自动生成实例 ID）。
        //    残留节点已在上面 remove_all_nodes 清理，创建失败如实报错即可
        //    （原「已存在则 SetupDiOpenDeviceInfoW」回退把类名当实例 ID 传参，是死路径）
        let class_name_cstr: Vec<u16> = class_name
            .iter()
            .take_while(|&&c| c != 0)
            .copied()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            SetupDiCreateDeviceInfoW(
                devs,
                windows::core::PCWSTR(class_name_cstr.as_ptr()),
                &class_guid,
                None,
                None,
                DICD_GENERATE_ID,
                Some(&mut dev_info),
            )
        }
        .context("SetupDiCreateDeviceInfoW failed")?;

        // 3. 设置硬件 ID（REG_MULTI_SZ，双 null 结尾）
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

        // 4. 注册设备节点
        unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, devs, Some(&dev_info)) }
            .with_context(|| "DIF_REGISTERDEVICE failed")?;

        // 5. 用 DiInstallDriverW 把 INF 装入驱动存储并安装到匹配设备（neflib/nefcon 同款）
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
        .with_context(|| "DiInstallDriverW failed（驱动包是否已签名/证书是否已装？）")?;
        if reboot.as_bool() {
            println!("系统提示需要重启以完成安装");
        }

        Ok(())
    })();

    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result
}

/// 移除所有 vdev-display 设备节点（含非 present 残留），返回移除数量
fn remove_all_nodes() -> Result<usize> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_DISPLAY),
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

/// 卸载：移除 Root\vdev-display 设备节点。返回是否找到并移除。
pub fn uninstall() -> Result<bool> {
    let Some(instance_id) = find_device()? else {
        println!("未找到 vdev 虚拟显示器设备");
        return Ok(false);
    };

    // 与 find_device 同域（flags=0，含非 present 残留），保证找到的实例一定能在此
    // 集合里重新打开；Windows 运行时行为（SetupAPI + DIF_REMOVE），无法在 macOS 交叉环境单测
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_DISPLAY),
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
    println!("已移除 vdev 虚拟显示器设备");
    Ok(true)
}

/// 用配置管理器（CfgMgr）移除设备子树。
///
/// 不用 DIF_REMOVE：那条路在本机两条写法都失败——`SetupDiSetDeviceInstallParamsW`
/// 强转 `SP_REMOVEDEVICE_PARAMS` 报 `0x800706F8`，改 `SetupDiSetClassInstallParamsW`
/// 同样失败（HID 侧实测结论，显示器侧同款代码）。`CM_Query_And_Remove_SubTreeW`
/// 是 neflib/nefcon 同款做法，实测可用。
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

/// 查询 vdev 虚拟显示器设备是否已安装及其驱动信息
pub fn status() -> Result<DeviceStatus> {
    let Some(instance_id) = find_device()? else {
        return Ok(DeviceStatus {
            present: false,
            driver: None,
            friendly_name: None,
        });
    };

    // 与 find_device 同域（flags=0），在自己集合上按实例 ID 重新打开后再读属性
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_DISPLAY),
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
