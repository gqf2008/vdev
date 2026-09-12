//! 纯显示器状态操作逻辑（无 tokio / Win32 / winreg / thiserror 依赖）。
//!
//! 从 [`crate::driver_client`] 抽出：整箱因 tokio windows named pipe / winreg
//! 只能在 Windows 目标编译，本模块保持零第三方依赖，可在宿主直接
//! `rustc --edition 2021 --test` 跑单测（见 `scripts/state-tests-host.sh`）。
//! 字段访问仅经由 [`crate::core`] 的 `Id` / `Mode` / `Monitor`。

use std::collections::HashSet;

use crate::core::{Id, Mode, Monitor};

/// 给 `monitors` 分配一个空闲 ID。
///
/// - `preferred_id` 未被占用 → 直接返回它；
/// - 否则返回从 0 起第一个未被占用的 ID；
/// - ID 全部被占用（u32 耗尽，实际不可能出现）→ 返回 [`None`]。
pub fn new_id(monitors: &[Monitor], preferred_id: Option<Id>) -> Option<Id> {
    let existing_ids = monitors
        .iter()
        .map(|monitor| monitor.id)
        .collect::<HashSet<_>>();

    if let Some(id) = preferred_id {
        if !existing_ids.contains(&id) {
            return Some(id);
        }
    }

    #[allow(clippy::maybe_infinite_iter)] // RangeFrom<u32> 有界：u32 耗尽自然返回 None
    (0..).find(|id| !existing_ids.contains(id))
}

/// 把 `mode` 追加到 `monitors` 中 ID 为 `id` 的显示器上。
///
/// 显示器不存在、分辨率重复或刷新率重复时返回 `Err`，且不修改状态。
pub fn add_mode(monitors: &mut [Monitor], id: Id, mode: Mode) -> Result<(), error::AddModeError> {
    let Some(mon) = monitors.iter_mut().find(|mon| mon.id == id) else {
        return Err(error::AddModeError::MonNotFound(id));
    };

    // 新 mode 自身的刷新率查重：重复刷新率 → DupRefreshRate
    match mode_has_duplicates(&mode, id) {
        Ok(()) => {}
        Err(error::DuplicateError::RefreshRate(rr, w, h, mid)) => {
            return Err(error::AddModeError::DupRefreshRate(rr, w, h, mid));
        }
        Err(_) => unreachable!("单个 mode 只可能报刷新率重复"),
    }

    if mon
        .modes
        .iter()
        .any(|_mode| _mode.height == mode.height && _mode.width == mode.width)
    {
        return Err(error::AddModeError::DupMode(mode.width, mode.height, id));
    }

    mon.modes.push(mode);

    Ok(())
}

pub fn mons_have_duplicates(monitors: &[Monitor]) -> Result<(), error::DuplicateError> {
    let mut monitor_iter = monitors.iter();
    while let Some(monitor) = monitor_iter.next() {
        let duplicate_id = monitor_iter.clone().any(|b| monitor.id == b.id);
        if duplicate_id {
            return Err(error::DuplicateError::Monitor(monitor.id));
        }

        mon_has_duplicates(monitor)?;
    }

    Ok(())
}

pub fn mon_has_duplicates(monitor: &Monitor) -> Result<(), error::DuplicateError> {
    let mut mode_iter = monitor.modes.iter();
    while let Some(mode) = mode_iter.next() {
        let duplicate_mode = mode_iter
            .clone()
            .any(|m| mode.height == m.height && mode.width == m.width);
        if duplicate_mode {
            return Err(error::DuplicateError::Mode(
                mode.width,
                mode.height,
                monitor.id,
            ));
        }

        mode_has_duplicates(mode, monitor.id)?;
    }

    Ok(())
}

pub fn mode_has_duplicates(mode: &Mode, id: Id) -> Result<(), error::DuplicateError> {
    let mut refresh_iter = mode.refresh_rates.iter().copied();
    while let Some(rr) = refresh_iter.next() {
        let duplicate_rr = refresh_iter.clone().any(|r| rr == r);
        if duplicate_rr {
            return Err(error::DuplicateError::RefreshRate(
                rr,
                mode.width,
                mode.height,
                id,
            ));
        }
    }

    Ok(())
}

