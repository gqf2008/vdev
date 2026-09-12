//! IPC 输入校验（M2）—— 纯函数模块
//!
//! 故意保持零 FFI、零外部 crate 依赖（仅 std）：
//! driver crate 依赖 wdf-umdf-sys（WDK bindgen 只能在 Windows 主机构建），
//! 这些校验逻辑无法随 crate 在 macOS 宿主跑 `cargo test`；
//! 抽成独立模块后可用 `rustc --test` 单独编译本文件在宿主运行回归测试
//! （命令见模块底部测试注释 / 报告）。
//!
//! 校验决策全部集中在本模块，ipc.rs 只做 Vec 搬运与日志，保证测试与
//! 驱动实际行为用的是同一份代码。

/// 最大显示器数量（与 IddCx 适配器能力 `IDDCX_ADAPTER_CAPS::MaxMonitorsSupported`
/// 一致，context.rs 的 `MAX_MONITORS` 从这里取值，单一事实来源）
pub const MAX_MONITORS: usize = 16;
/// 每显示器最大模式数
pub const MAX_MODES_PER_MONITOR: usize = 64;
/// 每模式最大刷新率数量（与 `MAX_MODES_PER_MONITOR` 同风格：超限截断保序）
pub const MAX_RATES_PER_MODE: usize = 64;
/// 分辨率下限（含）
pub const MIN_DIMEN: u32 = 64;
/// 分辨率上限（含）。16384 = 16K 显示器，见 CLAUDE 任务规格 M2(b)
pub const MAX_DIMEN: u32 = 16384;
/// 刷新率下限（含）
pub const MIN_REFRESH_RATE: u32 = 1;
/// 刷新率上限（含）
pub const MAX_REFRESH_RATE: u32 = 1000;
/// 单条 IPC 消息的字节上限。
///
/// 取 512 KiB 而不是任务建议里的 64 KiB：按 M2(b) 上限推算，最坏合法 `Notify`
/// （16 显示器 x 64 模式 x 每模式 64 刷新率 —— `MAX_RATES_PER_MODE` 截断生效，
/// id 取 `u32::MAX`、`name` 按 `None` 计）的紧凑 JSON 体量为 378,748 字节
/// （约 370 KiB，宿主单测 `worst_case_legal_notify_fits_in_max_msg_bytes`
/// 固化该数字），64 KiB 会把合法的最坏情况输入静默丢弃。512 KiB 既能容纳
/// 全部合法输入，又把每连接的无界积累限制到常数内存（超限即清空）。
/// （`Monitor::name` 长度未另设上限，注释推算按 `None` 计。）
pub const MAX_MSG_BYTES: usize = 512 * 1024;

/// 单个显示模式被拒绝的原因（供调用方记日志）
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModeRejection {
    /// 宽度越界
    WidthOutOfRange { width: u32 },
    /// 高度越界
    HeightOutOfRange { height: u32 },
}

impl core::fmt::Display for ModeRejection {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::WidthOutOfRange { width } => {
                write!(f, "width {width} outside [{MIN_DIMEN}, {MAX_DIMEN}]")
            }
            Self::HeightOutOfRange { height } => {
                write!(f, "height {height} outside [{MIN_DIMEN}, {MAX_DIMEN}]")
            }
        }
    }
}

/// 校验单个显示模式的分辨率参数（M2(b)）
///
/// 越界时返回 `Err(原因)`，调用方应拒收整个 mode 并记日志。
pub fn validate_monitor_params(width: u32, height: u32) -> Result<(), ModeRejection> {
    if !(MIN_DIMEN..=MAX_DIMEN).contains(&width) {
        return Err(ModeRejection::WidthOutOfRange { width });
    }
    if !(MIN_DIMEN..=MAX_DIMEN).contains(&height) {
        return Err(ModeRejection::HeightOutOfRange { height });
    }
    Ok(())
}

