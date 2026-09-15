//! GetPosition 时间锚点推进的纯数学（B4）。
//!
//! 本模块不做任何内核调用、无条件编译进 crate——因此可在 macOS 宿主跑
//! `#[test]`；`miniport.rs`（kernel-gated，无法宿主单测）负责调用这些函数
//! 并处理 QPC 读取与 DMA/环形缓冲副作用。

/// 把性能计数器时间增量换算为应推进的字节数：`delta_ticks / freq * bytes_per_sec`。
///
/// `freq` 或 `bytes_per_sec` 为 0 时返回 0（防御除零）；极端乘法用 saturating 兜底。
pub const fn bytes_for_interval(delta_ticks: u64, freq: u64, bytes_per_sec: u64) -> u64 {
    if freq == 0 || bytes_per_sec == 0 {
        return 0;
    }
    delta_ticks.saturating_mul(bytes_per_sec) / freq
}

/// 把「本次应推进的字节数」向下对齐到整帧（`block_align` 字节）。
///
/// **P0：字节推进必须落在帧栅格上**（环回 ×19 / 幅度翻倍缺陷的根因）。
///
/// WaveRT 的 DMA 缓冲与环回环形缓冲都是**字节**地址空间，而音频引擎只按
/// `block_align` 字节的**帧**栅格读写缓冲。`bytes_for_interval()` 按 QPC 时间
/// 换算出的字节数天然是任意整数（例如 10 MHz QPC 下 1 ms @192000 B/s = 192，
/// 但 0.5 ms 就是 96、7.3 ms 是 1401——不是 4 的倍数）。旧实现直接拿这个数推进
/// `last_processed`，于是：
///
/// 1. `off = last_processed % dma_size` 不再是帧对齐的缓冲偏移，驱动从 DMA
///    缓冲的**半个样本中间**开始搬运字节；
/// 2. 环形缓冲的读写索引同样带上这一字节相位；
/// 3. 引擎再按 16bit 立体声帧栅格解码时，样本边界与写入时的边界错开 1 字节
///    （高/低字节互换），整段环回被"解码错位"。
///
/// 实测特征（修复前，vdev 扬声器注入 1 kHz → vdev 麦克风采集）：坏样本与
/// 正确样本**逐字节同源**、仅相差固定 1 字节相位（按正确字节流在偏移 1/3 处
/// 解码即可完美重建，残差 ~0.7%）；听感上幅度翻倍、频谱被高频伪造分量占据
/// （1 kHz → 19 kHz 之类的"×19"是错位非线性变换的产物，不是变速）。
///
/// 对齐后再由 QPC 锚点（`anchor_position`）在下一轮补齐欠账，因此不会累积漂移；
/// `block_align == 0`（格式未设定）时保守地原样返回。
pub const fn frame_aligned_advance(bytes: u64, block_align: u32) -> u64 {
    if block_align == 0 {
        return bytes;
    }
    bytes - (bytes % block_align as u64)
}

/// 环形 DMA 跨度切分：从 `start`（按 `size` 取模后）起处理 `n` 字节，
/// 返回 `(起始偏移, 首段长度)`，尾段长度 = `n - 首段长度`（从偏移 0 续处理）。
/// `size == 0` 时返回 `(0, 0)`（调用方应先行剔除，此处仅防御）。
///
/// 调用方保证 `start`（及 `n`）是 `block_align` 的整数倍时，`off`/`first`
/// 同样落在帧栅格上——配合 `frame_aligned_advance()` 使 DMA 缓冲与环形
/// 缓冲的字节相位永不漂移。
pub const fn split_ring_span(start: usize, n: usize, size: usize) -> (usize, usize) {
    if size == 0 {
        return (0, 0);
    }
    let off = start % size;
    // Ord::min 尚未 const 化，用 if 折叠
    let first = if size - off < n { size - off } else { n };
    (off, first)
}

