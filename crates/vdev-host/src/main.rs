//! vdev — macOS 虚拟设备工具箱 CLI。

use anyhow::{anyhow, bail, Result};
use clap::{Parser, Subcommand};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "vdev", version, about = "macOS 虚拟设备工具箱（Rust）")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// 虚拟键盘 / 鼠标
    Hid {
        #[command(subcommand)]
        cmd: HidCmd,
    },
    /// 虚拟屏幕
    Screen {
        #[command(subcommand)]
        cmd: ScreenCmd,
    },
    /// 虚拟摄像头
    Camera {
        #[command(subcommand)]
        cmd: CameraCmd,
    },
}

#[derive(Subcommand)]
enum HidCmd {
    /// 输入一段文本（支持 Unicode）
    Type { text: String },
    /// 点按一个键，可带修饰键，如：vdev hid key space --modifiers cmd,shift
    #[command(after_help = key_names_help())]
    Key {
        key: String,
        #[arg(long, value_delimiter = ',')]
        modifiers: Vec<String>,
    },
    /// 按住一个键（配合 up 模拟长按）
    Down { key: String },
    /// 松开一个键
    Up { key: String },
    /// 移动鼠标到绝对坐标
    Move { x: f64, y: f64 },
    /// 鼠标点击
    Click {
        x: f64,
        y: f64,
        #[arg(long, default_value = "left")]
        button: String,
    },
    /// 滚轮滚动（正数向上）
    Scroll { delta_y: i32 },
    /// 打印当前身份的输入权限/环境状态（辅助功能权限、安全输入）
    Access,
    /// 监听键盘/鼠标事件（需要辅助功能权限）
    Listen {
        #[arg(long, default_value_t = 10)]
        seconds: u64,
    },
}

#[derive(Subcommand)]
enum ScreenCmd {
    /// 列出在线显示器
    List,
    /// 创建一个虚拟显示器（进程退出时销毁；--hold 控制存活秒数）
    Create {
        #[arg(long, default_value_t = 1920)]
        width: u32,
        #[arg(long, default_value_t = 1080)]
        height: u32,
        #[arg(long, default_value = "vdev")]
        name: String,
        #[arg(long, default_value_t = 60.0)]
        refresh: f64,
        #[arg(long, value_parser = parse_display_id)]
        mirror: Option<u32>,
        #[arg(long, default_value_t = 10)]
        hold: u64,
    },
}

#[derive(Subcommand)]
enum CameraCmd {
    /// 设置设备侧滤镜（经 127.0.0.1:27892 控制通道，立即生效、无需重启扩展）
    ///
    /// spec 形如 "0.3,1,1,0,0,0,0"（亮度,对比度,饱和度,绿幕阈值,锐化,美颜,美白）；
    /// 传 "off" **全部关闭**（参数滤镜与背景模糊）。
    ///
    /// 语义是**整段替换**：本命令总会把 `VDEV_FILTER` / `VDEV_BG` 两个键都发过去，
    /// 没显式打开的那个按关闭处理（所以 `filter 0.3` 会把背景模糊一起关掉，
    /// 要同时开就写 `filter 0.3 --bg blur`）。
    Filter {
        spec: String,
        #[arg(long, default_value = "none")]
        bg: String,
    },
    /// 渲染一帧测试图案并写出（PPM），验证帧核心
    Frame {
        #[arg(long, default_value_t = 640)]
        width: u32,
        #[arg(long, default_value_t = 480)]
        height: u32,
        #[arg(long, default_value = "smpte")]
        pattern: String,
        #[arg(long, default_value = "/tmp/vdev-frame.ppm")]
        out: String,
        #[arg(long, default_value_t = 0.0)]
        t: f64,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Hid { cmd } => run_hid(cmd),
        Cmd::Screen { cmd } => run_screen(cmd),
        Cmd::Camera { cmd } => run_camera(cmd),
    }
}

fn parse_button(s: &str) -> Result<vdev_hid::MouseButton> {
    match s.to_ascii_lowercase().as_str() {
        "left" => Ok(vdev_hid::MouseButton::Left),
        "right" => Ok(vdev_hid::MouseButton::Right),
        "middle" | "center" => Ok(vdev_hid::MouseButton::Center),
        other => Err(anyhow!("unknown button: {other}")),
    }
}

