#!/bin/bash
# vdev-audio 环回测试：播放 440Hz 到输出流，同时从输入流录制，检查非静音占比。
set -e
ffmpeg -hide_banner -f lavfi -i "sine=frequency=440:duration=6" -f audiotoolbox -audio_device_index 5 "x" > /tmp/vdev-audio-play.log 2>&1 &
PLAY=$!
sleep 2
ffmpeg -hide_banner -f avfoundation -i ":3" -t 3 -y /tmp/vdev-audio-loopback.wav > /tmp/vdev-audio-rec.log 2>&1 || true
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
