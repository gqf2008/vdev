//! vdev-filter：BGRA32 实时图像滤镜管线。
//! 供「远端 WebRTC 流 → 本地虚拟摄像头」链路使用：解码后的 BGRA 帧
//! 经过可插拔滤镜链，再推给 FrameChannel → 虚拟摄像头。
//! 设计目标：无堆分配、纯整数/浮点逐像素，实时帧处理友好。

/// 一个 8-bit 像素（BGRA 字节序：b,g,r,a）
#[derive(Clone, Copy, Debug, Default)]
pub struct Pixel {
    pub b: u8,
    pub g: u8,
    pub r: u8,
    pub a: u8,
}

impl Pixel {
    pub fn from_slice(s: &[u8]) -> Self {
        Self { b: s[0], g: s[1], r: s[2], a: s[3] }
    }
    pub fn to_slice(self, s: &mut [u8]) {
        s[0] = self.b; s[1] = self.g; s[2] = self.r; s[3] = self.a;
    }
}

/// 滤镜参数集合（可叠加）
#[derive(Clone, Copy, Debug)]
pub struct FilterParams {
    /// 亮度：-1.0..=1.0，0 = 不变
    pub brightness: f32,
    /// 对比度：0.0..=2.0，1 = 不变
    pub contrast: f32,
    /// 饱和度：0.0..=2.0，1 = 不变（0 = 灰度）
    pub saturation: f32,
    /// 绿幕抠像阈值：0..=255，0 = 关闭；|g - max(r,b)| > threshold 则抠成透明
    pub green_screen_threshold: u8,
    /// 锐化强度：0.0..=2.0，0 = 不变
    pub sharpen: f32,
    /// 磨皮强度：0.0..=1.0，0 = 关闭（双边滤波，保留边缘平滑皮肤）
    pub beauty_strength: f32,
    /// 美白强度：0.0..=1.0，0 = 关闭（提亮 + 轻微降饱和）
    pub whiten_strength: f32,
}

impl Default for FilterParams {
    fn default() -> Self {
        Self {
            brightness: 0.0,
            contrast: 1.0,
            saturation: 1.0,
            green_screen_threshold: 0,
            sharpen: 0.0,
            beauty_strength: 0.0,
            whiten_strength: 0.0,
        }
    }
}

/// 亮度/对比度/饱和度（BCS）单像素处理
#[inline]
fn bcs(p: Pixel, params: &FilterParams) -> Pixel {
    let f = |c: u8| -> u8 {
        let mut v = c as f32 / 255.0;
        // 亮度（加性）
        v += params.brightness;
        // 对比度（围绕 0.5 缩放）
        v = (v - 0.5) * params.contrast + 0.5;
        (v.clamp(0.0, 1.0) * 255.0).round() as u8
    };
    let mut r = f(p.r);
    let mut g = f(p.g);
    let mut b = f(p.b);
    // 饱和度：先转亮度，再线性插值
    if params.saturation != 1.0 {
        let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
        let s = params.saturation;
        r = ((r as f32 - luma) * s + luma).clamp(0.0, 255.0) as u8;
        g = ((g as f32 - luma) * s + luma).clamp(0.0, 255.0) as u8;
        b = ((b as f32 - luma) * s + luma).clamp(0.0, 255.0) as u8;
    }
    Pixel { r, g, b, a: p.a }
}

/// 绿幕抠像：绿色明显高于红蓝时置透明
#[inline]
fn green_screen(p: Pixel, threshold: u8) -> Pixel {
    if threshold == 0 {
        return p;
    }
    let g = p.g as i32;
    let r = p.r as i32;
    let b = p.b as i32;
    if g - r.max(b) > threshold as i32 {
        Pixel { b: 0, g: 0, r: 0, a: 0 }
    } else {
        p
    }
}

/// 对一整帧 BGRA 应用滤镜（原地处理，无分配）。
pub fn process_frame(bgra: &mut [u8], width: u32, height: u32, params: &FilterParams) {
    let px = (width * height) as usize;
    let n = px * 4;
    assert!(bgra.len() >= n, "buffer 太小");
    // 先做逐像素 BCS + 绿幕
    for i in 0..px {
        let o = i * 4;
        let p = Pixel::from_slice(&bgra[o..o + 4]);
        let p = bcs(p, params);
        let p = green_screen(p, params.green_screen_threshold);
        p.to_slice(&mut bgra[o..o + 4]);
    }
    // 锐化：3x3 拉普拉斯（需要拷贝一份原图，避免原地污染）
    if params.sharpen > 0.0 {
        sharpen_in_place(bgra, width, height, params.sharpen);
    }
    // 磨皮（双边滤波，保留边缘平滑皮肤）
    if params.beauty_strength > 0.0 {
        bilateral_smooth(bgra, width, height, params.beauty_strength);
    }
    // 美白（提亮 + 降饱和）
    if params.whiten_strength > 0.0 {
        whiten(bgra, width, height, params.whiten_strength);
    }
}

