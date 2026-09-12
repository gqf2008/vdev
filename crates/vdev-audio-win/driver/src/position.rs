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

/// 环形 DMA 跨度切分：从 `start`（按 `size` 取模后）起处理 `n` 字节，
/// 返回 `(起始偏移, 首段长度)`，尾段长度 = `n - 首段长度`（从偏移 0 续处理）。
/// `size == 0` 时返回 `(0, 0)`（调用方应先行剔除，此处仅防御）。
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
