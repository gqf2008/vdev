//! 跨进程共享帧通道安全封装。
//!
//! 生产者（宿主进程）与消费者（DirectShow filter，被加载进消费 App 进程）
//! 通过「命名共享内存 + 命名事件」交换 BGRA 帧：
//!
//! - 双缓冲 + 发布序号（`seq`）实现无锁发布/订阅：生产者写 `buf[seq&1]` 后
//!   以 Release 顺序发布新序号；消费者以 Acquire 顺序读 `seq` 并拷贝 `buf[seq&1]`。
//! - 命名事件用于唤醒消费者（新帧到达）；消费者超时则回退到测试图案。
//! - 两端都用「打开或创建」语义（先到者创建并定容量），因此谁先启动都行。

use std::io;
use std::sync::atomic::{AtomicU32, Ordering};

use windows::Win32::Foundation::{
    CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_ALL_ACCESS,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};
use windows::Win32::System::Threading::{
    CreateEventW, CreateMutexW, ReleaseMutex, SetEvent, WaitForSingleObject,
};
use windows_core::PCWSTR;

use super::channel_logic::{
    judge_lock_wait, judge_seqlock_read, LockWaitVerdict, SeqlockVerdict, LOCK_WAIT_MS,
    MAX_SEQLOCK_ATTEMPTS,
};

/// 命名共享内存。
pub const SHM_NAME: &str = "Local\\vdev-camera-win-frames";
/// 命名事件（新帧信号）。
pub const EVENT_NAME: &str = "Local\\vdev-camera-win-frame-event";
/// 命名互斥体（跨进程保护多生产者并发 publish）。
pub const PUBLISH_MUTEX_NAME: &str = "Local\\vdev-camera-win-publish-lock";

const MAGIC: u32 = 0x5644_4556; // "VDEV" 小端
const BPP: u32 = 4; // BGRA
/// 通道容量上限（1920x1080x4）。
const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;
const MAX_BUF: usize = (MAX_WIDTH * MAX_HEIGHT * BPP) as usize;

/// 校验帧尺寸并计算期望字节数（纯函数，可宿主单测）。
/// 上限校验先于 u32 乘法，杜绝大入参回绕绕过校验（审查 M-f）。
fn checked_frame_len(width: u32, height: u32) -> Option<usize> {
    if width > MAX_WIDTH || height > MAX_HEIGHT {
        return None;
    }
    Some((width * height * BPP) as usize)
}
/// 头部大小（64 字节对齐，含 pad）。
const HEADER_LEN: usize = 64;

/// 共享内存头部布局。
#[repr(C)]
struct Header {
    magic: u32,
    width: u32,
    height: u32,
    stride: u32,
    buf_len: u32,
    seq: AtomicU32,
    ready: AtomicU32,
    pad: [u32; 5],
}

/// 共享帧通道。`writer=true` 为生产者（宿主），`false` 为消费者（filter）。
///
/// `publish`/`latest`/`wait_frame` 均可跨线程调用；内部用原子序号保证发布/订阅顺序。
pub struct SharedFrameChannel {
    mapping: HANDLE,
    event: HANDLE,
    /// 跨进程发布锁（命名互斥体）：多个生产者（CLI push / GUI）并发 publish
    /// 时保护双缓冲写入，避免数据竞争。
    publish_lock: HANDLE,
    view: *mut u8,
    writer: bool,
}

// SAFETY: 视图指向系统共享内存，访问顺序由 Header 内原子序号约束；
// 该通道设计为跨线程使用（宿主推流线程 / filter 推流线程各一个）。
unsafe impl Send for SharedFrameChannel {}
unsafe impl Sync for SharedFrameChannel {}

