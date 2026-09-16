//! 虚拟声卡驱动安装 / 卸载 / 状态（SetupAPI，Media 类，Root\vdev-audio）。
//!
//! 卸载路径与 `vdev-hid-win/src/kernel.rs` 同款（CfgMgr 重试、wait_gone 轮询、
//! pnputil 兜底、零残留断言）：真机实测 `CM_Query_And_Remove_SubTreeW` 会
//! 返回 `CR_SUCCESS` 而节点仍在设备树（"假成功"），只看返回值就把幽灵节点
//! 留给下一次 install（见 LESSON_CM移除设备返回成功不等于节点消失）。

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

fn ascii_lower_u16(c: u16) -> u16 {
    if (b'A' as u16..=b'Z' as u16).contains(&c) {
        c + 0x20
    } else {
        c
    }
}

/// REG_MULTI_SZ 硬件 ID 精确匹配：逐段（NUL 分隔）整段不区分大小写相等。
/// 与 hid 侧 `report::hwid_matches` 同款（审查 L1 修复）：原实现 `wide_contains`
/// 做子串匹配，`Root\vdev-audio` 会误中 `Root\vdev-audio-extra` 之类前缀兄弟。
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

/// 打开 Media 类设备集合（flags=0：含非 present 残留节点）
fn open_class_devices() -> Result<HDEVINFO> {
    unsafe {
        SetupDiGetClassDevsW(
            Some(&GUID_DEVCLASS_MEDIA),
            None,
            None,
            SETUP_DI_GET_CLASS_DEVS_FLAGS(0),
        )
    }
    .context("SetupDiGetClassDevsW failed")
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

/// 枚举 Media 类下所有 vdev-audio 节点。`remove_all_nodes` 与卸载后的
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

/// 当前 vdev-audio 节点的实例 ID 列表（如 `ROOT\MEDIA\0007`）。
///
/// 这是"节点是否真的消失"的唯一可信判据：真机实测 `CM_Query_And_Remove_SubTreeW`
/// 会返回 `CR_SUCCESS` 而节点**仍然留在设备树里**（随后 `install` 又建一个，
/// 反复装卸就把节点堆起来），所以删除后必须按实例 ID 轮询确认。
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

/// 移除所有 vdev-audio 设备节点（逐节点重试 + pnputil 兜底，单点失败不中断整轮）。
///
/// 用配置管理器（CfgMgr）移除整棵子树：DIF_REMOVE 那条路在本机不可用——
/// `SetupDiSetDeviceInstallParamsW` 强转 `SP_REMOVEDEVICE_PARAMS` 报 0x800706F8
/// （与 HID、显示器两侧实测同款结论，RULE_Windows驱动CLI卸载禁用DIF_REMOVE改用CfgMgr）。
///
/// 审查 M-c 修复：原实现只看 `CM_Query_And_Remove_SubTreeW` 返回值，而真机上 CM
/// 会假成功（返回 CR_SUCCESS 节点仍在），且单个节点失败就 bail 整轮；现与 hid 侧
/// 同款：逐节点 CfgMgr 最多 3 轮 → 以 wait_gone（实例 ID 轮询）为准 → pnputil
/// 兜底 → 失败汇总，宁可非零退出也不静默留幽灵节点。
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
            "有 {} 个 vdev-audio 设备节点未能移除（管理员权限？设备被占用？）",
            failures.len()
        );
    }
    Ok(removed)
}

/// 安装时的节点处置决策（纯函数，Windows 宿主可单测）。
///
/// 背景：`install` 原先每次都「先清空全部节点、再新建一个」，真机上踩了两件事：
///
/// 1. 清空那步会「假成功」——`CM_Query_And_Remove_SubTreeW` 回 `CR_SUCCESS` 而
///    节点仍在设备树（2a2f4bd 的 M-c 修复）。一旦没清掉，本次新建的节点就与残留
///    节点共存；而驱动侧适配器是**单例**（`adapter::create` 只允许一个实例，见
///    `crates/vdev-audio-win/driver/src/adapter.rs`），第二个节点在 `start_device`
///    里 create 返回 null，直接上报 `STATUS_INSUFFICIENT_RESOURCES`——设备管理器
///    里 `ROOT\MEDIA\0002/0003` 的 `0xC000009A` 就是这么来的。
/// 2. 即便清空成功，「删旧建新」也会换掉设备实例 ID，音频端点 GUID 随之漂移
///    （每次 install 后 setupapi 日志里都有一对 `Delete Device - SWD\MMDEVAPI\{...}`），
///    正在使用该端点的应用会被静默打断。
///
/// 所以安装必须**幂等**：没有节点才新建；已有一个节点就只更新驱动、不动节点；
/// 发现多个（历史残留）才先全清再重建。
#[derive(Debug, PartialEq, Eq)]
enum InstallPlan {
    /// 设备树里一个 `Root\vdev-audio` 节点都没有：需要新建一个
    Create,
    /// 恰好一个节点：就地更新驱动，不重建节点（端点 GUID 保持不变）
    Reuse,
    /// 多个节点（历史残留）：先全部清掉再重建
    Recreate,
}

fn install_plan(existing_nodes: usize) -> InstallPlan {
    match existing_nodes {
        0 => InstallPlan::Create,
        1 => InstallPlan::Reuse,
        _ => InstallPlan::Recreate,
    }
}

