#!/bin/bash
# vdev HID 权限/安全输入路径验收（macOS 真机）。
#
# 为什么需要这个脚本：#48 里「HID 无权限 / Secure Input 路径」一直没验过——不是
# 权限没开，而是从没在**未授权身份**下测过。2026-09-16 实测发现注入其实**需要**
# 「辅助功能」权限，未授权时 CGEventPost 被系统静默丢弃（窗口 0 字符、进程 exit 0），
# 也就是典型的"假成功"。本脚本把三种身份/环境摆在一起对照：
#
#   A) 未授权 .app 身份  → 必须报错且 0 字符（**每次全新 bundle id**，保证从未被授权）
#   A2) 阳性对照         → 同一身份跳过权限检查，必须复现"0 字符但 exit 0"的静默假成功
#   B) 已授权身份        → 必须逐字送达（否则脚本自己就是假绿）
#   C) 安全输入开启      → 必须报错（用 EnableSecureEventInput 确定性开启）
#   D) 稳定注入器身份    → 人工在系统设置里授权过的话，此时应当能送达（信息项）
#
# 注意：本脚本注入的是真实合成键事件，"逐字一致"断言在**机器满载**时会因系统丢弃
# 事件而失败（实测 load 165 时 B/D 段丢字、空载即全绿）。真机跑验收请在空闲机器上做，
# 失败时先看 `uptime` 再判断是被测功能坏了还是被负载拖了。
#
# 用法：macos-hid-access-verify.sh [path/to/vdev]
# 退出码：0 = 全部通过；1 = 有用例失败；2 = 环境不具备（没验证，不算通过）
set -u

# `--self-test` 可前置：它只跑"准备注入器 app"的行为自测，不需要 vdev / 探针 / 前台焦点
SELF_TEST=0
if [ "${1:-}" = "--self-test" ]; then SELF_TEST=1; shift; fi
VDEV=${1:-target/release/vdev}
if [ "${SELF_TEST}" = "0" ] && [ ! -x "${VDEV}" ]; then
  echo "FAIL: vdev 二进制不可执行：${VDEV}（先 cargo build -p vdev-host --release）"
  exit 2
fi
VDEV=$(cd "$(dirname "${VDEV}")" && pwd)/$(basename "${VDEV}")
if [ "${SELF_TEST}" = "0" ]; then
  for tool in swiftc python3 open; do
    command -v "${tool}" >/dev/null 2>&1 || { echo "FAIL: 缺 ${tool}"; exit 2; }
  done
fi

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
RUN_DIR=$(mktemp -d -t vdev-hid-access.XXXXXX)
# 注入器 app 有两份：
#   INJECTOR_APP        —— 固定路径 + 固定 bundle id，供人工在系统设置里授权
#                          （授权后复现"同一身份从报错变成能注入"）
#   INJECTOR_APP_UNAUTH —— 每次运行都换新 bundle id，**保证从未被授权**，
#                          作为 A / A2 的未授权对照（用固定 id 会被历史授权污染）
INJECTOR_DIR="${HOME}/.vdev-hid-injector"
INJECTOR_APP="${INJECTOR_DIR}/VdevHidInjector.app"
LOCK_DIR="${INJECTOR_DIR}/.lock"
UNAUTH_ID="com.vdev.hid.injector.unauth.$$.$(date +%s)"
INJECTOR_APP_UNAUTH="${RUN_DIR}/VdevHidUnauth.app"
SECURE_HELPER_BIN="${RUN_DIR}/secure-input-hold"