impl SharedFrameChannel {
    /// 打开或创建共享帧通道。
    pub fn open_or_create(writer: bool) -> io::Result<Self> {
        let total_len = HEADER_LEN + MAX_BUF * 2;
        let shm_name = to_wide(SHM_NAME);
        // SAFETY: 匿名映射（INVALID_HANDLE_VALUE），默认安全描述符，名称在调用期间存活。
        let mapping = unsafe {
            CreateFileMappingW(
                INVALID_HANDLE_VALUE,
                None,
                PAGE_READWRITE,
                0,
                total_len as u32,
                PCWSTR(shm_name.as_ptr()),
            )
        }
        .map_err(os_error)?;

        // SAFETY: 视图在 Self 生命周期内保持映射。
        let view = unsafe { MapViewOfFile(mapping, FILE_MAP_ALL_ACCESS, 0, 0, total_len) };
        if view.Value.is_null() {
            let err = io::Error::last_os_error();
            // SAFETY: mapping 有效（CreateFileMappingW 成功）。
            unsafe { CloseHandle(mapping) }.ok();
            return Err(err);
        }

        let event_name = to_wide(EVENT_NAME);
        // SAFETY: 命名事件（auto-reset），初始无信号，名称在调用期间存活。
        let event = unsafe { CreateEventW(None, false, false, PCWSTR(event_name.as_ptr())) }
            .map_err(|e| {
                // SAFETY: view/mapping 均有效，错误路径释放句柄。
                unsafe { UnmapViewOfFile(view) }.ok();
                unsafe { CloseHandle(mapping) }.ok();
                io::Error::from_raw_os_error(e.code().0)
            })?;

        // 首个创建者初始化头部。魔数是「已初始化」标志，必须**最后**写：
        // 其余打开者凭 magic == MAGIC 跳过初始化，若先写魔数，并发打开者
        // 会在字段尚未清零时误判初始化完成、读到未初始化值（审查 M-f 附带）。
        // SAFETY: view 已映射且长度 >= HEADER_LEN，对齐由 Header 定义保证。
        let header = unsafe { &mut *view.Value.cast::<Header>() };
        if header.magic != MAGIC {
            header.width = 0;
            header.height = 0;
            header.stride = 0;
            header.buf_len = 0;
            header.seq = AtomicU32::new(0);
            header.ready = AtomicU32::new(0);
            // Release 栅栏保证以上字段写入先于魔数对其他进程可见
            std::sync::atomic::fence(Ordering::Release);
            header.magic = MAGIC;
        }

        let mutex_name = to_wide(PUBLISH_MUTEX_NAME);
        // SAFETY: 命名互斥体（初始无主），名称在调用期间存活。
        let publish_lock = unsafe { CreateMutexW(None, false, PCWSTR(mutex_name.as_ptr())) }
            .map_err(|e| {
                // SAFETY: view/mapping/event 均有效，错误路径释放句柄。
                unsafe {
                    UnmapViewOfFile(view).ok();
                    CloseHandle(mapping).ok();
                    CloseHandle(event).ok();
                }
                io::Error::from_raw_os_error(e.code().0)
            })?;

        Ok(Self {
            mapping,
            event,
            publish_lock,
            view: view.Value.cast::<u8>(),
            writer,
        })
    }

    fn header(&self) -> &Header {
        // SAFETY: view 在生命周期内有效，头部在偏移 0。
        unsafe { &*self.view.cast::<Header>() }
    }

    /// 可变头部（仅生产者使用；发布序号的 Release 保证字段写入先于读者可见）。
    #[allow(clippy::mut_from_ref)] // 可变性来自共享内存映射（内部可变性），非 &self 借用
    fn header_mut(&self) -> &mut Header {
        // SAFETY: 写者唯一（writer 模式），且 view 在生命周期内有效。
        unsafe { &mut *self.view.cast::<Header>() }
    }

    fn buffer(&self, slot: u32) -> &[u8] {
        let off = HEADER_LEN + slot as usize * MAX_BUF;
        // SAFETY: view + 偏移在映射范围内（HEADER_LEN + 2*MAX_BUF == total_len）。
        unsafe { std::slice::from_raw_parts(self.view.add(off), MAX_BUF) }
    }

    /// 可写缓冲槽（仅生产者使用；双缓冲避免与消费者正在读的槽冲突）。
    #[allow(clippy::mut_from_ref)] // 可变性来自共享内存映射（内部可变性），非 &self 借用
    fn buffer_mut(&self, slot: u32) -> &mut [u8] {
        let off = HEADER_LEN + slot as usize * MAX_BUF;
        // SAFETY: 写者唯一；偏移在映射范围内。
        unsafe { std::slice::from_raw_parts_mut(self.view.add(off), MAX_BUF) }
    }

