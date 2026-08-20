//! 虚拟摄像头帧核心：RGB24 测试图案生成。

/// 一帧 RGB24 图像。
#[derive(Debug, Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// 可用测试图案。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePattern {
    /// SMPTE 彩条（7 根竖条）
    SmpteBars = 0,
    /// 随时间滚动的渐变
    Gradient = 1,
    /// 滚动的棋盘格（带时间动画）
    Checker = 2,
}

impl FramePattern {
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::SmpteBars),
            1 => Some(Self::Gradient),
            2 => Some(Self::Checker),
            _ => None,
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "smpte" | "bars" => Some(Self::SmpteBars),
            "gradient" => Some(Self::Gradient),
            "checker" => Some(Self::Checker),
            _ => None,
        }
    }

    pub const fn name(self) -> &'static str {
        match self {
            Self::SmpteBars => "smpte",
            Self::Gradient => "gradient",
            Self::Checker => "checker",
        }
    }
}

fn set_px(data: &mut [u8], w: u32, x: u32, y: u32, rgb: (u8, u8, u8)) {
    let i = ((y * w + x) * 3) as usize;
    data[i] = rgb.0;
    data[i + 1] = rgb.1;
    data[i + 2] = rgb.2;
}

/// 渲染一帧。
pub fn render(pattern: FramePattern, width: u32, height: u32, t: f64) -> Frame {
    let mut data = vec![0u8; (width * height * 3) as usize];
    match pattern {
        FramePattern::SmpteBars => {
            // 75% SMPTE 彩条：白 黄 青 绿 品红 红 蓝
            const COLORS: [(u8, u8, u8); 7] = [
                (191, 191, 191),
                (191, 191, 0),
                (0, 191, 191),
                (0, 191, 0),
                (191, 0, 191),
                (191, 0, 0),
                (0, 0, 191),
            ];
            for y in 0..height {
                for x in 0..width {
                    let bar = ((x * 7 / width) as usize).min(6);
                    let rgb = if y >= height * 9 / 10 {
                        // 底部 10% 反相，模拟 SMPTE 下半部分
                        let c = COLORS[bar];
                        (255 - c.0, 255 - c.1, 255 - c.2)
                    } else {
                        COLORS[bar]
                    };
                    set_px(&mut data, width, x, y, rgb);
                }
            }
        }
        FramePattern::Gradient => {
            let shift = ((t * 60.0) as u32) % width;
            for y in 0..height {
                for x in 0..width {
                    let sx = (x + shift) % width;
                    let r = (sx * 255 / width) as u8;
                    let g = (y * 255 / height) as u8;
                    let b = ((sx + y) * 255 / (width + height)) as u8;
                    set_px(&mut data, width, x, y, (r, g, b));
                }
            }
        }
        FramePattern::Checker => {
            let cell = 32;
            let shift = ((t * 60.0) as u32) % cell;
            for y in 0..height {
                for x in 0..width {
                    let sx = x + shift;
                    let on = ((sx / cell) % 2) ^ ((y / cell) % 2);
                    let rgb = if on == 1 {
                        (255, 255, 255)
                    } else {
                        (16, 16, 16)
                    };
                    set_px(&mut data, width, x, y, rgb);
                }
            }
        }
    }
    Frame { width, height, data }
}

/// 写出 PPM（P6）文件，方便快速肉眼检查。
pub fn write_ppm(path: &std::path::Path, frame: &Frame) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    write!(
        f,
        "P6\n{} {}\n255\n",
        frame.width, frame.height
    )?;
    f.write_all(&frame.data)?;
    Ok(())
}
