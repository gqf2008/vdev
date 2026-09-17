#!/bin/bash
# 为 macOS 发布包构建 librnnoise.dylib（vdev-mic-agent 的降噪后端，运行时 dlopen 加载）。
#
# 为什么从源码构建而不是取预编译包：
#   * Homebrew 的 `rnnoise` 是 LADSPA/VST 插件 cask，不是 xiph 的库；
#   * 我们要的是导出 `rnnoise_*` C API 的 `librnnoise.dylib`。
#
# 为什么钉 master 的某个 commit 而不是 tag：
#   * 上游 tag `v0.2` 在 arm64 上编不过（`src/vec_neon.h` include 了一个仓库里不存在的
#     `os_support.h`，属上游打包 bug），而 master 已删掉该文件、可正常构建；
#   * `70f1d25` 正是本仓库 mic-agent 全部延迟/质量实测所用的那个 commit（README 里的
#     20 ms / SI-SDR 数字都是它跑出来的），发布包与文档因此同源。
#   * Windows 侧继续用 MSYS2 的 `mingw-w64-ucrt-x86_64-rnnoise` 0.2-2（该平台历史一直如此），
#     两边版本差异会在 Release 说明里写明。
#
# 用法：build-rnnoise-macos.sh <输出 dylib 路径>
set -euo pipefail

# 全量 commit sha（短名 fetch 不到，必须给全量）
REF="${RNNOISE_REF:-70f1d256acd4b34a572f999a05c87bf00b67730d}"
OUT="${1:?用法: build-rnnoise-macos.sh <输出 dylib 路径>}"
REPO_URL="${RNNOISE_REPO_URL:-https://github.com/xiph/rnnoise.git}"
# 模型标识 = master 分支 `model_version` 的内容（该分支存的就是全量 sha256），文件名就是它；
# sha256 单独再写一遍字面量，是为了让 `macos-smoke.sh` 能钉住"哈希没被悄悄改过"。
MODEL_TAG="0a8755f8e2d834eff6a54714ecc7d75f9932e845df35f8b59bc52a7cfe6e8b37"
MODEL_SHA256="0a8755f8e2d834eff6a54714ecc7d75f9932e845df35f8b59bc52a7cfe6e8b37"
MODEL_SIZE="58603099"

for tool in git curl tar make autoreconf; do
  command -v "${tool}" >/dev/null 2>&1 || {
    echo "缺少 ${tool}（macOS: brew install autoconf automake libtool）" >&2
    exit 1
  }
done

WORK=$(mktemp -d "${TMPDIR:-/tmp}/rnnoise-build.XXXXXX")
trap 'rm -rf "${WORK}"' EXIT

echo "== 取 xiph/rnnoise @ ${REF:0:7}（${REF}）"
git init --quiet "${WORK}/rnnoise"
cd "${WORK}/rnnoise"
git remote add origin "${REPO_URL}"
git fetch --quiet --depth 1 origin "${REF}"
git checkout --quiet FETCH_HEAD
HEAD_SHA=$(git rev-parse HEAD)
case "${HEAD_SHA}" in
  "${REF}"*) ;;
  *) echo "取到的 commit 不是钉子：期望 ${REF}，实际 ${HEAD_SHA}" >&2; exit 1;;
esac

# 模型：上游 master 的 model_version 就是全量 sha256，且其 download_model.sh 会校验；
# 我们额外钉住字节数（截断/半包会红），并且先断言 model_version 没变。
GOT_TAG=$(cat model_version)
[ "${GOT_TAG}" = "${MODEL_TAG}" ] || {
  echo "model_version 变了：期望 ${MODEL_TAG}，实际 ${GOT_TAG}（上游换模型 → 需重新跑延迟/质量实测再更新钉子）" >&2
  exit 1
}
MODEL="rnnoise_data-${MODEL_TAG}.tar.gz"
echo "== 下载模型（56 MiB，全量 sha256 校验）"
curl -fsSL --retry 3 -o "${MODEL}" "https://media.xiph.org/rnnoise/models/${MODEL}"
ACTUAL=$(shasum -a 256 "${MODEL}" | awk '{print $1}')
[ "${ACTUAL}" = "${MODEL_SHA256}" ] || {
  echo "模型校验失败：期望 ${MODEL_SHA256}，实际 ${ACTUAL}" >&2
  exit 1
}
SIZE=$(wc -c < "${MODEL}" | tr -d ' ')
[ "${SIZE}" = "${MODEL_SIZE}" ] || { echo "模型大小异常：${SIZE} != ${MODEL_SIZE}" >&2; exit 1; }
tar xvomf "${MODEL}" >/dev/null   # 解出 src/rnnoise_data.c

echo "== autoreconf + configure + make"
./autogen.sh >/dev/null 2>&1
./configure --quiet
make -j"$(sysctl -n hw.ncpu)" >/dev/null

mkdir -p "$(dirname "${OUT}")"
cp -f .libs/librnnoise.0.dylib "${OUT}"
chmod 755 "${OUT}"

echo "== 产物：${OUT}"
shasum -a 256 "${OUT}"
file "${OUT}"
# 导出符号是 mic-agent 的硬依赖，缺一个就会在运行时才炸，所以这里直接断言
for sym in rnnoise_get_frame_size rnnoise_get_size rnnoise_create rnnoise_process_frame rnnoise_destroy; do
  nm -gU "${OUT}" | grep -q " _${sym}\$" || { echo "产物缺少导出符号 ${sym}" >&2; exit 1; }
done
echo "== 导出符号齐全（${REF}）"