/// 生成 `vdev hid key --help` 的 `after_help`：列出全部可解析键名，
/// 兑现 `unknown key` 错误信息里 "see vdev hid key --help" 的承诺。
fn key_names_help() -> String {
    const LINE_WIDTH: usize = 76; // 在常规终端宽度内自行换行（clap 会尊重已有换行）
    let mut help = String::from("可解析键名（含别名，大小写不敏感）：\n");
    let mut line_len = 0usize;
    for name in vdev_hid::key_names() {
        // 分隔空格只放在词间，避免行尾空白
        if line_len > 0 {
            if line_len + 1 + name.len() > LINE_WIDTH {
                help.push('\n');
                line_len = 0;
            } else {
                help.push(' ');
                line_len += 1;
            }
        }
        help.push_str(name);
        line_len += name.len();
    }
    help.push('\n');
    help
}

/// `screen create` 入口校验：零值/超界参数直达 `CGVirtualDisplay` 私有 API
/// 只会得到笼统报错，先在 CLI 层拦下并给出可操作的范围提示。
fn validate_screen_geometry(width: u32, height: u32, refresh: f64) -> Result<()> {
    const MAX_DIMENSION: u32 = 8192;
    if width == 0 || width > MAX_DIMENSION {
        return Err(anyhow!(
            "invalid --width {width}: 需在 1..={MAX_DIMENSION} 之间（像素），如 --width 1920"
        ));
    }
    if height == 0 || height > MAX_DIMENSION {
        return Err(anyhow!(
            "invalid --height {height}: 需在 1..={MAX_DIMENSION} 之间（像素），如 --height 1080"
        ));
    }
    // 单独排除 NaN：NaN 不满足 `>= 1.0`，但 `refresh < 1.0` 对 NaN 也不成立；
    // ±inf 则会通过上述比较，同样显式拒绝
    if refresh.is_nan() || refresh.is_infinite() || refresh < 1.0 {
        return Err(anyhow!(
            "invalid --refresh {refresh}: 需为有限值且 ≥ 1（Hz），如 --refresh 60"
        ));
    }
    Ok(())
}

fn run_hid(cmd: HidCmd) -> Result<()> {
    match cmd {
        HidCmd::Access => {
            let a = vdev_hid::input_access();
            // 机器可读：验收脚本按 key=value 解析，不要改成人类散文
            println!("post_access={}", a.post);
            println!("listen_access={}", a.listen);
            println!("secure_input={}", a.secure_input);
            if !a.post {
                println!(
                    "hint=缺少「辅助功能」权限时 CGEventPost 会被静默丢弃（窗口收不到任何字符，命令仍 exit 0）"
                );
            }
            if a.secure_input {
                println!("hint=系统处于安全输入（Secure Input），合成事件会被丢弃");
            }
        }
        HidCmd::Type { text } => {
            vdev_hid::type_text(&text)?;
            println!("typed {} chars", text.chars().count());
        }
        HidCmd::Key { key, modifiers } => {
            let code = vdev_hid::keycodes::by_name(&key)
                .ok_or_else(|| anyhow!("unknown key: {key} (see vdev hid key --help)"))?;
            let flags = vdev_hid::parse_modifiers(&modifiers)?;
            vdev_hid::tap_key(code, flags)?;
            println!("tapped {key} (keycode 0x{code:x}, modifiers {modifiers:?})");
        }
        HidCmd::Down { key } => {
            let code =
                vdev_hid::keycodes::by_name(&key).ok_or_else(|| anyhow!("unknown key: {key}"))?;
            vdev_hid::key(code, true)?;
            println!("down {key}");
        }
        HidCmd::Up { key } => {
            let code =
                vdev_hid::keycodes::by_name(&key).ok_or_else(|| anyhow!("unknown key: {key}"))?;
            vdev_hid::key(code, false)?;
            println!("up {key}");
        }
        HidCmd::Move { x, y } => {
            vdev_hid::mouse_move(x, y)?;
            println!("mouse moved to ({x}, {y})");
        }
        HidCmd::Click { x, y, button } => {
            let btn = parse_button(&button)?;
            vdev_hid::mouse_click(x, y, btn)?;
            println!("mouse clicked {button} at ({x}, {y})");
        }
        HidCmd::Scroll { delta_y } => {
            vdev_hid::scroll(delta_y)?;
            println!("scrolled {delta_y} lines");
        }
        HidCmd::Listen { seconds } => {
            // vdev-hid 的 listen 改收 Option<u64>（None=不限时）；CLI 仍为定时监听
            vdev_hid::listen(Some(seconds))?;
        }
    }
    Ok(())
}

