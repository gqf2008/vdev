//! vdev-hid-win 内核路线（路线 B）：虚拟 HID 内核驱动的安装/状态与报告注入。
//! 安装/卸载/状态走 SetupAPI（HIDClass，Root\vdev-hid[-mouse]）；注入经 HID 接口
//! `HidD_SetFeature` 写 8 字节键盘 / 4 字节鼠标 Feature 报告（厂商 Feature 管道），
//! 由驱动投递给 hidclass（WriteFile 输出报告仅作兜底，系统通常会拒绝）。
//!
//! 纯逻辑（键码映射/报告组装/HWID 匹配）在 `crate::report`（windows-free，可宿主单测），
//! 本模块经重导出保持既有调用面不变。本模块整体仅 Windows 可编译（SetupAPI/WMI）。

use std::path::Path;

use anyhow::{Context as _, Result, bail};
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_Get_Device_IDW, CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CM_Query_And_Remove_SubTreeW,
    CR_SUCCESS, DICD_GENERATE_ID, DIF_REGISTERDEVICE, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
    DIIRFLAG_FORCE_INF, DiInstallDriverW, GUID_DEVCLASS_HIDCLASS, SETUP_DI_GET_CLASS_DEVS_FLAGS,
    SETUP_DI_REGISTRY_PROPERTY, SP_DEVICE_INTERFACE_DATA, SP_DEVINFO_DATA, SPDRP_DRIVER,
    SPDRP_FRIENDLYNAME, SPDRP_HARDWAREID, SetupDiCallClassInstaller, SetupDiCreateDeviceInfoList,
    SetupDiCreateDeviceInfoW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW, SetupDiGetDeviceInterfaceDetailW,
    SetupDiGetDeviceRegistryPropertyW, SetupDiGetINFClassW, SetupDiOpenDeviceInfoW,
    SetupDiSetDeviceRegistryPropertyW,
};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDD_ATTRIBUTES, HidD_GetAttributes, HidD_GetHidGuid, HidD_SetFeature,
};
use windows::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, WriteFile,
};
use windows::core::PCWSTR;

// ---------------- 纯逻辑重导出（保持既有调用面） ----------------

use crate::report::hwid_matches;
pub use crate::report::{PID_KBD, PID_MOUSE};
pub use crate::report::{key_to_hid, make_report, mouse_button_bit, mouse_report};

/// 键盘设备硬件 ID（与 INF `Root\vdev-hid` 一致）
pub const HARDWARE_ID: &str = r"Root\vdev-hid";
/// 鼠标设备硬件 ID（与 INF `Root\vdev-hid-mouse` 一致）
pub const MOUSE_HARDWARE_ID: &str = r"Root\vdev-hid-mouse";
/// vdev 虚拟键盘 VID/PID（与驱动一致）
const VID: u16 = 0x5644;

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
    buf.as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect()
}

/// 按 HWID（精确匹配，大小写不敏感）在设备集合中查找节点
fn find_device_by_hwid(
    devs: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    hwid: &str,
) -> Option<SP_DEVINFO_DATA> {
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
        if hwid_matches(&hwids, hwid) {
            return Some(info);
        }
    }
    None
}

fn open_class_devices() -> Result<windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO> {
    let devs = unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_HIDCLASS),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")?;
    Ok(devs)
}

fn find_device() -> Result<Option<SP_DEVINFO_DATA>> {
    let devs = open_class_devices()?;
    let found = find_device_by_hwid(devs, HARDWARE_ID);
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    Ok(found)
}

