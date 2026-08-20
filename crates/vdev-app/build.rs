fn main() {
    let config = slint_build::CompilerConfiguration::new()
        .with_library_paths(slint_pixel::library_paths());
    slint_build::compile_with_config("ui/main.slint", config).expect("Slint UI 编译失败");
}
