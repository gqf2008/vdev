//! 环形缓冲：虚拟扬声器写入、虚拟麦克风读取（输出环回输入）
#![allow(clippy::missing_errors_doc)]

use core::sync::atomic::{AtomicUsize, Ordering};

/// SPSC 环形缓冲（扬声器→麦克风环回）。
/// 内核环境：单写者（WaveRT render 流）+ 单读者（WaveRT capture 流），
/// 用原子索引 + 顺序一致性保证无锁正确性。
///
/// 并发约定（索引所有权，审查 S1 修复）：
/// - `write` 索引只有 render 写者推进（单写者 load/store）；
/// - `read` 索引可能由两端推进（capture 正常读、render 满载丢最旧），
///   但两端一律用 `fetch_max` 单调推进，**绝不回退**——修复前写者
///   `drop_oldest` 与读者 `read` 对同一索引各自 load→store，读者可
///   覆盖写者的推进，read 回退并与独立的 count 永久脱同步（count 还
///   可能 fetch_sub 下溢回绕），环回流结构性损坏；
/// - `count` 不再独立存储，由 `write - read` 派生，从根上消除
///   「索引与计数脱同步」这一类错误（`read <= write` 恒成立，见下）；
/// - 数据面仍是尽力而为的环回链路：满载丢最旧的窗口内，读者可能拷到
///   写者正在覆盖的字节（个别撕裂帧），但索引/计数永不错位。
///
/// `read <= write` 不变式：读者推进量 `n <= 其快照的可读量`、写者丢弃量
/// `m <= 其快照的可读量`，两者的目标值都不超过当时的 `write`；`fetch_max`
/// 只会把 `read` 推向某个历史合法值中的最大者，故 `read` 永不越过 `write`。
pub struct RingBuffer {
    data: *mut u8,
    capacity: usize,
    /// 读位置（单调递增，取模得缓冲区偏移）
    read: AtomicUsize,
    /// 写位置（单调递增，取模得缓冲区偏移）；仅写者推进
    write: AtomicUsize,
}

// SAFETY: 索引全部经原子访问；data 指向调用方保证存活的存储，构造后不再变更
unsafe impl Sync for RingBuffer {}
unsafe impl Send for RingBuffer {}