/// 校验单个刷新率是否在合法范围（M2(b)）
#[must_use]
pub fn is_valid_refresh_rate(refresh_rate: u32) -> bool {
    (MIN_REFRESH_RATE..=MAX_REFRESH_RATE).contains(&refresh_rate)
}

/// 对单个显示模式做完整校验（M2(b)）：
/// 分辨率越界 => `Err`（拒收整个 mode）；
/// 否则 => `Ok(保序过滤后的刷新率列表)`（越界刷新率被剔除，超过
/// `MAX_RATES_PER_MODE` 的合法刷新率被截断，与 modes 上限同风格）。
/// 调用方对空列表应拒收该 mode。
pub fn validate_mode(
    width: u32,
    height: u32,
    refresh_rates: &[u32],
) -> Result<Vec<u32>, ModeRejection> {
    validate_monitor_params(width, height)?;
    let mut rates: Vec<u32> = refresh_rates
        .iter()
        .copied()
        .filter(|&rr| is_valid_refresh_rate(rr))
        .collect();
    rates.truncate(MAX_RATES_PER_MODE);
    Ok(rates)
}

/// 显示器数量收敛到上限（M2(b)：超出部分截断）
#[must_use]
pub fn enforce_monitor_cap(count: usize) -> usize {
    count.min(MAX_MONITORS)
}

