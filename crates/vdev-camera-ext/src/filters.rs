//! 设备侧滤镜：虚拟摄像头自己对本机帧做美颜 / 背景替换。
//!
//! 这段原先跑在推流方（旧的 vdev-bridge，已迁往 aerodesk 侧）。归到设备侧的理由：
//! 滤镜是虚拟摄像头**自身成像**的一部分——任何推帧方（宿主 App、外部桥、
//! 第三方）都能拿到同一套处理，推流方不必各自再装一份滤镜依赖。
//!
//! 配置沿用旧 bridge 的环境变量名：
//! ```text
//! VDEV_FILTER="brightness,contrast,saturation,green,sharpen,beauty,whiten"
//! VDEV_BG=blur|1        开启背景模糊（Vision 人像分割）
//! ```
//!
//! **但环境变量到不了扩展进程**：扩展由 cmiod 以 `_cmiodalassistants` 身份拉起，
//! 既不是用户 shell 的子进程，也不在用户 launchd 域里（实测 `ps eww -p <ext>`
//! 里没有任何 `VDEV_*`；用户域 `launchctl setenv` 不生效，系统域要 root）。
//! 所以真机实际可用的配置方式是**扩展自己读配置文件**，见 `config_file_paths`：
//!
//! ```text
//! # ~/Library/Group Containers/XFXU84HVK3.com.vdev.camera/vdev-camera-filter.conf
//! VDEV_FILTER=0.3,1,1,0,0,0,0
//! VDEV_BG=blur
//! ```
//!
//! 优先级：环境变量（将来若能注入则零改动生效）→ 配置文件。未配置任何滤镜时
//! 整段跳过，保持原来的直通路径（零额外开销）。

use std::sync::{Arc, RwLock};

/// 配置文件文件名（放在用户可写、扩展可读的共享位置，见 `config_file_paths`）。
const CONFIG_FILE_NAME: &str = "vdev-camera-filter.conf";

use vdev_filter::{process_frame, FilterParams};

struct Config {
    params: FilterParams,
    /// `VDEV_FILTER` 里给过参数（哪怕全是默认值）
    params_active: bool,
    /// `VDEV_BG` 开了背景模糊
    bg_blur: bool,
    /// 配置来源（`env` / `file=<path>` / `未配置`），用于启动日志取证
    source: String,
}

/// 配置文件里认的键（与旧 bridge 的环境变量同名，迁移无感）。
const KEY_FILTER: &str = "VDEV_FILTER";
const KEY_BG: &str = "VDEV_BG";

/// 配置文件候选路径，按优先级排列；第一个能读出 `KEY=VALUE` 的生效。
///
/// 1. `$HOME/Library/Group Containers/<TEAM>.<BUNDLE>/`：App Group 共享容器，
///    宿主 App 也在这里，是"正规"的共享位置；
/// 2. `$HOME/`：沙盒里 HOME 是容器 Data 目录，两个位置都试，避免受沙盒差异影响；
/// 3. `/tmp`、`/var/tmp`：兜底（扩展的日志落盘也是同样兜底）。
fn config_file_paths() -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    // 沙盒里 `NSHomeDirectory()` 给的是容器 Data 目录，**比环境变量 HOME 可靠**
    // （扩展是 _cmiodalassistants 身份拉起的守护进程，HOME 可能压根没设）。
    let ns_home = objc2_foundation::NSHomeDirectory().to_string();
    if !ns_home.is_empty() {
        roots.push(ns_home);
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && !roots.contains(&home) {
            roots.push(home);
        }
    }
    if let Ok(tmp) = std::env::var("TMPDIR") {
        if !tmp.is_empty() {
            roots.push(tmp.trim_end_matches('/').to_string());
        }
    }
    let mut paths = Vec::new();
    for root in &roots {
        paths.push(format!(
            "{root}/Library/Group Containers/XFXU84HVK3.com.vdev.camera/{CONFIG_FILE_NAME}"
        ));
        paths.push(format!("{root}/{CONFIG_FILE_NAME}"));
    }
    // 兜底：非沙盒进程（如本地单测/宿主）常读得到这两个
    paths.push(format!("/tmp/{CONFIG_FILE_NAME}"));
    paths.push(format!("/var/tmp/{CONFIG_FILE_NAME}"));
    paths
}

/// 配置探测详情（启动日志用）：把候选路径与"哪个读到了"打出来，
/// 便于真机上确认沙盒到底放行哪个位置。
pub fn probe_report() -> String {
    let paths = config_file_paths();
    let hit = paths
        .iter()
        .find(|p| std::fs::read_to_string(p).is_ok())
        .cloned()
        .unwrap_or_else(|| "无可读候选".to_string());
    format!(
        "home={} HOME={:?} TMPDIR={:?} 命中={hit}",
        objc2_foundation::NSHomeDirectory(),
        std::env::var("HOME").unwrap_or_default(),
        std::env::var("TMPDIR").unwrap_or_default()
    )
}

