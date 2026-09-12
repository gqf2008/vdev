//! 端点子设备注册名与物理连接 pin 常量（纯数据，无条件编译以支持宿主 #[test]）
//!
//! 这些常量被 adapter.rs（kernel feature）用于 PcRegisterSubdevice /
//! PcRegisterPhysicalConnection；单独成模块是因为「注册名与 INF 模板逐字节
//! 一致」「pin 编号与 filter 布局一致」是纯逻辑断言，必须在 macOS 宿主跑通
//!（adapter 模块被 kernel feature 门控，宿主 cargo test 编译不到）。

/// 子设备名（UTF-16，NUL 结尾；PcRegisterSubdevice 要求）
pub const WAVE_CAPTURE_NAME: &[u16] = &[
    0x57, 0x61, 0x76, 0x65, 0x43, 0x61, 0x70, 0x74, 0x75, 0x72, 0x65, 0x2d, 0x30, 0x00,
]; // "WaveCapture-0"
pub const WAVE_RENDER_NAME: &[u16] = &[
    0x57, 0x61, 0x76, 0x65, 0x52, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x2d, 0x30, 0x00,
]; // "WaveRender-0"
pub const TOPOLOGY_CAPTURE_NAME: &[u16] = &[
    0x54, 0x6f, 0x70, 0x6f, 0x6c, 0x6f, 0x67, 0x79, 0x43, 0x61, 0x70, 0x74, 0x75, 0x72, 0x65, 0x2d,
    0x30, 0x00,
]; // "TopologyCapture-0"
pub const TOPOLOGY_RENDER_NAME: &[u16] = &[
    0x54, 0x6f, 0x70, 0x6f, 0x6c, 0x6f, 0x67, 0x79, 0x52, 0x65, 0x6e, 0x64, 0x65, 0x72, 0x2d, 0x30,
    0x00,
]; // "TopologyRender-0"

// 物理（filter 间）连接的 pin 编号。本驱动 wave 小端口每 filter 仅 1 个 pin
//（render pin0 DataFlow=OUT / capture pin0 DataFlow=IN，见 miniport.rs），
// 连接即挂在其上；topology 小端口 2 pin（topology.rs 对照 sysvad *toptable.h）：
//   render：pin0 = KSPIN_TOPO_WAVEOUT_SOURCE（自 wave 汇入，DataFlow IN）
//   capture：pin1 = KSPIN_TOPO_BRIDGE（桥接出至 wave，DataFlow OUT）
// 数据流方向与 sysvad ConnectTopologies 两条连接一致：
//   render 路径：wave → topology；capture 路径：topology → wave。
pub const WAVE_PIN: u32 = 0;
pub const TOPO_RENDER_FROM_WAVE_PIN: u32 = 0;
pub const TOPO_CAPTURE_TO_WAVE_PIN: u32 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    /// 解码 NUL 结尾 UTF-16 常量为 String（PcRegisterSubdevice 名）
    fn wide_to_string(wide: &[u16]) -> String {
        let end = wide.iter().position(|&c| c == 0).expect("NUL 结尾");
        wide[..end]
            .iter()
            .map(|&c| char::from_u32(u32::from(c)).expect("ASCII"))
            .collect()
    }

    /// 子设备注册名与 INF AddInterface 的 KSNAME_* 模板逐字节一致。
    ///
    /// PcRegisterSubdevice 的 Name 即 KS filter 名；INF
    /// `AddInterface=%KSCATEGORY_…%,%KSNAME_…%` 按该名引用子设备，
    /// 任何一侧改名（哪怕差一个字节）都会让设备接口不出现。
    #[test]
    fn subdevice_names_match_inf_templates() {
        assert_eq!(wide_to_string(WAVE_CAPTURE_NAME), "WaveCapture-0");
        assert_eq!(wide_to_string(WAVE_RENDER_NAME), "WaveRender-0");
        assert_eq!(wide_to_string(TOPOLOGY_CAPTURE_NAME), "TopologyCapture-0");
        assert_eq!(wide_to_string(TOPOLOGY_RENDER_NAME), "TopologyRender-0");
    }

    /// INF 必须是 UTF-16 LE（带 BOM）且 KSNAME 模板与驱动注册名一致。
    ///
    /// InfVerify/Windows 的 SetupAPI 只认 UTF-16 LE 编码的 INF（对照 sysvad
    /// 随包 INF）；测试同时校验编码与四个模板字符串。
    #[test]
    fn inf_is_utf16le_and_templates_match() {
        let path = format!("{}/vdev-audio.inf", env!("CARGO_MANIFEST_DIR"));
        let bytes = std::fs::read(&path).expect("INF 文件存在");
        // BOM：FF FE（UTF-16 LE）
        assert_eq!(&bytes[..2], &[0xFF, 0xFE], "INF 须以 UTF-16 LE BOM 开头");
        assert_eq!(bytes.len() % 2, 0, "UTF-16 字节数须为偶数");
        // 逐对取小端 u16（array_chunks 在本工具链尚未稳定，故按下标成对解码）
        let tail = &bytes[2..];
        let units: Vec<u16> = (0..tail.len() / 2)
            .map(|i| u16::from_le_bytes([tail[2 * i], tail[2 * i + 1]]))
            .collect();
        let text = String::from_utf16(&units).expect("合法 UTF-16");
        // BOM 唯一（回归：文件曾以 FF FE FF FE 双 BOM 开头，第二个 U+FEFF
        // 会混入首行；SetupAPI/InfVerif 对此不容忍）
        assert!(
            !text.contains('\u{feff}'),
            "BOM 之后不得再出现 U+FEFF（双 BOM 回归）"
        );
        // 换行统一 CRLF（回归：曾出现 \r\r\n 的 CRCRLF 行尾）
        assert_eq!(
            text.matches('\r').count(),
            text.matches("\r\n").count(),
            "所有 CR 必须属于 CRLF（禁止孤立 CR/CRCRLF）"
        );
        for (ksname, driver_name) in [
            ("KSNAME_WaveCapture", wide_to_string(WAVE_CAPTURE_NAME)),
            ("KSNAME_WaveRender", wide_to_string(WAVE_RENDER_NAME)),
            (
                "KSNAME_TopologyCapture",
                wide_to_string(TOPOLOGY_CAPTURE_NAME),
            ),
            (
                "KSNAME_TopologyRender",
                wide_to_string(TOPOLOGY_RENDER_NAME),
            ),
        ] {
            let template = format!("{ksname}=\"{driver_name}\"");
            assert!(
                text.contains(&template),
                "INF 缺少模板 {template}（驱动注册名须与 INF 接口模板逐字节一致）"
            );
        }
    }

    /// 物理连接 pin 编号与 miniport/topology 的 filter 布局一致
    ///（wave 每 filter 1 pin；topology render pin0 自 wave 汇入、
    /// capture pin1 桥接出至 wave——对照 sysvad *toptable.h）
    #[test]
    fn physical_connection_pins() {
        assert_eq!(WAVE_PIN, 0);
        assert_eq!(TOPO_RENDER_FROM_WAVE_PIN, 0);
        assert_eq!(TOPO_CAPTURE_TO_WAVE_PIN, 1);
    }
}
