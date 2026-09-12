//! 显示器模式（分辨率/刷新率）命令行解析，格式如 `1920x1080` / `3840x2160@60/120`。
//!
//! 纯解析/校验逻辑在 [`crate::mode_check`]（零依赖，宿主 `rustc --test` 直跑）；
//! 本文件只负责与 `driver_ipc` 线上类型的转换适配。

pub use mode_check::Mode;

use anyhow::Result;

use crate::mode_check;

impl From<driver_ipc::Mode> for Mode {
    fn from(value: driver_ipc::Mode) -> Self {
        Self {
            width: value.width,
            height: value.height,
            refresh_rates: value.refresh_rates.into_iter().collect(),
        }
    }
}

impl From<Mode> for driver_ipc::Mode {
    fn from(mut value: Mode) -> Self {
        value.ensure_refresh_rate();
        Self {
            width: value.width,
            height: value.height,
            refresh_rates: value.refresh_rates.into_iter().collect(),
        }
    }
}

/// 校验模式下限（IddCx 至少要支持 64x64 之类的合法值）
pub fn validate(mode: &driver_ipc::Mode) -> Result<()> {
    Ok(mode_check::validate(
        mode.width,
        mode.height,
        &mode.refresh_rates,
    )?)
}