fn remove_all_nodes() -> Result<usize> {
    let devs = open_class_devices()?;
    let mut to_remove = collect_vdev_nodes(devs)?;
    // 用配置管理器（CfgMgr）移除整棵子树：DIF_REMOVE 那条路在本机不可用——
    // SetupDiSetDeviceInstallParamsW 强转 SP_REMOVEDEVICE_PARAMS 报 0x800706F8，
    // 改 SetupDiSetClassInstallParamsW 后同样失败（实测），而 pnputil /remove-device
    // 能删掉同样的节点。CM_Query_And_Remove_SubTreeW 是 neflib/nefcon 同款做法。
    //
    // 回归（真机，2026-09-14）：单跑一轮 `CM_Query_And_Remove_SubTreeW` 时，部分节点会返回
    // `ERROR_NOT_READY(0x17)`（设备正被占用/需重启才可移除），而原实现对**一个**节点失败就
    // `bail!` 整轮退出 —— 结果是"已移除 N 个"但剩下的节点留在系统里，紧接着 `install` 又建一对，
    // 反复装卸后设备管理器涨到 6 个（键盘×3/鼠标×3）。修法：逐节点**重试 + pnputil 兜底**，
    // 单个节点失败不再中断整轮；最后断言"没有残留"，有残留就报错并把实例 ID 打出来。
    let mut removed = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();
    for info in &mut to_remove {
        let mut id_buf = [0u16; 512];
        // SAFETY: info.DevInst 来自 SetupDiEnumDeviceInfo；id_buf 可写且有长度
        let cr = unsafe { CM_Get_Device_IDW(info.DevInst, &mut id_buf, 0) };
        if cr != CR_SUCCESS {
            failures.push((
                String::from("<unknown>"),
                format!("CM_Get_Device_IDW 0x{:08X}", cr.0),
            ));
            continue;
        }
        let instance_id = String::from_utf16_lossy(
            &id_buf[..id_buf.iter().position(|&c| c == 0).unwrap_or(id_buf.len())],
        );
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
            failures.push((instance_id, format!("CM_Locate_DevNodeW 0x{:08X}", cr.0)));
            continue;
        }

        // 1) CfgMgr 直删，最多 3 轮：ERROR_NOT_READY 多为瞬时状态，等一拍再来。
        // SAFETY: devinst 有效；不关心 veto 详情
        let mut last = unsafe { CM_Query_And_Remove_SubTreeW(devinst, None, None, 0) };
        for _ in 0..2 {
            if last == CR_SUCCESS {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
            // SAFETY: 同上
            last = unsafe { CM_Query_And_Remove_SubTreeW(devinst, None, None, 0) };
        }
        // **以"节点真的消失"为准**，不看 CM 的返回值：真机上 CM 返回 CR_SUCCESS 而节点仍在
        // （这正是"卸载报成功、设备管理器里却越堆越多"的根因）。
        if wait_gone(&instance_id, 4)? {
            removed += 1;
            continue;
        }

        // 2) 兜底：pnputil /remove-device（实测能真正删掉这类节点）。用户态 CLI 里调用系统
        //    工具不优雅，但"留一堆幽灵 HID 节点"更糟；失败也照旧如实报告。
        let alt = std::process::Command::new("pnputil")
            .args(["/remove-device", &instance_id])
            .output();
        match alt {
            Ok(out) if out.status.success() => {
                if wait_gone(&instance_id, 4)? {
                    println!(
                        "  （CfgMgr 回 0x{:08X} 但节点未消失，已用 pnputil 兜底移除）",
                        last.0
                    );
                    removed += 1;
                } else {
                    failures.push((instance_id, "pnputil 报成功但节点仍在".to_string()));
                }
            }
            Ok(out) => failures.push((
                instance_id,
                format!(
                    "CfgMgr 0x{:08X}；pnputil exit={:?} {}",
                    last.0,
                    out.status.code(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
            )),
            Err(e) => failures.push((
                instance_id,
                format!("CfgMgr 0x{:08X}；pnputil 无法执行: {e}", last.0),
            )),
        }
    }
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    if !failures.is_empty() {
        for (id, why) in &failures {
            eprintln!("  ! 未能移除 {id}：{why}");
        }
        bail!(
            "有 {} 个 vdev 设备节点未能移除（管理员权限？设备被占用？）",
            failures.len()
        );
    }
    Ok(removed)
}

/// 枚举 class 下所有 vdev 的 HID 节点（键盘 + 鼠标）。`remove_all_nodes` 与卸载后的
/// "无残留"断言共用，避免两处枚举逻辑漂移。
fn collect_vdev_nodes(
    devs: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
) -> Result<Vec<SP_DEVINFO_DATA>> {
    let mut out: Vec<SP_DEVINFO_DATA> = Vec::new();
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
        if hwid_matches(&hwids, HARDWARE_ID) || hwid_matches(&hwids, MOUSE_HARDWARE_ID) {
            out.push(info);
        }
    }
    Ok(out)
}

/// 当前 vdev HID 节点的实例 ID 列表（如 `ROOT\HIDCLASS\0004`）。
///
/// 这是"节点是否真的消失"的唯一可信判据：真机实测 `CM_Query_And_Remove_SubTreeW` 会返回
/// `CR_SUCCESS` 而节点**仍然留在设备树里**（随后 `install` 又建一对，反复装卸就把节点堆起来），
/// 所以删除后必须按实例 ID 轮询确认。
pub fn vdev_instance_ids() -> Result<Vec<String>> {
    let devs = open_class_devices()?;
    let nodes = collect_vdev_nodes(devs)?;
    let mut out = Vec::with_capacity(nodes.len());
    for info in &nodes {
        let mut id_buf = [0u16; 512];
        // SAFETY: info.DevInst 来自枚举；id_buf 可写且有长度
        let cr = unsafe { CM_Get_Device_IDW(info.DevInst, &mut id_buf, 0) };
        if cr != CR_SUCCESS {
            continue;
        }
        let end = id_buf.iter().position(|&c| c == 0).unwrap_or(id_buf.len());
        out.push(String::from_utf16_lossy(&id_buf[..end]));
    }
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    Ok(out)
}

/// 轮询等待某个实例 ID 从设备树消失（最多 `tries` × 500ms）
fn wait_gone(instance_id: &str, tries: usize) -> Result<bool> {
    for _ in 0..tries {
        if !vdev_instance_ids()?
            .iter()
            .any(|id| id.eq_ignore_ascii_case(instance_id))
        {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    Ok(false)
}

/// 创建并注册一个设备节点（硬件 ID 由 INF 模型行匹配到对应安装节，
/// Role 键由 INF 的 AddReg 写入，用户态不重复设置）
fn create_and_register_device(
    devs: windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO,
    class_name: &[u16],
    class_guid: &windows::core::GUID,
    hardware_id: &str,
) -> Result<()> {
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
            class_guid,
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
    let mut hwid: Vec<u16> = hardware_id.encode_utf16().collect();
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
    Ok(())
}

/// 安装驱动：清理旧节点 + 创建键盘/鼠标两个设备节点 + DiInstallDriverW 装入驱动存储
///
/// 修复记录：原实现只创建 `Root\vdev-hid`（键盘）节点，INF 的鼠标安装节
/// （`Root\vdev-hid-mouse`）永远匹配不上，鼠标设备无法枚举。
pub fn install(inf_dir: &Path) -> Result<()> {
    let inf_path = inf_dir.join("vdev-hid.inf");
    if !inf_path.exists() {
        bail!("找不到 INF: {}", inf_path.display());
    }

    // 清残留节点是尽力而为：正在工作的虚拟键鼠（HID 子设备被系统占用）可能拒绝移除，
    // 此时不应阻断安装——DiInstallDriverW 会把驱动包应用到所有匹配设备（含新节点）。
    match remove_all_nodes() {
        Ok(0) => {}
        Ok(n) => println!("已清理 {n} 个残留设备节点"),
        Err(e) => println!("提示：清理残留设备节点未成功（{e}），继续安装"),
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
        // 键盘 + 鼠标两个根枚举节点，HWID 与 INF 模型行逐一对应
        for hardware_id in [HARDWARE_ID, MOUSE_HARDWARE_ID] {
            create_and_register_device(devs, &class_name, &class_guid, hardware_id)?;
        }

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

/// 卸载：移除键盘与鼠标全部残留节点
pub fn uninstall() -> Result<bool> {
    let removed = remove_all_nodes()?;
    if removed == 0 {
        println!("未找到 vdev 虚拟键盘/鼠标设备");
        return Ok(false);
    }
    println!("已移除 {removed} 个 vdev 虚拟设备节点");
    // 事后断言：真的没有残留（节点移除是异步的，给它最多 ~3s）。
    // 真机上出现过"报成功但节点还在"（反复装卸把节点涨到 6 个）——宁可让 CLI 非零退出，
    // 也不要静默留一堆幽灵节点。
    let mut left = vdev_instance_ids()?;
    for _ in 0..6 {
        if left.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        left = vdev_instance_ids()?;
    }
    if !left.is_empty() {
        bail!(
            "卸载后仍有 {} 个 vdev HID 节点残留：{}（可能被占用，请重试或重启后重试）",
            left.len(),
            left.join(", ")
        );
    }
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
    let devs = open_class_devices()?;
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

// ---------------- 报告注入（经 HID 接口 HidD_SetFeature，WriteFile 兜底） ----------------

/// 按 VID/PID 收集 vdev 虚拟键盘/鼠标的**全部** HID 接口路径。
///
/// 一个 HID 设备可能有多个顶层集合（TLC）：键/鼠本身，以及厂商注入管道。
/// 注入只对厂商管道有效，因此这里返回全部候选、由 `write_report` 逐个试写。
fn find_hid_paths(pid: u16) -> Result<Vec<String>> {
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
    let mut result: Vec<String> = Vec::new();
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
        // 有界 NUL 扫描：路径长度受缓冲区字节数约束（修复前无上限，畸形数据会越界读）
        let max_chars = buf.len().saturating_sub(std::mem::size_of::<u32>()) / 2;
        let mut path_w: Vec<u16> = Vec::new();
        let mut i = 0usize;
        unsafe {
            while i < max_chars {
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
        // 只请求写权限：键盘/鼠标顶层集合（TLC）被系统限制，非管理员以
        // GENERIC_READ|GENERIC_WRITE 打开会得到 ERROR_ACCESS_DENIED(5)——实测
        // `\\?\hid#...#\kbd` 用 rw 打开 err=5、只写打开成功。注入只需要写。
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
        .ok();
        let Some(handle) = handle else { continue };
        let mut attrs = HIDD_ATTRIBUTES {
            Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
            ..Default::default()
        };
        let ok = unsafe { HidD_GetAttributes(handle, &mut attrs) };
        unsafe { CloseHandle(handle) }.ok();
        if ok.as_bool() && attrs.VendorID == VID && attrs.ProductID == pid {
            result.push(path);
        }
    }
    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    if result.is_empty() {
        bail!("未找到 vdev 虚拟 HID 设备（先安装驱动）");
    }
    Ok(result)
}

/// 写入一份报告（键盘 8 字节 / 鼠标 4 字节，按下/抬起）
///
/// 候选接口可能不止一个（键/鼠 TLC + 厂商注入管道 TLC）；系统只允许对**厂商管道**
/// 写输出报告，对键/鼠 TLC 写会返回 ERROR_INVALID_FUNCTION。这里逐个试写，
/// 第一个成功即返回，全部失败才报最后一个错误。
pub fn write_report(pid: u16, report: &[u8]) -> Result<()> {
    let paths = find_hid_paths(pid)?;
    let mut last_error = None;
    for path in &paths {
        match write_report_to(path, report) {
            Ok(()) => return Ok(()),
            Err(e) => last_error = Some(e),
        }
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("写入 HID 报告失败：没有可用接口")))
}

/// 向指定 HID 接口路径写入一份输出报告
///
/// 通道优先级：Feature 报告（`HidD_SetFeature`，带/不带 Report ID 前缀各试一次）→
/// 输出报告（`WriteFile`）。键盘/鼠标顶层集合的输出报告会被系统拒绝
/// （ERROR_INVALID_FUNCTION），Feature 报告是这条链路上真正可行的注入通道。
fn write_report_to(path: &str, report: &[u8]) -> Result<()> {
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

    // 1) Feature 报告：hidclass 对 VHF 设备把 Report ID 计入报告长度，先试带前缀
    let mut framed = Vec::with_capacity(report.len() + 1);
    framed.push(0u8);
    framed.extend_from_slice(report);
    let mut ok =
        unsafe { HidD_SetFeature(handle, framed.as_ptr().cast(), framed.len() as u32) }.as_bool();
    // 2) Feature 报告：不带前缀
    if !ok {
        ok = unsafe { HidD_SetFeature(handle, report.as_ptr().cast(), report.len() as u32) }
            .as_bool();
    }
    // 3) 兜底：输出报告
    if !ok {
        let mut written = 0u32;
        ok = unsafe { WriteFile(handle, Some(report), Some(&mut written), None) }.is_ok();
    }
    // L6：last-error 必须在 CloseHandle 之前取——CloseHandle 可能改写线程的
    // last-error，失败原因会被覆盖成无关错误码
    let last_err = windows::core::Error::from_win32();
    unsafe { CloseHandle(handle) }.ok();
    if !ok {
        bail!("写入 HID 报告失败：{last_err}");
    }
    Ok(())
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
