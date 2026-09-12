// 非 Windows 宿主（macOS 交叉编译）上，下方整套 WDK 探测/bindgen 机制从 main()
// 不可达（generate() 仅在 Windows 宿主被调用），统一豁免 dead_code 告警。
#![cfg_attr(not(windows), allow(dead_code))]

use std::env;
use std::fmt::{self, Display};
use std::path::{Path, PathBuf};

use bindgen::Abi;

// minor(f)：winreg 只用于「Windows 宿主」的 WDK 注册表探测（build 脚本编译且
// 运行于宿主，此处的 cfg(windows) 即宿主判定）。依赖已移入
// [target.'cfg(windows)'.build-dependencies]（按宿主解析），macOS 宿主不再编译
// winreg。CARGO_CFG_WINDOWS（env）表达的是「目标平台」，无法阻止依赖编译，故不用。
#[cfg(windows)]
use winreg::enums::HKEY_LOCAL_MACHINE;
#[cfg(windows)]
use winreg::RegKey;

const UMDF_V: &str = "2.31";
const IDDCX_V: &str = "1.4";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("cannot find the directory")]
    DirectoryNotFound,
}

/// Retrieves the path to the Windows Kits directory. The default should be
/// `C:\Program Files (x86)\Windows Kits\10`.
///
/// # Errors
/// Returns IO error if failed
#[cfg(windows)]
fn get_windows_kits_dir() -> Result<PathBuf, Error> {
    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let key = r"SOFTWARE\Microsoft\Windows Kits\Installed Roots";
    let dir: String = hklm.open_subkey(key)?.get_value("KitsRoot10")?;

    Ok(dir.into())
}

/// 非 Windows 宿主的编译占位实现：generate()（bindgen 生成）只在 Windows 宿主被
/// main() 调用（WDK 头文件/库只在 Windows 主机存在），此分支永不执行。
#[cfg(not(windows))]
fn get_windows_kits_dir() -> Result<PathBuf, Error> {
    Err(Error::DirectoryNotFound)
}

#[derive(Clone, Copy, PartialEq)]
enum DirectoryType {
    Include,
    Library,
}

#[derive(Clone, Copy, PartialEq)]
enum Target {
    X86_64,
    ARM64,
}

impl Default for Target {
    fn default() -> Self {
        let target = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
        match &*target {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::ARM64,
            _ => unimplemented!("{target} arch is unsupported"),
        }
    }
}

impl Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::X86_64 => f.write_str("x64"),
            Target::ARM64 => f.write_str("arm64"),
        }
    }
}

fn get_base_path<S: AsRef<Path>>(dir_type: DirectoryType, subs: &[S]) -> Result<PathBuf, Error> {
    let mut dir = get_windows_kits_dir()?.join(match dir_type {
        DirectoryType::Include => "Include",
        DirectoryType::Library => "Lib",
    });

    dir.extend(subs);
    if !dir.is_dir() {
        return Err(Error::DirectoryNotFound);
    }

    Ok(dir)
}

fn get_sdk_path<S: AsRef<Path>>(dir_type: DirectoryType, subs: &[S]) -> Result<PathBuf, Error> {
    // We first append lib to the path and read the directory..
    let dir = get_windows_kits_dir()?
        .join(match dir_type {
            DirectoryType::Include => "Include",
            DirectoryType::Library => "Lib",
        })
        .read_dir()?;

    // In the lib directory we may have one or more directories named after the version of Windows,
    // we will be looking for the highest version number.
    let mut dir = dir
        .filter_map(Result::ok)
        .map(|dir| dir.path())
        .filter(|dir| {
            let is_sdk = dir
                .components()
                .next_back()
                .and_then(|c| c.as_os_str().to_str())
                .is_some_and(|c| c.starts_with("10."));

            let mut sub_dir = dir.clone();
            sub_dir.extend(subs);

            is_sdk && sub_dir.is_dir()
        })
        .max()
        .ok_or(Error::DirectoryNotFound)?;

    dir.extend(subs);
    if !dir.is_dir() {
        return Err(Error::DirectoryNotFound);
    }

    // Finally append um to the path to get the path to the user mode libraries.
    Ok(dir)
}

/// Retrieves the path to the user mode libraries. The path may look something like:
/// `C:\Program Files (x86)\Windows Kits\10\lib\10.0.18362.0\um`.
///
/// # Errors
/// Returns IO error if failed
fn get_um_dir(dir_type: DirectoryType) -> Result<PathBuf, Error> {
    let target = Target::default().to_string();

    let binding = &["um", &target];
    let subs: &[&str] = match dir_type {
        DirectoryType::Include => &["um"],
        DirectoryType::Library => binding,
    };

    let dir = get_sdk_path(dir_type, subs)?;
    Ok(dir)
}

/// # Errors
/// Returns IO error if failed
fn get_umdf_dir(dir_type: DirectoryType) -> Result<PathBuf, Error> {
    match dir_type {
        DirectoryType::Include => get_base_path(dir_type, &["wdf", "umdf", UMDF_V]),
        DirectoryType::Library => get_base_path(
            dir_type,
            &["wdf", "umdf", &Target::default().to_string(), UMDF_V],
        ),
    }
}