impl RingBuffer {
    /// 用已分配内存创建环形缓冲（调用方提供非分页池内存）
    ///
    /// # Safety
    /// `storage` 必须指向 `capacity` 字节有效内存并存活于本对象生命周期。
    pub unsafe fn new(storage: *mut u8, capacity: usize) -> Self {
        debug_assert!(capacity > 0);
        Self {
            data: storage,
            capacity,
            read: AtomicUsize::new(0),
            write: AtomicUsize::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// 当前可读字节数（派生量：`write - read`；不变式保证不为负）
    fn count(&self) -> usize {
        self.write
            .load(Ordering::SeqCst)
            .saturating_sub(self.read.load(Ordering::SeqCst))
    }

    /// 可写字节数
    pub fn writable(&self) -> usize {
        self.capacity - self.count().min(self.capacity)
    }

    /// 可读字节数
    pub fn readable(&self) -> usize {
        self.count().min(self.capacity)
    }

    /// 写入 `src`（最多写满），返回实际写入字节数。
    ///
    /// 回绕切分（B2）：写入位距缓冲区尾部的剩余空间是
    /// `capacity - write % capacity`（旧实现误作 `write % capacity`，
    /// 跨回绕点时数据错位、丢序）。
    pub fn write(&self, src: &[u8]) -> usize {
        let n = self.writable().min(src.len());
        if n == 0 {
            return 0;
        }
        // write 只有本端（单写者）推进，load/store 即可
        let w = self.write.load(Ordering::SeqCst);
        let off = w % self.capacity;
        // SAFETY: off ∈ [0, capacity)；首段 off..capacity、次段 0..n-first 均在数据区内，
        // 数据区由调用方保证存活（单写者独占写路径）
        unsafe {
            let tail = core::slice::from_raw_parts_mut(self.data.add(off), self.capacity - off);
            let first = tail.len().min(n);
            tail[..first].copy_from_slice(&src[..first]);
            if first < n {
                let head = core::slice::from_raw_parts_mut(self.data, n - first);
                head.copy_from_slice(&src[first..n]);
            }
        }
        self.write.store(w + n, Ordering::SeqCst);
        n
    }

    /// 读取到 `dst`（最多读满），返回实际读取字节数。
    ///
    /// 回绕切分（B2）：与 `write` 同理，首段长度按 `capacity - read % capacity` 切。
    pub fn read(&self, dst: &mut [u8]) -> usize {
        let n = self.readable().min(dst.len());
        if n == 0 {
            return 0;
        }
        let r = self.read.load(Ordering::SeqCst);
        let off = r % self.capacity;
        // SAFETY: off ∈ [0, capacity)；首段 off..capacity、次段 0..n-first 均在数据区内
        unsafe {
            let tail = core::slice::from_raw_parts(self.data.add(off), self.capacity - off);
            let first = tail.len().min(n);
            dst[..first].copy_from_slice(&tail[..first]);
            if first < n {
                let head = core::slice::from_raw_parts(self.data, n - first);
                dst[first..n].copy_from_slice(head);
            }
        }
        // fetch_max：若写者在此期间丢最旧把 read 推得更远，保留较大值，
        // read 绝不回退（索引/计数不错位的关键，见模块注释）
        self.read.fetch_max(r + n, Ordering::SeqCst);
        n
    }

    /// 写入 `src`；写不下时丢弃最旧数据腾位后补写（render 满载策略，M3）。
    /// 返回实际写入字节数（腾位足够时等于 `src.len()`）。
    pub fn write_drop_oldest(&self, src: &[u8]) -> usize {
        let written = self.write(src);
        if written < src.len() {
            let shortfall = src.len() - written;
            self.drop_oldest(shortfall);
            return written + self.write(&src[written..]);
        }
        written
    }

    /// 丢弃最旧 `n` 字节（render 满载腾位用；单调推进 read 索引）
    pub fn drop_oldest(&self, n: usize) {
        let n = n.min(self.readable());
        if n == 0 {
            return;
        }
        let r = self.read.load(Ordering::SeqCst);
        self.read.fetch_max(r + n, Ordering::SeqCst);
    }

    /// 读取到 `dst`；环内数据不足时尾部补零（capture 欠载策略，M3）。
    /// 返回实际读取字节数（补零部分不计入）。
    pub fn read_zero_fill(&self, dst: &mut [u8]) -> usize {
        let got = self.read(dst);
        if got < dst.len() {
            dst[got..].fill(0);
        }
        got
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

    /// B2 回归：写跨越回绕点——尾部剩 5 字节空间写 6 字节必须 5+1 切分
    #[test]
    fn write_splits_across_tail_boundary() {
        let mut storage = [0u8; 8];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 8) };
        assert_eq!(rb.write(&[1, 2, 3]), 3);
        let mut one = [0u8; 1];
        assert_eq!(rb.read(&mut one), 1);
        assert_eq!(one[0], 1);
        // 写 6 字节：尾部仅剩 5 → 10..15 填 3..8，15 回绕到 0
        let payload: [u8; 6] = [10, 11, 12, 13, 14, 15];
        assert_eq!(rb.write(&payload), 6);
        let mut out = [0u8; 8];
        assert_eq!(rb.read(&mut out), 8);
        assert_eq!(&out, &[2, 3, 10, 11, 12, 13, 14, 15]);
    }

    /// B2 回归：读跨越回绕点——read 索引在 6 处，可读数据横跨尾部
    #[test]
    fn read_splits_across_tail_boundary() {
        let mut storage = [0u8; 8];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 8) };
        assert_eq!(rb.write(&(1..=8).collect::<Vec<u8>>()), 8);
        let mut out = [0u8; 6];
        assert_eq!(rb.read(&mut out), 6);
        assert_eq!(&out, &[1, 2, 3, 4, 5, 6]);
        assert_eq!(rb.write(&[9, 10, 11]), 3);
        // 可读数据跨尾：位置 6,7 = 7,8；位置 0..3 = 9,10,11
        let mut out2 = [0u8; 5];
        assert_eq!(rb.read(&mut out2), 5);
        assert_eq!(&out2, &[7, 8, 9, 10, 11]);
    }

    /// B2 回归：整圈回绕（读 4 后写满 8，写切 4+4 两段）
    #[test]
    fn full_capacity_write_after_partial_read() {
        let mut storage = [0u8; 8];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 8) };
        assert_eq!(rb.write(&[1, 2, 3, 4]), 4);
        let mut out = [0u8; 4];
        assert_eq!(rb.read(&mut out), 4);
        assert_eq!(rb.write(&[5, 6, 7, 8, 9, 10, 11, 12]), 8);
        let mut out2 = [0u8; 8];
        assert_eq!(rb.read(&mut out2), 8);
        assert_eq!(&out2, &[5, 6, 7, 8, 9, 10, 11, 12]);
    }

    /// M3 回归：满载写入丢最旧数据
    #[test]
    fn write_drop_oldest_overwrites_when_full() {
        let mut storage = [0u8; 8];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 8) };
        assert_eq!(rb.write(&[1, 2, 3, 4, 5, 6, 7, 8]), 8);
        assert_eq!(rb.write(&[9]), 0); // 满载直写失败
        assert_eq!(rb.write_drop_oldest(&[9, 10]), 2);
        let mut out = [0u8; 8];
        assert_eq!(rb.read(&mut out), 8);
        assert_eq!(&out, &[3, 4, 5, 6, 7, 8, 9, 10]);
    }

    /// M3 回归：欠载读取补零
    #[test]
    fn read_zero_fill_pads_underrun() {
        let mut storage = [0u8; 8];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), 8) };
        assert_eq!(rb.write(&[7, 8, 9]), 3);
        let mut out = [0xFFu8; 6];
        assert_eq!(rb.read_zero_fill(&mut out), 3);
        assert_eq!(&out, &[7, 8, 9, 0, 0, 0]);
        // 空读全部补零
        let mut out2 = [0xFFu8; 2];
        assert_eq!(rb.read_zero_fill(&mut out2), 0);
        assert_eq!(&out2, &[0, 0]);
    }

    struct XorShift(u64);

    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    /// B2 性质测试：任意写/读序列 vs 线性参考实现（宿主 PRNG，固定种子可复现）
    #[test]
    fn property_matches_linear_reference() {
        const CAP: usize = 13;
        let mut storage = [0u8; CAP];
        let rb = unsafe { RingBuffer::new(storage.as_mut_ptr(), CAP) };
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let mut reference: Vec<u8> = Vec::new();
        let mut seq: u8 = 0;
        for _ in 0..4096 {
            if rng.next() & 1 == 0 || reference.is_empty() {
                // 写入 1..=CAP 字节
                let k = (rng.next() % CAP as u64) as usize + 1;
                let payload: Vec<u8> = (0..k)
                    .map(|_| {
                        seq = seq.wrapping_add(1);
                        seq
                    })
                    .collect();
                let expect = (CAP - reference.len()).min(k);
                assert_eq!(rb.write(&payload), expect);
                reference.extend_from_slice(&payload[..expect]);
            } else {
                // 读取 1..=len 字节并逐字节比对
                let k = (rng.next() % reference.len() as u64) as usize + 1;
                let mut dst = vec![0u8; k];
                let got = rb.read(&mut dst);
                assert_eq!(got, k);
                assert_eq!(&dst[..got], &reference[..got]);
                reference.drain(..got);
            }
            assert_eq!(rb.readable(), reference.len());
            assert_eq!(rb.writable(), CAP - reference.len());
        }
    }

    /// S1 回归：无满载压力的并发读写必须无损有序。修复前读者 store 与
    /// 写者 store 对同一 read 索引互相覆盖可致回退/脱同步；现 read 单调不减。
    #[test]
    fn concurrent_lossless_when_not_full() {
        const CAP: usize = 256;
        const TOTAL: usize = 200_000;
        let mut storage = [0u8; CAP];
        let rb = std::sync::Arc::new(unsafe { RingBuffer::new(storage.as_mut_ptr(), CAP) });

        let producer = {
            let rb = std::sync::Arc::clone(&rb);
            std::thread::spawn(move || {
                let mut next: u64 = 0;
                while next < TOTAL as u64 {
                    let k = ((next % 61) as usize + 1).min(TOTAL - next as usize);
                    let payload: Vec<u8> = (0..k).map(|i| (next + i as u64) as u8).collect();
                    // 只在放得下时写（不触发丢最旧），放不下自旋
                    let mut off = 0;
                    while off < k {
                        let w = rb.write(&payload[off..]);
                        if w == 0 {
                            std::hint::spin_loop();
                        }
                        off += w;
                    }
                    next += k as u64;
                }
            })
        };

        let consumer = {
            let rb = std::sync::Arc::clone(&rb);
            std::thread::spawn(move || {
                let mut expect: u64 = 0;
                let mut out = [0u8; 64];
                while expect < TOTAL as u64 {
                    let n = rb.read(&mut out);
                    if n == 0 {
                        std::hint::spin_loop();
                        continue;
                    }
                    for &b in &out[..n] {
                        assert_eq!(b, expect as u8, "sample {expect} out of order");
                        expect += 1;
                    }
                }
            })
        };

        producer.join().unwrap();
        consumer.join().unwrap();
        assert_eq!(rb.readable(), 0);
    }

    /// S1 回归：并发 + 持续满载丢最旧压力下不变式必须始终成立——
    /// `read <= write`、`readable() <= capacity`（修复前 count 独立存储，
    /// 双写者交错可让 count 下溢回绕成 usize::MAX，随后 readable/writable
    /// 下溢 panic 或回绕）。
    #[test]
    fn concurrent_drop_oldest_keeps_invariants() {
        const CAP: usize = 64;
        let mut storage = [0u8; CAP];
        let rb = std::sync::Arc::new(unsafe { RingBuffer::new(storage.as_mut_ptr(), CAP) });

        let producer = {
            let rb = std::sync::Arc::clone(&rb);
            std::thread::spawn(move || {
                let mut seq: u8 = 0;
                for _ in 0..50_000 {
                    seq = seq.wrapping_add(1);
                    // 每次写 32 字节：容量 64 → 高频触发 write_drop_oldest
                    let payload = [seq; 32];
                    rb.write_drop_oldest(&payload);
                }
            })
        };

        let consumer = {
            let rb = std::sync::Arc::clone(&rb);
            std::thread::spawn(move || {
                let mut out = [0u8; 24];
                for _ in 0..50_000 {
                    rb.read(&mut out);
                }
            })
        };

        producer.join().unwrap();
        consumer.join().unwrap();
        // 收尾断言不变式：压力过程中 readable/writable 的每次调用都走同一
        // 派生计算，若曾回绕/脱同步，这里必炸
        let r = rb.read.load(Ordering::SeqCst);
        let w = rb.write.load(Ordering::SeqCst);
        assert!(r <= w, "read ({r}) must never pass write ({w})");
        assert!(
            rb.readable() <= CAP,
            "readable {} > capacity {CAP}",
            rb.readable()
        );
        assert_eq!(rb.readable() + rb.writable(), CAP);
    }
}
