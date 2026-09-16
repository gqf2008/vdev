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
# 用法：macos-hid-access-verify.sh [path/to/vdev]
# 退出码：0 = 全部通过；1 = 有用例失败；2 = 环境不具备（没验证，不算通过）
set -u

VDEV=${1:-target/release/vdev}
if [ ! -x "${VDEV}" ]; then
  echo "FAIL: vdev 二进制不可执行：${VDEV}（先 cargo build -p vdev-host --release）"
  exit 2
fi
VDEV=$(cd "$(dirname "${VDEV}")" && pwd)/$(basename "${VDEV}")
for tool in swiftc python3 open; do
  command -v "${tool}" >/dev/null 2>&1 || { echo "FAIL: 缺 ${tool}"; exit 2; }
done

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
RUN_DIR=$(mktemp -d -t vdev-hid-access.XXXXXX)
# 注入器 app 有两份：
#   INJECTOR_APP        —— 固定路径 + 固定 bundle id，供人工在系统设置里授权
#                          （授权后复现"同一身份从报错变成能注入"）
#   INJECTOR_APP_UNAUTH —— 每次运行都换新 bundle id，**保证从未被授权**，
#                          作为 A / A2 的未授权对照（用固定 id 会被历史授权污染）
INJECTOR_DIR="${HOME}/.vdev-hid-injector"
INJECTOR_APP="${INJECTOR_DIR}/VdevHidInjector.app"
UNAUTH_ID="com.vdev.hid.injector.unauth.$$.$(date +%s)"
INJECTOR_APP_UNAUTH="${RUN_DIR}/VdevHidUnauth.app"
SECURE_HELPER_BIN="${RUN_DIR}/secure-input-hold"

cleanup() {
  pkill -x VdevHidProbe 2>/dev/null || true
  pkill -x SecureInputHold 2>/dev/null || true
  rm -rf "${RUN_DIR}"
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

# 2) 稳定注入器 app（固定 id + **稳定签名**，供人工授权一次后长期有效）
#
# 为什么要签名：未签名的 app，TCC 的授权要求是 `cdhash H"..."`（实测 csreq 40 字节），
# 而本脚本每轮都会把新的 vdev 二进制拷进 bundle —— cdhash 一变，人工授权立刻失配
# （issue #58）。用 Apple Development/Developer ID 签名后，要求变成
# `identifier "com.vdev.hid.injector" and anchor apple generic and certificate leaf[…]`，
# 与二进制内容无关，授权才能跨运行跨重建稳定。
#
# 旁证：本机签名版 VDCamera.app 的 TCC csreq 就是这种身份型要求（160 字节）。
INJECTOR_BIN="${INJECTOR_APP}/Contents/MacOS/VdevHidInjector"
# 戳记必须记"源二进制的 sha256"，**不能**拿 bundle 内二进制去 cmp：
# codesign 会把签名嵌进 Mach-O 本身，签过名的可执行文件永远不等于源文件。
INJECTOR_STAMP="${INJECTOR_DIR}/.injector-source.sha256"
mkdir -p "${INJECTOR_APP}/Contents/MacOS" "${INJECTOR_DIR}"
VDEV_SHA=$(shasum -a 256 "${VDEV}" | awk '{print $1}')
if [ ! -f "${INJECTOR_BIN}" ] || [ ! -f "${INJECTOR_STAMP}" ] \
   || [ "$(cat "${INJECTOR_STAMP}" 2>/dev/null)" != "${VDEV_SHA}" ]; then
  cp "${VDEV}" "${INJECTOR_BIN}"
  INJECTOR_NEEDS_SIGN=1
fi
cat > "${INJECTOR_APP}/Contents/Info.plist" <<'IPLIST'
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
IPLIST

# 2a) 签名（身份可复现时）
# 优先 Apple Development（与本仓 crates/vdev-camera/Makefile 的 SIGN_APP_IDENTITY 一致），
# 其次 Developer ID；都没有则退回 ad-hoc 并**明确警告**——那种情况下 D 段的授权
# 每轮都会失效，必须让使用者知道。
INJECTOR_SIGN_IDENTITY="${VDEV_HID_SIGN_IDENTITY:-Apple Development: qingfeng gao (7L8FV63FAP)}"
if [ -n "${INJECTOR_NEEDS_SIGN:-}" ]; then
  if codesign --force --sign "${INJECTOR_SIGN_IDENTITY}" -i com.vdev.hid.injector \
       --options runtime --timestamp=none "${INJECTOR_APP}" >/dev/null 2>&1; then
    echo "  注入器已用稳定身份签名：${INJECTOR_SIGN_IDENTITY}"
    printf '%s' "${VDEV_SHA}" > "${INJECTOR_STAMP}"
  else
    echo "WARN: 用 '${INJECTOR_SIGN_IDENTITY}' 签名失败 → 退回 ad-hoc。"
    # 没签成也记戳：至少避免每轮都无谓重写（重写会让 TCC 的 cdhash 型授权失效）
    echo "WARN: ad-hoc 的 TCC 授权要求是 cdhash，二进制一变授权就失效（issue #58）；"
    echo "WARN: D 段（授权前报错 → 授权后可用）将无法跨运行复现。"
    printf '%s' "${VDEV_SHA}" > "${INJECTOR_STAMP}"
  fi
fi
# 把「TCC 会拿到的要求」打出来做证据：身份型要求里不该出现 cdhash
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
open -W -n "${PROBE_APP}" --args "${warm_out}" 1 >/dev/null 2>&1 || true

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
      osascript -e 'tell application "System Events" to set frontmost of process "VdevHidProbe" to true' >/dev/null 2>&1 || true
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
    # READY 之后仍可能有极短的在途事件，注入前留一小段静默期（探针已在 READY
  # 时清零计数，这里只是再降一点概率）
  sleep 0.3
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
