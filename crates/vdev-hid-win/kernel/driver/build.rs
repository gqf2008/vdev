//! KMDF 驱动构建（官方 windows-drivers-rs 模式）
fn main() -> Result<(), wdk_build::ConfigError> {
    wdk_build::configure_wdk_binary_build()?;
    // VHF（Virtual HID Framework，vhf.sys）的入口不在 wdf 函数表里，而是独立导入库
    // VhfKm.lib（WDK: Lib\<ver>\km\x64\VhfKm.lib）。wdk-build 默认不链接它，
    // 其 LIBPATH 已由 wdk-build 设到该目录，这里补一个库名即可。
    println!("cargo:rustc-link-arg=VhfKm.lib");
    Ok(())
}
