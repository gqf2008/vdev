//! 虚拟显示器驱动安装 / 卸载 / 状态（SetupAPI）。
//!
//! 创建 Root 枚举设备节点（Display 类，硬件 ID `Root\vdev-display`），
//! 并从 INF 安装 UMDF 驱动；卸载时移除设备节点。

use std::path::Path;

use anyhow::{bail, Context as _, Result};
use serde::Serialize;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList, SetupDiCreateDeviceInfoW,
    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiEnumDriverInfoW,
    SetupDiGetClassDevsW, SetupDiGetDeviceRegistryPropertyW, SetupDiOpenDeviceInfoW,
    SetupDiSetDeviceInstallParamsW, SetupDiSetDeviceRegistryPropertyW, SetupDiSetSelectedDriverW,
    DIF_INSTALLDEVICE, DIF_REGISTERDEVICE, DIF_REMOVE, DIF_SELECTBESTCOMPATDRV, DIGCF_PRESENT,
    DIOD_INHERIT_CLASSDRVS, DI_ENUMSINGLEINF, DI_QUIETINSTALL, DI_REMOVEDEVICE_GLOBAL,
    GUID_DEVCLASS_DISPLAY, SETUP_DI_DEVICE_CREATION_FLAGS, SETUP_DI_REGISTRY_PROPERTY,
    SPDIT_COMPATDRIVER, SPDRP_DRIVER, SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SP_CLASSINSTALL_HEADER,
    SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W, SP_DRVINFO_DATA_V2_W, SP_REMOVEDEVICE_PARAMS,
};

pub const HARDWARE_ID: &str = r"Root\vdev-display";
const DEVICE_NODE_NAME: &str = r"Root\vdev-display";

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
        if err.code().0 != windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER.0 as i32 {
            return Vec::new();
        }
        if required == 0 {
            return Vec::new();
        }
        buf.resize(required as usize, 0);
    }
    // 以 u16 对齐读取
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