cleanup() {
  pkill -x VdevHidProbe 2>/dev/null || true
  pkill -x SecureInputHold 2>/dev/null || true
  rm -rf "${RUN_DIR}"
  # 互斥锁只由**持锁者**删除：非持锁者也删锁会让"看到 NOT_RUN 再重跑"变成三实例并发（审查 L1）
  if [ "${LOCK_HELD:-0}" = "1" ]; then
    rmdir "${INJECTOR_DIR:-/nonexistent}/.lock" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# 1) 探针窗（收文本的窗口）
PROBE_BIN="${RUN_DIR}/probe"
if ! swiftc -O "${SCRIPT_DIR}/macos-hid-type-probe.swift" -o "${PROBE_BIN}"; then
  echo "FAIL: 探针编译失败"; exit 2
fi
PROBE_APP="${RUN_DIR}/VdevHidProbe.app"
mkdir -p "${PROBE_APP}/Contents/MacOS"
cp "${PROBE_BIN}" "${PROBE_APP}/Contents/MacOS/VdevHidProbe"
cat > "${PROBE_APP}/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.vdev.hid.type.probe</string>
<key>CFBundleExecutable</key><string>VdevHidProbe</string>
<key>CFBundleName</key><string>VdevHidProbe</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>NSHighResolutionCapable</key><true/>
</dict></plist>
PLIST

# 2) 准备"稳定注入器 app"（固定 id + 稳定签名 + 只在必要时重写）
#
# 为什么要签名：未签名的 app，TCC 的授权要求是 `cdhash H"..."`（实测 csreq 40B），
# 而脚本每轮都会把新的 vdev 二进制拷进 bundle —— cdhash 一变，人工授权立刻失配
# （issue #58）。用 Apple Development/Developer ID 签名后，要求变成
# `identifier "com.vdev.hid.injector" and anchor apple generic and certificate leaf[…]`，
# 与二进制内容无关（实测把可执行文件换掉重签，DR 一字不变）。
#
# 准备策略（审查 B1 修正版）：**只有"源 sha 同名 + 期望身份一致 + codesign 校验通过"
# 三者都满足才复用**；否则重写+重签。签名或校验失败时**不写戳**，避免把坏状态永久固化。
resolve_sign_identity() {
  # 优先 Apple Development（与本仓 crates/vdev-camera/Makefile 一致），其次 Developer ID
  if [ -n "${VDEV_HID_SIGN_IDENTITY:-}" ]; then
    printf '%s' "${VDEV_HID_SIGN_IDENTITY}"; return
  fi
  local pref="Apple Development: qingfeng gao (7L8FV63FAP)"
  if security find-identity -v -p codesigning 2>/dev/null | grep -qF "${pref}"; then
    printf '%s' "${pref}"; return
  fi
  local devid
  devid=$(security find-identity -v -p codesigning 2>/dev/null | sed -nE 's/.*"(Developer ID Application: [^"]+)".*/\1/p' | head -1)
  if [ -n "${devid}" ]; then printf '%s' "${devid}"; return; fi
  printf '%s' "-"   # 没有任何可用身份 → ad-hoc（并会在下面 WARN）
}

# 盘上这份 bundle 当前的实际签名状态：`adhoc` 或 `signed:<Authority 叶子 CN>`。
signature_state() {
  local app=$1
  if codesign -dv "${app}" 2>&1 | grep -q '^Signature=adhoc'; then
    printf 'adhoc'
  else
    printf 'signed:%s' "$(codesign -dv --verbose=4 "${app}" 2>&1 | sed -n 's/^Authority=//p' | head -1)"
  fi
}

# 返回 0=刚准备好（重写/重签过），1=复用了现有 bundle，2=没签成（下轮重试）
prepare_injector_app() {
  local src=$1 app=$2 stamp=$3 identity=$4
  local bin="${app}/Contents/MacOS/VdevHidInjector"
  # 戳里存三段：源 sha、**请求的**身份串、**实际**签名状态。
  # - 请求串用于"操作者显式换了身份就该重签"；
  # - 实际状态用于"盘上是不是真的这个签名"（防 `--sign -` 悄悄降级）；
  # - 两者分开存，别名身份（如传证书 SHA-1）才不会被误判成"每轮都变了"而反复重签。
  local src_sha
  src_sha=$(shasum -a 256 "${src}" | awk '{print $1}')
  if [ -f "${bin}" ] && [ -f "${stamp}" ]; then
    local got_sha got_req got_actual actual_now
    got_sha=$(cut -f1 "${stamp}" 2>/dev/null)
    got_req=$(cut -f2 "${stamp}" 2>/dev/null)
    got_actual=$(cut -f3 "${stamp}" 2>/dev/null)
    actual_now="$(signature_state "${app}")"
    if [ "${got_sha}" = "${src_sha}" ] && [ "${got_req}" = "${identity}" ] \
       && [ "${got_actual}" = "${actual_now}" ] \
       && { [ "${identity}" = "-" ] || [ "${actual_now}" != "adhoc" ]; } \
       && codesign --verify --strict "${app}" >/dev/null 2>&1; then
      return 1
    fi
  fi
  mkdir -p "${app}/Contents/MacOS" "$(dirname "${stamp}")"
  cp "${src}" "${bin}"
  # Info.plist 也只在内容变了才写：签名之后再改 plist 会让签名失效（审查 B1）
  local plist="${app}/Contents/Info.plist"
  local tmp_plist
  tmp_plist=$(mktemp -t vdev-injector-plist)
  cat > "${tmp_plist}" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>com.vdev.hid.injector</string>
<key>CFBundleExecutable</key><string>VdevHidInjector</string>
<key>CFBundleName</key><string>VdevHidInjector</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>LSUIElement</key><true/>
</dict></plist>
PLIST
  if [ ! -f "${plist}" ] || ! cmp -s "${tmp_plist}" "${plist}"; then
    cp "${tmp_plist}" "${plist}"
  fi
  rm -f "${tmp_plist}"
  if ! codesign --force --sign "${identity}" -i com.vdev.hid.injector \
       --options runtime --timestamp=none "${app}" >/dev/null 2>&1; then
    echo "WARN: 签名失败（identity='${identity}'）：本 bundle 不会被标记就绪，下次运行会重试。" >&2
    return 2
  fi
  if ! codesign --verify --strict "${app}" >/dev/null 2>&1; then
    echo "WARN: 签名后 codesign --verify --strict 失败：不写戳，下次运行会重试。" >&2
    return 2
  fi
  # 实际签成了什么？**不能拿请求值当结果**（审查 L2）：`--sign -` 或身份失效时
  # codesign 仍可能以 ad-hoc 成功，此时戳若写 signed:<身份> 就是撒谎，复用条件
  # 会把降级的 bundle 当"最新"，且永不自愈。
  local actual_state
  actual_state="$(signature_state "${app}")"
  if [ "${identity}" != "-" ] && [ "${actual_state}" = "adhoc" ]; then
    echo "WARN: 请求用身份 '${identity}' 签名，实际落成 ad-hoc（证书不可用？）：不写戳，下次重试。" >&2
    return 2
  fi
  # 只有"签名 + 校验"都过了才写戳，且写的是**实际**状态
  printf '%s\t%s\t%s' "${src_sha}" "${identity}" "${actual_state}" > "${stamp}"
  return 0
}

# ---- 自测模式：不需要证书、不需要前台焦点，供 CI/冒烟做**行为级**守卫 ----
# 用法：macos-hid-access-verify.sh --self-test
#
# 为什么要它：只 grep 标识符的守卫抓不住"把修复摘掉"的写法（例如把签名换成
# `--sign -`、删掉 INJECTOR_NEEDS_SIGN 的赋值、把写戳删掉）——审查 B2 实测那些
# 变异都能骗过纯字符串守卫。这里直接跑 prepare_injector_app 的行为。
if [ "${SELF_TEST}" = "1" ]; then
  TMP_SELF=$(mktemp -d -t vdev-injector-selftest.XXXXXX)
  SELF_OK=1
  SELF_SRC="${TMP_SELF}/src.bin"
  SELF_APP="${TMP_SELF}/App.app"
  SELF_STAMP="${TMP_SELF}/stamp"
  SELF_BIN="${SELF_APP}/Contents/MacOS/VdevHidInjector"
  # 夹具用**未被改动的** Mach-O：给 Apple 自带二进制追加字节会破坏它的签名，
  # codesign 会以 "main executable failed strict validation" 拒签——那是夹具问题，
  # 不是被测逻辑问题。所以"源变了"用**换一个源文件**表达，而不是改同一个文件。
  if [ -x "${VDEV}" ]; then SELF_SRC0="${VDEV}"; else SELF_SRC0="/bin/echo"; fi
  cp "${SELF_SRC0}" "${SELF_SRC}"
  # "源变了"必须换**内容不同**的文件：拿同一个 vdev 复制一份 sha 不变，会被正确复用
  SELF_SRC2="${TMP_SELF}/src2.bin"
  cp /bin/ls "${SELF_SRC2}"
  sha_of() { shasum -a 256 "$1" | awk '{print $1}'; }
  stat_of() { stat -f "%m %i" "$1"; }

  echo "== 1) 首跑：应重写并写状态戳 =="
  prepare_injector_app "${SELF_SRC}" "${SELF_APP}" "${SELF_STAMP}" "-" >/dev/null || true
  [ -f "${SELF_STAMP}" ] || { echo "FAIL: 首跑没有写戳"; SELF_OK=0; }
  grep -q "adhoc" "${SELF_STAMP}" 2>/dev/null || { echo "FAIL: 戳里没有实际签名状态"; SELF_OK=0; }
  [ "$(cut -f3 "${SELF_STAMP}" 2>/dev/null)" = "adhoc" ] || { echo "FAIL: 第 3 字段应为实际状态 adhoc"; SELF_OK=0; }
  SELF_M1=$(stat_of "${SELF_BIN}")
  SELF_SHA1=$(sha_of "${SELF_SRC2}")

  echo "== 2) 源与身份都没变：应复用（不重写、不重签）=="
  if prepare_injector_app "${SELF_SRC}" "${SELF_APP}" "${SELF_STAMP}" "-" >/dev/null 2>&1; then
    echo "FAIL: 源没变却重写了 bundle（会打掉 cdhash 型授权）"; SELF_OK=0
  fi
  [ "$(stat_of "${SELF_BIN}")" = "${SELF_M1}" ] || { echo "FAIL: 二进制 mtime/inode 变了"; SELF_OK=0; }

  echo "== 3) 源换了：应重签并更新戳 =="
  prepare_injector_app "${SELF_SRC2}" "${SELF_APP}" "${SELF_STAMP}" "-" >/dev/null || true
  [ "$(stat_of "${SELF_BIN}")" != "${SELF_M1}" ] || { echo "FAIL: 源变了却没有重写"; SELF_OK=0; }
  [ "$(cut -f1 "${SELF_STAMP}")" = "$(sha_of "${SELF_SRC2}")" ] || { echo "FAIL: 戳没有跟源更新"; SELF_OK=0; }

  echo "== 4) bundle 被改坏（签名失效）：戳匹配也不该复用（审查 B1）=="
  printf '<!--tamper-->' >> "${SELF_APP}/Contents/Info.plist"
  if codesign --verify --strict "${SELF_APP}" >/dev/null 2>&1; then
    echo "SKIP: 篡改未使 codesign 校验失败（环境差异），本段未验证"
  else
    SELF_M4=$(stat_of "${SELF_BIN}")
    prepare_injector_app "${SELF_SRC2}" "${SELF_APP}" "${SELF_STAMP}" "-" >/dev/null || true
    if codesign --verify --strict "${SELF_APP}" >/dev/null 2>&1; then
      echo "  坏签名已被自动重做 ✓"
    else
      echo "FAIL: 校验失败的 bundle 被戳固化了（没有重做）"; SELF_OK=0
    fi
    [ "$(stat_of "${SELF_BIN}")" != "${SELF_M4}" ] || { echo "FAIL: 没有真的重写二进制"; SELF_OK=0; }
  fi

  echo "== 5) 身份变化：应重签（戳里记了身份，不只是 sha）=="
  # 注意返回码：0=重做过，1=复用了，2=没签成（身份不可用时正是这条）。
  # "没有复用"才是本步要断言的（rc != 1）。
  set +e
  prepare_injector_app "${SELF_SRC2}" "${SELF_APP}" "${SELF_STAMP}" "signed:fake-identity" >/dev/null 2>&1
  SELF_RC5=$?
  set -e
  if [ "${SELF_RC5}" != "1" ]; then
    echo "  身份变化没有复用（rc=${SELF_RC5}）✓"
  else
    echo "FAIL: 身份变化被复用了（戳只比了 sha）"; SELF_OK=0
  fi

  echo "== 6) 请求一个不可用的身份：绝不能把 ad-hoc 结果记成 signed:（审查 L2）=="
  prepare_injector_app "${SELF_SRC2}" "${SELF_APP}" "${SELF_STAMP}" "Apple Development: 不存在的证书 (XXXXXXXXXX)" >/dev/null 2>&1 || true
  if grep -q "signed:Apple Development: 不存在的证书" "${SELF_STAMP}" 2>/dev/null; then
    echo "FAIL: 戳把 ad-hoc 结果记成了 signed:<请求身份>（戳会撒谎，复用条件不再可信）"; SELF_OK=0
  elif codesign -dv "${SELF_APP}" 2>&1 | grep -q '^Signature=adhoc' && [ "$(cut -f3 "${SELF_STAMP}" 2>/dev/null)" != "adhoc" ]; then
    echo "FAIL: 实际是 ad-hoc，戳的实际状态字段却不是 adhoc（$(cut -f3 "${SELF_STAMP}" 2>/dev/null)）"; SELF_OK=0
  else
    echo "  戳状态与实际签名一致 ✓"
  fi

  rm -rf "${TMP_SELF}"
  if [ "${SELF_OK}" = "1" ]; then echo HID_INJECTOR_SELFTEST=PASS; exit 0; fi
  echo HID_INJECTOR_SELFTEST=FAIL
  exit 1
fi

INJECTOR_STAMP="${INJECTOR_DIR}/.injector-source.sha256"
mkdir -p "${INJECTOR_DIR}"
# 并发保护：两份脚本同时改同一个 bundle 会把签名踩坏（审查 B1 实测）
if mkdir "${LOCK_DIR}" 2>/dev/null; then
  LOCK_HELD=1
else
  echo "NOT_RUN: 另一个 ${0##*/} 实例正在准备同一个注入器（${LOCK_DIR}）；本脚本不支持并发运行。"
  echo "  若确认没有实例在跑（例如上次被 kill -9），手动清锁：rmdir '${LOCK_DIR}'"
  echo "HID_ACCESS_RESULT=NOT_RUN"
  exit 2
fi

INJECTOR_SIGN_IDENTITY=$(resolve_sign_identity)
PREP_RC=0
prepare_injector_app "${VDEV}" "${INJECTOR_APP}" "${INJECTOR_STAMP}" "${INJECTOR_SIGN_IDENTITY}" || PREP_RC=$?
if [ "${PREP_RC}" = "2" ]; then
  echo "WARN: 注入器本次没有签成稳定身份（见上），下轮会重试；D 段可能因此无法持久。"
elif [ "${PREP_RC}" = "0" ]; then
  if [ "${INJECTOR_SIGN_IDENTITY}" = "-" ]; then
    echo "WARN: 没有可用的签名身份 → 注入器退回 ad-hoc。"
    echo "WARN: ad-hoc 的 TCC 授权要求是 cdhash，二进制一变授权就失效（issue #58）；"
    echo "WARN: D 段（授权前报错 → 授权后可用）将无法跨运行复现。"
  else
    echo "  注入器已用稳定身份签名：${INJECTOR_SIGN_IDENTITY}"
  fi
else
  echo "  注入器复用现有 bundle（源与身份都未变、签名校验通过）"
fi
echo "  注入器 codesign --verify --strict: $(codesign --verify --strict "${INJECTOR_APP}" >/dev/null 2>&1 && echo OK || echo FAILED)"
INJECTOR_DR=$(codesign -dr - "${INJECTOR_APP}" 2>&1 | tail -1)
echo "  注入器 designated requirement: ${INJECTOR_DR}"
case "${INJECTOR_DR}" in
  *cdhash*)
    echo "WARN: 注入器身份是 cdhash 型 —— 人工授权只对当前这份二进制有效（issue #58）" ;;