/// 在 Media 类下新建一个 `Root\vdev-audio` 设备节点（`DIF_REGISTERDEVICE`），
/// 供随后的 `DiInstallDriverW` 绑定。
fn create_device_node(class_guid: &windows::core::GUID, class_name: &[u16]) -> Result<()> {
    let devs = unsafe { SetupDiCreateDeviceInfoList(Some(class_guid), None) }
        .context("SetupDiCreateDeviceInfoList failed")?;

    let result = (|| -> Result<()> {
        let mut dev_info = SP_DEVINFO_DATA {
            cbSize: std::mem::size_of::<SP_DEVINFO_DATA>() as u32,
            ..Default::default()
        };
        // 审查 L2 修复：删除 0xE0000207 → SetupDiOpenDeviceInfoW(类名) 的"死路径"
        // 回退——SetupDiOpenDeviceInfoW 要的是设备实例 ID，类名永远打不开设备
        // （display 侧同款回退已标"死路径"并移除）；创建失败直接如实报错。
        unsafe {
            SetupDiCreateDeviceInfoW(
                devs,
                windows::core::PCWSTR(class_name.as_ptr()),
                class_guid,
                None,
                None,
                DICD_GENERATE_ID,
                Some(&mut dev_info),
            )
        }
        .context("SetupDiCreateDeviceInfoW failed")?;
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
        Ok(())
    })();

    unsafe { SetupDiDestroyDeviceInfoList(devs) }.ok();
    result
}

/// 安装驱动：幂等处置设备节点 + `DiInstallDriverW` 装入驱动存储 + 事后断言恰好一个节点
pub fn install(inf_dir: &Path) -> Result<()> {
    let inf_path = inf_dir.join("vdev-audio.inf");
    if !inf_path.exists() {
        bail!("找不到 INF: {}", inf_path.display());
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
    let class_name_cstr: Vec<u16> = class_name
        .iter()
        .take_while(|&&c| c != 0)
        .copied()
        .chain(std::iter::once(0))
        .collect();

    // 幂等处置：理由见 `install_plan` 的注释（删旧建新会漂移端点 GUID；残留节点
    // 会因驱动单例而上报 0xC000009A）
    let existing = vdev_instance_ids()?;
    let plan = install_plan(existing.len());
    match plan {
        InstallPlan::Create => {}
        InstallPlan::Reuse => {
            println!(
                "已存在设备节点 {}，就地更新驱动（不重建节点，端点 GUID 保持不变）",
                existing.join(", ")
            );
        }
        InstallPlan::Recreate => {
            eprintln!(
                "检测到 {} 个 vdev-audio 节点（应为 1）：{}，先全部清理后重建",
                existing.len(),
                existing.join(", ")
            );
            let removed = remove_all_nodes()?;
            println!("已清理 {removed} 个残留设备节点");
        }
    }
    if plan != InstallPlan::Reuse {
        create_device_node(&class_guid, &class_name_cstr)?;
    }

    // DiInstallDriverW 装入驱动存储并安装：它按 INF 的硬件 ID 更新**所有**匹配设备
    // （含刚新建的这个），所以复用路径不需要额外的绑定动作。
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

    // 事后断言：安装完成后必须**恰好一个**节点（设备注册/移除是异步的，给它最多
    // ~3s）。多节点意味着后建的节点必然因驱动单例失败——宁可 CLI 非零退出，也不要
    // 静默留下「看着装好了、其实有个死节点」的现场。
    let mut ids = vdev_instance_ids()?;
    for _ in 0..6 {
        if ids.len() == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        ids = vdev_instance_ids()?;
    }
    if ids.len() != 1 {
        bail!(
            "安装后 vdev-audio 节点数为 {}（应为 1）：{}（残留节点请先 uninstall 清理）",
            ids.len(),
            if ids.is_empty() {
                "<none>".to_string()
            } else {
                ids.join(", ")
            }
        );
    }
    println!("虚拟声卡驱动已安装（设备节点 {}）", ids[0]);
    Ok(())
}

/// 卸载：移除全部 Root\vdev-audio 残留节点，并断言零残留（有残留非零退出）。
/// 返回是否找到并移除。
pub fn uninstall() -> Result<bool> {
    let removed = remove_all_nodes()?;
    if removed == 0 {
        println!("未找到 vdev 虚拟声卡设备");
        return Ok(false);
    }
    println!("已移除 {removed} 个 vdev 虚拟声卡设备节点");
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
            "卸载后仍有 {} 个 vdev-audio 节点残留：{}（可能被占用，请重试或重启后重试）",
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
        let ids = multi(&[r"Root\vdev-audio", r"ROOT\OTHER"]);
        assert!(hwid_matches(&ids, r"Root\vdev-audio"));
        // 大小写不敏感（Windows 硬件 ID 比较语义）
        assert!(hwid_matches(&ids, r"root\VDEV-AUDIO"));
    }

    #[test]
    fn hwid_rejects_prefix_sibling() {
        // 子串实现会把 `Root\vdev-audio-extra` 误判为命中
        let ids = multi(&[r"Root\vdev-audio-extra"]);
        assert!(!hwid_matches(&ids, r"Root\vdev-audio"));
        // 也不能中更短的公共前缀
        let ids2 = multi(&[r"Root\vdev"]);
        assert!(!hwid_matches(&ids2, r"Root\vdev-audio"));
    }

    /// 安装幂等决策回归：0 → 新建、1 → 就地更新、≥2 → 先清后建。
    /// 回归点是「每次 install 都删旧建新」——清空那步一旦假成功，节点就会堆成
    /// `ROOT\MEDIA\0001/0002/0003`，后建的节点因驱动单例上报 `0xC000009A`。
    #[test]
    fn install_plan_is_idempotent() {
        assert_eq!(install_plan(0), InstallPlan::Create);
        assert_eq!(install_plan(1), InstallPlan::Reuse);
        assert_eq!(install_plan(2), InstallPlan::Recreate);
        assert_eq!(install_plan(3), InstallPlan::Recreate);
    }
}
