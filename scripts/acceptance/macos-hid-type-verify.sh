#!/bin/bash
# vdev hid type 真机验收：全局 EventTap 计数 + AppKit 窗口实际收到的文本。
# 用法：macos-hid-type-verify.sh [path/to/vdev]
#
# 阳性对照（修复前应红）：旧实现 cgevents::type_string 背靠背发 down/up，
# `hello from vdev` 在 EventTap 上只到 2/15；修复后应到 15/15。
#
# 安全/假绿边界：
# - 探针窗口未取得前台焦点时不注入（SKIP），绝不把合成键发到别的 App；
# - swiftc 编译失败 / python3 缺失 → exit 2，不进入 SKIP 分支；
# - 所有用例都 SKIP → exit 2（"没验证"不能算通过）；
# - 注入期间探针失去前台焦点（TOCTOU）→ 该用例按 SKIP 处理并打印原因。
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
cleanup() { rm -rf "${RUN_DIR}"; }
trap cleanup EXIT
PROBE_BIN="${RUN_DIR}/probe"
if ! swiftc -O "${SCRIPT_DIR}/macos-hid-type-probe.swift" -o "${PROBE_BIN}"; then
  echo "FAIL: 探针编译失败（swiftc）"
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

fails=0
skipped=0
executed=0
i=0
for text in "${CASES[@]}"; do
  i=$((i + 1))
  log="${RUN_DIR}/case-${i}.log"
  "${PROBE_BIN}" 5 >"${log}" 2>&1 &
  probe_pid=$!

  ready=""
  for _ in $(seq 1 80); do
    ready=$(grep -m1 '^READY' "${log}" 2>/dev/null || true)
    [ -n "${ready}" ] && break
    kill -0 "${probe_pid}" 2>/dev/null || break
    sleep 0.1
  done
  if [ -z "${ready}" ]; then
    echo "SKIP '${text}'（探针 8s 内未就绪）"
    kill "${probe_pid}" 2>/dev/null || true
    wait "${probe_pid}" 2>/dev/null || true
    skipped=$((skipped + 1))
    continue
  fi
  if ! echo "${ready}" | grep -q 'active=true key=true'; then
    echo "SKIP '${text}'（探针窗口未取得前台焦点：${ready}）"
    kill "${probe_pid}" 2>/dev/null || true
    wait "${probe_pid}" 2>/dev/null || true
    skipped=$((skipped + 1))
    continue
  fi

  tap_before=$(echo "${ready}" | sed -nE 's/.*tap=([0-9]+).*/\1/p')
  "${VDEV}" hid type "${text}" >/dev/null 2>&1
  wait "${probe_pid}"

  summary=$(grep -m1 '^SUMMARY' "${log}" || true)
  typed=$(grep -m1 '^TYPED=' "${log}" | sed 's/^TYPED=//' || true)
  tap_after=$(echo "${summary}" | sed -nE 's/.*tap=([0-9]+).*/\1/p')
  # 用环境变量传文本，避免文本以 '-' 开头时被 python 当成选项。
  chars=$(TEXT="${text}" python3 -c 'import os; print(len(os.environ["TEXT"]))')
  tap_delta=$((tap_after - tap_before))

  if ! echo "${summary}" | grep -q 'active=true key=true'; then
    # 注入期间焦点被抢走：结果不可信，按 skip 处理并明确标出，绝不冒充通过。
    echo "SKIP '${text}'（注入期间探针失去前台焦点，结果不可信：${summary}）"
    skipped=$((skipped + 1))
  elif [ "${typed}" = "${text}" ] && [ "${tap_delta}" -ge "${chars}" ]; then
    echo "PASS '${text}'（chars=${chars} tap_delta=${tap_delta}）"
    executed=$((executed + 1))
  else
    echo "FAIL '${text}'（chars=${chars} tap_delta=${tap_delta} typed='${typed}'；${summary}）"
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