esac

# 2b) 未授权对照 app（bundle id 每次不同 → 不可能有历史授权）
mkdir -p "${INJECTOR_APP_UNAUTH}/Contents/MacOS"
cp "${VDEV}" "${INJECTOR_APP_UNAUTH}/Contents/MacOS/VdevHidUnauth"
cat > "${INJECTOR_APP_UNAUTH}/Contents/Info.plist" <<UPLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>${UNAUTH_ID}</string>
<key>CFBundleExecutable</key><string>VdevHidUnauth</string>
<key>CFBundleName</key><string>VdevHidUnauth</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>LSMinimumSystemVersion</key><string>13.0</string>
<key>LSUIElement</key><true/>
</dict></plist>
UPLIST

# 3) 安全输入保持器：Carbon EnableSecureEventInput() 在进程存活期间全局生效
cat > "${RUN_DIR}/secure-input-hold.swift" <<'SWIFT'
import Foundation
import Carbon
let status = EnableSecureEventInput()
FileHandle.standardError.write("secure-input-hold: EnableSecureEventInput=\(status)\n".data(using: .utf8)!)
// 保持到被杀；退出前尽量恢复
let deadline = Date().addingTimeInterval(60)
while Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.2)) }
_ = DisableSecureEventInput()
SWIFT
swiftc -O "${RUN_DIR}/secure-input-hold.swift" -o "${SECURE_HELPER_BIN}" 2>/dev/null || {
  echo "FAIL: 安全输入保持器编译失败"; exit 2;
}