/// 3x3 拉普拉斯锐化（原地，先拷贝输入）
/// 双边滤波磨皮：空间 + 值域高斯加权，平滑平坦区域同时保留边缘。
/// strength 控制平滑半径与值域宽容度（0..=1）。
fn bilateral_smooth(bgra: &mut [u8], width: u32, height: u32, strength: f32) {
    let w = width as usize;
    let h = height as usize;
    let src = bgra[..w * h * 4].to_vec();
    // 半径随强度 1..=3，值域 sigma 随强度放宽（更白更平滑）
    let r: i32 = 1 + (strength * 2.0) as i32; // 1..=3
    let sigma_spatial = 1.5;
    let sigma_range = 30.0 + strength * 40.0; // 30..=70
    let range_factor = -0.5 / (sigma_range * sigma_range);
    // 预计算空间权重（与值无关）
    let mut spatial = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
    for dy in -r..=r {
        for dx in -r..=r {
            let d2 = (dx * dx + dy * dy) as f32;
            spatial.push((-0.5 * d2 / (sigma_spatial * sigma_spatial)).exp());
        }
    }
    for y in 0..h {
        for x in 0..w {
            let c = y * w + x;
            for ch in 0..3 {
                let center = src[c * 4 + ch] as f32;
                let mut sum = 0.0f32;
                let mut wsum = 0.0f32;
                let mut si = 0usize;
                for dy in -r..=r {
                    let ny = y as i32 + dy;
                    if ny < 0 || ny >= h as i32 { si += (2 * r + 1) as usize; continue; }
                    for dx in -r..=r {
                        let nx = x as i32 + dx;
                        let ws = spatial[si];
                        si += 1;
                        if nx < 0 || nx >= w as i32 { continue; }
                        let ni = (ny as usize) * w + nx as usize;
                        let diff = src[ni * 4 + ch] as f32 - center;
                        let wgt = ws * (diff * diff * range_factor).exp();
                        sum += wgt * src[ni * 4 + ch] as f32;
                        wsum += wgt;
                    }
                }
                let v = if wsum > 0.0 { sum / wsum } else { center };
                bgra[c * 4 + ch] = v.clamp(0.0, 255.0) as u8;
            }
        }
    }
}

/// 美白：提亮 + 轻微降低饱和度（肤色更白皙）。
fn whiten(bgra: &mut [u8], width: u32, height: u32, strength: f32) {
    let px = (width * height) as usize;
    for i in 0..px {
        let o = i * 4;
        let b = bgra[o] as f32;
        let g = bgra[o + 1] as f32;
        let r = bgra[o + 2] as f32;
        // 提亮（越暗提得越多）
        let lift = strength * 18.0;
        let mut b2 = b + lift;
        let mut g2 = g + lift;
        let mut r2 = r + lift;
        // 降饱和（向亮度靠拢，程度随强度）
        if strength > 0.0 {
            let luma = 0.299 * r2 + 0.587 * g2 + 0.114 * b2;
            let s = 1.0 - strength * 0.15;
            b2 = (b2 - luma) * s + luma;
            g2 = (g2 - luma) * s + luma;
            r2 = (r2 - luma) * s + luma;
        }
        bgra[o] = b2.clamp(0.0, 255.0) as u8;
        bgra[o + 1] = g2.clamp(0.0, 255.0) as u8;
        bgra[o + 2] = r2.clamp(0.0, 255.0) as u8;
    }
}