/// 单条消息是否超过字节上限（M2(a)：超限调用方应丢弃已积累 buffer）
#[must_use]
pub fn msg_over_limit(len: usize) -> bool {
    len > MAX_MSG_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- validate_monitor_params / validate_mode：分辨率边界 ----

    #[test]
    fn dimensions_at_bounds_are_accepted() {
        assert_eq!(validate_monitor_params(MIN_DIMEN, MIN_DIMEN), Ok(()));
        assert_eq!(validate_monitor_params(MAX_DIMEN, MAX_DIMEN), Ok(()));
        // 典型分辨率
        assert_eq!(validate_monitor_params(1920, 1080), Ok(()));
        assert_eq!(validate_monitor_params(3840, 2160), Ok(()));
    }

    #[test]
    fn dimensions_out_of_bounds_are_rejected() {
        assert_eq!(
            validate_monitor_params(MIN_DIMEN - 1, 1080),
            Err(ModeRejection::WidthOutOfRange {
                width: MIN_DIMEN - 1
            })
        );
        assert_eq!(
            validate_monitor_params(1920, MAX_DIMEN + 1),
            Err(ModeRejection::HeightOutOfRange {
                height: MAX_DIMEN + 1
            })
        );
        // 攻击者典型输入：0 / 超大值（u32 溢出防护的历史坑）
        assert!(validate_monitor_params(0, 0).is_err());
        assert!(validate_monitor_params(u32::MAX, u32::MAX).is_err());
    }

    // ---- validate_mode：刷新率过滤 ----

    #[test]
    fn refresh_rates_at_bounds_are_kept_in_order() {
        let rates = [MIN_REFRESH_RATE, 24, 60, 144, MAX_REFRESH_RATE];
        assert_eq!(validate_mode(1920, 1080, &rates), Ok(rates.to_vec()));
    }

    #[test]
    fn out_of_range_refresh_rates_are_dropped_not_rejecting_mode() {
        assert_eq!(
            validate_mode(1920, 1080, &[0, 60, MAX_REFRESH_RATE + 1, 75, u32::MAX]),
            Ok(vec![60, 75])
        );
    }

    #[test]
    fn mode_with_no_valid_refresh_rates_yields_empty_list() {
        assert_eq!(validate_mode(1920, 1080, &[0, 1001]), Ok(Vec::new()));
    }

    #[test]
    fn invalid_dimensions_reject_whole_mode_before_refresh_filtering() {
        assert_eq!(
            validate_mode(63, 1080, &[60]),
            Err(ModeRejection::WidthOutOfRange { width: 63 })
        );
    }

    // ---- validate_mode：每模式刷新率数量上限（M2(b) MAX_RATES_PER_MODE）----

    #[test]
    fn refresh_rates_at_cap_are_all_kept() {
        // 恰好 64 个合法刷新率：全部保留（末位取上边界值）
        let mut rates: Vec<u32> = (1..MAX_RATES_PER_MODE as u32).collect();
        rates.push(MAX_REFRESH_RATE);
        let expected = rates.clone();
        assert_eq!(
            validate_mode(1920, 1080, &rates),
            Ok(expected),
            "64 valid rates must all pass"
        );
    }

    #[test]
    fn refresh_rates_over_cap_are_truncated_keeping_order() {
        // 65 个合法刷新率：截断到 64，保序（与 modes 上限同风格）
        let rates: Vec<u32> = (1..=(MAX_RATES_PER_MODE as u32 + 1)).collect();
        assert_eq!(
            validate_mode(1920, 1080, &rates),
            Ok((1..=MAX_RATES_PER_MODE as u32).collect::<Vec<_>>()),
            "65 valid rates: the 65th must be dropped, order preserved"
        );
    }

    #[test]
    fn refresh_rate_cap_applies_after_range_filtering() {
        // 65 个输入但含越界值：先过滤再截断，仍是 64 个合法值
        let mut rates: Vec<u32> = (1..=(MAX_RATES_PER_MODE as u32 + 1)).collect();
        rates.push(0);
        rates.push(MAX_REFRESH_RATE + 1);
        assert_eq!(
            validate_mode(1920, 1080, &rates),
            Ok((1..=MAX_RATES_PER_MODE as u32).collect::<Vec<_>>())
        );
    }

    #[test]
    fn worst_case_legal_notify_fits_in_max_msg_bytes() {
        // 最坏合法 Notify：16 显示器 x 64 模式 x 每模式 64 刷新率（M2(b) 上限全部
        // 取满，id = u32::MAX、name = None）。validate.rs 零依赖（无 serde），
        // 按 serde_json 紧凑格式 + 线上结构体字段顺序（core.rs）手工拼装等价串；
        // 若 M2(b) 任一上限或字段变化导致此断言失败，须同步复算 MAX_MSG_BYTES
        // 注释里的推算数字。
        let rates = vec![MAX_REFRESH_RATE; MAX_RATES_PER_MODE];
        let mode = format!(
            "{{\"width\":{MAX_DIMEN},\"height\":{MAX_DIMEN},\"refresh_rates\":[{}]}},",
            rates
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        let modes = mode.repeat(MAX_MODES_PER_MONITOR);
        let modes = modes.strip_suffix(',').expect("modes non-empty");
        let monitor = format!(
            "{{\"id\":{},\"name\":null,\"enabled\":true,\"modes\":[{modes}]}},",
            u32::MAX
        );
        let monitors = monitor.repeat(MAX_MONITORS);
        let monitors = monitors.strip_suffix(',').expect("monitors non-empty");
        let payload = format!("{{\"Notify\":[{monitors}]}}");

        // 注释推算数字的固化（MAX_MSG_BYTES 注释同源）
        assert_eq!(payload.len(), 378_748);

        // 取值依据：最坏合法 Notify 必须能通过 M2(a) 单消息上限
        assert!(!msg_over_limit(payload.len()));
        assert!(payload.len() > 64 * 1024, "64 KiB 会丢掉最坏合法输入");
    }

    // ---- 数量收敛 ----

    #[test]
    fn monitor_count_is_capped() {
        assert_eq!(enforce_monitor_cap(0), 0);
        assert_eq!(enforce_monitor_cap(MAX_MONITORS), MAX_MONITORS);
        assert_eq!(enforce_monitor_cap(MAX_MONITORS + 1), MAX_MONITORS);
        assert_eq!(enforce_monitor_cap(usize::MAX), MAX_MONITORS);
    }

    // ---- 单消息上限 ----

    #[test]
    fn message_limit_boundary() {
        assert!(!msg_over_limit(0));
        assert!(!msg_over_limit(MAX_MSG_BYTES));
        assert!(msg_over_limit(MAX_MSG_BYTES + 1));
    }
}
