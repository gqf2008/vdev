//! FrameChannel：真实帧推流通道（TCP 127.0.0.1:27890）。
//! 协议对齐 host 端 crates/vdev-app/src/frame.rs 与旧 Swift FrameChannel.swift：
//! 36 字节小端头 [magic=0x56444652(VDFR), version=1, width u32, height u32,
//! stride u32, ptsNs u64, payloadLen u64] + BGRA payload。
//! 只保留最新一帧（与 Swift 的 injectFrame 语义一致）。

use std::io::Read;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct InjectedFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pts_ns: u64,
    pub received_at: Instant,
}

static INJECTED: Mutex<Option<InjectedFrame>> = Mutex::new(None);
/// 当前接入的推流客户端连接句柄：新连接 accept 时对旧 socket shutdown(Both)
/// 实现真接管（旧 `handle_conn` 线程随读失败退出），避免两客户端帧交替覆盖 INJECTED。
/// 无法单测：接管依赖真实 TCP accept 与线程时序。
static CURRENT_CLIENT: Mutex<Option<TcpStream>> = Mutex::new(None);

const HEADER_SIZE: usize = 36;
/// 控制通道端口：与推流通道（27890）分开。27890 的新连接会 `shutdown` 掉旧连接
/// （那是"推流接管"语义），配置操作不该把正在推流的客户端踢掉。
const CONTROL_PORT: u16 = 27892;
/// 控制消息 opcode（放在头的 `height` 字段）：设置设备侧滤镜。
pub const OP_SET_FILTER: u32 = 1;
/// 控制连接的单向读/写超时。
const CONTROL_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const MAGIC: u32 = 0x5644_4652; // "VDFR"
const VERSION: u32 = 1;
/// 头字段钳制：宽高 ≤ 8K；stride 允许 padded（w*4 ≤ stride ≤ w*4 + 8KiB）；
/// 单帧载荷 ≤ 256MiB。恶意客户端的无界头在此全部拒绝，防 pending 无界堆积/巨分配。
const MAX_DIM: u32 = 7680;
const MAX_STRIDE_PAD: u64 = 8192;
const MAX_PAYLOAD: u64 = 256 * 1024 * 1024;

/// 36 字节头解析结果
#[derive(Debug, PartialEq, Eq)]
struct FrameHeader {
    width: u32,
    height: u32,
    stride: u32,
    pts_ns: u64,
    payload_len: usize,
}

/// 头校验失败原因（协议违规 → 立即关闭连接）
#[derive(Debug, PartialEq, Eq)]
enum HeaderError {
    Magic,
    Version,
    Width,
    Height,
    Stride,
    PayloadLen,
    /// 控制消息的 opcode 不认识（帧路径不会产生这个错误）
    Opcode,
}

/// 取最新注入帧；超过 `max_age` 视为过期（返回 None → 回落彩条）。
pub fn take_fresh(max_age: Duration) -> Option<(Vec<u8>, u32, u32, u32, u64)> {
    let mut g = INJECTED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(f) = g.as_ref() {
        if f.received_at.elapsed() < max_age {
            return Some((f.data.clone(), f.width, f.height, f.stride, f.pts_ns));
        }
        // 过期帧清掉，避免一直残留
        *g = None;
    }
    None
}