    /// 生产者：发布一帧 BGRA。
    pub fn publish(&self, width: u32, height: u32, bgra: &[u8]) -> io::Result<()> {
        if !self.writer {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "channel is not in writer mode",
            ));
        }
        // 尺寸上限校验必须先于乘法：width*height*BPP 在 u32 域计算，未限幅的
        // 入参（如 65535x65535）会先回绕再与 bgra.len() 比较，校验形同虚设、
        // 还可能让 expected 小于真实帧长而越界写共享缓冲（审查 M-f）
        let Some(expected) = checked_frame_len(width, height) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("frame too large: {width}x{height} (max {MAX_WIDTH}x{MAX_HEIGHT})"),
            ));
        };
        if bgra.len() != expected {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "frame size {} != {}x{}x{} = {}",
                    bgra.len(),
                    width,
                    height,
                    BPP,
                    expected
                ),
            ));
        }

        // 跨进程互斥：多个生产者（CLI push / GUI 推流）并发写双缓冲时必须互斥，
        // 否则数据竞争（Rust unsafe 下为 UB，可能卡死/画面错乱）。
        // 修 B1：INFINITE 改为有界等待（LOCK_WAIT_MS）——正常持锁
        // 窗口是微秒级 memcpy+序号发布，超时即持锁方异常，放弃本帧由下一次推流
        // 自然重试，避免推流线程在互斥体被异常持有时无界阻塞。
        // SAFETY: publish_lock 有效；返回值为等待结果码（取裸码裁决）。
        let verdict =
            judge_lock_wait(unsafe { WaitForSingleObject(self.publish_lock, LOCK_WAIT_MS) }.0);
        match verdict {
            // WAIT_ABANDONED（接管语义，修 B1 核心）：上一持锁线程所在进程已退出，
            // 内核已把互斥体所有权移交本次等待——必须按「已获得锁」继续。原实现
            // 把它当失败返回，导致持锁方崩溃后无人再能接管互斥体、推帧永久失败。
            // 接管后锁内数据可能被崩溃写撕坏：本函数按槽整体覆盖写，读方按
            // seqlock 序号裁决兜底（见 `latest`，读到坏帧即丢弃）。
            LockWaitVerdict::Acquired | LockWaitVerdict::AcquiredAbandoned => {}
            LockWaitVerdict::TimedOut => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "publish lock not acquired within {LOCK_WAIT_MS}ms (previous holder stuck?)"
                    ),
                ));
            }
            LockWaitVerdict::Failed => {
                return Err(io::Error::other("publish lock wait failed"));
            }
        }

        let header = self.header();
        let next = header.seq.load(Ordering::Relaxed).wrapping_add(1);
        let header = self.header_mut();
        let slot = next & 1;
        // 写当前发布者的另一槽，避免与消费者正在读的槽冲突。
        self.buffer_mut(slot)[..expected].copy_from_slice(bgra);

        header.stride = width * BPP;
        header.buf_len = expected as u32;
        header.width = width;
        header.height = height;
        header.seq.store(next, Ordering::Release);
        header.ready.store(1, Ordering::Release);
        // SAFETY: event 句柄有效。
        unsafe { SetEvent(self.event) }.ok();

        // SAFETY: 本线程持有该互斥体。
        unsafe { ReleaseMutex(self.publish_lock) }.ok();
        Ok(())
    }

    /// 消费者：取最新一帧；从未发布过、头部无效或持续撕裂返回 `None`。
    /// 返回 `None` 时 `out` 内容未定义（撕裂轮会留下按单槽容量截断的拷贝），
    /// 调用方必须整帧覆盖后再使用（现调用方 `render_pattern` 满足）。
    ///
    /// 修 M2：经典 seqlock——读数据前/后各采一次发布序号，两次一致（期间无新
    /// 发布）且头部自洽才采纳；序号不一致说明读到撕裂数据，**必须重读**而非
    /// 使用旧读取值。最多重读 [`MAX_SEQLOCK_ATTEMPTS`] 轮（防写方高频发布导致
    /// 活锁），超限按「无帧」返回，由调用方回退测试图案。
    ///
    /// 修 M2b：`seq_after` 在负载拷贝**完成之后**采样，序号括弧覆盖整个
    /// （头部+负载）读取——拷贝期间写方完成两次发布回到同一槽（写方 2 倍以上
    /// 吞吐且读方被抢占）时，拷贝的正是正在被覆盖的槽，只能靠拷贝后采样检出
    /// 序号差值重读；修前 `seq_after` 在拷贝前采样，括弧只护住头部读取，
    /// 这类撕裂会通过校验、撕裂像素被当稳定帧采纳。
    pub fn latest(&self, out: &mut Vec<u8>) -> Option<(u32, u32)> {
        let header = self.header();
        if header.ready.load(Ordering::Acquire) == 0 {
            return None;
        }
        for _ in 0..MAX_SEQLOCK_ATTEMPTS {
            let seq_before = header.seq.load(Ordering::Acquire);
            let width = header.width;
            let height = header.height;
            let buf_len = header.buf_len;
            let slot_buf = self.buffer(seq_before & 1);
            // 修 M2b：负载拷贝必须先于 `seq_after` 采样完成，序号括弧才能把
            // 拷贝也括住（见函数文档）。拷贝长度按单槽容量截断：此刻 buf_len
            // 尚未通过 judge 校验（可能是崩溃残留的垃圾值），截断阻断越界
            // 拷贝；校验通过时 buf_len <= MAX_BUF，截断为恒等，正确路径不变。
            let copy_len = (buf_len as usize).min(MAX_BUF);
            out.resize(copy_len, 0);
            out.copy_from_slice(&slot_buf[..copy_len]);
            let seq_after = header.seq.load(Ordering::Acquire);
            match judge_seqlock_read(seq_before, seq_after, width, height, buf_len, MAX_BUF) {
                // 序号括弧内（头部+负载）读取完整且头部自洽：采纳 out 中拷贝。
                SeqlockVerdict::Accept => return Some((width, height)),
                // 序号稳定但头部无效（发布方崩溃残留）：重读无意义，按无帧处理。
                SeqlockVerdict::NoFrame => return None,
                // 撕裂：重读。
                SeqlockVerdict::Retry => continue,
            }
        }
        None
    }

    /// 等待新帧事件；超时返回 `false`。
    pub fn wait_frame(&self, timeout_ms: u32) -> bool {
        // SAFETY: event 句柄有效；返回值非错误。
        match unsafe { WaitForSingleObject(self.event, timeout_ms) } {
            WAIT_OBJECT_0 => true,
            WAIT_TIMEOUT => false,
            _ => false,
        }
    }
}