/// 流位置对 DMA 环取模（B4：PlayOffset/WriteOffset 是环内偏移，不是累计字节）。
/// `dma_size == 0`（缓冲未分配）时返回 0。
pub const fn mod_position(position: u64, dma_size: u32) -> u64 {
    if dma_size == 0 {
        return 0;
    }
    position % (dma_size as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 48kHz × 2ch × 16bit = 192000 B/s；QPC 典型 10 MHz：100_000 tick = 10 ms
    #[test]
    fn bytes_for_interval_10ms_at_48k_16bit_stereo() {
        assert_eq!(bytes_for_interval(100_000, 10_000_000, 192_000), 1_920);
        // 1 ms → 192 字节
        assert_eq!(bytes_for_interval(10_000, 10_000_000, 192_000), 192);
    }

    #[test]
    fn bytes_for_interval_guards() {
        assert_eq!(bytes_for_interval(1_000, 0, 192_000), 0);
        assert_eq!(bytes_for_interval(1_000, 10_000_000, 0), 0);
        assert_eq!(bytes_for_interval(0, 10_000_000, 192_000), 0);
    }

    #[test]
    fn bytes_for_interval_saturates() {
        // saturating 兜底：不 panic、不回绕
        assert_eq!(bytes_for_interval(u64::MAX, 1, u64::MAX), u64::MAX);
    }

    /// P0 回归：`bytes_for_interval` 的结果天然**不是**整帧——旧实现直接拿它
    /// 推进 `last_processed`，字节相位因此脱离引擎的帧栅格（环回被解码错位）。
    #[test]
    fn bytes_for_interval_is_not_frame_aligned() {
        // 7.3 ms @ 192000 B/s = 1401 字节（4 字节/帧 → 余 1）
        assert_eq!(bytes_for_interval(73_000, 10_000_000, 192_000), 1_401);
        assert_ne!(1_401 % 4, 0);
        // 0.5 ms → 96（是 4 的倍数，说明"有时对齐"正是 ~50% 好/坏样本的来源）
        assert_eq!(bytes_for_interval(5_000, 10_000_000, 192_000), 96);
    }

    /// P0 回归：`frame_aligned_advance` 向下取整，且**任意**推进序列之后位置
    /// 始终落在帧栅格上（这是 DMA 缓冲偏移 `last_processed % dma_size` 与
    /// 环形缓冲读写索引不再漂移的充分条件）。
    #[test]
    fn frame_aligned_advance_keeps_positions_on_frame_grid() {
        const BLOCK: u32 = 4;
        assert_eq!(frame_aligned_advance(1_401, BLOCK), 1_400);
        assert_eq!(frame_aligned_advance(3, BLOCK), 0);
        assert_eq!(frame_aligned_advance(0, BLOCK), 0);
        assert_eq!(frame_aligned_advance(1_920, BLOCK), 1_920);
        assert_eq!(frame_aligned_advance(5, 0), 5); // 无帧概念：保守不改

        // 模拟一串真实 QPC 换算量（含各种非整帧值）连续推进
        let mut pos = 0u64;
        for delta in [7u64, 1_401, 1, 3, 1_920, 19, 96, 100_003, 4] {
            pos += frame_aligned_advance(delta, BLOCK);
            assert_eq!(pos % u64::from(BLOCK), 0, "delta={delta} 后位置脱离帧栅格");
        }
        // 未对齐的旧实现会脱栅格（累加任一非整帧量即脱栅格）
        let mut old = 0u64;
        for delta in [7u64, 1_401, 1, 3, 1_920, 19, 96, 100_003, 4] {
            old += delta;
        }
        assert_ne!(old % u64::from(BLOCK), 0);
    }

    /// P0 回归：DMA 窗口偏移与分片长度在整帧推进下同为整帧倍数——
    /// 引擎按 16bit 立体声帧栅格解码时不会跨到相邻样本的字节上。
    #[test]
    fn split_ring_span_keeps_frame_grid_when_inputs_aligned() {
        const BLOCK: u32 = 4;
        const DMA: usize = 8_192; // 引擎请求的缓冲（整帧）
        let mut pos = 0u64;
        for delta in [1_401u64, 1_920, 19, 96, 100_003] {
            let n = frame_aligned_advance(delta, BLOCK) as usize;
            let (off, first) = split_ring_span(pos as usize, n, DMA);
            assert_eq!(off % 4, 0);
            assert_eq!(first % 4, 0);
            assert_eq!((n - first) % 4, 0);
            pos += n as u64;
            assert_eq!((pos as usize % DMA) % 4, 0);
        }
    }

    #[test]
    fn split_ring_span_cases() {
        assert_eq!(split_ring_span(0, 4, 8), (0, 4));
        assert_eq!(split_ring_span(6, 8, 8), (6, 2)); // 尾 2 + 头 6
        assert_eq!(split_ring_span(7, 20, 8), (7, 1)); // 尾 1 + 头 19
        assert_eq!(split_ring_span(3, 0, 8), (3, 0));
        assert_eq!(split_ring_span(0, 5, 0), (0, 0)); // size==0 防御
    }

    #[test]
    fn mod_position_cases() {
        assert_eq!(mod_position(10, 0), 0);
        assert_eq!(mod_position(192_000 + 5, 192_000), 5);
        assert_eq!(mod_position(u64::MAX, 4), 3);
    }
}
