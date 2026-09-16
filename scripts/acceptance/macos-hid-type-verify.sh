#!/bin/bash
# vdev hid type 真机验收：最小 .app 探针窗收文本 + 可选全局 EventTap 计数。
#
# 用法：macos-hid-type-verify.sh [path/to/vdev]
#
# 为什么打成 .app：裸二进制在 macOS 26 上拿不到前台焦点（focus-stealing
# prevention），探针永远 active=false；同一份二进制打成最小 .app 用
# `open -W -n` 启动即可 active=true key=true。
#
# 阳性对照（修复前应红）：旧实现 cgevents::type_string 背靠背发 down/up，
# `hello from vdev` 窗口只收到 2/17 或 EventTap 只到 2/15；修复后逐字一致。
#
# 安全/假绿边界：
# - 探针窗口未取得前台焦点时不注入（SKIP），绝不把合成键发到别的 App；
# - swiftc/open 失败 / python3 缺失 → FAIL（exit 2），不进入 SKIP 分支；
# - 所有用例都 SKIP → exit 2（"没验证"不能算通过）；
# - 注入期间探针失去前台焦点（TOCTOU）→ 该用例按 SKIP 处理并打印原因；
# - EventTap 在 .app 身份下可能拿不到辅助功能权限 → tap_ok=false 时以窗口
#   TYPED == 输入为准；tap_ok=true 时额外要求 tap_delta >= 字符数。
set -u

VDEV=${1:-target/release/vdev}
if [ ! -x "${VDEV}" ]; then
  echo "FAIL: vdev 二进制不可执行：${VDEV}（先 cargo build -p vdev-host --release）"
  exit 2
fi
if ! command -v python3 >/dev/null 2>&1; then
  echo "FAIL: 缺 python3（用于按字符数断言）"
  exit 2
fi

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
RUN_DIR=$(mktemp -d -t vdev-hid-type.XXXXXX)
# 看门狗：open -W 只等启动器，app 若卡死需按精确进程名收口（禁用 pkill -f）。
cleanup() {
  pkill -x VdevHidProbe 2>/dev/null || true
  rm -rf "${RUN_DIR}"
}
trap cleanup EXIT

PROBE_BIN="${RUN_DIR}/probe"
if ! swiftc -O "${SCRIPT_DIR}/macos-hid-type-probe.swift" -o "${PROBE_BIN}"; then
  echo "FAIL: 探针编译失败（swiftc）"
  exit 2
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
if ! command -v open >/dev/null 2>&1; then
  echo "FAIL: 缺 open（macOS 启动器）"
  exit 2
fi

CASES=(
  "hello from vdev"
  "vdev-accept-中文-42"
  "the quick brown fox"
  "ABC123!@#"
  "q1q1"
  "中文中文"
  "abcdefghijklmnop"
  "0123456789"
)
total=${#CASES[@]}

# 有界停止：pkill 精确名杀掉卡死的 app，让 open -W 返回后再 wait。
stop_probe() {
  pkill -x VdevHidProbe 2>/dev/null || true
  wait "${open_pid}" 2>/dev/null || true
}

fails=0
skipped=0
executed=0
# 预热一次：首次 open 一个有 bundle 身份的 App 常有激活竞态，先跑一个
# throwaway 实例（激活等待 4s + 注入窗口 1s ≈ 5s），避免首个真实用例首发竞态。
warm_out="${RUN_DIR}/warmup.out"
: > "${warm_out}"
open -W -n "${PROBE_APP}" --args "${warm_out}" 1 >/dev/null 2>&1 || true

i=0
for text in "${CASES[@]}"; do
  i=$((i + 1))
  out="${RUN_DIR}/case-${i}.out"
  open_log="${RUN_DIR}/case-${i}.open.log"
  : > "${out}"
  open -W -n "${PROBE_APP}" --args "${out}" 3 >"${open_log}" 2>&1 &
  open_pid=$!

  ready=""
  for _ in $(seq 1 80); do
    ready=$(grep -m1 '^READY' "${out}" 2>/dev/null || true)
    [ -n "${ready}" ] && break
    kill -0 "${open_pid}" 2>/dev/null || break
    sleep 0.1
  done
  if [ -z "${ready}" ]; then
    echo "FAIL '${text}'（探针未就绪/启动失败，open_log: ${open_log}）"
    cat "${open_log}" 2>/dev/null | tail -3
    stop_probe
    fails=$((fails + 1))
    continue
  fi
  if ! echo "${ready}" | grep -q 'active=true key=true'; then
    echo "SKIP '${text}'（探针窗口未取得前台焦点：${ready}）"
    stop_probe
    skipped=$((skipped + 1))
    continue
  fi

  tap_before=$(echo "${ready}" | sed -nE 's/.*tap=([0-9]+).*/\1/p')
  tap_ok=$(echo "${ready}" | sed -nE 's/.*tap_ok=(true|false).*/\1/p')
  if [ "${tap_ok}" != "true" ] && [ "${tap_ok}" != "false" ]; then
    echo "FAIL '${text}'（READY 缺少合法 tap_ok：${ready}）"
    stop_probe
    fails=$((fails + 1))
    continue
  fi
  "${VDEV}" hid type "${text}" >/dev/null 2>&1
  wait "${open_pid}"

  summary=$(grep -m1 '^SUMMARY' "${out}" || true)
  typed=$(grep -m1 '^TYPED=' "${out}" | sed 's/^TYPED=//' || true)
  tap_after=$(echo "${summary}" | sed -nE 's/.*tap=([0-9]+).*/\1/p')
  tap_delta=$((tap_after - tap_before))
  # 用环境变量传文本，避免文本以 '-' 开头时被 python 当成选项。
  chars=$(TEXT="${text}" python3 -c 'import os; print(len(os.environ["TEXT"]))')

  if [ "${typed}" = "${text}" ] && { [ "${tap_ok}" != "true" ] || [ "${tap_delta}" -ge "${chars}" ]; }; then
    # 窗口已经逐字收到全部文本（tap 可用时计数也达标）——注入发生在 SUMMARY
    # 之前，结束时的焦点变化不影响这次已完成的注入。
    echo "PASS '${text}'（chars=${chars} tap_ok=${tap_ok} tap_delta=${tap_delta}）"
    executed=$((executed + 1))
  elif ! echo "${summary}" | grep -q 'active=true key=true'; then
    # TYPED 不匹配且结束焦点已丢失：可能只收到部分字符，按 skip 处理，绝不冒充通过。
    echo "SKIP '${text}'（注入期间探针失去前台焦点，结果不可信：${summary}）"
    skipped=$((skipped + 1))
  else
    echo "FAIL '${text}'（窗口 TYPED='${typed}'，期望 chars=${chars}；${summary}）"
    fails=$((fails + 1))
  fi
done

echo "fails=${fails} skipped=${skipped} executed=${executed} total=${total}"
if [ "${fails}" -gt 0 ]; then
  echo HID_TYPE_RESULT=FAIL
  exit 1
fi
if [ "${executed}" -eq 0 ]; then
  echo "HID_TYPE_RESULT=NOT_RUN（没有任何用例取得前台焦点并完成注入，本次没有验证，不能算通过）"
  exit 2
fi
if [ "${skipped}" -gt 0 ]; then
  echo "HID_TYPE_RESULT=PARTIAL（${skipped}/${total} 个用例未执行；已执行的 ${executed} 个全过）"
  exit 0
fi
echo HID_TYPE_RESULT=PASS
