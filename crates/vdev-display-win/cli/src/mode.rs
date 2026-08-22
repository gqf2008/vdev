//! 显示器模式（分辨率/刷新率）命令行解析，格式如 `1920x1080` / `3840x2160@60/120`。

use std::collections::BTreeSet;

use anyhow::{bail, Context as _, Result};

const DEFAULT_REFRESH_RATE: driver_ipc::RefreshRate = 60;

/// 用户命令行指定的模式，与 [`driver_ipc::Mode`] 相似但刷新率列表可为空
#[derive(Debug, Clone)]
pub struct Mode {
    pub width: driver_ipc::Dimen,
    pub height: driver_ipc::Dimen,
    pub refresh_rates: BTreeSet<driver_ipc::RefreshRate>,
}

impl Mode {
    fn ensure_refresh_rate(&mut self) {
        if self.refresh_rates.is_empty() {
            self.refresh_rates.insert(DEFAULT_REFRESH_RATE);
        }
    }
}

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

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.refresh_rates.is_empty() {
            write!(f, "{}x{}", self.width, self.height)
        } else {
            let rates = self
                .refresh_rates
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("/");
            write!(f, "{}x{}@{}", self.width, self.height, rates)
        }
    }
}

impl std::str::FromStr for Mode {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        let (resolution, refresh_rate_list) = match s.split_once('@') {
            Some((resolution, refresh_rate_list)) => (resolution, Some(refresh_rate_list)),
            None => (s, None),
        };

        let (width, height) = resolution
            .split_once('x')
            .ok_or_else(|| anyhow::anyhow!("无效分辨率 {s:?}，应为类似 \"1920x1080\" 的格式"))?;
        let width = width
            .parse()
            .with_context(|| format!("无效宽度 {s:?}，应为数字"))?;
        let height = height
            .parse()
            .with_context(|| format!("无效高度 {s:?}，应为数字"))?;

        let refresh_rates = match refresh_rate_list {
            Some(rates) => rates
                .split('/')
                .map(|r| {
                    r.parse()
                        .with_context(|| format!("无效刷新率 {s:?}，应为数字"))
                })
                .collect::<Result<BTreeSet<_>>>()?,
            None => BTreeSet::new(),
        };

        Ok(Self {
            width,
            height,
            refresh_rates,
        })
    }
}

/// 校验模式下限（IddCx 至少要支持 64x64 之类的合法值）
pub fn validate(mode: &driver_ipc::Mode) -> Result<()> {
    if mode.width < 64 || mode.height < 64 {
        bail!("分辨率过小：{}x{}（最小 64x64）", mode.width, mode.height);
    }
    if mode.refresh_rates.is_empty() {
        bail!("至少需要一个刷新率");
    }
    for rate in &mode.refresh_rates {
        if *rate == 0 {
            bail!("刷新率不能为 0");
        }
    }
    Ok(())
}
