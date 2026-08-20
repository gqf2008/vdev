//! vdev — macOS 虚拟设备工具箱 CLI。

use anyhow::{anyhow, Result};
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
    /// 点按一个键，可带修饰键，如：vdev hid key space --modifiers cmd shift
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
        #[arg(long)]
        mirror: Option<u32>,
        #[arg(long, default_value_t = 10)]
        hold: u64,
    },
}

#[derive(Subcommand)]
enum CameraCmd {
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

fn run_hid(cmd: HidCmd) -> Result<()> {
    match cmd {
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
            let code = vdev_hid::keycodes::by_name(&key)
                .ok_or_else(|| anyhow!("unknown key: {key}"))?;
            vdev_hid::key(code, true)?;
            println!("down {key}");
        }
        HidCmd::Up { key } => {
            let code = vdev_hid::keycodes::by_name(&key)
                .ok_or_else(|| anyhow!("unknown key: {key}"))?;
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
            vdev_hid::listen(seconds)?;
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

fn run_camera(cmd: CameraCmd) -> Result<()> {
    match cmd {
        CameraCmd::Frame {
            width,
            height,
            pattern,
            out,
            t,
        } => {
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
