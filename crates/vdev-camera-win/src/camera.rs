//! 面向宿主的高层安全 API：注册/注销虚拟摄像头、推流。

use std::path::Path;

use anyhow::{anyhow, Context, Result};
use windows::Win32::Media::MediaFoundation::CLSID_VideoInputDeviceCategory;
use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
use windows_core::GUID;

use crate::com::registry::{delete_tree, guid_string, module_file_path, RegKey};
use crate::com::shm::SharedFrameChannel;
use crate::dshow::filter::{CLSID_VirtualCameraFilter, FILTER_NAME};

/// 虚拟摄像头服务器（宿主端，生产者）。
pub struct CameraServer {
    channel: SharedFrameChannel,
}

impl CameraServer {
    /// 打开共享帧通道（生产者模式）。
    pub fn open() -> Result<Self> {
        let channel = SharedFrameChannel::open_or_create(true)
            .context("open shared frame channel (producer)")?;
        Ok(Self { channel })
    }

    /// 推送一帧 BGRA 到虚拟摄像头。
    pub fn push_frame(&self, width: u32, height: u32, bgra: &[u8]) -> Result<()> {
        self.channel
            .publish(width, height, bgra)
            .map_err(|e| anyhow!("push frame: {e}"))
    }
}

/// 注册过滤器到系统（CLSID + Video Capture Sources 类别）。
///
/// 同时注册 **64 位视图**（`Software\Classes`）与 **32 位视图**
/// （`Software\Classes\WOW6432Node`）：32 位进程（如 32 位 VLC）通过 WOW64
/// 重定向只能看到 WOW6432Node 视图，只注册 64 位视图时它们看不到设备。
/// 32 位 DLL（`vdev_camera_win32.dll`）存在时才注册 32 位视图。
///
/// 优先系统级（HKLM，需管理员），失败自动回退到当前用户级（HKCU，无需管理员，
/// HKCR 合并视图同样可见）。模块路径取「当前可执行文件同目录下的 filter DLL」。
pub fn register_filter() -> Result<()> {
    let exe = module_file_path().context("get module path")?;
    let dir = Path::new(&exe)
        .parent()
        .ok_or_else(|| anyhow!("module path has no parent: {exe}"))?;
    let dll64 = dir.join("vdev_camera_win.dll");
    register_arch(&dll64.to_string_lossy(), "Software\\Classes")?;

    let dll32 = dir.join("vdev_camera_win32.dll");
    if dll32.exists() {
        log::info!("检测到 32 位 DLL，注册 32 位（WOW6432Node）视图");
        register_arch(&dll32.to_string_lossy(), "Software\\Classes\\WOW6432Node")?;
    } else {
        log::warn!("未找到 vdev_camera_win32.dll，跳过 32 位注册（32 位应用将看不到）");
    }
    Ok(())
}

/// 注销过滤器（清理 HKLM 与 HKCU 两个根的注册）。
pub fn unregister_filter() -> Result<()> {
    let clsid = guid_string(&CLSID_VirtualCameraFilter);
    let cat = guid_string(&CLSID_VideoInputDeviceCategory);
    // 64 位视图（Software\Classes）与 32 位视图（Software\Classes\WOW6432Node）
    // × HKLM/HKCU 两个根，全部清理。
    for (root, prefix) in [
        (HKEY_LOCAL_MACHINE, "Software\\Classes"),
        (HKEY_CURRENT_USER, "Software\\Classes"),
        (HKEY_LOCAL_MACHINE, "Software\\Classes\\WOW6432Node"),
        (HKEY_CURRENT_USER, "Software\\Classes\\WOW6432Node"),
    ] {
        let _ = delete_tree(root, &format!("{prefix}\\CLSID\\{cat}\\Instance\\{clsid}"));
        let _ = delete_tree(root, &format!("{prefix}\\CLSID\\{clsid}"));
    }
    Ok(())
}

/// 用显式 DLL 路径注册（64 位视图；32 位视图见 [`register_filter`]）。
pub fn register_with_path(dll_path: &str) -> Result<()> {
    register_arch(dll_path, "Software\\Classes")
}