fn run_screen(cmd: ScreenCmd) -> Result<()> {
    match cmd {
        ScreenCmd::List => {
            let displays = vdev_screen::list_displays()?;
            if displays.is_empty() {
                println!("no online displays");
            }
            for d in &displays {
                println!(
                    "0x{:08x}  {}  {}x{}  {:.0}x{:.0}mm  vendor=0x{:x} product=0x{:x}",
                    d.id,
                    if d.builtin { "builtin " } else { "external" },
                    d.width,
                    d.height,
                    d.width_mm,
                    d.height_mm,
                    d.vendor,
                    d.product,
                );
            }
            Ok(())
        }
        ScreenCmd::Create {
            width,
            height,
            name,
            refresh,
            mirror,
            hold,
        } => {
            validate_screen_geometry(width, height, refresh)?;
            let vd = vdev_screen::create(vdev_screen::CreateOptions {
                width,
                height,
                refresh_rate: refresh,
                name,
                ..Default::default()
            })?;
            println!(
                "virtual display created: 0x{:08x} ({}x{} @ {refresh}Hz)",
                vd.display_id, width, height
            );
            if let Some(target) = mirror {
                vd.mirror(target)?;
                println!("mirrored to physical display 0x{target:08x}");
            }
            println!("holding for {hold}s (Ctrl-C to destroy) ...");
            std::thread::sleep(Duration::from_secs(hold));
            Ok(())
        }
    }
}

/// 显示器 ID 解析：接受十进制或 `0x` 十六进制。
///
/// `vdev screen list` 打的是十六进制（`0x00000001`），而 clap 的 u32 默认只吃十进制，
/// 用户没法把 list 的输出直接粘给 `--mirror`——这个解析器就是补这个缺口。
fn parse_display_id(s: &str) -> Result<u32, String> {
    let t = s.trim();
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => t.parse::<u32>(),
    };
    parsed.map_err(|e| format!("显示器 ID 只接受十进制或 0x 十六进制（收到 {s}）：{e}"))
}

/// 设备侧滤镜控制通道（扩展监听 127.0.0.1:27892）。
///
/// 为什么不是环境变量/配置文件：扩展由 cmiod 以 `_cmiodalassistants` 身份拉起，
/// 它的家目录是 `/var/db/cmiodalassistants/...`（用户写不进去）、也读不到 /tmp，
/// 唯一被沙盒放行的配置途径就是网络——所以走这条和推流同源的 TCP 通道。
const CAMERA_CONTROL_PORT: u16 = 27892;
const CAMERA_CHANNEL_MAGIC: u32 = 0x5644_4652; // "VDFR"，与扩展侧一致
const CAMERA_CHANNEL_VERSION: u32 = 1;
const CAMERA_OP_SET_FILTER: u32 = 1;

fn send_camera_control(payload: &str) -> Result<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", CAMERA_CONTROL_PORT))
        .map_err(|e| anyhow!("连不上设备侧控制通道 127.0.0.1:{CAMERA_CONTROL_PORT}：{e}（虚拟摄像头扩展没在跑？）"))?;
    // 不设超时的话，端口被别的进程占着且不应答时 CLI 会一直挂着（审查 S3）
    let timeout = Some(std::time::Duration::from_secs(3));
    stream.set_read_timeout(timeout)?;
    stream.set_write_timeout(timeout)?;
    let len = u32::try_from(payload.len()).map_err(|_| anyhow!("配置过长"))?;
    let mut hdr = [0u8; 36];
    hdr[0..4].copy_from_slice(&CAMERA_CHANNEL_MAGIC.to_le_bytes());
    hdr[4..8].copy_from_slice(&CAMERA_CHANNEL_VERSION.to_le_bytes());
    hdr[8..12].copy_from_slice(&0u32.to_le_bytes()); // width=0 → 控制消息
    hdr[12..16].copy_from_slice(&CAMERA_OP_SET_FILTER.to_le_bytes());
    // stride / pts 保持 0
    hdr[28..32].copy_from_slice(&len.to_le_bytes());
    stream.write_all(&hdr)?;
    stream.write_all(payload.as_bytes())?;
    let mut ack = String::new();
    stream
        .read_to_string(&mut ack)
        .map_err(|e| anyhow!("控制通道无响应（{e}）：扩展可能没在跑，或 127.0.0.1:{CAMERA_CONTROL_PORT} 被别的进程占着"))?;
    Ok(ack)
}