fn u32le(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn u64le(b: &[u8], off: usize) -> u64 {
    u64::from_le_bytes([
        b[off],
        b[off + 1],
        b[off + 2],
        b[off + 3],
        b[off + 4],
        b[off + 5],
        b[off + 6],
        b[off + 7],
    ])
}

/// 解析并校验 36 字节**控制**头（`width == 0` 即控制消息）。
///
/// 复用同一套头布局，便于两端共用常量：`width=0`、`height=opcode`、
/// `stride=0`、`pts=0`、`payloadLen` = 配置文本字节数。
fn parse_control(raw: &[u8; HEADER_SIZE]) -> Result<(u32, usize), HeaderError> {
    if u32le(raw, 0) != MAGIC {
        return Err(HeaderError::Magic);
    }
    if u32le(raw, 4) != VERSION {
        return Err(HeaderError::Version);
    }
    if u32le(raw, 8) != 0 {
        return Err(HeaderError::Width);
    }
    let opcode = u32le(raw, 12);
    if opcode != OP_SET_FILTER {
        return Err(HeaderError::Opcode);
    }
    if u32le(raw, 16) != 0 {
        return Err(HeaderError::Stride);
    }
    let len = u64le(raw, 28);
    // 配置文本很小；沿用 256MiB 上限会给出不必要的分配空间，这里单独收紧
    if len > 64 * 1024 {
        return Err(HeaderError::PayloadLen);
    }
    // 目标平台 64 位 macOS：u64→usize 截断不可能
    #[allow(clippy::cast_possible_truncation)]
    Ok((opcode, len as usize))
}

/// 解析并校验 36 字节帧头（纯函数，网络层薄壳 `handle_conn` 调用；
/// 单独抽出以覆盖回绕/钳制等攻击面单测）。
/// 所有乘法与比较在 u64 域进行——此前 `stride < w * 4` 用 u32 乘法，
/// w ≥ 2^30 时 release 下回绕（如 w=2^30+1 → w*4=4）可绕过校验。
fn parse_header(hdr: &[u8; HEADER_SIZE]) -> Result<FrameHeader, HeaderError> {
    if u32le(hdr, 0) != MAGIC {
        return Err(HeaderError::Magic);
    }
    if u32le(hdr, 4) != VERSION {
        return Err(HeaderError::Version);
    }
    let w = u32le(hdr, 8);
    let h = u32le(hdr, 12);
    let stride = u32le(hdr, 16);
    let pts = u64le(hdr, 20);
    let len = u64le(hdr, 28);
    if w == 0 || w > MAX_DIM {
        return Err(HeaderError::Width);
    }
    if h == 0 || h > MAX_DIM {
        return Err(HeaderError::Height);
    }
    // 协议允许 padded stride：w*4 ≤ stride ≤ w*4 + 8KiB
    let min_stride = u64::from(w) * 4;
    let max_stride = min_stride + MAX_STRIDE_PAD;
    if u64::from(stride) < min_stride || u64::from(stride) > max_stride {
        return Err(HeaderError::Stride);
    }
    // 推流端 frame.rs 的 payloadLen 恒为 data.len() == stride*h 整帧，故取 ==；
    // 另加 256MiB 全局上限，防恶意巨分配
    let frame_bytes = u64::from(stride) * u64::from(h);
    if len != frame_bytes || len > MAX_PAYLOAD {
        return Err(HeaderError::PayloadLen);
    }
    // 已保证 len ≤ 256MiB；目标平台 64 位 macOS，u64→usize 截断不可能
    #[allow(clippy::cast_possible_truncation)]
    let payload_len = len as usize;
    Ok(FrameHeader {
        width: w,
        height: h,
        stride,
        pts_ns: pts,
        payload_len,
    })
}

fn handle_conn(mut stream: TcpStream) {
    let mut pending: Vec<u8> = Vec::new();
    let mut reading_header = true;
    let mut expected = 0usize;
    let mut hdr: Option<FrameHeader> = None;
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut chunk) {
            // 对端关闭或读错误：结束本连接
            Ok(0) | Err(_) => return,
            Ok(n) => {
                pending.extend_from_slice(&chunk[..n]);
                loop {
                    if reading_header {
                        if pending.len() < HEADER_SIZE {
                            break;
                        }
                        let mut raw = [0u8; HEADER_SIZE];
                        raw.copy_from_slice(&pending[..HEADER_SIZE]);
                        let Ok(head) = parse_header(&raw) else {
                            // 协议违规（magic/版本/尺寸/stride/payload 校验不过）：
                            // 立即关闭连接。此前是 break，只跳出内层解析循环，
                            // 脏 36 字节永不 drain、pending 无界堆积（OOM 面）。
                            return;
                        };
                        expected = head.payload_len;
                        hdr = Some(head);
                        reading_header = false;
                        pending.drain(..HEADER_SIZE);
                    } else {
                        if pending.len() < expected {
                            break;
                        }
                        let payload = pending[..expected].to_vec();
                        pending.drain(..expected);
                        if let Some(head) = hdr.take() {
                            *INJECTED
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                Some(InjectedFrame {
                                    data: payload,
                                    width: head.width,
                                    height: head.height,
                                    stride: head.stride,
                                    pts_ns: head.pts_ns,
                                    received_at: Instant::now(),
                                });
                        }
                        reading_header = true;
                        expected = 0;
                    }
                }
            }
        }
    }
}