fn sharpen_in_place(bgra: &mut [u8], width: u32, height: u32, strength: f32) {
    let w = width as usize;
    let h = height as usize;
    let src = bgra[..w * h * 4].to_vec();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let c = y * w + x;
            for ch in 0..3 {
                let o = c * 4 + ch;
                let center = src[o] as f32;
                let lap = 4.0 * center
                    - src[o - w * 4] as f32
                    - src[o + w * 4] as f32
                    - src[o - 4] as f32
                    - src[o + 4] as f32;
                let v = center + strength * lap;
                bgra[o] = v.clamp(0.0, 255.0) as u8;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(color: Pixel, w: u32, h: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h {
            v.push(color.b); v.push(color.g); v.push(color.r); v.push(color.a);
        }
        v
    }

    #[test]
    fn test_identity() {
        let mut f = frame(Pixel { b: 10, g: 100, r: 200, a: 255 }, 4, 4);
        let before = f.clone();
        process_frame(&mut f, 4, 4, &FilterParams::default());
        assert_eq!(f, before, "默认参数应直通");
    }

    #[test]
    fn test_brightness_up() {
        let mut f = frame(Pixel { b: 100, g: 100, r: 100, a: 255 }, 4, 4);
        process_frame(&mut f, 4, 4, &FilterParams { brightness: 0.5, ..Default::default() });
        let p = Pixel::from_slice(&f[0..4]);
        assert!(p.r > 100 && p.g > 100 && p.b > 100, "亮度应提升，实际 r={}", p.r);
    }

    #[test]
    fn test_saturation_zero_grayscale() {
        let mut f = frame(Pixel { b: 50, g: 150, r: 250, a: 255 }, 4, 4);
        process_frame(&mut f, 4, 4, &FilterParams { saturation: 0.0, ..Default::default() });
        let p = Pixel::from_slice(&f[0..4]);
        assert!((p.r as i32 - p.g as i32).abs() < 2 && (p.g as i32 - p.b as i32).abs() < 2,
            "灰度化后 RGB 应接近，实际 r={} g={} b={}", p.r, p.g, p.b);
    }

    #[test]
    fn test_beauty_smooth_reduces_noise() {
        // 平坦灰底 + 随机噪点，磨皮后噪点方差应显著降低
        let w = 16; let h = 16;
        let mut f = vec![0u8; (w * h * 4) as usize];
        let mut seed = 42u32;
        for i in 0..(w*h) as usize {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            let noise = ((seed >> 16) & 31) as i32 - 15; // -15..15
            let v = (128i32 + noise).clamp(0, 255) as u8;
            let o = i * 4;
            f[o] = v; f[o+1] = v; f[o+2] = v; f[o+3] = 255;
        }
        let var = |d: &[u8]| -> f32 {
            let mut sum = 0f32; let mut n = 0usize;
            for i in 0..(w*h) as usize { sum += d[i*4] as f32; n += 1; }
            let mean = sum / n as f32;
            let mut v = 0f32;
            for i in 0..(w*h) as usize { let d = d[i*4] as f32 - mean; v += d*d; }
            v / n as f32
        };
        let before = var(&f);
        process_frame(&mut f, w, h, &FilterParams { beauty_strength: 1.0, ..Default::default() });
        let after = var(&f);
        assert!(after < before * 0.5, "磨皮应降低噪点方差：before={before:.1} after={after:.1}");
    }

    #[test]
    fn test_beauty_keeps_edge() {
        // 左黑右白边界：磨皮后边界两侧仍接近黑/白（边缘保留）
        let w = 16; let h = 16;
        let mut f = vec![0u8; (w * h * 4) as usize];
        for y in 0..h as usize { for x in 0..w as usize {
            let v = if x < w as usize / 2 { 10 } else { 245 };
            let o = (y * w as usize + x) * 4;
            f[o]=v; f[o+1]=v; f[o+2]=v; f[o+3]=255;
        }}
        process_frame(&mut f, w, h, &FilterParams { beauty_strength: 1.0, ..Default::default() });
        // 检查离边界至少 3 像素的点仍保持接近原值
        let left = f[(3 * w as usize + 3) * 4] as i32;      // 左侧内部
        let right = f[(3 * w as usize + (w as usize-3)) * 4] as i32; // 右侧内部
        assert!(left < 40, "左侧应仍偏黑，实际 {left}");
        assert!(right > 215, "右侧应仍偏白，实际 {right}");
    }

    #[test]
    fn test_whiten_brightens() {
        let mut f = frame(Pixel { b: 100, g: 100, r: 100, a: 255 }, 4, 4);
        process_frame(&mut f, 4, 4, &FilterParams { whiten_strength: 1.0, ..Default::default() });
        let p = Pixel::from_slice(&f[0..4]);
        assert!(p.r > 100 && p.g > 100 && p.b > 100, "美白应提亮，实际 r={}", p.r);
    }

    #[test]
    fn test_green_screen() {
        // 纯绿背景 → 透明
        let mut f = frame(Pixel { b: 0, g: 255, r: 0, a: 255 }, 4, 4);
        process_frame(&mut f, 4, 4, &FilterParams { green_screen_threshold: 30, ..Default::default() });
        let p = Pixel::from_slice(&f[0..4]);
        assert_eq!(p.a, 0, "绿幕应抠成透明");

        // 红色保留
        let mut f2 = frame(Pixel { b: 0, g: 0, r: 255, a: 255 }, 4, 4);
        process_frame(&mut f2, 4, 4, &FilterParams { green_screen_threshold: 30, ..Default::default() });
        let p2 = Pixel::from_slice(&f2[0..4]);
        assert_eq!(p2.a, 255, "非绿色应保留");
    }
}
