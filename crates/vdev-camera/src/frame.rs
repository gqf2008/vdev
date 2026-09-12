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
    #[must_use]
    pub fn from_i32(v: i32) -> Option<Self> {
        match v {
            0 => Some(Self::SmpteBars),
            1 => Some(Self::Gradient),
            2 => Some(Self::Checker),
            _ => None,
        }
    }

    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "smpte" | "bars" => Some(Self::SmpteBars),
            "gradient" => Some(Self::Gradient),
            "checker" => Some(Self::Checker),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::SmpteBars => "smpte",
            Self::Gradient => "gradient",
            Self::Checker => "checker",
        }
    }
}

/// 单帧像素总数上限：8192×8192。超过该值的尺寸视为非法输入，拒绝渲染
/// （防 u32 域尺寸乘法溢出/回绕导致 debug panic 或 release 越界写）。
pub const MAX_PIXELS: u64 = 8192 * 8192;

/// 尺寸合法性判定（`render` 与 C ABI 出口共用同一规则）：宽高均非零，
/// 且像素总数不超过 [`MAX_PIXELS`]。比较在 u64 域进行（u32×u32 必然不溢出），
/// 32/64 位平台判定一致。
#[must_use]
pub fn dims_valid(width: u32, height: u32) -> bool {
    width != 0 && height != 0 && u64::from(width) * u64::from(height) <= MAX_PIXELS
}

/// 写入 (x,y) 处像素；索引全程 usize 域计算，不回绕。
fn set_px(data: &mut [u8], w: usize, x: usize, y: usize, rgb: (u8, u8, u8)) {
    let i = (y * w + x) * 3;
    data[i] = rgb.0;
    data[i + 1] = rgb.1;
    data[i + 2] = rgb.2;
}

/// 渲染一帧。
///
/// # 尺寸前置条件
///
/// 宽高须通过 [`dims_valid`]：非零且像素总数 ≤ [`MAX_PIXELS`]。非法尺寸
/// 返回空 Frame（0×0、空数据）而不 panic；返回值恒满足
/// `data.len() == width as usize * height as usize * 3`。
// 颜色换算（如 sx<width 时 sx*255/width<256）数学上必然落在 u8 域内，
// f64 相位经饱和截断即可；尺寸算术第一操作数即转 usize 域，杜绝 u32 乘法回绕。
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
#[must_use]
pub fn render(pattern: FramePattern, width: u32, height: u32, t: f64) -> Frame {
    if !dims_valid(width, height) {
        return Frame {
            width: 0,
            height: 0,
            data: Vec::new(),
        };
    }
    let width = width as usize;
    let height = height as usize;
    let mut data = vec![0u8; width * height * 3];
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
                    let bar = (x * 7 / width).min(6);
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
            // width>0 由 dims_valid 保证，取模除零不可达；相位 u32→usize 无损
            let shift = ((t * 60.0) as u32) as usize % width;
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
            let cell: usize = 32;
            let shift = ((t * 60.0) as u32) as usize % cell;
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
    // dims_valid 已保证合法尺寸每维 ≤ u32::MAX，usize 回转 u32 无损
    Frame {
        width: width as u32,
        height: height as u32,
        data,
    }
}

