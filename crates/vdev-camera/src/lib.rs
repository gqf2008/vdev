//! vdev-camera — 虚拟摄像头（Rust 核心）。
//!
//! 阶段 1：帧生成核心 + C ABI（本 crate）。
//! 阶段 2：CMIOExtension Swift 薄壳通过 `cabi.rs` 的 C 函数取帧（旧 DAL 插件已移除）。

pub mod cabi;
pub mod frame;

pub use frame::{Frame, FramePattern};