# 安全输入前置判定：全局面板开着时 A/A2 会因拿到安全输入报错而假红（审查 S9）
if "${VDEV}" hid access 2>/dev/null | grep -q '^secure_input=true$'; then
  echo "NOT_RUN: 当前系统处于安全输入（Secure Input），先把密码框 / 终端「安全键盘输入」关掉再跑。"
  echo "HID_ACCESS_RESULT=NOT_RUN"
  exit 2
fi

# 预热：首次 `open` 一个带 bundle 身份的 App 常有激活竞态（探针 READY 了但窗口
# 还没真正拿到前台），先跑一个 throwaway 实例把它消化掉。
warm_out="${RUN_DIR}/warmup.out"
: > "${warm_out}"
open -W -n "${PROBE_APP}" --args "${warm_out}" 1 >/dev/null || true

# 锁屏前置判定：锁屏下任何 App 都无法成为前台/键窗口（探针会一直 READY
# active=false），此时既不能验证也不该判红——按"没验证 ≠ 通过"退出 2。
if ioreg -n Root -d1 -a 2>/dev/null | grep -A1 CGSSessionScreenIsLocked | grep -q '<true/>'; then
  echo "NOT_RUN: 当前会话处于锁屏（CGSSessionScreenIsLocked=true）。"
  echo "         解锁后重跑本脚本；锁屏下前台窗口与键窗口都拿不到，测不了注入路径。"
  echo "HID_ACCESS_RESULT=NOT_RUN"
  exit 2
