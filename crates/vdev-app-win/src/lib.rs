//! vdev-app-win 纯逻辑模块（无 Windows / Slint 依赖，可在任何宿主单测）。
//!
//! 单独拆出 lib 目标的原因：bin（main.rs）依赖 vdev-camera-win（windows-rs
//! API）与 slint，两者均无法在 macOS 宿主编译，宿主 `cargo test` 连 bin 都
//! 构建不了；纯逻辑抽到这里才能 `cargo test --lib` 真正跑通。
//! Slint UI 与 Windows 推流/HID 路径无法宿主单测（组件需事件循环、推流依赖
//! Windows 共享内存通道），只能在 Windows 实机验证。

/// 推流开关动作：一次「开始/停止推流」按钮点击对应的转移（M4 状态机）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushAction {
    /// 当前未推流 → 开始推流。调用方须先占用 PUSH_RUNNING；
    /// 启动失败路径（打不开通道 / 线程创建失败）必须复位标志，
    /// 否则标志滞留 true，下次点击被当成「停止」，按钮卡死无法再次启动。
    Start,
    /// 当前正在推流 → 停止推流。复位 PUSH_RUNNING 并等待推流线程退出。
    Stop,
}

/// 推流状态机决策（纯函数）：按 PUSH_RUNNING 当前值决定本次点击的动作。
///
/// UI 线程串行处理按钮点击，故「读当前状态 → 得出动作」无需加锁；
/// 标志本身的占用/复位由调用方用 `PUSH_RUNNING.swap(true, …)` 原子完成。
pub fn next_push_action(running: bool) -> PushAction {
    if running {
        PushAction::Stop
    } else {
        PushAction::Start
    }
}

pub mod dsp_ui;

#[cfg(test)]
mod tests {
    use super::*;

    /// M4 回归：空闲态点击 → 开始推流。
    #[test]
    fn idle_click_yields_start() {
        assert_eq!(next_push_action(false), PushAction::Start);
    }

    /// M4 回归：运行态点击 → 停止推流（停止即复位标志）。
    #[test]
    fn running_click_yields_stop() {
        assert_eq!(next_push_action(true), PushAction::Stop);
    }
}
