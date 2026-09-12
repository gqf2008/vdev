//! 共享帧通道的纯逻辑裁决（**零 Windows 依赖**，可在 macOS 宿主单测）。
//!
//! [`super::shm`] 的跨进程路径必须调用 Win32 API（共享内存/命名互斥体），
//! 进程内单测需要 Windows 运行时。本模块把其中的**决策逻辑**抽成纯函数：
//!
//! - [`judge_lock_wait`]：publish 锁等待结果裁决（含 WAIT_ABANDONED 接管语义，修 B1）；
//! - [`judge_seqlock_read`]：seqlock 单轮读取裁决（撕裂重读 / 无效判空，修 M2；
//!   `seq_after` 须在负载拷贝完成后采样，序号括弧覆盖头部+负载，修 M2b）。
//!
//! 裸等待码常量取自 windows 0.62.2 绑定（`Win32::Foundation`：WAIT_OBJECT_0=0、
//! WAIT_ABANDONED=128、WAIT_TIMEOUT=258），并由 `shm.rs` 的
//! `channel_logic_wait_codes_match_bindings` 测试与绑定常量互检（改错即红）。
//!
//! 宿主验证：本文件零依赖，可独立编译运行其 `#[test]`：
//! `rustc --edition 2021 --test src/com/channel_logic.rs && ./channel_logic`

/// `WAIT_OBJECT_0` 裸码（与 windows 绑定一致，互检见 `shm.rs` 测试）。
pub const WAIT_OBJECT_0_CODE: u32 = 0;
/// `WAIT_ABANDONED` 裸码（0x80）：上一持锁线程所在进程已退出。
pub const WAIT_ABANDONED_CODE: u32 = 0x80;
/// `WAIT_TIMEOUT` 裸码（0x102）。
pub const WAIT_TIMEOUT_CODE: u32 = 0x102;

/// publish 锁单次等待上限（毫秒）。
///
/// 正常持锁窗口是微秒级（槽内 memcpy + 序号发布）；超时说明持锁方异常
/// （挂起/死锁），放弃本次发布、由下一帧自然重试，避免推流线程永久挂死
/// （原 INFINITE 在互斥体被异常持有时无界阻塞）。
pub const LOCK_WAIT_MS: u32 = 500;

/// seqlock 重读上限（防活锁）。
///
/// 写方按帧率发布（约 30~120Hz），单轮读取是微秒级拷贝；连续多轮撕裂
/// 意味着写方异常，超限按「无帧」返回，由调用方回退测试图案。
pub const MAX_SEQLOCK_ATTEMPTS: u32 = 8;

/// [`judge_lock_wait`] 的裁决结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockWaitVerdict {
    /// 正常获得锁（WAIT_OBJECT_0）。
    Acquired,
    /// 获得锁，但上一持锁者所在进程已退出（WAIT_ABANDONED）：内核已把所有权
    /// 移交给本次等待，必须继续持有并正常 Release；锁保护的数据可能被崩溃
    /// 写撕坏，由读方 seqlock 序号裁决兜底丢弃。
    AcquiredAbandoned,
    /// 超时未获得锁（WAIT_TIMEOUT）：本次发布放弃。
    TimedOut,
    /// 其他等待错误（WAIT_FAILED 等）。
    Failed,
}

/// 裁决 `WaitForSingleObject` 的返回码（传裸码 `code.0`）。
pub fn judge_lock_wait(code: u32) -> LockWaitVerdict {
    match code {
        WAIT_OBJECT_0_CODE => LockWaitVerdict::Acquired,
        // 修 B1 核心语义：abandoned mutex 必须能被接管，绝不能当失败处理——
        // 否则持锁方崩溃后无人再获得互斥体，推帧永久失败（blocker）。
        WAIT_ABANDONED_CODE => LockWaitVerdict::AcquiredAbandoned,
        WAIT_TIMEOUT_CODE => LockWaitVerdict::TimedOut,
        _ => LockWaitVerdict::Failed,
    }
}

/// [`judge_seqlock_read`] 的裁决结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeqlockVerdict {
    /// 序号一致且头部自洽：采纳本轮数据。
    Accept,
    /// 序号一致但头部字段无效（发布方异常/崩溃残留的半更新头部）：
    /// 重读无意义，按「无帧」处理。
    NoFrame,
    /// 序号不一致（读取期间写方发布了新帧，读到撕裂数据）：重读。
    Retry,
}