/// 解析配置文件正文：`KEY=VALUE` 逐行，`#` 注释与空行忽略，未知键忽略，
/// 值两侧的空白与成对引号去掉；同名键后来的覆盖先前的。纯函数，便于单测。
fn parse_config(text: &str) -> (Option<String>, Option<String>) {
    let mut filter = None;
    let mut bg = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            KEY_FILTER => filter = Some(value),
            KEY_BG => bg = Some(value),
            _ => {}
        }
    }
    (filter, bg)
}

/// 按候选顺序读第一个可用的配置文件；返回 `(值, 来源路径)`。
fn read_file_config() -> Option<(Option<String>, Option<String>, String)> {
    for path in config_file_paths() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (filter, bg) = parse_config(&text);
        if filter.is_some() || bg.is_some() {
            return Some((filter, bg, path));
        }
    }
    None
}

/// 运行时配置：可以被 TCP 控制通道热更新（见 `apply_config`），所以不是 `OnceLock`。
static CFG: RwLock<Option<Arc<Config>>> = RwLock::new(None);

/// 取当前配置快照（`Arc` 克隆，60Hz 调用无分配）。
fn config() -> Arc<Config> {
    {
        let guard = CFG
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(cfg) = guard.as_ref() {
            return Arc::clone(cfg);
        }
    }
    let mut guard = CFG
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let cfg = guard.get_or_insert_with(|| Arc::new(initial_config()));
    Arc::clone(cfg)
}

/// 首次使用时的配置：环境变量优先，其次配置文件（真机上两者通常都拿不到，
/// 真正的通道是 `apply_config` 的 TCP 控制消息，见模块头注释）。
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn initial_config() -> Config {
    {
        // env 优先（将来若真能把变量注进扩展进程，这条零改动生效），否则读文件
        let env_filter = std::env::var(KEY_FILTER).ok();
        let env_bg = std::env::var(KEY_BG).ok();
        let (raw_filter, raw_bg, source) = if env_filter.is_some() || env_bg.is_some() {
            (env_filter, env_bg, "env".to_string())
        } else if let Some((f, b, path)) = read_file_config() {
            (f, b, format!("file={path}"))
        } else {
            (None, None, "未配置".to_string())
        };
        let mut params = FilterParams::default();
        let mut params_active = false;
        if let Some(v) = raw_filter.as_deref() {
            apply_params(v, &mut params, &mut params_active);
        }
        let bg = raw_bg.unwrap_or_default();
        let bg_blur = bg == "blur" || bg == "1";
        Config {
            params,
            params_active,
            bg_blur,
            source,
        }
    }
}

/// 用一段配置文本热更新设备侧滤镜（TCP 控制通道的落地函数）。
///
/// 文本格式与配置文件一致：`VDEV_FILTER=...` / `VDEV_BG=...` 逐行。
/// 返回人类可读的新配置摘要；两侧键都没有时返回 Err（不改动当前配置）。
pub fn apply_config(text: &str) -> Result<String, String> {
    let (filter, bg) = parse_config(text);
    if filter.is_none() && bg.is_none() {
        return Err("配置里没有 VDEV_FILTER / VDEV_BG 行".to_string());
    }
    let mut params = FilterParams::default();
    let mut params_active = false;
    if let Some(v) = filter.as_deref() {
        apply_params(v, &mut params, &mut params_active);
    }
    let bg_value = bg.unwrap_or_default();
    let bg_blur = bg_value == "blur" || bg_value == "1";
    let cfg = Config {
        params,
        params_active,
        bg_blur,
        source: "tcp 控制通道".to_string(),
    };
    let summary = summary_of(&cfg).unwrap_or_else(|| "全部关闭（直通）".to_string());
    *CFG.write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(cfg));
    Ok(summary)
}

/// 解析 `brightness,contrast,...` 参数串（越界值按各自域 clamp）。
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn apply_params(v: &str, params: &mut FilterParams, params_active: &mut bool) {
    let parts: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    if !parts.is_empty() {
        params.brightness = parts[0];
        *params_active = true;
    }
    if parts.len() >= 2 {
        params.contrast = parts[1];
    }
    if parts.len() >= 3 {
        params.saturation = parts[2];
    }
    if parts.len() >= 4 {
        params.green_screen_threshold = parts[3].clamp(0.0, 255.0) as u8;
    }
    if parts.len() >= 5 {
        params.sharpen = parts[4];
    }
    if parts.len() >= 6 {
        params.beauty_strength = parts[5].clamp(0.0, 1.0);
    }
    if parts.len() >= 7 {
        params.whiten_strength = parts[6].clamp(0.0, 1.0);
    }
}

/// 是否配置了任何滤镜；false 时调用方应走原来的直通路径。
pub fn enabled() -> bool {
    let c = config();
    c.params_active || c.bg_blur
}

