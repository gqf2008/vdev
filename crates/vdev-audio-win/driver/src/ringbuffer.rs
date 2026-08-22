//! 环形缓冲：虚拟扬声器写入、虚拟麦克风读取（输出环回输入）
#![allow(clippy::missing_errors_doc)]

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicUsize, Ordering};

/// SPSC 环形缓冲（扬声器→麦克风环回）。
/// 内核环境：单写者（WaveRT render 流）+ 单读者（WaveRT capture 流），
/// 用原子索引 + 顺序一致性保证无锁正确性。
pub struct RingBuffer {
    data: UnsafeCell<&'static mut [u8]>,
    capacity: usize,
    read: AtomicUsize,
    write: AtomicUsize,
    count: AtomicUsize,
}

// SAFETY: 通过原子索引访问，单写单读，跨线程安全
unsafe impl Sync for RingBuffer {}
unsafe impl Send for RingBuffer {}

impl RingBuffer {
    /// 用已分配内存创建环形缓冲（调用方提供非分页池内存）
    ///
    /// # Safety
    /// `storage` 必须指向 `capacity` 字节有效内存并存活于本对象生命周期。
    pub unsafe fn new(storage: *mut u8, capacity: usize) -> Self {
        // SAFETY: 调用方保证内存有效
        let slice = unsafe { core::slice::from_raw_parts_mut(storage, capacity) };
        Self {
            // SAFETY: 静态生命周期是为了让 SPSC 双指针安全共享
            data: UnsafeCell::new(unsafe {
                core::mem::transmute::<&mut [u8], &'static mut [u8]>(slice)
            }),
            capacity,
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
            count: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }

    /// 可写字节数
    pub fn writable(&self) -> usize {
        self.capacity - self.count()
    }

    /// 可读字节数
    pub fn readable(&self) -> usize {
        self.count()
    }

    /// 写入 `src`（最多写满），返回实际写入字节数
    pub fn write(&self, src: &[u8]) -> usize {
        let writable = self.writable();
        let n = writable.min(src.len());
        if n == 0 {
            return 0;
        }
        // SAFETY: 单写者；write 索引与 data 访问受原子保护
        let data = unsafe { &mut *self.data.get() };
        let w = self.write.load(Ordering::SeqCst);
        let first = (w % self.capacity).min(n);
        data[w % self.capacity..w % self.capacity + first].copy_from_slice(&src[..first]);
        if first < n {
            data[..n - first].copy_from_slice(&src[first..n]);
        }
        self.write.store((w + n) % self.capacity, Ordering::SeqCst);
        self.count.fetch_add(n, Ordering::SeqCst);
        n
    }

    /// 读取到 `dst`（最多读满），返回实际读取字节数
    pub fn read(&self, dst: &mut [u8]) -> usize {
        let readable = self.readable();
        let n = readable.min(dst.len());
        if n == 0 {
            return 0;
        }
        // SAFETY: 单读者；read 索引与 data 访问受原子保护
        let data = unsafe { &mut *self.data.get() };
        let r = self.read.load(Ordering::SeqCst);
        let first = (r % self.capacity).min(n);
        dst[..first].copy_from_slice(&data[r % self.capacity..r % self.capacity + first]);
        if first < n {
            dst[first..n].copy_from_slice(&data[..n - first]);
        }
        self.read.store((r + n) % self.capacity, Ordering::SeqCst);
        self.count.fetch_sub(n, Ordering::SeqCst);
        n
    }

    /// 丢弃 `n` 字节（麦克风端跳过）
    pub fn discard(&self, n: usize) {
        let n = n.min(self.readable());
        let r = self.read.load(Ordering::SeqCst);
        self.read.store((r + n) % self.capacity, Ordering::SeqCst);
        self.count.fetch_sub(n, Ordering::SeqCst);
    }

    /// 清空
    pub fn reset(&self) {
        self.read.store(0, Ordering::SeqCst);
        self.write.store(0, Ordering::SeqCst);
        self.count.store(0, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_read_roundtrip() {
        let mut storage = [0u8; 64];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 64) };
        let payload = b"hello vdev audio";
        assert_eq!(rb.write(payload), payload.len());
        let mut out = [0u8; 64];
        let n = rb.read(&mut out);
        assert_eq!(n, payload.len());
        assert_eq!(&out[..n], payload);
    }

    #[test]
    fn wraps_around() {
        let mut storage = [0u8; 8];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 8) };
        // 填满
        assert_eq!(rb.write(&[1, 2, 3, 4]), 4);
        assert_eq!(rb.write(&[5, 6, 7, 8]), 4);
        // 已满
        assert_eq!(rb.write(&[9]), 0);
        // 读一半
        let mut out = [0u8; 4];
        assert_eq!(rb.read(&mut out), 4);
        assert_eq!(&out, &[1, 2, 3, 4]);
        // 写入可回绕
        assert_eq!(rb.write(&[9, 10, 11, 12]), 4);
        let mut out2 = [0u8; 8];
        assert_eq!(rb.read(&mut out2), 8);
        assert_eq!(&out2, &[5, 6, 7, 8, 9, 10, 11, 12]);
    }

    #[test]
    fn empty_read_zero() {
        let mut storage = [0u8; 16];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 16) };
        let mut out = [0u8; 8];
        assert_eq!(rb.read(&mut out), 0);
    }
}