fi

fails=0
env_bad=0
pass() { echo "PASS $*"; }
fail() { echo "FAIL $*"; fails=$((fails + 1)); }
# 环境不具备（不是被测功能坏了）：结束时按 NOT_RUN 退出 2
env_fail() { echo "ENV: $*"; env_bad=$((env_bad + 1)); }

# 跑一次探针窗 + 注入：$1 = direct|app，$2 标签，其余为注入命令
#   direct: 直接 exec，**继承调用者身份**（已授权终端里就是已授权）
#   app   : 经 LaunchServices 启动 .app，拿到该 bundle 自己的 TCC 身份
# 结果经全局 probe_typed / probe_summary / inject_out_text 返回
probe_typed=""
probe_summary=""
inject_out_text=""
run_case() {
  local mode=$1 label=$2; shift 2
  local slug
  slug=$(echo "${label}" | tr -c 'a-zA-Z0-9' '_')
  local out="${RUN_DIR}/${slug}.out"
  : > "${out}"
  open -W -n "${PROBE_APP}" --args "${out}" 3 >/dev/null 2>&1 &
  local open_pid=$!
  # 抢焦点兜底：macOS 26 会拦"非用户发起"的 App 自激活（探针自己
  # `app.activate` 可能被判为抢焦点），这里用 System Events 显式置前；
  # 探针有 4s 等待窗口，所以要在它跑起来后立刻置前。
  for _ in $(seq 1 30); do
    if pgrep -x VdevHidProbe >/dev/null 2>&1; then
      osascript -e 'tell application "System Events" to set frontmost of process "VdevHidProbe" to true' >/dev/null || true
      break
    fi
    sleep 0.1
  done
  local ready=""
  for _ in $(seq 1 80); do
    ready=$(grep -m1 '^READY' "${out}" 2>/dev/null || true)
    [ -n "${ready}" ] && break
    kill -0 "${open_pid}" 2>/dev/null || break
    sleep 0.1
  done
  if [ -z "${ready}" ]; then
    probe_typed="__NO_PROBE__"; probe_summary="探针未就绪"
    pkill -x VdevHidProbe 2>/dev/null || true
    wait "${open_pid}" 2>/dev/null || true
    return
  fi
  if ! echo "${ready}" | grep -q 'active=true key=true'; then
    probe_typed="__NO_FOCUS__"; probe_summary="${ready}"
    pkill -x VdevHidProbe 2>/dev/null || true
    wait "${open_pid}" 2>/dev/null || true
    return
  fi
  : > "${RUN_DIR}/inject.out"; : > "${RUN_DIR}/inject.err"
  # READY 之后仍可能有极短的在途事件（探针已在 READY 时清零计数，这里再降一点概率）。
  # 两种注入方式都要等，不能只等 direct —— app 分支正是 #58 的路径（审查 S2）。
  sleep 0.3
  if [ "${mode}" = "app" ]; then
    local app=$1; shift
    # VDEV_HID_CASE_ENV：可选的 `open --env K=V`（阳性对照用，见 A2）。
    # 注意不要用数组 + `set -u`：bash 3.2 下空数组展开会报 unbound variable。
    if [ -n "${VDEV_HID_CASE_ENV:-}" ]; then
      open -n -W --stdout "${RUN_DIR}/inject.out" --stderr "${RUN_DIR}/inject.err" \
        --env "${VDEV_HID_CASE_ENV}" "${app}" --args "$@" 2>/dev/null
    else
      open -n -W --stdout "${RUN_DIR}/inject.out" --stderr "${RUN_DIR}/inject.err" \
        "${app}" --args "$@" 2>/dev/null
    fi
  else
    "$@" > "${RUN_DIR}/inject.out" 2>&1
  fi
  inject_out_text="$(cat "${RUN_DIR}/inject.out" "${RUN_DIR}/inject.err" 2>/dev/null)"
  echo "  inject_out=$(echo "${inject_out_text}" | tr '\n' ' ' | tail -c 160)"
  wait "${open_pid}"
  probe_typed=$(grep -m1 '^TYPED=' "${out}" | sed 's/^TYPED=//' || true)
  probe_summary=$(grep -m1 '^SUMMARY' "${out}" || true)
}