/// Retrieves the path to the shared headers. The path may look something like:
/// `C:\Program Files (x86)\Windows Kits\10\lib\10.0.18362.0\shared`.
///
/// # Errors
/// Returns IO error if failed
fn get_shared_dir() -> Result<PathBuf, Error> {
    let dir = get_sdk_path(DirectoryType::Include, &["shared"])?;
    Ok(dir)
}

fn build_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("OUT_DIR").expect("the environment variable OUT_DIR is undefined"),
    )
}

fn generate() {
    // Find the include directory containing the user headers.
    let include_um_dir = get_um_dir(DirectoryType::Include).unwrap();
    let lib_um_dir = get_um_dir(DirectoryType::Library).unwrap();
    let shared = get_shared_dir().unwrap();

    println!("cargo:rustc-link-search={}", lib_um_dir.display());

    // Tell Cargo to re-run this if src/wrapper.h gets changed.
    println!("cargo:rerun-if-changed=c/wrapper.h");

    //
    // UMDF
    //

    let umdf_lib_dir = get_umdf_dir(DirectoryType::Library).unwrap();

    println!("cargo:rustc-link-search={}", umdf_lib_dir.display());

    let wdf_include_dir = get_umdf_dir(DirectoryType::Include).unwrap();

    // need to link to umdf lib
    println!("cargo:rustc-link-lib=static=WdfDriverStubUm");

    //
    // IDDCX
    //

    let mut iddcx_lib_dir = lib_um_dir.clone();
    iddcx_lib_dir.push("iddcx");
    iddcx_lib_dir.push(IDDCX_V);

    println!("cargo:rustc-link-search={}", iddcx_lib_dir.display());

    // need to link to iddcx lib
    println!("cargo:rustc-link-lib=static=IddCxStub");

    //
    // REST
    //

    // Get the build directory.
    let out_path = build_dir();

    // Generate the bindings
    let mut builder = bindgen::Builder::default()
        .derive_debug(false)
        .layout_tests(false)
        .default_enum_style(bindgen::EnumVariation::NewType {
            is_bitfield: false,
            is_global: false,
        })
        .merge_extern_blocks(true)
        .header("c/wrapper.h")
        .header(
            get_sdk_path(DirectoryType::Include, &["um", "iddcx", IDDCX_V])
                .unwrap()
                .join("IddCx.h")
                .to_string_lossy()
                .to_string(),
        )
        // general um includes
        .clang_arg(format!("-I{}", include_um_dir.display()))
        // umdf includes
        .clang_arg(format!("-I{}", wdf_include_dir.display()))
        .clang_arg(format!("-I{}", shared.display()))
        // because aarch64 needs to find excpt.h
        .clang_arg(format!(
            "-I{}",
            get_sdk_path(DirectoryType::Include, &["km", "crt"])
                .unwrap()
                .display()
        ))
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()))
        .blocklist_type("_?P?IMAGE_TLS_DIRECTORY.*")
        // we will use our own custom type
        .blocklist_item("NTSTATUS")
        .blocklist_item("IddMinimumVersionRequired")
        .blocklist_item("WdfMinimumVersionRequired")
        // CRT 内存/串函数随 WDK 头混进生成结果，且被下面的 override_abi(CUnwind, ".*")
        // 一并声明成 extern "C-unwind"——rustc 对 std 依赖的运行时符号有签名校验
        // （必须 extern "C"），C-unwind 声明直接硬错误（CI WDK job 首跑实测）。
        // 仓库代码不直接调用这些符号，屏蔽生成即可；memmove 一并预防。
        .blocklist_item("memcmp")
        .blocklist_item("memcpy")
        .blocklist_item("memset")
        .blocklist_item("memmove")
        .blocklist_item("strlen")
        .clang_arg("--language=c++")
        .clang_arg("-fms-compatibility")
        .clang_arg("-fms-extensions")
        .override_abi(Abi::CUnwind, ".*")
        .generate_cstr(true)
        .derive_default(true);

    let defines = match Target::default() {
        Target::X86_64 => ["AMD64", "_AMD64_"],
        Target::ARM64 => ["ARM64", "_ARM64_"],
    };

    for define in defines {
        builder = builder.clang_arg(format!("-D{define}"));
    }

    // generate
    let umdf = builder.generate().unwrap();

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    umdf.write_to_file(out_path.join("umdf.rs")).unwrap();
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // build 脚本编译并运行于「宿主」。bindgen 需要 WDK 头文件/库（只在 Windows
    // 主机存在），因此 generate() 仅在 Windows 宿主执行；非 Windows 宿主（如
    // macOS 交叉编译 Windows 目标）跳过生成并给出提示——后续 wdf-umdf-sys 库
    // 编译因缺少 OUT_DIR/umdf.rs 而失败属预期（本包只能在 Windows 主机构建），
    // 不会伪装成功。
    #[cfg(windows)]
    generate();

    #[cfg(not(windows))]
    println!(
        "cargo:warning=wdf-umdf-sys: non-Windows host: skipping WDK bindgen; \
         bindings can only be generated on a Windows host (WDK required)"
    );
}
