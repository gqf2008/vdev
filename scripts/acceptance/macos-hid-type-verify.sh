#!/bin/bash
# vdev hid type 真机验收：全局 EventTap 计数 + AppKit 窗口实际收到的文本。
# 用法：macos-hid-type-verify.sh [path/to/vdev]
#
# 阳性对照（修复前应红）：旧实现 cgevents::type_string 背靠背发 down/up，
# `hello from vdev` 在 EventTap 上只到 2/15；修复后应到 15/15。
# 本脚本在探针未取得前台焦点时跳过注入并打印 SKIP，绝不把合成键发到别的 App。
set -u

VDEV=${1:-target/release/vdev}
if [ ! -x "$VDEV" ]; then
  echo "FAIL: vdev 二进制不可执行：${VDEV}（先 cargo build -p vdev-host --release）"
  exit 2
fi

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
PROBE_BIN=$(mktemp -t vdev-hid-type-probe.XXXXXX)
trap 'rm -f "$PROBE_BIN"' EXIT
swiftc -O "$SCRIPT_DIR/macos-hid-type-probe.swift" -o "$PROBE_BIN"

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

fails=0
skipped=0
for text in "${CASES[@]}"; do
  log=$(mktemp -t vdev-hid-type-log.XXXXXX)
  "$PROBE_BIN" 5 >"$log" 2>&1 &
  probe_pid=$!
  ready=""
  for _ in $(seq 1 80); do
    ready=$(grep -m1 '^READY' "$log" 2>/dev/null || true)
    [ -n "$ready" ] && break
    sleep 0.1
  done
  if [ -z "$ready" ]; then
    echo "SKIP '${text}'（探针 4s 内未就绪）"
    kill "$probe_pid" 2>/dev/null || true
    wait "$probe_pid" 2>/dev/null || true
    skipped=$((skipped + 1))
    rm -f "$log"
    continue
  fi
  if ! echo "$ready" | grep -q 'active=true key=true'; then
    echo "SKIP '${text}'（探针窗口未取得前台焦点：${ready}）"
    kill "$probe_pid" 2>/dev/null || true
    wait "$probe_pid" 2>/dev/null || true
    skipped=$((skipped + 1))
    rm -f "$log"
    continue
  fi
  tap_before=$(echo "$ready" | sed -nE 's/.*tap=([0-9]+).*/\1/p')
  "$VDEV" hid type "$text" >/dev/null 2>&1
  wait "$probe_pid"
  summary=$(grep -m1 '^SUMMARY' "$log" || true)
  typed=$(grep -m1 '^TYPED=' "$log" | sed 's/^TYPED=//' || true)
  tap_after=$(echo "$summary" | sed -nE 's/.*tap=([0-9]+).*/\1/p')
  chars=$(python3 -c 'import sys; print(len(sys.argv[1]))' "$text")
  tap_delta=$((tap_after - tap_before))
  if [ "$typed" = "$text" ] && [ "$tap_delta" -ge "$chars" ]; then
    echo "PASS '${text}'（chars=$chars tap_delta=$tap_delta）"
  else
    echo "FAIL '${text}'（chars=$chars tap_delta=$tap_delta typed='$typed'；$summary）"
    fails=$((fails + 1))
  fi
  rm -f "$log"
done

echo "fails=$fails skipped=$skipped"
if [ "$fails" -gt 0 ]; then
  echo HID_TYPE_RESULT=FAIL
  exit 1
fi
if [ "$skipped" -gt 0 ]; then
  echo "HID_TYPE_RESULT=PARTIAL（$skipped 个用例因前台焦点不可用被跳过；本脚本不允许无焦点注入）"
  exit 0
fi
echo HID_TYPE_RESULT=PASS
