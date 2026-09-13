//! 驱动侧共享契约：报告描述符与其布局断言来自 `contract.rs`（单一来源——
//! 用户态 crate 的 `src/report.rs` 也以 `#[path]` 共享同一文件并跑宿主单测）。
//!
//! VHF 驱动只需要两份报告描述符；`contract.rs` 里的 IOCTL 常量是 hidport/hidclass
//! 契约的回归锚点，由宿主侧单测消费，驱动侧不再使用（minidriver 路线已废弃）。

// contract.rs 自带 `#![allow(dead_code)]`（其 IOCTL 常量由宿主侧单测消费）
#[path = "contract.rs"]
mod contract;

pub use contract::{KEYBOARD_REPORT_DESCRIPTOR, MOUSE_REPORT_DESCRIPTOR};
