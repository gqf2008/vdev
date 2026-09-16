#!/bin/bash
# macOS 验收脚本的无注入冒烟：只做 shell 语法检查 + Swift 探针类型检查。
#
# 这些脚本离开真机设备/前台焦点无法完整跑，但"脚本自身腐烂"必须能在 CI 变红：
# 本脚本不注入键鼠、不装驱动、不访问 /Library 或 /Applications，只跑静态检查。
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO_ROOT"

bash -n scripts/acceptance/macos-hid-type-verify.sh
bash -n crates/vdev-audio/test_loopback.sh

if ! command -v swiftc >/dev/null 2>&1; then
  echo "FAIL: 未找到 swiftc（macOS 验收冒烟需要 Xcode command line tools）" >&2
  exit 1
fi
swiftc -O -typecheck scripts/acceptance/macos-hid-type-probe.swift

echo "macos acceptance smoke: PASS"
