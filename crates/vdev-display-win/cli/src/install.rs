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

fn ascii_lower_u16(c: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&c) {
        c + 0x20
    } else {
        c
    }
}

/// REG_MULTI_SZ 硬件 ID 精确匹配：逐段（NUL 分隔）整段不区分大小写相等。
/// 与 hid 侧 `report::hwid_matches` 同款（审查 L1 修复）：原实现 `wide_contains`
/// 做子串匹配，`Root\vdev-display` 会误中 `Root\vdev-display-2` 之类前缀兄弟。
fn hwid_matches(haystack: &[u16], needle: &str) -> bool {
    let needle: Vec<u16> = needle.encode_utf16().collect();
    let mut start = 0usize;
    for (i, &ch) in haystack.iter().enumerate() {
        if ch == 0 {
            let part = &haystack[start..i];
            if part.len() == needle.len()
                && part
                    .iter()
                    .zip(needle.iter())
                    .all(|(a, b)| ascii_lower_u16(*a) == ascii_lower_u16(*b))
            {
                return true;
            }
            start = i + 1;
        }
    }
    false
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

/// 打开 Display 类设备集合（flags=0：含非 present 残留节点）
fn open_class_devices() -> Result<HDEVINFO> {
    unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_DISPLAY),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")
}

/// 在 Display 类已枚举设备里按硬件 ID 找 vdev-display，返回其设备实例 ID。
///
/// 返回实例 ID 而非 [`SP_DEVINFO_DATA`]：元素只属于枚举它的那个设备信息集合，
/// 集合销毁后配到别的集合上用违反 SetupAPI 契约（元素属于传入集合）；
/// 调用方应建立自己的集合并用 [`open_device`] 重新打开。
fn find_device() -> Result<Option<String>> {
    let devs = open_class_devices()?;
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
            if hwid_matches(&hwids, HARDWARE_ID) {
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

/// 枚举 Display 类下所有 vdev-display 节点。`remove_all_nodes` 与卸载后的
/// "无残留"断言共用，避免两处枚举逻辑漂移（与 hid 侧同款结构）。
fn collect_vdev_nodes(devs: HDEVINFO) -> Vec<SP_DEVINFO_DATA> {
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
        if hwid_matches(&hwids, HARDWARE_ID) {
            out.push(info);
        }
    }
    out
}

/// 当前 vdev-display 节点的实例 ID 列表（如 `ROOT\DISPLAY\0001`）。
///
/// 这是"节点是否真的消失"的唯一可信判据：真机实测（HID 侧）
/// `CM_Query_And_Remove_SubTreeW` 会返回 `CR_SUCCESS` 而节点**仍然留在设备树里**，
/// 所以删除后必须按实例 ID 轮询确认（LESSON_CM移除设备返回成功不等于节点消失）。
pub fn vdev_instance_ids() -> Result<Vec<String>> {
    let devs = open_class_devices()?;
    let nodes = collect_vdev_nodes(devs);
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

/// 移除所有 vdev-display 设备节点（含非 present 残留；逐节点重试 + pnputil
/// 兜底，单点失败不中断整轮），返回移除数量。
///
/// 不用 DIF_REMOVE：那条路在本机两条写法都失败——`SetupDiSetDeviceInstallParamsW`
/// 强转 `SP_REMOVEDEVICE_PARAMS` 报 `0x800706F8`，改 `SetupDiSetClassInstallParamsW`
/// 同样失败（HID 侧实测结论，RULE_Windows驱动CLI卸载禁用DIF_REMOVE改用CfgMgr）。
///
/// 审查 M-c/L3 修复：原实现只看 `CM_Query_And_Remove_SubTreeW` 返回值（真机会
/// 假成功）、单节点失败 bail 整轮、且提前返回时漏 `SetupDiDestroyDeviceInfoList`
/// 泄漏 HDEVINFO；现与 hid 侧同款：逐节点 CfgMgr 最多 3 轮 → 以 wait_gone
/// （实例 ID 轮询）为准 → pnputil 兜底 → 失败汇总，句柄在所有出口都销毁。
fn remove_all_nodes() -> Result<usize> {
    let devs = open_class_devices()?;
    let to_remove = collect_vdev_nodes(devs);
    let mut removed = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();
    for info in &to_remove {
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
        // **以"节点真的消失"为准**，不看 CM 的返回值（真机假成功实测）。
        if wait_gone(&instance_id, 4)? {
            removed += 1;
            continue;
        }

        // 2) 兜底：pnputil /remove-device（hid 侧实测能真正删掉这类节点）。
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
            "有 {} 个 vdev-display 设备节点未能移除（管理员权限？设备被占用？）",
            failures.len()
        );
    }
    Ok(removed)
}

/// 卸载：移除全部 Root\vdev-display 残留节点，并断言零残留（有残留非零退出）。
/// 返回是否找到并移除。
pub fn uninstall() -> Result<bool> {
    let removed = remove_all_nodes()?;
    if removed == 0 {
        println!("未找到 vdev 虚拟显示器设备");
        return Ok(false);
    }
    println!("已移除 {removed} 个 vdev 虚拟显示器设备节点");
    // 事后断言：真的没有残留（节点移除是异步的，给它最多 ~3s）。真机上出现过
    // "报成功但节点还在"——宁可让 CLI 非零退出，也不要静默留幽灵节点。
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
            "卸载后仍有 {} 个 vdev-display 节点残留：{}（可能被占用，请重试或重启后重试）",
            left.len(),
            left.join(", ")
        );
    }
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
    let Some(instance_id) = find_device()? else {
        return Ok(DeviceStatus {
            present: false,
            driver: None,
            friendly_name: None,
        });
    };

    // 与 find_device 同域（flags=0），在自己集合上按实例 ID 重新打开后再读属性
    let devs = open_class_devices()?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn multi(parts: &[&str]) -> Vec<u16> {
        let mut v: Vec<u16> = Vec::new();
        for p in parts {
            v.extend(p.encode_utf16());
            v.push(0);
        }
        v.push(0);
        v
    }

    /// L1 回归：硬件 ID 精确分段匹配（子串实现会误中前缀兄弟）
    #[test]
    fn hwid_matches_exact_segment_case_insensitive() {
        let ids = multi(&[r"Root\vdev-display", r"ROOT\OTHER"]);
        assert!(hwid_matches(&ids, r"Root\vdev-display"));
        assert!(hwid_matches(&ids, r"root\VDEV-DISPLAY"));
    }

    #[test]
    fn hwid_rejects_prefix_sibling() {
        // 子串实现会把 `Root\vdev-display-2` 误判为命中
        let ids = multi(&[r"Root\vdev-display-2"]);
        assert!(!hwid_matches(&ids, r"Root\vdev-display"));
        let ids2 = multi(&[r"Root\vdev"]);
        assert!(!hwid_matches(&ids2, r"Root\vdev-display"));
    }
}