/// 裁决一轮 seqlock 读取。
///
/// 经典 seqlock 语义（修 M2）：`seq_before`（读头部前采）与 `seq_after`
/// （**负载拷贝完成后**采，修 M2b）必须一致，否则本轮数据可能被并发写撕坏，
/// **必须重读**，不得使用旧读取值。序号括弧必须覆盖整个（头部+负载）读取：
/// `seq_after` 若在拷贝前采样，拷贝期间写方两次发布回到同一槽的撕裂（写方
/// 2 倍以上吞吐且读方被抢占）会漏检。序号一致时还需头部自洽才有效：
/// width/height/buf_len 非零、`buf_len <= max_buf`，且
/// `buf_len == width * height * 4`（BGRA 帧大小自洽；崩溃残留的半更新
/// 头部由此拦下）。`max_buf` 传共享内存单槽容量（shm 侧 `MAX_BUF`）。
pub fn judge_seqlock_read(
    seq_before: u32,
    seq_after: u32,
    width: u32,
    height: u32,
    buf_len: u32,
    max_buf: usize,
) -> SeqlockVerdict {
    if seq_before != seq_after {
        return SeqlockVerdict::Retry;
    }
    let consistent = width > 0
        && height > 0
        && buf_len > 0
        && buf_len as usize <= max_buf
        && buf_len as u64 == width as u64 * height as u64 * 4;
    if consistent {
        SeqlockVerdict::Accept
    } else {
        SeqlockVerdict::NoFrame
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── B1 回归：WAIT_ABANDONED 必须可接管 ──

    #[test]
    fn abandoned_mutex_is_taken_over_not_failed() {
        // 推帧方进程持锁崩溃 → 读方等到 WAIT_ABANDONED：内核已把所有权移交本次
        // 等待，必须按「已获得锁」继续；绝不能当失败（原 bug：当作失败返回后
        // 无人再接管互斥体 → 推帧永久失败/读方永久卡死）。
        assert_eq!(
            judge_lock_wait(WAIT_ABANDONED_CODE),
            LockWaitVerdict::AcquiredAbandoned
        );
        assert_ne!(
            judge_lock_wait(WAIT_ABANDONED_CODE),
            LockWaitVerdict::Failed
        );
    }

    #[test]
    fn normal_timeout_and_error_verdicts() {
        assert_eq!(
            judge_lock_wait(WAIT_OBJECT_0_CODE),
            LockWaitVerdict::Acquired
        );
        assert_eq!(
            judge_lock_wait(WAIT_TIMEOUT_CODE),
            LockWaitVerdict::TimedOut
        );
        // 非等待结果码（如 WAIT_IO_COMPLETION=0xC0、WAIT_FAILED=0xFFFFFFFF）→ Failed。
        assert_eq!(judge_lock_wait(0xC0), LockWaitVerdict::Failed);
        assert_eq!(judge_lock_wait(0xFFFF_FFFF), LockWaitVerdict::Failed);
    }

    // ── M2 回归：seqlock 撕裂必须重读，不得用旧读取值 ──

    #[test]
    fn stable_seq_with_valid_header_accepts() {
        assert_eq!(
            judge_seqlock_read(7, 7, 640, 480, 640 * 480 * 4, 1 << 21),
            SeqlockVerdict::Accept
        );
        // 1920x1080 恰好等于 MAX_BUF（8294400）时同样有效。
        assert_eq!(
            judge_seqlock_read(1, 1, 1920, 1080, 1920 * 1080 * 4, 1920 * 1080 * 4),
            SeqlockVerdict::Accept
        );
    }

    #[test]
    fn torn_read_requires_reread() {
        // 读前 seq=7、读后 seq=8：写方在读取中途发布了新帧 → 撕裂 → 重读。
        assert_eq!(
            judge_seqlock_read(7, 8, 640, 480, 640 * 480 * 4, 1 << 21),
            SeqlockVerdict::Retry
        );
        // 序号回绕（wrapping）方向同理：0xFFFF_FFFF → 0。
        assert_eq!(
            judge_seqlock_read(0xFFFF_FFFF, 0, 640, 480, 640 * 480 * 4, 1 << 21),
            SeqlockVerdict::Retry
        );
    }

    #[test]
    fn stable_but_invalid_header_is_noframe() {
        let cap = 1 << 21;
        // 序号稳定但字段无效：零宽/零高/零长度 → 无帧（重读无意义）。
        assert_eq!(
            judge_seqlock_read(3, 3, 0, 480, 480 * 4, cap),
            SeqlockVerdict::NoFrame
        );
        assert_eq!(
            judge_seqlock_read(3, 3, 640, 0, 640 * 4, cap),
            SeqlockVerdict::NoFrame
        );
        assert_eq!(
            judge_seqlock_read(3, 3, 640, 480, 0, cap),
            SeqlockVerdict::NoFrame
        );
        // buf_len 超过单槽容量 → 拒绝（防越界拷贝）。
        assert_eq!(
            judge_seqlock_read(3, 3, 640, 480, 640 * 480 * 4, 640 * 480 * 4 - 1),
            SeqlockVerdict::NoFrame
        );
        // 尺寸与字节数不自洽（崩溃残留的半更新头部）→ 拒绝。
        assert_eq!(
            judge_seqlock_read(3, 3, 640, 480, 320 * 240 * 4, cap),
            SeqlockVerdict::NoFrame
        );
    }

    /// 模拟 `latest()` 的重读循环：撕裂轮重读，稳定轮采纳。
    #[test]
    fn reread_loop_converges_on_stable_frame() {
        // 写方连发两帧（7→8→9），第三轮读稳定（9,9）→ 采纳，共 3 轮。
        let rounds: [(u32, u32); 3] = [(7, 8), (8, 9), (9, 9)];
        let mut attempts = 0;
        let mut verdict = SeqlockVerdict::Retry;
        for &(before, after) in &rounds {
            attempts += 1;
            verdict = judge_seqlock_read(before, after, 640, 480, 640 * 480 * 4, 1 << 21);
            if verdict != SeqlockVerdict::Retry {
                break;
            }
        }
        assert_eq!(attempts, 3);
        assert_eq!(verdict, SeqlockVerdict::Accept);
    }

    /// 模拟持续撕裂（写方异常/死循环发布）：达上限后判无帧，不活锁。
    #[test]
    fn reread_loop_gives_up_after_bound() {
        let mut accepted = false;
        let mut rounds = 0;
        for _ in 0..MAX_SEQLOCK_ATTEMPTS {
            rounds += 1;
            if judge_seqlock_read(1, 2, 640, 480, 640 * 480 * 4, 1 << 21) == SeqlockVerdict::Accept
            {
                accepted = true;
                break;
            }
        }
        assert!(!accepted);
        assert_eq!(rounds, MAX_SEQLOCK_ATTEMPTS);
    }

    // ── M2b 回归：拷贝期间双发布回到同一槽，必须重读 ──

    /// 修 M2b：读方拷贝负载期间写方连发两帧（7→8→9）回到同一槽（写方 2 倍
    /// 以上吞吐且读方被抢占）——第二次发布写回读者正在拷贝的槽，拷贝出的是
    /// 撕裂像素。`seq_after` 在拷贝完成后采样，观察到 (7, 9) → Retry → 重读；
    /// 修前 `seq_after` 在拷贝前采样只会看到 (7, 7) → Accept，撕裂帧被采纳
    /// （序号括弧未覆盖负载拷贝）。
    #[test]
    fn double_publish_during_payload_copy_requires_reread() {
        // 双发布回到同一槽：7 与 9 同奇偶（同一槽 1），8 是另一槽。
        assert_eq!(7 & 1, 9 & 1);
        assert_ne!(7 & 1, 8 & 1);
        assert_eq!(
            judge_seqlock_read(7, 9, 640, 480, 640 * 480 * 4, 1 << 21),
            SeqlockVerdict::Retry
        );
    }

    /// 修 M2b 的重读循环：撕裂轮 (7, 9) 重读，写方安静后 (9, 9) 稳定 → 采纳。
    /// 共 2 轮即收敛，远低于 [`MAX_SEQLOCK_ATTEMPTS`]（防活锁上限不被逼近，
    /// 超限判无帧的兜底语义不受影响）。
    #[test]
    fn double_publish_reread_converges_next_round() {
        let rounds: [(u32, u32); 2] = [(7, 9), (9, 9)];
        let mut attempts = 0;
        let mut verdict = SeqlockVerdict::Retry;
        for &(before, after) in &rounds {
            attempts += 1;
            verdict = judge_seqlock_read(before, after, 640, 480, 640 * 480 * 4, 1 << 21);
            if verdict != SeqlockVerdict::Retry {
                break;
            }
        }
        assert_eq!(attempts, 2);
        assert_eq!(verdict, SeqlockVerdict::Accept);
    }
}