/// 在 Display 类已枚举设备里按硬件 ID 找 vdev-display
fn find_device() -> Result<Option<SP_DEVINFO_DATA>> {
    let devs =
        unsafe { SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), None, None, DIGCF_PRESENT) }
            .context("SetupDiGetClassDevsW failed")?;
    let mut found = None;
    let mut index = 0u32;
    loop {
        let mut info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        let ok = unsafe { SetupDiEnumDeviceInfo(devs, index, &mut info) };
        if ok.is_err() {
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

/// 安装驱动：创建设备节点 + 从 INF 装驱动。
/// `inf_dir` 必须包含 `vdev-display.inf` 与 `vdev_display.dll`。
pub fn install(inf_dir: &Path) -> Result<()> {
    let inf_path = inf_dir.join("vdev-display.inf");
    if !inf_path.exists() {
        bail!("找不到 INF: {}", inf_path.display());
    }

    // 已经装过就直接进入驱动安装
    let already = find_device()?.is_some();
    if already {
        println!("设备节点已存在（{HARDWARE_ID}），跳过创建");
    }

    // 1. 建设备信息集
    let devs = unsafe { SetupDiCreateDeviceInfoList(Some(&GUID_DEVCLASS_DISPLAY), None) }
        .context("SetupDiCreateDeviceInfoList failed")?;

    let result = (|| -> Result<()> {
        let mut dev_info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };

        if !already {
            // 2. 创建设备信息
            let name = windows::core::PCWSTR(
                DEVICE_NODE_NAME
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect::<Vec<u16>>()
                    .as_ptr(),
            );
            let create_res = unsafe {
                SetupDiCreateDeviceInfoW(
                    devs,
                    name,
                    &GUID_DEVCLASS_DISPLAY,
                    None,
                    None,
                    SETUP_DI_DEVICE_CREATION_FLAGS(DIOD_INHERIT_CLASSDRVS),
                    Some(&mut dev_info),
                )
            };
            match create_res {
                Ok(()) => {}
                // 设备节点已存在（可能上次安装中断留下）→ 直接打开
                Err(e) if e.code().0 as u32 == 0xE0000207 => {
                    unsafe { SetupDiOpenDeviceInfoW(devs, name, None, 0, Some(&mut dev_info)) }
                        .with_context(|| "SetupDiOpenDeviceInfoW failed")?;
                }
                Err(e) => return Err(e).context("SetupDiCreateDeviceInfoW failed"),
            }

            // 3. 设置硬件 ID（REG_MULTI_SZ，双 null 结尾）
            let mut hwid: Vec<u16> = HARDWARE_ID.encode_utf16().collect();
            hwid.push(0);
            hwid.push(0);
            let bytes = hwid
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<u8>>();
            unsafe {
                SetupDiSetDeviceRegistryPropertyW(
                    devs,
                    &mut dev_info,
                    SPDRP_HARDWAREID,
                    Some(&bytes),
                )
            }
            .context("SetupDiSetDeviceRegistryPropertyW(SPDRP_HARDWAREID) failed")?;

            // 4. 注册设备节点
            unsafe { SetupDiCallClassInstaller(DIF_REGISTERDEVICE, devs, Some(&dev_info)) }
                .with_context(|| "DIF_REGISTERDEVICE failed")?;
        }

        // 5. 用 INF 枚举驱动并选中（DIF_SELECTBESTCOMPATDRV 需要先设 DriverPath + DI_ENUMSINGLEINF）
        let mut params = SP_DEVINSTALL_PARAMS_W {
            cbSize: std::mem::size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
            Flags: DI_ENUMSINGLEINF | DI_QUIETINSTALL,
            ..Default::default()
        };
        // DriverPath: INF 所在目录（WCHAR[260]，双 null 结尾）
        let dir_wide: Vec<u16> = inf_dir
            .as_os_str()
            .to_str()
            .context("INF 目录不是合法 UTF-8")?
            .encode_utf16()
            .chain(std::iter::once(0))
            .chain(std::iter::once(0))
            .collect();
        if dir_wide.len() > params.DriverPath.len() {
            bail!("INF 目录过长（>260 字符）");
        }
        params.DriverPath[..dir_wide.len()].copy_from_slice(&dir_wide);
        unsafe { SetupDiSetDeviceInstallParamsW(devs, Some(&dev_info), &params) }
            .context("SetupDiSetDeviceInstallParamsW failed")?;

        unsafe { SetupDiCallClassInstaller(DIF_SELECTBESTCOMPATDRV, devs, Some(&dev_info)) }
            .with_context(|| "DIF_SELECTBESTCOMPATDRV failed（驱动包是否已签名/证书是否已装？）")?;

        let mut drv_info = SP_DRVINFO_DATA_V2_W {
            cbSize: std::mem::size_of::<SP_DRVINFO_DATA_V2_W>() as u32,
            ..Default::default()
        };
        unsafe {
            SetupDiEnumDriverInfoW(devs, Some(&dev_info), SPDIT_COMPATDRIVER, 0, &mut drv_info)
        }
        .context("SetupDiEnumDriverInfoW failed（INF 里没有兼容驱动？）")?;

        unsafe { SetupDiSetSelectedDriverW(devs, Some(&mut dev_info), Some(&mut drv_info)) }
            .context("SetupDiSetSelectedDriverW failed")?;

        // 6. 安装
        unsafe { SetupDiCallClassInstaller(DIF_INSTALLDEVICE, devs, Some(&dev_info)) }
            .with_context(|| "DIF_INSTALLDEVICE failed（签名/权限问题？请用管理员运行）")?;

        Ok(())
    })();

    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result
}

/// 卸载：移除 Root\vdev-display 设备节点。返回是否找到并移除。
pub fn uninstall() -> Result<bool> {
    let Some(dev_info) = find_device()? else {
        println!("未找到 vdev 虚拟显示器设备");
        return Ok(false);
    };

    let devs =
        unsafe { SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), None, None, DIGCF_PRESENT) }
            .context("SetupDiGetClassDevsW failed")?;

    let result = (|| -> Result<()> {
        let params = SP_REMOVEDEVICE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: std::mem::size_of::<SP_REMOVEDEVICE_PARAMS>() as u32,
                InstallFunction: DIF_REMOVE,
            },
            Scope: DI_REMOVEDEVICE_GLOBAL,
            HwProfile: 0,
        };
        unsafe {
            SetupDiSetDeviceInstallParamsW(
                devs,
                Some(&dev_info),
                (&params as *const SP_REMOVEDEVICE_PARAMS).cast::<SP_DEVINSTALL_PARAMS_W>(),
            )
        }
        .context("SetupDiSetDeviceInstallParamsW failed")?;

        unsafe { SetupDiCallClassInstaller(DIF_REMOVE, devs, Some(&dev_info)) }
            .with_context(|| "DIF_REMOVE failed")?;

        Ok(())
    })();

    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result?;
    println!("已移除 vdev 虚拟显示器设备");
    Ok(true)
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
    let Some(dev_info) = find_device()? else {
        return Ok(DeviceStatus {
            present: false,
            driver: None,
            friendly_name: None,
        });
    };

    let devs =
        unsafe { SetupDiGetClassDevsW(Some(&GUID_DEVCLASS_DISPLAY), None, None, DIGCF_PRESENT) }
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
