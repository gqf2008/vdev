// PortCls 内核驱动链接配置。
// 仅在 `kernel` feature 下生效：默认 `cargo test`/`cargo build` 走普通 cdylib 链接，
// `cargo build --features kernel` 才产出真正的 .sys（DriverEntry 入口 / Native 子系统 / 无用户态 CRT）。

#[cfg(windows)]
use std::path::PathBuf;

fn main() {
    let kernel = std::env::var("CARGO_FEATURE_KERNEL").is_ok();
    if !kernel {
        return;
    }
    // 链接参数只对 Windows 目标有意义（CARGO_CFG_WINDOWS 反映目标平台）
    if std::env::var("CARGO_CFG_WINDOWS").is_err() {
        return;
    }
    // WDK 库目录探测依赖 Windows 注册表：build script 按宿主编译，winreg 依赖
    // 声明在 [target.'cfg(windows)'.build-dependencies]（仅 Windows 宿主参与编译），
    // 因此源码层再用 #[cfg(windows)] 门控，非 Windows 宿主交叉构建时由调用方
    // 经 RUSTFLAGS=-L 自备 WDK km/x64 库搜索路径。
    #[cfg(windows)]
    wdk_link_setup();

    println!("cargo:rustc-link-lib=ntoskrnl");
    println!("cargo:rustc-link-lib=portcls");
    println!("cargo:rustc-link-lib=ks");
    println!("cargo:rustc-link-lib=wmilib");
    println!("cargo:rustc-link-lib=stdunk");
    println!("cargo:rustc-link-lib=libcntpr");
    // hal.lib：KeQueryPerformanceCounter 由 hal 导出（ntoskrnl.lib 里没有），
    // 位置演算（miniport.rs 的 KSSTATE_RUN 时间锚点）要用到它；缺这一条链接报
    // LNK2019: unresolved external symbol KeQueryPerformanceCounter。
    println!("cargo:rustc-link-lib=hal");
    // 内核驱动 PE：入口 DriverEntry，WDM 子系统，不链接用户态 CRT
    println!("cargo:rustc-link-arg=/ENTRY:DriverEntry");
    println!("cargo:rustc-link-arg=/SUBSYSTEM:NATIVE");
    println!("cargo:rustc-link-arg=/DRIVER:WDM");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:libcmt");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:libucrt");
    println!("cargo:rustc-link-arg=/NODEFAULTLIB:libvcruntime");
    // /INTEGRITYCHECK：强制 PE 校验和，内核映像加载要求
    println!("cargo:rustc-link-arg=/INTEGRITYCHECK");
}

/// 仅 Windows 宿主编译：从注册表读 WDK 根目录并取最高版本 SDK 的 km\x64 库目录
#[cfg(windows)]
fn wdk_link_setup() {
    let kits_root: String = {
        let hklm = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE);
        hklm.open_subkey(r"SOFTWARE\Microsoft\Windows Kits\Installed Roots")
            .and_then(|k| k.get_value("KitsRoot10"))
            .expect("读取 KitsRoot10 失败，请确认已安装 WDK")
    };
    let lib_root = PathBuf::from(&kits_root).join("Lib");
    let km_lib = lib_root
        .read_dir()
        .expect("读 Lib 目录失败")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.components()
                .next_back()
                .and_then(|c| c.as_os_str().to_str())
                .is_some_and(|c| c.starts_with("10."))
        })
        .max()
        .expect("找不到 SDK 版本目录")
        .join("km")
        .join("x64");
    println!("cargo:rustc-link-search={}", km_lib.display());
}
