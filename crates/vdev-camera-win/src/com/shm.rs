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
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForSingleObject};
use windows_core::PCWSTR;

/// 命名共享内存。
pub const SHM_NAME: &str = "Local\\vdev-camera-win-frames";
/// 命名事件（新帧信号）。
pub const EVENT_NAME: &str = "Local\\vdev-camera-win-frame-event";

const MAGIC: u32 = 0x5644_4556; // "VDEV" 小端
const BPP: u32 = 4; // BGRA
/// 通道容量上限（1920x1080x4）。
const MAX_WIDTH: u32 = 1920;
const MAX_HEIGHT: u32 = 1080;
const MAX_BUF: usize = (MAX_WIDTH * MAX_HEIGHT * BPP) as usize;
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
            unsafe { CloseHandle(mapping) }.ok();
            return Err(err);
        }

        let event_name = to_wide(EVENT_NAME);
        // SAFETY: 命名事件（auto-reset），初始无信号，名称在调用期间存活。
        let event = unsafe { CreateEventW(None, false, false, PCWSTR(event_name.as_ptr())) }
            .map_err(|e| {
                unsafe { UnmapViewOfFile(view) }.ok();
                unsafe { CloseHandle(mapping) }.ok();
                io::Error::from_raw_os_error(e.code().0)
            })?;

        // 首个创建者写入魔数。
        let header = unsafe { &mut *view.Value.cast::<Header>() };
        if header.magic != MAGIC {
            header.magic = MAGIC;
            header.width = 0;
            header.height = 0;
            header.stride = 0;
            header.buf_len = 0;
            header.seq = AtomicU32::new(0);
            header.ready = AtomicU32::new(0);
        }

        Ok(Self {
            mapping,
            event,
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
        let expected = (width * height * BPP) as usize;
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
        if width > MAX_WIDTH || height > MAX_HEIGHT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("frame too large: {width}x{height} (max {MAX_WIDTH}x{MAX_HEIGHT})"),
            ));
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
        Ok(())
    }

    /// 消费者：取最新一帧；从未发布过返回 `None`。
    pub fn latest(&self, out: &mut Vec<u8>) -> Option<(u32, u32)> {
        let header = self.header();
        if header.ready.load(Ordering::Acquire) == 0 {
            return None;
        }
        let seq = header.seq.load(Ordering::Acquire);
        let width = header.width;
        let height = header.height;
        let buf_len = header.buf_len as usize;
        if width == 0 || height == 0 || buf_len == 0 || buf_len > MAX_BUF {
            return None;
        }
        out.resize(buf_len, 0);
        out.copy_from_slice(&self.buffer(seq & 1)[..buf_len]);
        Some((width, height))
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
        }
    }
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn os_error(e: windows_core::Error) -> io::Error {
    io::Error::from_raw_os_error(e.code().0)
}