chars_of() { TEXT="$1" python3 -c 'import os; print(len(os.environ["TEXT"]))'; }

# ---- A) 未授权身份 ----
echo "== A) 未授权 .app 身份 =="
TEXT_A="unauth-identity-check"
access_out="${RUN_DIR}/injector-access.out"
echo "  未授权对照 bundle id=${UNAUTH_ID}"
open -n -W --stdout "${access_out}" "${INJECTOR_APP_UNAUTH}" --args hid access >/dev/null 2>&1
post_a=$(sed -nE 's/^post_access=(.*)$/\1/p' "${access_out}" | head -1)
echo "  未授权身份 post_access=${post_a:-<空>}"
if [ "${post_a}" != "false" ]; then
  env_fail "A: 全新 bundle id 的 post_access 却是 '${post_a}'——未授权对照不成立（环境异常，不是被测功能坏了）"
fi

run_case app A_unauth "${INJECTOR_APP_UNAUTH}" hid type "${TEXT_A}"
if [ "${probe_typed}" = "__NO_PROBE__" ] || [ "${probe_typed}" = "__NO_FOCUS__" ]; then
  fail "A: 探针未取得前台焦点（${probe_summary}）→ 本次没有验证，不能算通过"
else
  chars_a=$(chars_of "${TEXT_A}")
  got_a=$(python3 -c 'import sys; print(len(sys.argv[1]))' "${probe_typed}")
  # 未授权身份：要么被系统丢弃（0 字符 + 我们的护栏报错），要么系统真的放行（字符送达）
  if [ "${got_a}" -eq 0 ] && echo "${inject_out_text}" | grep -q "辅助功能"; then
    pass "A: 未授权身份被系统丢弃（0/${chars_a} 字符）且 vdev 明确报错（不再静默假成功）"
  elif [ "${got_a}" -eq "${chars_a}" ]; then
    fail "A: 未授权身份竟然送达了全部字符——那说明注入不需要辅助功能权限，本 issue 的前提要重写"
  elif [ "${got_a}" -eq 0 ]; then
    fail "A: 0 字符但 vdev 没有报错（静默假成功，护栏没生效）：$(echo "${inject_out_text}" | tail -1)"
  else
    fail "A: 只送达 ${got_a}/${chars_a} 字符（部分送达）"
  fi
