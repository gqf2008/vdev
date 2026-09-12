//! 显示器模式（分辨率/刷新率）命令行解析与校验的纯逻辑核心。
//!
//! cli 整箱依赖 windows crate 只能在 Windows 目标编译；本模块只用 std，
//! 可在宿主直接 `rustc --edition 2021 --test` 跑单测（见
//! `scripts/state-tests-host.sh`）。[`crate::mode`] 只做与 `driver_ipc`
//! 线上类型的转换适配。

use std::collections::BTreeSet;
use std::fmt;

/// 刷新率列表为空时补的默认刷新率
pub const DEFAULT_REFRESH_RATE: u32 = 60;

/// 校验分辨率下限（IddCx 至少要支持 64x64 之类的合法值）
pub const MIN_DIMENSION: u32 = 64;

/// 用户命令行指定的模式，与 `driver_ipc::Mode` 相似但刷新率列表可为空
/// （空集在转成 `driver_ipc::Mode` 时补 [`DEFAULT_REFRESH_RATE`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub refresh_rates: BTreeSet<u32>,
}

impl Mode {
    /// 刷新率列表为空时补默认刷新率
    pub fn ensure_refresh_rate(&mut self) {
        if self.refresh_rates.is_empty() {
            self.refresh_rates.insert(DEFAULT_REFRESH_RATE);
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
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

/// [`parse_mode`] 的错误（消息文本与原 anyhow 版本逐字一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 缺少 `宽x高` 分隔
    NoResolution(String),
    /// 宽度不是数字
    BadWidth(String),
    /// 高度不是数字
    BadHeight(String),
    /// 刷新率不是数字
    BadRate(String),
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoResolution(s) => {
                write!(f, "无效分辨率 {s:?}，应为类似 \"1920x1080\" 的格式")
            }
            Self::BadWidth(s) => write!(f, "无效宽度 {s:?}，应为数字"),
            Self::BadHeight(s) => write!(f, "无效高度 {s:?}，应为数字"),
            Self::BadRate(s) => write!(f, "无效刷新率 {s:?}，应为数字"),
        }
    }
}

impl std::error::Error for ParseError {}

impl std::str::FromStr for Mode {
    type Err = ParseError;

    fn from_str(s: &str) -> Result<Self, ParseError> {
        parse_mode(s)
    }
}

/// 解析 `1920x1080` / `3840x2160@60/120` 形式的模式。
///
/// 刷新率收集进 `BTreeSet`：去重且升序（与原实现 collect 到 BTreeSet 一致）。
pub fn parse_mode(s: &str) -> Result<Mode, ParseError> {
    let (resolution, refresh_rate_list) = match s.split_once('@') {
        Some((resolution, refresh_rate_list)) => (resolution, Some(refresh_rate_list)),
        None => (s, None),
    };

    let (width, height) = resolution
        .split_once('x')
        .ok_or_else(|| ParseError::NoResolution(s.to_owned()))?;
    let width = width
        .parse()
        .map_err(|_| ParseError::BadWidth(s.to_owned()))?;
    let height = height
        .parse()
        .map_err(|_| ParseError::BadHeight(s.to_owned()))?;

    let refresh_rates = match refresh_rate_list {
        Some(rates) => rates
            .split('/')
            .map(|r| r.parse().map_err(|_| ParseError::BadRate(s.to_owned())))
            .collect::<Result<BTreeSet<_>, ParseError>>()?,
        None => BTreeSet::new(),
    };

    Ok(Mode {
        width,
        height,
        refresh_rates,
    })
}

/// [`validate`] 的错误（消息文本与原 anyhow 版本逐字一致）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidateError {
    /// 分辨率小于下限
    TooSmall { width: u32, height: u32, min: u32 },
    /// 刷新率列表为空
    NoRefreshRate,
    /// 刷新率为 0
    ZeroRate(u32),
}

impl fmt::Display for ValidateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooSmall { width, height, min } => {
                write!(f, "分辨率过小：{width}x{height}（最小 {min}x{min}）")
            }
            Self::NoRefreshRate => write!(f, "至少需要一个刷新率"),
            Self::ZeroRate(_) => write!(f, "刷新率不能为 0"),
        }
    }
}

impl std::error::Error for ValidateError {}