/// 处理一条控制连接：读一条控制消息 → 应用配置 → 回一行 ack（便于 CLI 立即可见）。
fn handle_control(mut stream: TcpStream) {
    // 读超时：只发 1 个字节就能让线程永久阻塞在 read_exact，而每条连接一个线程、
    // 线程数无上限——本机任意进程都能反复"挂线"把扩展拖住（审查 S3）。
    let _ = stream.set_read_timeout(Some(CONTROL_READ_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CONTROL_READ_TIMEOUT));
    let mut raw = [0u8; HEADER_SIZE];
    if std::io::Read::read_exact(&mut stream, &mut raw).is_err() {
        return;
    }
    let Ok((_opcode, len)) = parse_control(&raw) else {
        let _ = std::io::Write::write_all(&mut stream, "err 控制头校验失败\n".as_bytes());
        return;
    };
    let mut payload = vec![0u8; len];
    if len > 0 && std::io::Read::read_exact(&mut stream, &mut payload).is_err() {
        return;
    }
    let text = String::from_utf8_lossy(&payload);
    let ack = match crate::filters::apply_config(&text) {
        Ok(summary) => {
            crate::elog_for_channel(format!("控制通道：设备侧滤镜已更新 → {summary}"));
            format!("ok {summary}\n")
        }
        Err(e) => {
            crate::elog_for_channel(format!("控制通道：配置被拒绝（{e}）"));
            format!("err {e}\n")
        }
    };
    let _ = std::io::Write::write_all(&mut stream, ack.as_bytes());
}

/// 启动控制通道监听（后台线程；端口见 `CONTROL_PORT`）。
pub fn start_control() {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind(("127.0.0.1", CONTROL_PORT)) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("vdev-camera-ext: 控制通道 bind 127.0.0.1:{CONTROL_PORT} 失败: {e}");
                return;
            }
        };
        eprintln!("vdev-camera-ext: 控制通道监听 127.0.0.1:{CONTROL_PORT}");
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || handle_control(stream));
        }
    });
}

