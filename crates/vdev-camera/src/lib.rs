//! vdev-camera — 虚拟摄像头（Rust 核心）。
//!
//! 阶段 1：帧生成核心 + C ABI（本 crate）。
//! 阶段 2：CMIOExtension 扩展（`crates/vdev-camera-ext`，100% Rust）直接实现 provider
//! 并调用本 crate 的 `cabi.rs` 取帧；旧的 Swift 薄壳与 DAL 插件均已移除。

pub mod cabi;
pub mod frame;

pub use frame::{Frame, FramePattern};
