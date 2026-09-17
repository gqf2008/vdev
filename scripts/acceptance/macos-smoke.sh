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
# 注入器准备逻辑（#58）：字符串守卫钉"用的是身份变量"（`--sign -` 会退化回 cdhash 型），
# 行为守卫交给脚本自带的 --self-test（下面直接跑），grep 挡不住的语义摘除由它兜住。
require_grep 'prepare_injector_app "${VDEV}" "${INJECTOR_APP}" "${INJECTOR_STAMP}" "${INJECTOR_SIGN_IDENTITY}"' scripts/acceptance/macos-hid-access-verify.sh '注入器必须把解析出的稳定身份传进 prepare_injector_app，不能传 -（否则退回 cdhash 型授权，issue #58）'
require_grep 'codesign --force --sign "${identity}"' scripts/acceptance/macos-hid-access-verify.sh '签名必须用函数传入的身份变量（写成 --sign - 会静默退回 ad-hoc，戳还会撒谎，审查 L2）'
require_grep 'security find-identity' scripts/acceptance/macos-hid-access-verify.sh '注入器签名身份必须真的从 keychain 里解析，不能写死一个可能不存在的名字'
require_grep 'INJECTOR_STAMP' scripts/acceptance/macos-hid-access-verify.sh '注入器必须用源二进制状态戳判断是否重写（每轮重写会让 cdhash 型授权失效，issue #58）'
require_grep 'codesign --verify --strict' scripts/acceptance/macos-hid-access-verify.sh '签名后必须 codesign --verify --strict，失败不能写戳（否则坏状态被固化，审查 B1）'
require_grep 'window.typed = ""' scripts/acceptance/macos-hid-type-probe.swift '探针必须在 READY 前清零计数（开窗瞬间的迟到事件会污染逐字断言）'

require_grep 'exit 2' scripts/acceptance/macos-hid-access-verify.sh 'HID 权限验收的环境不具备分支必须 exit 2（与用例失败区分）'
require_grep '辅助功能' crates/vdev-hid/src/lib.rs '注入路径必须有可诊断的「辅助功能」权限报错文案'
require_grep 'secure_input' crates/vdev-hid/src/lib.rs '注入路径必须报出安全输入状态'
# 相机像素池（#54-1）：单测只能证明"池本身会对/会回落"，证明不了 send_bgra 真的用了池——
# 这条守卫钉住调用点，防止优化被悄悄摘掉却仍然全绿（审查 B1）。
require_grep 'let Some(pb) = acquire_pixel_buffer(w, h) else {' crates/vdev-camera-ext/src/main.rs 'send_bgra 必须走 acquire_pixel_buffer（像素池复用）'
require_grep 'fn acquire_buffer_with<FP, FB, FD>' crates/vdev-camera-ext/src/main.rs '像素池必须有可注入的决策内核（复用/回落两条路径要能被单测钉住）'

# 数字探针的结论文案：模型成本 = 攒帧 + lookahead，两平台必须说同一件事，
# 且必须走共享的 digital_model_cost_note（曾经的旧文案只提 20 ms lookahead，漏了攒帧）。
require_grep 'super::digital_model_cost_note(frame_ms)' crates/vdev-mic-agent/src/platform/macos.rs '数字探针结论文案必须走共享的 digital_model_cost_note（攒帧 0-10ms + lookahead 20ms 都要说）'
require_grep 'super::digital_model_cost_note(frame_ms)' crates/vdev-mic-agent/src/platform/windows.rs '数字探针结论文案必须走共享的 digital_model_cost_note（Windows 同口径）'
require_grep 'if !parsed.is_finite() || !(0.0..=1.0).contains(&parsed)' crates/vdev-mic-agent/src/main.rs '--mix 必须校验 0..=1 且拒绝 NaN（下游会静默塌成纯 dry/纯 wet 或静音）'
require_grep '#[arg(long, default_value_t = 1.0, value_parser = parse_mix)]' crates/vdev-mic-agent/src/main.rs '--mix 的校验函数必须真的接在 clap 参数上（只留函数体等于没校验；串要含 #[arg(...)] 全属性，否则注释里提一句就能骗过守卫）'
require_grep 'plus its two-frame lookahead on top of this' crates/vdev-mic-agent/src/main.rs 'live --help 的 digital 说明必须同时提攒帧与 lookahead（用户可见的旧口径）'

# 行为级守卫：#58 的修复是一段"有条件重写"的状态机，纯 grep 挡不住语义摘除
# （把签名换成 --sign -、删掉复用条件、删掉写戳……）。用隔离临时目录直接跑它的
# 自测模式：首跑写戳 / 源不变不重写 / 源变则重签 / 坏签名不被戳固化 / 身份变化重签。
if ! bash scripts/acceptance/macos-hid-access-verify.sh --self-test >/tmp/vdev-injector-selftest.log 2>&1; then
  echo "FAIL: 注入器准备逻辑自测未通过（issue #58 的复用/重签状态机回归）" >&2
  cat /tmp/vdev-injector-selftest.log >&2
  exit 1
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "FAIL: 未找到 python3（check-docs.py 需要）" >&2
  exit 1
fi
python3 scripts/check-docs.py

# 行为级守卫：audio-denoise-metrics.py 现在是"降噪模型延迟"的唯一标尺，
# 它自己算错（索引/归一化/边界）会让所有延迟结论一起错，所以标尺必须自带自测：
# 造一个已知延迟的信号，回读必须等于它；峰值贴边界 / 相关太低 / 素材过短都要被拦下。
if ! python3 scripts/acceptance/audio-denoise-metrics.py --self-test >/tmp/vdev-denoise-metrics-selftest.log 2>&1; then
  echo "FAIL: 降噪延迟标尺自测未通过（lag 测量或可疑读数守卫回归）" >&2
  cat /tmp/vdev-denoise-metrics-selftest.log >&2
  exit 1
fi

echo "macos acceptance smoke: PASS"
