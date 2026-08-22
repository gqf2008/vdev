//! KMDF 驱动构建（官方 windows-drivers-rs 模式）
fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::configure_wdk_binary_build()
}