/// 配置摘要（未启用任何滤镜时返回 None）。
fn summary_of(c: &Config) -> Option<String> {
    let mode = match (c.params_active, c.bg_blur) {
        (true, true) => "参数滤镜 + 背景模糊",
        (true, false) => "参数滤镜",
        (false, true) => "背景模糊",
        (false, false) => return None,
    };
    Some(format!("{mode}，来源={}", c.source))
}

/// 是否启用了任何滤镜及模式 + 配置来源（用于启动日志与真机验收取证）。
pub fn describe() -> Option<String> {
    summary_of(&config())
}

/// 配置来源描述（未启用滤镜时也要打得出来，供"未配置=直通"取证）。
pub fn source() -> String {
    config().source.clone()
}

/// 把可能带 stride 填充的 BGRA 行打成紧凑 `w*h*4`。
///
/// 推帧方并不保证 stride == w*4（旧 bridge 是紧凑的，但协议允许填充）；
/// 滤镜按紧凑行宽逐像素处理，所以填充时必须先重排。
pub fn repack(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (w, h, st) = (width as usize, height as usize, stride as usize);
    let row = w * 4;
    if st == row {
        return src.to_vec();
    }
    let mut out = vec![0u8; row * h];
    for y in 0..h {
        let s = y * st;
        if s + row <= src.len() {
            out[y * row..(y + 1) * row].copy_from_slice(&src[s..s + row]);
        }
    }
    out
}

/// 对紧凑 BGRA 帧原地应用设备侧滤镜（含背景替换）。
pub fn apply(bgra: &mut [u8], width: u32, height: u32) {
    let c = config();
    if c.params_active {
        process_frame(bgra, width, height, &c.params);
    }
    if c.bg_blur {
        vdev_filter::vision::segment_and_replace(bgra, width, height, None, 8);
    }
}

#[cfg(test)]
// 测试数据取值远小于 u8 上限，截断不可能发生
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;

    #[test]
    fn repack_padded_stride() {
        let (w, h, stride) = (3u32, 2u32, 16u32);
        let mut src = vec![0xAAu8; stride as usize * h as usize];
        for y in 0..h as usize {
            for x in 0..w as usize {
                src[y * stride as usize + x * 4] = (y * 10 + x) as u8;
            }
        }
        let out = repack(&src, w, h, stride);
        assert_eq!(out.len(), (w * h * 4) as usize);
        for y in 0..h as usize {
            for x in 0..w as usize {
                assert_eq!(out[(y * w as usize + x) * 4], (y * 10 + x) as u8);
            }
        }
    }

    #[test]
    fn parse_config_reads_known_keys_ignores_noise() {
        // 典型配置正文：注释、空行、未知键、引号、重复键都要能处理
        let text = "# vdev 设备侧滤镜\n\nVDEV_FILTER=0.3,1,1,0,0,0,0\nUNKNOWN=1\nVDEV_BG=\"blur\"\nVDEV_FILTER=0.5,1,1,0,0,0,0\n";
        let (f, b) = parse_config(text);
        // 重复键后者覆盖前者（避免"改了没生效"的静默坑）
        assert_eq!(f.as_deref(), Some("0.5,1,1,0,0,0,0"));
        assert_eq!(b.as_deref(), Some("blur"));
    }

    #[test]
    fn parse_config_ignores_malformed_lines_without_panicking() {
        // 半截配置（没有 '='）、无值、只有注释 → 全部忽略，不 panic、不误判
        let (f, b) = parse_config("VDEV_FILTER\n=\nVDEV_BG\n# VDEV_BG=blur\n");
        assert_eq!(f, None);
        assert_eq!(b, None);
    }

    #[test]
    fn parse_config_treats_empty_value_as_present_but_inert() {
        // `VDEV_FILTER=` 是"给过键但没参数"：解析出空串，调用方按无参数处理（直通）
        let (f, b) = parse_config("VDEV_FILTER=\nVDEV_BG=1\n");
        assert_eq!(f.as_deref(), Some(""));
        assert_eq!(b.as_deref(), Some("1"));
    }

    #[test]
    fn config_file_paths_cover_app_group_and_tmp_fallbacks() {
        // 路径必须包含 App Group 共享容器（宿主也能写）与 /tmp 兜底；
        // 顺序上 App Group 在前（正规位置优先）。
        let paths = config_file_paths();
        let group = paths
            .iter()
            .position(|p| p.contains("Group Containers/XFXU84HVK3.com.vdev.camera"))
            .expect("App Group 共享容器路径缺失");
        let tmp = paths
            .iter()
            .position(|p| p.starts_with("/tmp/"))
            .expect("/tmp 兜底路径缺失");
        assert!(group < tmp, "App Group 应优先于 /tmp：{paths:?}");
        assert!(paths.iter().all(|p| p.ends_with(CONFIG_FILE_NAME)));
    }

    #[test]
    fn repack_aligned_is_identity() {
        let (w, h) = (2u32, 2u32);
        let src: Vec<u8> = (0..(w * h * 4) as u8).collect();
        assert_eq!(repack(&src, w, h, w * 4), src);
    }
}