/// 启动推流通道监听（后台线程）。
pub fn start() {
    std::thread::spawn(move || {
        let listener = match TcpListener::bind("127.0.0.1:27890") {
            Ok(l) => l,
            Err(e) => {
                eprintln!("vdev-camera-ext: FrameChannel bind 127.0.0.1:27890 失败: {e}");
                return;
            }
        };
        eprintln!("vdev-camera-ext: FrameChannel 监听 127.0.0.1:27890");
        for stream in listener.incoming().flatten() {
            // 新客户端接入即接管：对旧 socket shutdown(Both)，旧 handle_conn
            // 线程随读失败退出（此前旧连接从不关闭，两客户端帧交替覆盖 INJECTED）。
            // 接管语义依赖真实 TCP accept 时序，无法单测。
            if let Ok(dup) = stream.try_clone() {
                // try_clone 失败仅跳过接管注册，不影响本连接推流
                let old = CURRENT_CLIENT
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .replace(dup);
                if let Some(old) = old {
                    let _ = old.shutdown(Shutdown::Both);
                }
            } else {
                // 未注册接管 → 旧连接不会被 shutdown，多客户端帧交替覆盖
                // INJECTED（旧语义），须留痕供排查
                eprintln!(
                    "vdev-camera-ext: FrameChannel try_clone 失败，跳过接管注册，退化为多客户端交替覆盖语义"
                );
            }
            std::thread::spawn(move || handle_conn(stream));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 按协议布局构造 36 字节头（小端），单测里可逐字段注入非法值
    fn build_header(w: u32, h: u32, stride: u32, pts: u64, len: u64) -> [u8; HEADER_SIZE] {
        let mut b = [0u8; HEADER_SIZE];
        b[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        b[4..8].copy_from_slice(&VERSION.to_le_bytes());
        b[8..12].copy_from_slice(&w.to_le_bytes());
        b[12..16].copy_from_slice(&h.to_le_bytes());
        b[16..20].copy_from_slice(&stride.to_le_bytes());
        b[20..28].copy_from_slice(&pts.to_le_bytes());
        b[28..36].copy_from_slice(&len.to_le_bytes());
        b
    }

    /// 造一个控制头（测试与控制通道的客户端共用口径）。
    fn control_header(opcode: u32, stride: u32, len: u64) -> [u8; HEADER_SIZE] {
        let mut raw = [0u8; HEADER_SIZE];
        raw[0..4].copy_from_slice(&MAGIC.to_le_bytes());
        raw[4..8].copy_from_slice(&VERSION.to_le_bytes());
        raw[8..12].copy_from_slice(&0u32.to_le_bytes()); // width = 0 → 控制消息
        raw[12..16].copy_from_slice(&opcode.to_le_bytes());
        raw[16..20].copy_from_slice(&stride.to_le_bytes());
        raw[28..36].copy_from_slice(&len.to_le_bytes());
        raw
    }

    #[test]
    fn control_header_accepts_set_filter() {
        let raw = control_header(OP_SET_FILTER, 0, 32);
        assert_eq!(parse_control(&raw), Ok((OP_SET_FILTER, 32)));
    }

    #[test]
    fn control_header_rejects_unknown_opcode_and_bad_magic() {
        assert_eq!(
            parse_control(&control_header(7, 0, 8)),
            Err(HeaderError::Opcode)
        );
        let mut raw = control_header(OP_SET_FILTER, 0, 8);
        raw[0] ^= 0xFF;
        assert_eq!(parse_control(&raw), Err(HeaderError::Magic));
        let mut raw2 = control_header(OP_SET_FILTER, 0, 8);
        raw2[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert_eq!(parse_control(&raw2), Err(HeaderError::Version));
    }

    #[test]
    fn control_header_rejects_frame_shaped_and_oversized_messages() {
        // width != 0 → 这是帧，不是控制消息（控制口不接受推流）
        let mut raw = control_header(OP_SET_FILTER, 0, 8);
        raw[8..12].copy_from_slice(&1920u32.to_le_bytes());
        assert_eq!(parse_control(&raw), Err(HeaderError::Width));
        // stride 必须为 0（控制消息没有行距概念）
        assert_eq!(
            parse_control(&control_header(OP_SET_FILTER, 4, 8)),
            Err(HeaderError::Stride)
        );
        // 配置文本上限 64 KiB：超过即拒（不给 256MiB 的分配空间）
        assert_eq!(
            parse_control(&control_header(OP_SET_FILTER, 0, 64 * 1024 + 1)),
            Err(HeaderError::PayloadLen)
        );
    }

    #[test]
    fn header_roundtrip() {
        let raw = build_header(1920, 1080, 1920 * 4, 12_345, 1920 * 4 * 1080);
        let hdr = parse_header(&raw).expect("合法头应通过");
        assert_eq!(hdr.width, 1920);
        assert_eq!(hdr.height, 1080);
        assert_eq!(hdr.stride, 1920 * 4);
        assert_eq!(hdr.pts_ns, 12_345);
        assert_eq!(hdr.payload_len, 1920 * 4 * 1080);
    }

    #[test]
    fn legal_padded_stride_passes() {
        let w = 100u32;
        let h = 50u32;
        let stride = w * 4 + 64; // 协议允许 padded stride
        let raw = build_header(w, h, stride, 7, u64::from(stride) * u64::from(h));
        let hdr = parse_header(&raw).expect("合法 padded stride 应通过");
        assert_eq!(hdr.stride, stride);
        assert_eq!(hdr.payload_len, stride as usize * h as usize);
    }

    #[test]
    fn stride_at_pad_limit_passes() {
        let w = 100u32;
        let h = 10u32;
        let stride = w * 4 + 8192; // 恰在 padded 上限
        let raw = build_header(w, h, stride, 0, u64::from(stride) * u64::from(h));
        assert!(parse_header(&raw).is_ok());
    }

    #[test]
    fn bad_magic_rejected() {
        let mut raw = build_header(64, 64, 256, 0, 256 * 64);
        raw[0] = b'X';
        assert_eq!(parse_header(&raw), Err(HeaderError::Magic));
    }

    #[test]
    fn bad_version_rejected() {
        let mut raw = build_header(64, 64, 256, 0, 256 * 64);
        raw[5] = 2; // version = 2
        assert_eq!(parse_header(&raw), Err(HeaderError::Version));
    }

    #[test]
    fn zero_width_rejected() {
        let raw = build_header(0, 64, 256, 0, 256 * 64);
        assert_eq!(parse_header(&raw), Err(HeaderError::Width));
    }

    #[test]
    fn zero_height_rejected() {
        let raw = build_header(64, 0, 256, 0, 0);
        assert_eq!(parse_header(&raw), Err(HeaderError::Height));
    }

    #[test]
    fn width_overflow_wrap_attack_rejected() {
        // w = 2^30+1 时 w*4 在 u32 域回绕为 4，旧实现 `stride < w * 4`（u32 乘法）
        // 可被 stride=4 绕过；u64 域 + 上限钳制后必须拒绝
        let w: u32 = (1 << 30) + 1;
        let raw = build_header(w, 1, 4, 0, 4);
        assert_eq!(parse_header(&raw), Err(HeaderError::Width));
    }

    #[test]
    fn width_over_dim_cap_rejected() {
        let raw = build_header(MAX_DIM + 1, 10, (MAX_DIM + 1) * 4, 0, 0);
        assert_eq!(parse_header(&raw), Err(HeaderError::Width));
    }

    #[test]
    fn height_over_dim_cap_rejected() {
        let raw = build_header(100, MAX_DIM + 1, 400, 0, 0);
        assert_eq!(parse_header(&raw), Err(HeaderError::Height));
    }

    #[test]
    fn stride_below_row_bytes_rejected() {
        let w = 100u32;
        let raw = build_header(w, 10, w * 4 - 4, 0, u64::from(w * 4 - 4) * 10);
        assert_eq!(parse_header(&raw), Err(HeaderError::Stride));
    }

    #[test]
    fn stride_over_pad_limit_rejected() {
        let w = 100u32;
        let h = 10u32;
        let stride = w * 4 + 8193; // 超过 w*4 + 8192
        let raw = build_header(w, h, stride, 0, u64::from(stride) * u64::from(h));
        assert_eq!(parse_header(&raw), Err(HeaderError::Stride));
    }

    #[test]
    fn payload_len_mismatch_rejected() {
        // 与推流端"payloadLen == stride*h 整帧"语义不符
        let raw = build_header(100, 10, 400, 0, u64::from(400_u32) * 10 - 1);
        assert_eq!(parse_header(&raw), Err(HeaderError::PayloadLen));
    }

    #[test]
    fn payload_over_cap_rejected() {
        // 各字段单独看合法（8K + 满额 pad），但 stride*h ≈ 285MiB > 256MiB 上限
        let stride = MAX_DIM * 4 + 8192;
        let raw = build_header(
            MAX_DIM,
            MAX_DIM,
            stride,
            0,
            u64::from(stride) * u64::from(MAX_DIM),
        );
        assert_eq!(parse_header(&raw), Err(HeaderError::PayloadLen));
    }

    #[test]
    fn max_8k_frame_passes() {
        // 8K 标准帧（无 pad）：stride*h ≈ 126.6MiB < 256MiB，应通过
        let h = 4320u32;
        let stride = MAX_DIM * 4;
        let raw = build_header(MAX_DIM, h, stride, 0, u64::from(stride) * u64::from(h));
        assert!(parse_header(&raw).is_ok());
    }
}
