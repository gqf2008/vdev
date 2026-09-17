#!/bin/bash
# 组装 macOS 发布包：命令行工具 + librnnoise.dylib + CoreAudio HAL 插件。
#
# 用法: package-macos.sh <tag> <librnnoise.dylib> [<输出目录，默认 dist>]
#
# 产物: <输出目录>/vdev-macos-<tag>.zip（外加同名解包目录，便于 CI 里继续检查）
# 本脚本可本机直接跑（不依赖 CI），CI 的 macos-tools job 就是调它。
set -euo pipefail

TAG="${1:?用法: package-macos.sh <tag> <librnnoise.dylib> [输出目录]}"
DYNAMIC_LIB="${2:?用法: package-macos.sh <tag> <librnnoise.dylib> [输出目录]}"
OUT_DIR="${3:-dist}"

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
REPO_ROOT=$(cd "${SCRIPT_DIR}/../.." && pwd)
cd "${REPO_ROOT}"

[ -f "${DYNAMIC_LIB}" ] || { echo "找不到 dylib: ${DYNAMIC_LIB}" >&2; exit 1; }
TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"

echo "== 构建 Rust 产物"
cargo build --release -p vdev-mic-agent -p vdev-host
# HAL 插件：裸 make 只构建（不写系统目录），插件自身会 adhoc 签名（无 Developer ID 时）
make -C crates/vdev-audio build

echo "== 组装"
STAGE="${OUT_DIR}/vdev-macos-${TAG}"
if [ -d "${STAGE}" ]; then mv "${STAGE}" "/tmp/vdev-macos.stage.old.$$(date +%s)"; fi
mkdir -p "${STAGE}/bin"
cp "${TARGET_DIR}/release/vdev-mic-agent" "${STAGE}/bin/"
cp "${TARGET_DIR}/release/vdev" "${STAGE}/bin/"
cp "${TARGET_DIR}/release/vdev-audio-ctl" "${STAGE}/bin/"
# dylib 与 mic-agent 同目录：加载器会优先在可执行文件旁边找（用户不用带 --dll）
cp "${DYNAMIC_LIB}" "${STAGE}/bin/librnnoise.dylib"
chmod 755 "${STAGE}/bin/"*
cp -R crates/vdev-audio/build/vdev-audio.driver "${STAGE}/"
cp scripts/release/macos-README.md "${STAGE}/README.md"

echo "== 冒烟 1：adhoc 签名要能自证"
codesign --verify --strict "${STAGE}/vdev-audio.driver" || {
  echo "vdev-audio.driver 签名校验失败" >&2; exit 1; }

echo "== 冒烟 2：包里的 mic-agent 要能找到包里的 dylib 并真的跑起来"
TMP=$(mktemp -d "${TMPDIR:-/tmp}/vdev-macos-smoke.XXXXXX")
trap 'rm -rf "${TMP}"' EXIT
python3 - "${TMP}/probe.wav" <<'PY'
import math, struct, sys, wave
sr, secs = 48000, 1.0
with wave.open(sys.argv[1], "wb") as w:
    w.setnchannels(1); w.setsampwidth(2); w.setframerate(sr)
    w.writeframes(b"".join(
        struct.pack("<h", int(12000 * math.sin(2 * math.pi * 220 * i / sr))) for i in range(int(sr * secs))))
PY
# 不带 --dll：走"可执行文件旁边"的搜索路径，正是发布包的摆放方式
"${STAGE}/bin/vdev-mic-agent" run "${TMP}/probe.wav" --mix 1 --out "${TMP}/out.wav" --stats "${TMP}/stats.json" >/dev/null
python3 - "${TMP}/stats.json" <<'PY'
import json, sys
run = json.load(open(sys.argv[1]))["run"]
assert run["frames"] > 0, "mic-agent 没处理任何帧"
print(f"   mic-agent 处理 {run['frames']} 帧，帧长 {run['frame_ms']} ms（dylib 从包内自动加载）")
PY

echo "== 打包（COPYFILE_DISABLE 避免 ._ 资源叉）"
ZIP="${OUT_DIR}/vdev-macos-${TAG}.zip"
[ -f "${ZIP}" ] && mv "${ZIP}" "/tmp/vdev-macos.zip.old.$$(date +%s)"
( cd "${OUT_DIR}" && COPYFILE_DISABLE=1 zip -qry "vdev-macos-${TAG}.zip" "vdev-macos-${TAG}" )
if unzip -l "${ZIP}" | grep -q "__MACOSX\|/\._"; then
  echo "zip 里混进了 macOS 元数据（__MACOSX/._）" >&2; exit 1
fi
echo "== 产物：${ZIP}"
unzip -l "${ZIP}" | tail -20
shasum -a 256 "${ZIP}"