/// 校验模式下限：分辨率不小于 [`MIN_DIMENSION`]、至少一个刷新率且不为 0。
pub fn validate(width: u32, height: u32, refresh_rates: &[u32]) -> Result<(), ValidateError> {
    if width < MIN_DIMENSION || height < MIN_DIMENSION {
        return Err(ValidateError::TooSmall {
            width,
            height,
            min: MIN_DIMENSION,
        });
    }
    if refresh_rates.is_empty() {
        return Err(ValidateError::NoRefreshRate);
    }
    for &rate in refresh_rates {
        if rate == 0 {
            return Err(ValidateError::ZeroRate(rate));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rates(list: &[u32]) -> BTreeSet<u32> {
        list.iter().copied().collect()
    }

    // ---------- parse ----------

    #[test]
    fn parse_plain_resolution() {
        let m = parse_mode("1920x1080").expect("valid");
        assert_eq!(m.width, 1920);
        assert_eq!(m.height, 1080);
        assert!(m.refresh_rates.is_empty());
    }

    #[test]
    fn parse_with_rates() {
        let m = parse_mode("3840x2160@60/120").expect("valid");
        assert_eq!(m.refresh_rates, rates(&[60, 120]));
    }

    #[test]
    fn parse_dedups_and_sorts_rates() {
        // BTreeSet 去重 + 升序（原实现即 collect 进 BTreeSet）
        let m = parse_mode("800x600@120/60/60").expect("valid");
        assert_eq!(m.refresh_rates, rates(&[60, 120]));
    }

    #[test]
    fn parse_rejects_missing_x() {
        assert_eq!(
            parse_mode("1920"),
            Err(ParseError::NoResolution("1920".into()))
        );
    }

    #[test]
    fn parse_rejects_non_numeric_parts() {
        assert_eq!(
            parse_mode("ax1080"),
            Err(ParseError::BadWidth("ax1080".into()))
        );
        assert_eq!(
            parse_mode("1920xb"),
            Err(ParseError::BadHeight("1920xb".into()))
        );
        // 多余的 'x' 并进高度 → 高度解析失败（split_once 只切第一个）
        assert_eq!(
            parse_mode("1920x1080x60"),
            Err(ParseError::BadHeight("1920x1080x60".into()))
        );
    }

    #[test]
    fn parse_rejects_bad_rate() {
        assert_eq!(
            parse_mode("1920x1080@sixty"),
            Err(ParseError::BadRate("1920x1080@sixty".into()))
        );
        // 悬空的 '@' 也按无效刷新率处理（"" 解析不出数字）
        assert_eq!(
            parse_mode("1920x1080@"),
            Err(ParseError::BadRate("1920x1080@".into()))
        );
    }

    #[test]
    fn parse_error_messages_match_original_texts() {
        assert_eq!(
            ParseError::NoResolution("1920".into()).to_string(),
            "无效分辨率 \"1920\"，应为类似 \"1920x1080\" 的格式"
        );
        assert_eq!(
            ParseError::BadWidth("ax1080".into()).to_string(),
            "无效宽度 \"ax1080\"，应为数字"
        );
        assert_eq!(
            ParseError::BadHeight("1920xb".into()).to_string(),
            "无效高度 \"1920xb\"，应为数字"
        );
        assert_eq!(
            ParseError::BadRate("1920x1080@x".into()).to_string(),
            "无效刷新率 \"1920x1080@x\"，应为数字"
        );
    }

    // ---------- Display / 默认刷新率 ----------

    #[test]
    fn display_without_rates_has_no_at_sign() {
        let m = parse_mode("1920x1080").expect("valid");
        assert_eq!(m.to_string(), "1920x1080");
    }

    #[test]
    fn display_with_rates_sorted() {
        let m = parse_mode("3840x2160@120/60").expect("valid");
        assert_eq!(m.to_string(), "3840x2160@60/120");
    }

    #[test]
    fn parse_display_roundtrip() {
        for s in ["1920x1080", "3840x2160@60/120", "640x480@30"] {
            let parsed = parse_mode(s).expect("valid");
            assert_eq!(parsed.to_string(), s, "input: {s}");
        }
    }

    #[test]
    fn ensure_refresh_rate_fills_default_60() {
        let mut m = parse_mode("1920x1080").expect("valid");
        m.ensure_refresh_rate();
        assert_eq!(m.refresh_rates, rates(&[60]));

        // 已有刷新率时不变
        let mut m = parse_mode("1920x1080@120").expect("valid");
        m.ensure_refresh_rate();
        assert_eq!(m.refresh_rates, rates(&[120]));
    }

    // ---------- validate ----------

    #[test]
    fn validate_accepts_normal_mode() {
        assert_eq!(validate(1920, 1080, &[60]), Ok(()));
        // 下限边界：64x64 可过
        assert_eq!(validate(64, 64, &[1]), Ok(()));
    }

    #[test]
    fn validate_rejects_small_resolution() {
        assert_eq!(
            validate(63, 1080, &[60]),
            Err(ValidateError::TooSmall {
                width: 63,
                height: 1080,
                min: MIN_DIMENSION
            })
        );
        assert_eq!(
            validate(1920, 63, &[60]),
            Err(ValidateError::TooSmall {
                width: 1920,
                height: 63,
                min: MIN_DIMENSION
            })
        );
        assert_eq!(
            validate(63, 1080, &[60]).unwrap_err().to_string(),
            "分辨率过小：63x1080（最小 64x64）"
        );
    }

    #[test]
    fn validate_rejects_empty_rates() {
        assert_eq!(validate(1920, 1080, &[]), Err(ValidateError::NoRefreshRate));
        assert_eq!(
            validate(1920, 1080, &[]).unwrap_err().to_string(),
            "至少需要一个刷新率"
        );
    }

    #[test]
    fn validate_rejects_zero_rate() {
        assert_eq!(
            validate(1920, 1080, &[60, 0]),
            Err(ValidateError::ZeroRate(0))
        );
        assert_eq!(
            validate(1920, 1080, &[0]).unwrap_err().to_string(),
            "刷新率不能为 0"
        );
    }
}
