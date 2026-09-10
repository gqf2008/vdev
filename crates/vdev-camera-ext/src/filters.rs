//! 设备侧滤镜：虚拟摄像头自己对本机帧做美颜 / 背景替换。
//!
//! 这段原先跑在推流方（旧的 vdev-bridge，已迁往 aerodesk 侧）。归到设备侧的理由：
//! 滤镜是虚拟摄像头**自身成像**的一部分——任何推帧方（宿主 App、外部桥、
//! 第三方）都能拿到同一套处理，推流方不必各自再装一份滤镜依赖。
//!
//! 配置沿用旧 bridge 的环境变量名，迁移无感：
//! ```text
//! VDEV_FILTER="brightness,contrast,saturation,green,sharpen,beauty,whiten"
//! VDEV_BG=blur|1        开启背景模糊（Vision 人像分割）
//! ```
//! 未配置任何滤镜时整段跳过，保持原来的直通路径（零额外开销）。

use std::sync::OnceLock;

use vdev_filter::{process_frame, FilterParams};

struct Config {
    params: FilterParams,
    /// VDEV_FILTER 里给过参数（哪怕全是默认值）
    params_active: bool,
    /// VDEV_BG 开了背景模糊
    bg_blur: bool,
}

fn config() -> &'static Config {
    static CFG: OnceLock<Config> = OnceLock::new();
    CFG.get_or_init(|| {
        let mut params = FilterParams::default();
        let mut params_active = false;
        if let Ok(v) = std::env::var("VDEV_FILTER") {
            let parts: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if parts.len() >= 1 {
                params.brightness = parts[0];
                params_active = true;
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
        let bg = std::env::var("VDEV_BG").unwrap_or_default();
        let bg_blur = bg == "blur" || bg == "1";
        Config {
            params,
            params_active,
            bg_blur,
        }
    })
}

/// 是否配置了任何滤镜；false 时调用方应走原来的直通路径。
pub fn enabled() -> bool {
    let c = config();
    c.params_active || c.bg_blur
}

/// 是否启用了任何滤镜及模式（用于启动日志）。
pub fn describe() -> Option<&'static str> {
    let c = config();
    match (c.params_active, c.bg_blur) {
        (true, true) => Some("参数滤镜 + 背景模糊"),
        (true, false) => Some("参数滤镜"),
        (false, true) => Some("背景模糊"),
        (false, false) => None,
    }
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
    fn repack_aligned_is_identity() {
        let (w, h) = (2u32, 2u32);
        let src: Vec<u8> = (0..(w * h * 4) as u8).collect();
        assert_eq!(repack(&src, w, h, w * 4), src);
    }
}