fi

# ---- A2) 阳性对照：绕过护栏必须复现"静默假成功" ----
# 这一条是护栏的阳性对照：同一个未授权身份，只是跳过权限检查，就会回到"0 字符
# 但 exit 0 不报错"的老行为。若这里反而报错，说明前面的 A 段不是护栏的功劳。
echo "== A2) 阳性对照：跳过权限检查（VDEV_HID_SKIP_ACCESS_CHECK=1）=="
TEXT_A2="bypass-guard-check"
VDEV_HID_CASE_ENV="VDEV_HID_SKIP_ACCESS_CHECK=1" \
  run_case app A2_bypass "${INJECTOR_APP_UNAUTH}" hid type "${TEXT_A2}"
VDEV_HID_CASE_ENV=""
if [ "${probe_typed}" = "__NO_PROBE__" ] || [ "${probe_typed}" = "__NO_FOCUS__" ]; then
  fail "A2: 探针未取得前台焦点（${probe_summary}）→ 本次没有验证"
else
  got_a2=$(python3 -c 'import sys; print(len(sys.argv[1]))' "${probe_typed}")
  if [ "${got_a2}" -eq 0 ] && ! echo "${inject_out_text}" | grep -q "辅助功能"; then
    pass "A2: 绕过护栏后确实复现静默假成功（0 字符、无报错）→ A 段的报错确系护栏产生"
  elif [ "${got_a2}" -gt 0 ]; then
    fail "A2: 绕过护栏后字符竟然送达了（${got_a2}）——A 段前提要重写"
  else
    fail "A2: 绕过护栏后仍然报错，护栏没被跳过：$(echo "${inject_out_text}" | tail -1)"
  fi
fi

# ---- B) 已授权身份（必须绿）----
echo "== B) 当前（已授权）身份 =="
TEXT_B="authed-identity-ok"
"${VDEV}" hid access > "${RUN_DIR}/self-access.out" 2>&1
echo "  本身份读数: $(tr '\n' ' ' < "${RUN_DIR}/self-access.out")"
post_b=$(sed -nE 's/^post_access=(.*)$/\1/p' "${RUN_DIR}/self-access.out" | head -1)
run_case direct B_authed "${VDEV}" hid type "${TEXT_B}"
# 焦点竞态重试一次：探针就绪但一个字都没到，多半是激活时序而不是注入坏了
if [ "${probe_typed}" != "${TEXT_B}" ] && [ "${probe_typed}" != "__NO_PROBE__" ] \
   && [ "${probe_typed}" != "__NO_FOCUS__" ]; then
  echo "  （B 首次投递结果 '${probe_typed}'，重试一次）"
  run_case direct B_authed_retry "${VDEV}" hid type "${TEXT_B}"
fi
if [ "${probe_typed}" = "__NO_PROBE__" ] || [ "${probe_typed}" = "__NO_FOCUS__" ]; then
  fail "B: 探针未取得前台焦点（${probe_summary}）→ 本次没有验证，不能算通过"
elif echo "${probe_summary}" | grep -q 'active=true key=true' && [ -z "${probe_typed}" ]; then
  fail "B: 已授权身份一个字都没送达，且窗口全程在前台（不是焦点问题）：${probe_summary}"
elif [ "${post_b}" != "true" ]; then
  env_fail "B: 本身份 post_access=${post_b}（不是已授权身份，B 段对照组不成立；请在已授权终端里跑本脚本）"
