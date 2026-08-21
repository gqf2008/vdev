#!/bin/bash
# vdev-audio 环回测试：播放 440Hz 到输出流，同时从输入流录制，检查非静音占比。
# 设备索引随系统设备列表变化，动态探测。
set -e
OUT_IDX=$(ffmpeg -hide_banner -f lavfi -i "sine=frequency=440:duration=1" -f audiotoolbox -list_devices true - 2>&1 | grep 'vdev-audio,' | grep -oE '\[[0-9]+\]' | head -1 | tr -d '[]')
IN_IDX=$(ffmpeg -hide_banner -f avfoundation -list_devices true -i "" 2>&1 | grep 'vdev-audio' | grep -oE '\[[0-9]+\]' | head -1 | tr -d '[]')
if [ -z "$OUT_IDX" ] || [ -z "$IN_IDX" ]; then
  echo "FAIL: 未找到 vdev-audio 设备（输出索引='$OUT_IDX' 输入索引='$IN_IDX'）"
  exit 1
fi
ffmpeg -hide_banner -f lavfi -i "sine=frequency=440:duration=6" -f audiotoolbox -audio_device_index "$OUT_IDX" "x" > /tmp/vdev-audio-play.log 2>&1 &
PLAY=$!
sleep 2
ffmpeg -hide_banner -f avfoundation -i ":$IN_IDX" -t 3 -y /tmp/vdev-audio-loopback.wav > /tmp/vdev-audio-rec.log 2>&1 || true
kill $PLAY 2>/dev/null || true
sleep 1
ffmpeg -hide_banner -i /tmp/vdev-audio-loopback.wav -f s16le -acodec pcm_s16le /tmp/vdev-audio-loopback.pcm 2>/dev/null
python3 - <<'PY'
import struct
d=open('/tmp/vdev-audio-loopback.pcm','rb').read()
n=len(d)//2
s=struct.unpack('<%dh'%n, d[:n*2])
nz=sum(1 for v in s if abs(v)>200)
pct=100.0*nz/max(n,1)
print(f"环回非静音占比: {pct:.1f}%")
print("PASS" if pct>20 else "FAIL: 没有听到音频，检查设备选择/驱动状态")
PY
