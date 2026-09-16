#!/bin/bash
# macOS 验收脚本的无注入冒烟：shell 语法 + Swift 探针类型 + 针对性语义断言。
#
# 这些脚本离开真机设备/前台焦点无法完整跑，但"脚本自身腐烂"必须能在 CI 变红。
# 语法层用 bash -n / swiftc -typecheck；语义层用 grep 钉住 #33/#34/#35 修过的
# 不变量（stale PCM/WAV、executed=0 NOT_RUN 判定）——语法检查抓不到这些回归。
# 本脚本不注入键鼠、不装驱动、不访问 /Library 或 /Applications。
set -euo pipefail

REPO_ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$REPO_ROOT"

bash -n scripts/acceptance/macos-hid-type-verify.sh
bash -n scripts/acceptance/macos-hid-access-verify.sh
bash -n crates/vdev-audio/test_loopback.sh

if ! command -v swiftc >/dev/null 2>&1; then
  echo "FAIL: 未找到 swiftc（macOS 验收冒烟需要 Xcode command line tools）" >&2
  exit 1
fi
swiftc -O -typecheck scripts/acceptance/macos-hid-type-probe.swift

# 语义守护：每条断言都对应一次真实事故；删掉对应实现时这里必须红。
require_grep() {
  local pattern=$1
  local file=$2
  local message=$3
  if ! grep -qF -- "$pattern" "$file"; then
    echo "FAIL: ${message}（缺少固定字符串：${pattern}；文件：${file}）" >&2
    exit 1
  fi
}
require_grep '-y -i /tmp/vdev-audio-loopback.wav' crates/vdev-audio/test_loopback.sh 'wav→pcm 转换必须含 -y（#33/#34：否则 stale PCM 会让 set -e 空转）'
require_grep ': > /tmp/vdev-audio-loopback.wav' crates/vdev-audio/test_loopback.sh '录音前必须清空 WAV（#35：否则旧 WAV 可让 make test 假绿）'
require_grep 'executed}" -eq 0' scripts/acceptance/macos-hid-type-verify.sh 'HID 验收必须保留 executed==0 判定（防全 SKIP 假绿）'
require_grep 'NOT_RUN' scripts/acceptance/macos-hid-type-verify.sh 'HID 验收必须有 NOT_RUN 出口（防全 SKIP 假绿）'
require_grep 'open -W -n "${PROBE_APP}" --args "${out}"' scripts/acceptance/macos-hid-type-verify.sh 'HID 验收必须用最小 .app + open 启动每个用例（裸二进制拿不到前台焦点）'
require_grep 'tap_ok=$(echo' scripts/acceptance/macos-hid-type-verify.sh 'HID 验收必须保留 tap_ok 回退判定'
# HID 权限验收（#56）：这三条各自对应一次真实踩坑，删掉任一条都会让"验收"退化成摆设。
require_grep 'VDEV_HID_SKIP_ACCESS_CHECK=1' scripts/acceptance/macos-hid-access-verify.sh '权限护栏必须有阳性对照（绕过检查必须复现静默假成功）'
require_grep 'unauth.$$.' scripts/acceptance/macos-hid-access-verify.sh '未授权对照必须用每次全新的 bundle id（固定 id 会被历史授权污染）'
require_grep 'EnableSecureEventInput' scripts/acceptance/macos-hid-access-verify.sh '安全输入必须用 EnableSecureEventInput 确定性开启，不能靠人工凑状态'
require_grep 'secure_input=true' scripts/acceptance/macos-hid-access-verify.sh '安全输入用例必须先确认系统真的处于安全输入，再断言行为'
require_grep 'HID_ACCESS_RESULT=NOT_RUN' scripts/acceptance/macos-hid-access-verify.sh 'HID 权限验收必须保留 NOT_RUN 出口（锁屏/无权限身份等环境不具备时不算通过）'
require_grep 'codesign --force --sign' scripts/acceptance/macos-hid-access-verify.sh '注入器 app 必须用稳定身份签名（否则 TCC 授权是 cdhash 型，二进制一变就失效，issue #58）'
require_grep 'INJECTOR_STAMP' scripts/acceptance/macos-hid-access-verify.sh '注入器必须用源二进制 sha256 戳判断是否重写（每轮重写会让 cdhash 型授权失效，issue #58）'
require_grep 'window.typed = ""' scripts/acceptance/macos-hid-type-probe.swift '探针必须在 READY 前清零计数（开窗瞬间的迟到事件会污染逐字断言）'

require_grep 'exit 2' scripts/acceptance/macos-hid-access-verify.sh 'HID 权限验收的环境不具备分支必须 exit 2（与用例失败区分）'
require_grep '辅助功能' crates/vdev-hid/src/lib.rs '注入路径必须有可诊断的「辅助功能」权限报错文案'
require_grep 'secure_input' crates/vdev-hid/src/lib.rs '注入路径必须报出安全输入状态'
# 相机像素池（#54-1）：单测只能证明"池本身会对/会回落"，证明不了 send_bgra 真的用了池——
# 这条守卫钉住调用点，防止优化被悄悄摘掉却仍然全绿（审查 B1）。
require_grep 'let Some(pb) = acquire_pixel_buffer(w, h) else {' crates/vdev-camera-ext/src/main.rs 'send_bgra 必须走 acquire_pixel_buffer（像素池复用）'
require_grep 'fn acquire_buffer_with<FP, FB, FD>' crates/vdev-camera-ext/src/main.rs '像素池必须有可注入的决策内核（复用/回落两条路径要能被单测钉住）'

if ! command -v python3 >/dev/null 2>&1; then
  echo "FAIL: 未找到 python3（check-docs.py 需要）" >&2
  exit 1
fi
python3 scripts/check-docs.py

echo "macos acceptance smoke: PASS"