impl Drop for SharedFrameChannel {
    fn drop(&mut self) {
        // SAFETY: 句柄与视图在本对象生命周期内有效。
        unsafe {
            UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.view.cast(),
            })
            .ok();
            CloseHandle(self.event).ok();
            CloseHandle(self.mapping).ok();
            CloseHandle(self.publish_lock).ok();
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn os_error(e: windows_core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0)
}

#[cfg(test)]
mod tests {
    use super::super::channel_logic::{WAIT_ABANDONED_CODE, WAIT_OBJECT_0_CODE, WAIT_TIMEOUT_CODE};
    use windows::Win32::Foundation::{WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};

    /// channel_logic 的裸等待码常量必须与 windows 绑定一致
    /// （阳性对照：任一侧改错即红；此测试需 Windows 依赖树，随 crate 在
    /// Windows 侧 `cargo test` 运行——宿主侧对应测试在 channel_logic.rs 内）。
    #[test]
    fn channel_logic_wait_codes_match_bindings() {
        assert_eq!(WAIT_OBJECT_0_CODE, WAIT_OBJECT_0.0);
        assert_eq!(WAIT_ABANDONED_CODE, WAIT_ABANDONED.0);
        assert_eq!(WAIT_TIMEOUT_CODE, WAIT_TIMEOUT.0);
    }

    /// M-f 回归：尺寸上限校验必须先于 u32 乘法——超限输入返回 None，
    /// 不得先回绕出一个"合法"字节数
    #[test]
    fn checked_frame_len_rejects_oversize_before_multiply() {
        use super::{checked_frame_len, BPP, MAX_HEIGHT, MAX_WIDTH};
        // 合法上限帧
        assert_eq!(
            checked_frame_len(MAX_WIDTH, MAX_HEIGHT),
            Some((MAX_WIDTH * MAX_HEIGHT * BPP) as usize)
        );
        // 任一维度超限即拒绝
        assert_eq!(checked_frame_len(MAX_WIDTH + 1, MAX_HEIGHT), None);
        assert_eq!(checked_frame_len(MAX_WIDTH, MAX_HEIGHT + 1), None);
        // u32 乘法必回绕的极端输入：debug 下也不得 panic
        assert_eq!(checked_frame_len(u32::MAX, u32::MAX), None);
        assert_eq!(checked_frame_len(65535, 65535), None);
    }
}