/// `camera frame` 入口校验：非法尺寸此前直达 `render`，只会静默写出 0×0 空
/// PPM，先在 CLI 层拦下并给出可操作的范围提示（与 C ABI 共用同一规则）。
fn validate_camera_dims(width: u32, height: u32) -> Result<()> {
    if vdev_camera::frame::dims_valid(width, height) {
        return Ok(());
    }
    let max_px = vdev_camera::frame::MAX_PIXELS;
    Err(anyhow!(
        "invalid --width {width} / --height {height}: 需 ≥1 且 width×height ≤ {max_px}，如 --width 640 --height 480"
    ))
}

// 帧大小仅用于 KiB 级展示换算，usize→f64 精度足够
#[allow(clippy::cast_precision_loss)]
fn run_camera(cmd: CameraCmd) -> Result<()> {
    match cmd {
        CameraCmd::Filter { spec, bg } => {
            // 设备侧按"整段替换"处理配置，所以这里**两个键都显式给出**：
            // `off`/`none` 发空值（等于关掉），命令结果可预期、不受上一次设置影响。
            let filter_value = if spec == "off" { "" } else { spec.as_str() };
            let bg_value = match bg.as_str() {
                "blur" | "1" => "blur",
                "none" | "off" | "" => "",
                other => bail!("--bg 只接受 blur / none，收到 {other}"),
            };
            let payload = format!("VDEV_FILTER={filter_value}\nVDEV_BG={bg_value}\n");
            let ack = send_camera_control(&payload)?;
            print!("{ack}");
            if !ack.starts_with("ok") {
                bail!("设备侧拒绝该配置：{}", ack.trim());
            }
            Ok(())
        }
        CameraCmd::Frame {
            width,
            height,
            pattern,
            out,
            t,
        } => {
            validate_camera_dims(width, height)?;
            let pattern = vdev_camera::FramePattern::parse(&pattern)
                .ok_or_else(|| anyhow!("unknown pattern: {pattern} (smpte/gradient/checker)"))?;
            let frame = vdev_camera::frame::render(pattern, width, height, t);
            vdev_camera::frame::write_ppm(std::path::Path::new(&out), &frame)?;
            println!(
                "wrote {}x{} {} frame ({:.1} KiB) -> {out}",
                width,
                height,
                pattern.name(),
                frame.data.len() as f64 / 1024.0
            );
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn screen_geometry_accepts_valid_values() {
        assert!(validate_screen_geometry(1920, 1080, 60.0).is_ok());
        // 边界：最小合法值
        assert!(validate_screen_geometry(1, 1, 1.0).is_ok());
        // 边界：最大合法尺寸
        assert!(validate_screen_geometry(8192, 8192, 240.0).is_ok());
    }

    /// 回归：`screen create --width 0` / `--refresh 0` 这类零值此前直达私有 API
    /// 只得到笼统报错，现在必须在入口被拦下并给可操作错误。
    #[test]
    fn screen_geometry_rejects_zero_and_oversize() {
        assert!(validate_screen_geometry(0, 1080, 60.0).is_err());
        assert!(validate_screen_geometry(1920, 0, 60.0).is_err());
        assert!(validate_screen_geometry(8193, 1080, 60.0).is_err());
        assert!(validate_screen_geometry(1920, 8193, 60.0).is_err());
    }

    #[test]
    fn screen_geometry_rejects_bad_refresh() {
        assert!(validate_screen_geometry(1920, 1080, 0.0).is_err());
        assert!(validate_screen_geometry(1920, 1080, -30.0).is_err());
        // NaN 两个方向比较都不成立，必须显式排除
        assert!(validate_screen_geometry(1920, 1080, f64::NAN).is_err());
        // ±inf 能通过全部比较运算，必须显式拒绝
        assert!(validate_screen_geometry(1920, 1080, f64::INFINITY).is_err());
        assert!(validate_screen_geometry(1920, 1080, f64::NEG_INFINITY).is_err());
    }

    /// 回归：`camera frame --width 0` 这类非法尺寸此前直达 `render`，
    /// 静默写出 0×0 空 PPM；现在必须在入口被拦下并给可操作的范围提示。
    #[test]
    fn camera_frame_rejects_invalid_dims() {
        assert!(validate_camera_dims(0, 0).is_err());
        assert!(validate_camera_dims(0, 480).is_err());
        assert!(validate_camera_dims(640, 0).is_err());
        // 像素总数超 MAX_PIXELS（8192×8192）
        assert!(validate_camera_dims(8192, 8193).is_err());
        assert!(validate_camera_dims(u32::MAX, u32::MAX).is_err());
        // 错误信息给出合法范围提示
        let msg = format!("{}", validate_camera_dims(0, 480).unwrap_err());
        assert!(
            msg.contains("width×height ≤ 67108864"),
            "错误信息应含像素总数上限: {msg}"
        );
        // 合法边界
        assert!(validate_camera_dims(1, 1).is_ok());
        assert!(validate_camera_dims(640, 480).is_ok());
        assert!(validate_camera_dims(8192, 8192).is_ok());
    }

    /// 回归：`unknown key` 错误让用户 "see vdev hid key --help"，
    /// help 必须真的列出 `vdev_hid::key_names()` 的全部键名。
    /// 断言在 `after_help` 文本上按空白分词做整词匹配：单字符键名（如 "a"）
    /// 用 `contains` 子串匹配会被帮助里任意单词误判为已列出。
    #[test]
    fn hid_key_help_lists_all_key_names() {
        let names = vdev_hid::key_names();
        assert!(!names.is_empty(), "key_names() 不应为空");
        let mut cmd = Cli::command();
        cmd.build();
        let key_cmd = cmd
            .find_subcommand_mut("hid")
            .and_then(|hid| hid.find_subcommand_mut("key"))
            .expect("缺 hid key 子命令");
        let after_help = key_cmd
            .get_after_help()
            .unwrap_or_else(|| panic!("hid key 缺 after_help"))
            .to_string();
        let tokens: std::collections::HashSet<&str> = after_help.split_whitespace().collect();
        for name in names {
            assert!(
                tokens.contains(name),
                "vdev hid key --help 未以独立词形式列出键名 {name:?}"
            );
        }
        // 回归：键名行不得再有行尾空白（分隔空格只应出现在词间）
        for line in after_help.lines() {
            assert_eq!(line, line.trim_end(), "after_help 存在行尾空白: {line:?}");
        }
    }

    #[test]
    fn parse_display_id_accepts_hex_and_decimal() {
        // list 输出 0x00000001 → 用户原样粘贴必须能用（这正是本轮验收发现的缺口）
        assert_eq!(super::parse_display_id("0x00000001"), Ok(1));
        assert_eq!(super::parse_display_id("0X1234"), Ok(0x1234));
        assert_eq!(super::parse_display_id("1"), Ok(1));
        assert_eq!(super::parse_display_id("4660"), Ok(4660));
        assert_eq!(super::parse_display_id(" 0x0a "), Ok(10));
        // 非法输入要有可读错误，而不是 panic
        assert!(super::parse_display_id("0xzz").is_err());
        assert!(super::parse_display_id("").is_err());
        assert!(super::parse_display_id("-1").is_err());
    }

    /// 回归：--modifiers 的 `value_delimiter` 是 ','，帮助示例必须写
    /// `--modifiers cmd,shift`；空格分隔的旧示例会被 clap 当作多余位置参数 reject。
    #[test]
    fn modifiers_help_example_uses_comma_delimiter() {
        let mut cmd = Cli::command();
        cmd.build();
        let key_cmd = cmd
            .find_subcommand_mut("hid")
            .and_then(|hid| hid.find_subcommand_mut("key"))
            .expect("缺 hid key 子命令");
        let help = key_cmd.render_help().to_string();
        assert!(
            help.contains("--modifiers cmd,shift"),
            "vdev hid key --help 的示例应写 --modifiers cmd,shift"
        );
    }
}
