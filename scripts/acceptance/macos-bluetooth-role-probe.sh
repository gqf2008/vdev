#!/bin/bash
#
# macos-bluetooth-role-probe.sh — 构建并运行「macOS 能否当手机可识别的蓝牙耳麦/音箱」调研探针
#
# 背景、完整证据链与结论见 docs/dev/macos-bluetooth-role-survey.md（vdev issue: bt-role-survey-1）。
# 一句话结论：控制面可用（SLC / 来电显示 / 从 Mac 拨号 / 通话状态），
#             **通话音频拿不到**（macOS 26 的 IOBluetoothHandsFreeDevice 已无 SCO 音频桥），
#             且 macOS 没有 A2DP Sink，所以「当音箱」完全不可行。
#
# 行为分级（重要）：
#   list            只读，不改任何状态
#   sdp             只读，不改变持久状态（会向对端发起 SDP 查询并建立 ACL 连接）
#   hfp / hci       会真的连接手机；配合 --dial 会真的拨出电话、--auto-accept 会真的接听
#                   （均可逆：退出即断开）
#
# 用法:
#   ./macos-bluetooth-role-probe.sh [-OutDir DIR] build
#   ./macos-bluetooth-role-probe.sh [-OutDir DIR] list
#   ./macos-bluetooth-role-probe.sh [-OutDir DIR] sdp  <地址|名字>
#   ./macos-bluetooth-role-probe.sh [-OutDir DIR] hfp  <地址|名字> [--seconds N] [--audio] [--reset] [--dial N] ...
#   ./macos-bluetooth-role-probe.sh [-OutDir DIR] hci  --addr <aa:bb:cc:dd:ee:ff>
#
# 构建产物默认写 ${TMPDIR:-/tmp}/vdev-bt-probe（用 -OutDir 覆盖），不入库。
#
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OUT_DIR="${TMPDIR:-/tmp}/vdev-bt-probe"

HFP_SRC="$SCRIPT_DIR/macos-bluetooth-hfp-probe.m"
HCI_SRC="$SCRIPT_DIR/macos-bluetooth-hci-probe.m"

usage() {
  sed -n '3,20p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

if [ "${1:-}" = "-OutDir" ]; then
  [ $# -ge 2 ] || { echo "FAIL: -OutDir 需要一个目录参数" >&2; exit 2; }
  OUT_DIR="$2"
  shift 2
fi
OUT_DIR="${OUT_DIR%/}"

CMD="${1:-}"
[ $# -gt 0 ] && shift || true

if [ -z "$CMD" ]; then usage; exit 2; fi

# 缺工具立即失败：不然会退化成"什么都没跑却看起来正常"
if ! command -v clang >/dev/null 2>&1; then
  echo "FAIL: 未找到 clang（需要 Xcode command line tools）" >&2
  exit 2
fi
for f in "$HFP_SRC" "$HCI_SRC"; do
  if [ ! -f "$f" ]; then
    echo "FAIL: 缺少探针源码 $f" >&2
    exit 2
  fi
done

HFP_BIN="$OUT_DIR/hfp-probe"
HCI_BIN="$OUT_DIR/hci-probe"

# `-OutDir` 只认子命令之前的位置。子命令之后再出现就明确报错，
# 不要静默丢弃（`sdp -OutDir X` 会把 -OutDir 当成设备名，`list -OutDir X` 则完全没效果）。
reject_stray_outdir() {
  for a in "$@"; do
    if [ "$a" = "-OutDir" ]; then
      echo "FAIL: -OutDir 必须写在子命令之前，例如：$0 -OutDir DIR $CMD ..." >&2
      exit 2
    fi
  done
}

# 源文件没变就不重编：避免每次运行都重写二进制（重写会让该路径的 TCC/签名身份漂移）
need_build() { [ ! -x "$1" ] || [ "$2" -nt "$1" ]; }

build_all() {
  mkdir -p "$OUT_DIR"
  if need_build "$HFP_BIN" "$HFP_SRC"; then
    echo "编译 hfp-probe -> $HFP_BIN" >&2
    clang -O2 -fobjc-arc -framework Foundation -framework IOBluetooth -framework CoreAudio \
      -o "$HFP_BIN" "$HFP_SRC"
  fi
  if need_build "$HCI_BIN" "$HCI_SRC"; then
    echo "编译 hci-probe -> $HCI_BIN" >&2
    clang -O2 -fobjc-arc -framework Foundation -framework IOBluetooth \
      -o "$HCI_BIN" "$HCI_SRC"
  fi
}

case "$CMD" in
  build)
    build_all
    echo "产物目录: $OUT_DIR"
    ls -l "$HFP_BIN" "$HCI_BIN"
    ;;
  list)
    if [ $# -gt 0 ]; then
      reject_stray_outdir "$@"
      echo "FAIL: list 不接受额外参数（收到：$*）" >&2
      exit 2
    fi
    build_all
    exec "$HFP_BIN" --list
    ;;
  sdp)
    reject_stray_outdir "$@"
    [ $# -ge 1 ] || { echo "FAIL: sdp 需要 <地址|名字>" >&2; exit 2; }
    build_all
    exec "$HFP_BIN" --sdp "$@"
    ;;
  hfp)
    reject_stray_outdir "$@"
    [ $# -ge 1 ] || { echo "FAIL: hfp 需要 <地址|名字>" >&2; exit 2; }
    build_all
    echo "⚠️  hfp 模式会真的向目标手机发起 HFP 连接；带 --dial 会真的拨号，" \
         "--auto-accept 会真的接听（可逆，退出即断）" >&2
    exec "$HFP_BIN" --hfp "$@"
    ;;
  hci)
    reject_stray_outdir "$@"
    build_all
    echo "⚠️  hci 模式会向蓝牙控制器发 HCI 命令（含 Setup Synchronous Connection），" \
         "不做持久改动" >&2
    exec "$HCI_BIN" "$@"
    ;;
  -h|--help|help)
    usage
    ;;
  *)
    echo "FAIL: 未知子命令 '$CMD'" >&2
    usage
    exit 2
    ;;
esac
