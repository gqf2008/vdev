//! vdev-camera — 虚拟摄像头（Rust 核心）。
//!
//! 阶段 1：帧生成核心 + C ABI（本 crate）。
//! 阶段 2：`dal/` ObjC++ 薄壳实现 CoreMediaIO DAL 插件，帧从本 crate 来。

pub mod cabi;
pub mod frame;

pub use frame::{Frame, FramePattern};