/// 写出 PPM（P6）文件，方便快速肉眼检查。
pub fn write_ppm(path: &std::path::Path, frame: &Frame) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    write!(f, "P6\n{} {}\n255\n", frame.width, frame.height)?;
    f.write_all(&frame.data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SMPTE 首像素锚定：75% 白条 (191,191,191)（与 cabi 出口测试同源）。
    #[test]
    fn smpte_first_pixel_is_75_percent_white() {
        let f = render(FramePattern::SmpteBars, 7, 10, 0.0);
        assert_eq!(f.width, 7);
        assert_eq!(f.height, 10);
        assert_eq!(f.data.len(), 7 * 10 * 3);
        assert_eq!(&f.data[0..3], &[191, 191, 191]);
    }

    /// gradient 四角取值（t=0 无位移）：比例换算锚定，防算术域迁移语义漂移。
    #[test]
    fn gradient_corners() {
        let f = render(FramePattern::Gradient, 4, 4, 0.0);
        assert_eq!(f.data.len(), 4 * 4 * 3);
        let px = |x: usize, y: usize| &f.data[(y * 4 + x) * 3..(y * 4 + x) * 3 + 3];
        assert_eq!(px(0, 0), &[0, 0, 0]);
        assert_eq!(px(3, 0), &[191, 0, 95]); // r=3*255/4, b=3*255/8
        assert_eq!(px(0, 3), &[0, 191, 95]); // g=3*255/4
        assert_eq!(px(3, 3), &[191, 191, 191]); // r=g=3*255/4, b=6*255/8
    }

    /// checker 奇偶锚定：32px 棋盘，(0,0) 暗、(32,0)/(0,32) 亮、(32,32) 暗。
    #[test]
    fn checker_parity() {
        let f = render(FramePattern::Checker, 64, 64, 0.0);
        assert_eq!(f.data.len(), 64 * 64 * 3);
        let px = |x: usize, y: usize| &f.data[(y * 64 + x) * 3..(y * 64 + x) * 3 + 3];
        assert_eq!(px(0, 0), &[16, 16, 16]); // 偶×偶 → 暗
        assert_eq!(px(31, 31), &[16, 16, 16]); // 同格内仍暗
        assert_eq!(px(32, 0), &[255, 255, 255]); // 列格切换 → 亮
        assert_eq!(px(0, 32), &[255, 255, 255]); // 行格切换 → 亮
        assert_eq!(px(32, 32), &[16, 16, 16]); // 双切换 → 偶 → 暗
    }

    /// 尺寸判定边界：上限内合法（恰 8192×8192），超上限/零尺寸非法；
    /// u64 域比较保证 `u32::MAX` 级输入在 32 位平台同样不回绕。
    #[test]
    fn dims_valid_boundary() {
        assert!(dims_valid(1, 1));
        assert!(dims_valid(8192, 8192));
        assert!(!dims_valid(8192, 8193));
        assert!(!dims_valid(0, 5));
        assert!(!dims_valid(5, 0));
        assert!(!dims_valid(u32::MAX, 1));
        assert!(!dims_valid(u32::MAX, u32::MAX));
    }

    /// 极端长条（65536×1，像素总数在上限内）完整渲染：末像素比例换算
    /// r=b=65535*255/65536≈254.99→254、g=0，防超宽输入算术溢出复发。
    #[test]
    fn extreme_aspect_renders_fully() {
        let f = render(FramePattern::Gradient, 65_536, 1, 0.0);
        assert_eq!(f.data.len(), 65_536 * 3);
        assert_eq!(&f.data[0..3], &[0, 0, 0]);
        assert_eq!(&f.data[(65_536 - 1) * 3..], &[254, 0, 254]);
    }

    /// 非法尺寸不得 panic、返回空帧：零尺寸与超大 width*height（含 u32 域
    /// 乘法回绕输入 70000×70000 与 `u32::MAX`）。回绕类放最前，便于在未修复
    /// 代码上直接复现溢出 panic。
    #[test]
    fn invalid_dims_return_empty_frame_without_panic() {
        for (w, h) in [
            (70_000, 70_000),
            (u32::MAX, 1),
            (u32::MAX, u32::MAX),
            (0, 10),
            (10, 0),
            (0, 0),
        ] {
            for p in [
                FramePattern::SmpteBars,
                FramePattern::Gradient,
                FramePattern::Checker,
            ] {
                let f = render(p, w, h, 1.0);
                assert_eq!(
                    (f.width, f.height, f.data.len()),
                    (0, 0, 0),
                    "w={w} h={h} p={p:?}"
                );
            }
        }
    }
}
