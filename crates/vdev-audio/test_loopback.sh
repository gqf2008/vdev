#!/bin/bash
# vdev-audio 环回测试：播放 440Hz 到输出流，同时从输入流录制，检查非静音占比。
# 设备索引随系统设备列表变化，动态探测。
set -e
# AudioToolbox 设备行形如 `[2] vdev-audio A, vdev-audio-A-device`；ffmpeg 8.1
# 不再输出裸 `vdev-audio,`，必须按含 A/B 的设备名匹配。固定选 A，保证输出与输入
# 取的是同一台设备的环回端点。
OUT_IDX=$(ffmpeg -hide_banner -f lavfi -i "sine=frequency=440:duration=1" -f audiotoolbox -list_devices true - 2>&1 | grep -E 'vdev-audio A([,[:space:]]|$)' | grep -oE '\[[0-9]+\]' | head -1 | tr -d '[]')
IN_IDX=$(ffmpeg -hide_banner -f avfoundation -list_devices true -i "" 2>&1 | grep -E 'vdev-audio A([,[:space:]]|$)' | grep -oE '\[[0-9]+\]' | head -1 | tr -d '[]')
if [ -z "$OUT_IDX" ] || [ -z "$IN_IDX" ]; then
  echo "FAIL: 未找到 vdev-audio 设备（输出索引='$OUT_IDX' 输入索引='$IN_IDX'）"
  exit 1
fi
ffmpeg -hide_banner -f lavfi -i "sine=frequency=440:duration=6" -f audiotoolbox -audio_device_index "$OUT_IDX" "x" > /tmp/vdev-audio-play.log 2>&1 &
PLAY=$!
sleep 2
# 先清空 wav：录音 ffmpeg 若在打开输入就失败，不会碰 -y 输出，上一轮的旧 wav
# 会存活；后面转换再忠实转成 pcm，非静音的旧文件就会把"没录到"报成 PASS。
: > /tmp/vdev-audio-loopback.wav
if ! ffmpeg -hide_banner -f avfoundation -i ":$IN_IDX" -t 3 -y /tmp/vdev-audio-loopback.wav > /tmp/vdev-audio-rec.log 2>&1; then
  echo "WARN: 录音 ffmpeg 退出非 0，见 /tmp/vdev-audio-rec.log"
fi
[ -s /tmp/vdev-audio-loopback.wav ] || { echo "FAIL: 录音未产出 WAV（设备不可用？）"; exit 1; }
kill $PLAY 2>/dev/null || true
sleep 1
# -y 不能省：目标 PCM 已存在时 ffmpeg 会提示 overwrite，stdin 无输入则
# `Not overwriting - exiting`，但退出码仍是 0（set -e 抓不住）——不加 -y 会拿旧 PCM 做分析。
ffmpeg -hide_banner -y -i /tmp/vdev-audio-loopback.wav -f s16le -acodec pcm_s16le /tmp/vdev-audio-loopback.pcm 2>/dev/null
python3 - <<'PY'
import struct
import sys
d=open('/tmp/vdev-audio-loopback.pcm','rb').read()
n=len(d)//2
s=struct.unpack('<%dh'%n, d[:n*2])
nz=sum(1 for v in s if abs(v)>200)
pct=100.0*nz/max(n,1)
ok=pct>20
print(f"环回非静音占比: {pct:.1f}%")
print("PASS" if ok else "FAIL: 没有听到音频，检查设备选择/驱动状态")
sys.exit(0 if ok else 1)
PY
