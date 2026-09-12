#!/bin/sh
# 宿主（macOS/Linux）直跑 vdev-display-win 纯逻辑单测。
#
# 背景：driver-ipc 整箱依赖 tokio windows named pipe / winreg，cli 依赖 windows
# crate，都只能在 Windows 目标编译；且本 workspace 的 .cargo/config.toml 把
# build target 固定为 x86_64-pc-windows-msvc，cargo test 无法在宿主运行。
# 因此把无第三方依赖的纯逻辑模块用 rustc --test 直接在宿主编译运行：
#   1) driver-ipc/src/state.rs —— new_id / add_mode / 查重辅助（经最小类型镜像 include）
#   2) cli/src/quote.rs        —— MSVCRT argv 引号转义（零依赖直跑）
#   3) cli/src/mode_check.rs   —— 模式解析/校验（零依赖直跑）
#   4) driver/src/validate.rs  —— M2 IPC 输入校验（零依赖直跑，含最坏 Notify 体量固化）
#   5) wdf.rs ArcPointer 布局  —— M4 零初始化=Uninit 不变量（类型镜像，局限见下）
# 除 5) 外，测试同时也在 Windows 上随 `cargo test` 正常运行（cfg(test)）。
set -eu
cd "$(dirname "$0")/.."

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

# ---- 1) driver-ipc/src/state.rs ----
# state.rs 通过 `use crate::core::{...}` 引用 IPC 线上类型；这里提供一个字段布局
# 一致的最小镜像（不含 serde 派生），测试本身走真实 state.rs 源码（#[path] 引入，
# 保留其模块级文档与 cfg(test)）。
cp driver-ipc/src/state.rs "$TMP/state.rs"
cat > "$TMP/state_standalone.rs" <<'EOF'
//! 宿主直跑 harness：core 为 driver-ipc/src/core.rs 的最小类型镜像（仅字段），
//! state 为被测源码本体。
#![allow(dead_code)]
pub mod core {
    pub type Id = u32;
    pub type Dimen = u32;
    pub type RefreshRate = u32;

    #[derive(Debug, Clone, PartialEq, PartialOrd)]
    pub struct Monitor {
        pub id: Id,
        pub name: Option<String>,
        pub enabled: bool,
        pub modes: Vec<Mode>,
    }

    #[derive(Debug, Clone, PartialEq, PartialOrd)]
    pub struct Mode {
        pub width: Dimen,
        pub height: Dimen,
        pub refresh_rates: Vec<RefreshRate>,
    }
}

#[path = "state.rs"]
mod state;
EOF
echo "== rustc --test state.rs（宿主直跑 driver-ipc 纯逻辑单测）"
rustc --edition 2021 --test "$TMP/state_standalone.rs" -o "$TMP/state_tests"
"$TMP/state_tests"

# ---- 2) cli/src/quote.rs ----
echo "== rustc --test quote.rs（宿主直跑 argv 引号转义单测）"
rustc --edition 2021 --test cli/src/quote.rs -o "$TMP/quote_tests"
"$TMP/quote_tests"

# ---- 3) cli/src/mode_check.rs ----
echo "== rustc --test mode_check.rs（宿主直跑模式解析/校验单测）"
rustc --edition 2021 --test cli/src/mode_check.rs -o "$TMP/mode_tests"
"$TMP/mode_tests"

# ---- 4) driver/src/validate.rs ----
# validate.rs 零 FFI、零第三方依赖（仅 std），rustc --test 直接编译运行。
echo "== rustc --test validate.rs（宿主直跑 M2 输入校验单测）"
rustc --edition 2021 --test driver/src/validate.rs -o "$TMP/validate_tests"
"$TMP/validate_tests"

# ---- 5) wdf.rs ArcPointer 布局不变量（镜像） ----
# wdf-umdf 依赖 wdf-umdf-sys（WDK bindgen 只能在 Windows 主机构建），宏内真实类型
# 无法在宿主编译；此处镜像其 #[repr(C)] enum ArcPointer 定义并运行与宏内
# arc_pointer_layout_tests 相同的断言。镜像局限（同 1) 已披露）：镜像与本体的
# 同步靠人工维护，本体随 Windows `cargo test` 在宏内 cfg(test) 模块回归。
cat > "$TMP/arc_pointer_layout_standalone.rs" <<'EOF'
//! 宿主镜像 harness：wdf.rs 宏内 ArcPointer 的逐字段镜像 + 相同布局断言（M4）。
#![allow(dead_code)]

// 镜像自 wdf-umdf/src/wdf.rs 宏 WDF_DECLARE_CONTEXT_TYPE! 内的 ArcPointer 定义
#[repr(C)]
enum ArcPointer<T> {
    Uninit,
    Strong(std::sync::Arc<T>),
    Weak(std::sync::Weak<T>),
}

type Slot = ArcPointer<std::sync::RwLock<()>>;

#[test]
fn zeroed_slot_decodes_as_uninit() {
    // 布局尺寸：tag + 裸指针 = 2 * usize（64 位下 16 字节）
    assert_eq!(std::mem::size_of::<Slot>(), 2 * std::mem::size_of::<usize>());

    let zeroed = [0u8; std::mem::size_of::<Slot>()];
    // SAFETY: `zeroed` 与 `Slot` 等长；读出的值仅经 matches! 检视 discriminant，
    // 且以 ManuallyDrop 包裹永不 drop —— 即使未来布局被改坏（全零解码为
    // Strong/Weak），也不会析构无效指针
    let slot = std::mem::ManuallyDrop::new(unsafe {
        std::mem::transmute_copy::<[u8; std::mem::size_of::<Slot>()], Slot>(&zeroed)
    });
    // 全零字节解码为 Uninit ⇔ discriminant 在偏移 0 且首变体 tag 为 0
    assert!(matches!(&*slot, ArcPointer::Uninit));

    // 显式固化：合法 Uninit 值偏移 0 处的 tag 字节为 0
    let uninit = Slot::Uninit;
    // SAFETY: 读取已初始化合法值（Uninit）首字节的 1 字节 tag
    let tag0 = unsafe { std::ptr::addr_of!(uninit).cast::<u8>().read() };
    assert_eq!(tag0, 0);
}
EOF
echo "== rustc --test ArcPointer 布局（宿主镜像验证 M4 不变量）"
rustc --edition 2021 --test "$TMP/arc_pointer_layout_standalone.rs" -o "$TMP/arc_ptr_tests"
"$TMP/arc_ptr_tests"