/// 把过滤器注册到指定注册表视图（`prefix` = `Software\Classes` 或
/// `Software\Classes\WOW6432Node`）。先试系统级（HKLM，需管理员），失败
/// 自动回退到当前用户级（HKCU，无需管理员，HKCR 合并视图同样可见）。
fn register_arch(dll_path: &str, prefix: &str) -> Result<()> {
    let clsid = guid_string(&CLSID_VirtualCameraFilter);
    let cat = guid_string(&CLSID_VideoInputDeviceCategory);

    // 先试系统级，失败（通常是权限）再回退当前用户级。
    let mut last_err = None;
    for (root, root_prefix) in [(HKEY_LOCAL_MACHINE, prefix), (HKEY_CURRENT_USER, prefix)] {
        match write_registration(root, root_prefix, dll_path, &clsid, &cat) {
            Ok(()) => {
                if root == HKEY_CURRENT_USER {
                    log::warn!("系统级注册失败，已注册到当前用户（HKCR 视图可见）");
                }
                return Ok(());
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow!("registration failed")))
}

fn write_registration(
    root: HKEY,
    prefix: &str,
    dll_path: &str,
    clsid: &str,
    cat: &str,
) -> Result<()> {
    let key =
        RegKey::create(root, &format!("{prefix}\\CLSID\\{clsid}")).context("create CLSID key")?;
    key.set_string("", FILTER_NAME)
        .context("set friendly name")?;
    drop(key);

    let key = RegKey::create(root, &format!("{prefix}\\CLSID\\{clsid}\\InprocServer32"))
        .context("create InprocServer32")?;
    key.set_string("", dll_path).context("set module path")?;
    key.set_string("ThreadingModel", "Both")
        .context("set threading model")?;
    drop(key);

    // 类别 Instance 键：FriendlyName 决定设备枚举器（ICreateDevEnum）能否列出本过滤器
    // （缺 FriendlyName 时枚举直接跳过）；FilterData（REGFILTER2 v2 二进制）供
    // IFilterMapper2::EnumMatchingFilters 等基于 Filter Mapper 的消费方使用。
    // 结构与 Windows IFilterMapper2::RegisterFilter 写出的字节一致（对照 OBS/ToDesk）。
    let key = RegKey::create(root, &format!("{prefix}\\CLSID\\{cat}\\Instance\\{clsid}"))
        .context("create category instance")?;
    key.set_string("FriendlyName", FILTER_NAME)
        .context("set FriendlyName")?;
    key.set_string("CLSID", clsid)
        .context("set category CLSID")?;
    key.set_binary("FilterData", &serialize_filter_data())
        .context("set FilterData")?;
    Ok(())
}

/// DirectShow 注册表 `FilterData`（REG_BINARY）序列化。
///
/// 磁盘格式为 REGFILTER2 的 v2 布局（Windows quartz.dll /
/// `IFilterMapper2::RegisterFilter` 写出的字节一致，参照 Wine `FM2_WriteFilterData`
/// 与 ToDesk Camera 的真实 88 字节样例）：
/// - REG_RF（16B）：dwVersion=2 / dwMerit / dwPins / dwUnused
/// - 每个 pin 一个 REG_RFP（24B）：signature('0pi3') / dwFlags / dwInstances /
///   dwMediaTypes / dwMediums / bCategory
/// - 每个媒体类型一个 REG_TYPE（16B）：signature('0ty3') / dwUnused /
///   dwOffsetMajor / dwOffsetMinor
/// - 末尾 clsidStore：被偏移引用的原始 GUID
///
/// 本过滤器：单个 RGB32（BGRA）输出 pin、MERIT_DO_NOT_USE（不参与自动图连接，只能被显式选择）。
fn serialize_filter_data() -> Vec<u8> {
    use windows::Win32::Media::DirectShow::MERIT_DO_NOT_USE;
    use windows::Win32::Media::MediaFoundation::{MEDIATYPE_Video, MEDIASUBTYPE_RGB32};

    // mainStore 长度：REG_RF(16) + REG_RFP(24) + REG_TYPE(16) = 56。
    const MAIN_STORE: u32 = 56;
    let mut out = Vec::with_capacity(88);

    // REG_RF
    out.extend_from_slice(&2u32.to_le_bytes()); // dwVersion = 2
    out.extend_from_slice(&(MERIT_DO_NOT_USE.0 as u32).to_le_bytes()); // dwMerit
    out.extend_from_slice(&1u32.to_le_bytes()); // dwPins
    out.extend_from_slice(&0u32.to_le_bytes()); // dwUnused

    // REG_RFP：输出 pin（REG_PINFLAG_B_OUTPUT = 0x2，cInstances = 1）
    out.extend_from_slice(b"0pi3"); // signature
    out.extend_from_slice(&2u32.to_le_bytes()); // dwFlags = REG_PINFLAG_B_OUTPUT
    out.extend_from_slice(&1u32.to_le_bytes()); // dwInstances
    out.extend_from_slice(&1u32.to_le_bytes()); // dwMediaTypes
    out.extend_from_slice(&0u32.to_le_bytes()); // dwMediums
    out.extend_from_slice(&0u32.to_le_bytes()); // bCategory

    // REG_TYPE：RGB32（BGRA）输出媒体类型
    out.extend_from_slice(b"0ty3"); // signature
    out.extend_from_slice(&0u32.to_le_bytes()); // dwUnused
    out.extend_from_slice(&MAIN_STORE.to_le_bytes()); // dwOffsetMajor
    out.extend_from_slice(&(MAIN_STORE + 16).to_le_bytes()); // dwOffsetMinor

    // clsidStore
    out.extend_from_slice(&guid_bytes(&MEDIATYPE_Video));
    out.extend_from_slice(&guid_bytes(&MEDIASUBTYPE_RGB32));
    out
}

/// GUID 的注册表字节序（data1/data2/data3 小端 + data4 原序）。
fn guid_bytes(g: &GUID) -> [u8; 16] {
    let mut b = [0u8; 16];
    b[0..4].copy_from_slice(&g.data1.to_le_bytes());
    b[4..6].copy_from_slice(&g.data2.to_le_bytes());
    b[6..8].copy_from_slice(&g.data3.to_le_bytes());
    b[8..16].copy_from_slice(&g.data4);
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Media::MediaFoundation::{MEDIATYPE_Video, MEDIASUBTYPE_RGB32};

    /// FilterData 必须严格匹配 REGFILTER2 v2 磁盘布局（回归：缺字段会导致
    /// 设备枚举/Filter Mapper 消费方不可见）。
    #[test]
    fn filter_data_blob_layout() {
        let blob = serialize_filter_data();
        assert_eq!(blob.len(), 88);
        // REG_RF
        assert_eq!(&blob[0..4], &2u32.to_le_bytes());
        assert_eq!(&blob[4..8], &0x0020_0000u32.to_le_bytes()); // MERIT_DO_NOT_USE
        assert_eq!(&blob[8..12], &1u32.to_le_bytes()); // dwPins
        assert_eq!(&blob[12..16], &0u32.to_le_bytes()); // dwUnused
                                                        // REG_RFP
        assert_eq!(&blob[16..20], b"0pi3");
        assert_eq!(&blob[20..24], &2u32.to_le_bytes()); // REG_PINFLAG_B_OUTPUT
        assert_eq!(&blob[24..28], &1u32.to_le_bytes()); // dwInstances
        assert_eq!(&blob[28..32], &1u32.to_le_bytes()); // dwMediaTypes
        assert_eq!(&blob[32..36], &0u32.to_le_bytes()); // dwMediums
        assert_eq!(&blob[36..40], &0u32.to_le_bytes()); // bCategory
                                                        // REG_TYPE
        assert_eq!(&blob[40..44], b"0ty3");
        assert_eq!(&blob[44..48], &0u32.to_le_bytes()); // dwUnused
        assert_eq!(&blob[48..52], &56u32.to_le_bytes()); // dwOffsetMajor
        assert_eq!(&blob[52..56], &72u32.to_le_bytes()); // dwOffsetMinor
                                                         // clsidStore
        assert_eq!(&blob[56..72], &guid_bytes(&MEDIATYPE_Video));
        assert_eq!(&blob[72..88], &guid_bytes(&MEDIASUBTYPE_RGB32));
    }

    /// GUID 字节序：data1/data2/data3 小端，data4 原序。
    #[test]
    fn guid_bytes_little_endian() {
        let g = GUID::from_u128(0x73646976_0000_0010_8000_00aa00389b71); // MEDIATYPE_Video
        assert_eq!(guid_bytes(&g)[0..4], [0x76, 0x69, 0x64, 0x73]); // "vids"
        assert_eq!(guid_bytes(&g)[4..8], [0x00, 0x00, 0x10, 0x00]); // data2(0) + data3(0x10) 小端
    }
}