elif [ "${probe_typed}" = "${TEXT_B}" ]; then
  pass "B: 已授权身份逐字送达（${probe_typed}）"
else
  fail "B: 已授权身份送达 '${probe_typed}'，期望 '${TEXT_B}'"
fi

# ---- D) 稳定注入器身份（人工授权过就该能注入；信息项，不计失败）----
echo "== D) 稳定注入器身份（人工授权的那个 bundle）=="
d_access_out="${RUN_DIR}/injector-stable-access.out"
open -n -W --stdout "${d_access_out}" "${INJECTOR_APP}" --args hid access >/dev/null 2>&1
post_d=$(sed -nE 's/^post_access=(.*)$/\1/p' "${d_access_out}" | head -1)
echo "  ${INJECTOR_APP} post_access=${post_d:-<空>}"
if [ "${post_d}" = "true" ]; then
  TEXT_D="authorized-injector-ok"
  run_case app D_authorized "${INJECTOR_APP}" hid type "${TEXT_D}"
  if [ "${probe_typed}" = "${TEXT_D}" ]; then
    pass "D: 人工授权过的注入器身份现在能逐字送达（同一身份：授权前报错 → 授权后可用）"
  elif [ "${probe_typed}" = "__NO_PROBE__" ] || [ "${probe_typed}" = "__NO_FOCUS__" ]; then
    echo "  D: 探针未取得前台焦点，本段未验证（不计失败）"
  else
    fail "D: 该身份已授权（post_access=true）却只送达 '${probe_typed}'"
  fi
else
  env_fail "D: 稳定注入器身份尚未授权 → #58 的\"授权跨运行持久\"这一段没有验证（整体 PASS 不能代表它已验收）"
  echo "  D: 该身份尚未授权——在 系统设置 → 隐私与安全性 → 辅助功能 里加入"
  echo "     ${INJECTOR_APP} 后重跑本脚本，可复现「授权前报错 → 授权后可用」"
  echo "     （若列表里已有同名旧条目：那是 cdhash 型的失效授权，先 \"-\" 删掉再重新添加，"
  echo "       否则会出现\"看着已勾选但 post_access 仍为 false\"的假象）"
fi

# ---- C) 安全输入 ----
echo "== C) 安全输入（Secure Input）=="
"${SECURE_HELPER_BIN}" & secure_pid=$!
sec_ready=false
for _ in $(seq 1 60); do
  if "${VDEV}" hid access 2>/dev/null | grep -q '^secure_input=true$'; then sec_ready=true; break; fi
  sleep 0.2
done
if [ "${sec_ready}" != "true" ]; then
  fail "C: 没能确定性开启安全输入（EnableSecureEventInput 未对系统生效）→ 本段未验证"
else
  echo "  安全输入已开启（vdev hid access 报 secure_input=true），保持器 pid=${secure_pid} 在用例期间存活"
  TEXT_C="secure-input-blocked"
  run_case direct C_secure "${VDEV}" hid type "${TEXT_C}"
  if [ "${probe_typed}" = "__NO_PROBE__" ] || [ "${probe_typed}" = "__NO_FOCUS__" ]; then
    fail "C: 探针未取得前台焦点（${probe_summary}）→ 本次没有验证"
  else
    got_c=$(python3 -c 'import sys; print(len(sys.argv[1]))' "${probe_typed}")
    if echo "${inject_out_text}" | grep -q "安全输入"; then
      # 报错是确定性的（护栏）；窗口实际收到几个字符是**事实记录**：安全输入是否
      # 连普通窗口一起拦取决于系统，客观记录，不写死期望。
      pass "C: 安全输入下明确报错（护栏生效；该窗口实际收到 ${got_c} 字符）"
      echo "  事实记录：安全输入开启时，合成事件到达普通前台窗口 ${got_c} 个字符"
    else
      fail "C: 安全输入下没有报出安全输入相关错误：$(echo "${inject_out_text}" | tail -1)"
    fi
  fi
fi
kill "${secure_pid}" 2>/dev/null || true
wait "${secure_pid}" 2>/dev/null || true
if "${VDEV}" hid access 2>/dev/null | grep -q '^secure_input=true$'; then
  fail "C: 安全输入没有恢复（保持器退出后仍为 true）"
fi

echo "fails=${fails} env_bad=${env_bad}"
if [ "${fails}" -gt 0 ]; then
  echo HID_ACCESS_RESULT=FAIL
  exit 1
fi
if [ "${env_bad}" -gt 0 ]; then
  echo "HID_ACCESS_RESULT=NOT_RUN（环境不具备：请在已授权终端里、且系统未处于安全输入时重跑）"
  exit 2
fi
echo HID_ACCESS_RESULT=PASS
