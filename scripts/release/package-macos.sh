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
# 许可证是硬要求：BSD-3-Clause 要求二进制再分发随附版权声明，缺了就出不了包
[ -f "${DYNAMIC_LIB}.COPYING" ] || {
  echo "缺少 ${DYNAMIC_LIB}.COPYING（由 build-rnnoise-macos.sh 带出）——BSD-3 要求随包分发许可证原文" >&2
  exit 1
}
TARGET_DIR="${CARGO_TARGET_DIR:-${REPO_ROOT}/target}"

echo "== 构建 Rust 产物"
cargo build --release -p vdev-mic-agent -p vdev-host
# HAL 插件：裸 make 只构建（不写系统目录），插件自身会 adhoc 签名（无 Developer ID 时）
make -C crates/vdev-audio build

echo "== 组装"
STAGE="${OUT_DIR}/vdev-macos-${TAG}"
mkdir -p "${OUT_DIR}"
# 先在临时目录组装：中途失败时不会在 dist 里留下半个包
BUILD_STAGE=$(mktemp -d "${TMPDIR:-/tmp}/vdev-macos-stage.XXXXXX")
STAGE="${BUILD_STAGE}/vdev-macos-${TAG}"
mkdir -p "${STAGE}/bin" "${STAGE}/licenses"
cp "${TARGET_DIR}/release/vdev-mic-agent" "${STAGE}/bin/"
cp "${TARGET_DIR}/release/vdev" "${STAGE}/bin/"
cp "${TARGET_DIR}/release/vdev-audio-ctl" "${STAGE}/bin/"
# dylib 与 mic-agent 同目录：加载器会优先在可执行文件旁边找（用户不用带 --dll）
cp "${DYNAMIC_LIB}" "${STAGE}/bin/librnnoise.dylib"
chmod 755 "${STAGE}/bin/"*
cp -R crates/vdev-audio/build/vdev-audio.driver "${STAGE}/"
cp scripts/release/macos-README.md "${STAGE}/README.md"
cp crates/vdev-mic-agent/THIRD_PARTY.md "${STAGE}/THIRD_PARTY.md"
cp "${DYNAMIC_LIB}.COPYING" "${STAGE}/licenses/rnnoise-COPYING.txt"

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

echo "== 包内校验和（zip 是运输层，包内这些文件的哈希也留一份）"
( cd "${STAGE}" && find . -type f ! -name SHA256SUMS.txt -print0 | sort -z | xargs -0 shasum -a 256 > SHA256SUMS.txt )
wc -l < "${STAGE}/SHA256SUMS.txt" | xargs echo "   包内 SHA256SUMS.txt 条目数:"

echo "== 打包（COPYFILE_DISABLE 避免 ._ 资源叉）"
# 组装成功才把 stage 落到 dist（失败 = 一行都不留）
FINAL_STAGE="${OUT_DIR}/vdev-macos-${TAG}"
if [ -d "${FINAL_STAGE}" ]; then mv "${FINAL_STAGE}" "/tmp/vdev-macos.stage.old.$$(date +%s)"; fi
mv "${STAGE}" "${FINAL_STAGE}"
STAGE="${FINAL_STAGE}"
ZIP="${OUT_DIR}/vdev-macos-${TAG}.zip"
[ -f "${ZIP}" ] && mv "${ZIP}" "/tmp/vdev-macos.zip.old.$$(date +%s)"
( cd "${OUT_DIR}" && COPYFILE_DISABLE=1 zip -qry "vdev-macos-${TAG}.zip" "vdev-macos-${TAG}" )
if unzip -l "${ZIP}" | grep -q "__MACOSX\|/\._"; then
  echo "zip 里混进了 macOS 元数据（__MACOSX/._）" >&2; exit 1
fi
echo "== 产物：${ZIP}"
unzip -l "${ZIP}" | tail -20
shasum -a 256 "${ZIP}"