pub mod error {
    use std::fmt;

    use super::Id;

    /// 状态重复错误（显示器 ID / 模式分辨率 / 刷新率）。
    ///
    /// 手写 `Display`/`Error`（本模块不用 thiserror，保证可在宿主独立编译）；
    /// 消息文本与原 thiserror 版本逐字一致。
    #[derive(Debug, Clone)]
    pub enum DuplicateError {
        Monitor(Id),
        Mode(u32, u32, Id),
        RefreshRate(u32, u32, u32, Id),
    }

    impl fmt::Display for DuplicateError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::Monitor(id) => write!(f, "Duplicate monitor with ID {id}"),
                Self::Mode(w, h, id) => write!(f, "Duplicate mode {w}x{h} on monitor {id}"),
                Self::RefreshRate(rr, w, h, id) => {
                    write!(
                        f,
                        "Duplicate refresh rate {rr} on mode {w}x{h} on monitor {id}"
                    )
                }
            }
        }
    }

    impl std::error::Error for DuplicateError {}

    /// [`super::add_mode`] 的错误。
    #[derive(Debug, Clone)]
    pub enum AddModeError {
        MonNotFound(Id),
        DupMode(u32, u32, Id),
        DupRefreshRate(u32, u32, u32, Id),
    }

    impl fmt::Display for AddModeError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self {
                Self::MonNotFound(id) => write!(f, "Monitor not found: {id}"),
                Self::DupMode(w, h, id) => write!(f, "Duplicate mode {w}x{h} on monitor {id}"),
                Self::DupRefreshRate(rr, w, h, id) => {
                    write!(
                        f,
                        "Duplicate refresh rate {rr} on mode {w}x{h} on monitor {id}"
                    )
                }
            }
        }
    }

    impl std::error::Error for AddModeError {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mon(id: Id, modes: Vec<Mode>) -> Monitor {
        Monitor {
            id,
            name: None,
            enabled: true,
            modes,
        }
    }

    fn mode(w: u32, h: u32, rates: &[u32]) -> Mode {
        Mode {
            width: w,
            height: h,
            refresh_rates: rates.to_vec(),
        }
    }

    // ---------- new_id ----------

    #[test]
    fn new_id_returns_preferred_when_free() {
        // 回归：原实现条件反转——preferred 空闲时反而返回 None
        let state = vec![mon(0, vec![])];
        assert_eq!(new_id(&state, Some(5)), Some(5));
    }

    #[test]
    fn new_id_prefers_free_preferred_over_first_free() {
        let state = vec![mon(0, vec![]), mon(1, vec![])];
        assert_eq!(new_id(&state, Some(7)), Some(7));
    }

    #[test]
    fn new_id_falls_back_to_first_free_when_preferred_taken() {
        let state = vec![mon(0, vec![]), mon(1, vec![]), mon(3, vec![])];
        assert_eq!(new_id(&state, Some(0)), Some(2));
    }

    #[test]
    fn new_id_without_preference_returns_first_free() {
        let state = vec![mon(0, vec![]), mon(2, vec![])];
        assert_eq!(new_id(&state, None), Some(1));
    }

    #[test]
    fn new_id_on_empty_state_is_zero() {
        let state: Vec<Monitor> = Vec::new();
        assert_eq!(new_id(&state, None), Some(0));
        assert_eq!(new_id(&state, Some(0)), Some(0));
    }

    // ---------- add_mode ----------

    #[test]
    fn add_mode_appends_single_mode() {
        // 回归：原实现 skip(i) 从当前元素起比较恒真，任何 add_mode 都误报
        // DupRefreshRate——单个普通模式必须能加入
        let mut state = vec![mon(0, vec![])];
        add_mode(&mut state, 0, mode(1920, 1080, &[60])).expect("single mode must be addable");
        assert_eq!(state[0].modes.len(), 1);
        assert_eq!(state[0].modes[0].refresh_rates, vec![60]);
    }

    #[test]
    fn add_mode_appends_all_rates() {
        let mut state = vec![mon(0, vec![])];
        add_mode(&mut state, 0, mode(1920, 1080, &[60, 120, 144]))
            .expect("mode with distinct rates must be addable");
        assert_eq!(state[0].modes[0].refresh_rates, vec![60, 120, 144]);
    }

    #[test]
    fn add_mode_rejects_duplicate_refresh_rate() {
        let mut state = vec![mon(0, vec![])];
        let err = add_mode(&mut state, 0, mode(1920, 1080, &[60, 60])).unwrap_err();
        assert!(matches!(
            err,
            error::AddModeError::DupRefreshRate(60, 1920, 1080, 0)
        ));
        // 失败不改动状态
        assert!(state[0].modes.is_empty());
    }

    #[test]
    fn add_mode_rejects_duplicate_resolution() {
        let mut state = vec![mon(0, vec![mode(1920, 1080, &[60])])];
        let err = add_mode(&mut state, 0, mode(1920, 1080, &[120])).unwrap_err();
        assert!(matches!(err, error::AddModeError::DupMode(1920, 1080, 0)));
        assert_eq!(state[0].modes.len(), 1);
    }

    #[test]
    fn add_mode_rejects_missing_monitor() {
        let mut state = vec![mon(0, vec![])];
        let err = add_mode(&mut state, 7, mode(1920, 1080, &[60])).unwrap_err();
        assert!(matches!(err, error::AddModeError::MonNotFound(7)));
    }

    // ---------- 查重辅助 ----------

    #[test]
    fn mons_have_duplicates_detects_duplicate_ids() {
        let state = vec![mon(0, vec![]), mon(0, vec![])];
        assert!(matches!(
            mons_have_duplicates(&state),
            Err(error::DuplicateError::Monitor(0))
        ));
        assert!(mons_have_duplicates(&[mon(0, vec![]), mon(1, vec![])]).is_ok());
    }

    #[test]
    fn mon_has_duplicates_detects_duplicate_resolutions() {
        let monitor = mon(0, vec![mode(800, 600, &[60]), mode(800, 600, &[120])]);
        assert!(matches!(
            mon_has_duplicates(&monitor),
            Err(error::DuplicateError::Mode(800, 600, 0))
        ));
    }

    #[test]
    fn mode_has_duplicates_detects_duplicate_rates() {
        assert!(matches!(
            mode_has_duplicates(&mode(800, 600, &[60, 60]), 0),
            Err(error::DuplicateError::RefreshRate(60, 800, 600, 0))
        ));
        assert!(mode_has_duplicates(&mode(800, 600, &[60, 120]), 0).is_ok());
    }

    // ---------- 错误文本（与原 thiserror 版本逐字一致） ----------

    #[test]
    fn error_display_matches_original_thiserror_texts() {
        assert_eq!(
            error::DuplicateError::Monitor(3).to_string(),
            "Duplicate monitor with ID 3"
        );
        assert_eq!(
            error::DuplicateError::Mode(1920, 1080, 3).to_string(),
            "Duplicate mode 1920x1080 on monitor 3"
        );
        assert_eq!(
            error::DuplicateError::RefreshRate(60, 1920, 1080, 3).to_string(),
            "Duplicate refresh rate 60 on mode 1920x1080 on monitor 3"
        );
        assert_eq!(
            error::AddModeError::MonNotFound(3).to_string(),
            "Monitor not found: 3"
        );
        assert_eq!(
            error::AddModeError::DupMode(1920, 1080, 3).to_string(),
            "Duplicate mode 1920x1080 on monitor 3"
        );
        assert_eq!(
            error::AddModeError::DupRefreshRate(60, 1920, 1080, 3).to_string(),
            "Duplicate refresh rate 60 on mode 1920x1080 on monitor 3"
        );
    }
}
