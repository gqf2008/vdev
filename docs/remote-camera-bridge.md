# 远端 WebRTC 流 → 本地虚拟摄像头 + 声卡 + 滤镜管线

把远端（另一台机器/云端）的 WebRTC 音视频流，变成本机可用的虚拟设备：
对方画面 → 本地虚拟摄像头，对方声音 → 本地虚拟声卡，中间插滤镜管线。

## 数据流

```
远端发布端                             本机（vdev-bridge 新 crate）
  │                                      │
  │  WebRTC（H264/HEVC + Opus）          │
  └──────── aerodesk SFU ────────────────┤
                                         │
                    视频 track ──► str0m 收流 ──► depacketize ──► FFmpeg 解码（YUV→BGRA）
                    音频 track ──► str0m 收流 ──► Opus 解码 ──► PCM
                                         │
                    视频 BGRA ──► [滤镜管线] ──► FrameChannel(127.0.0.1:27890) ──► 虚拟摄像头
                    音频 PCM ──► AudioUnit ──► vdev-audio 声卡输出
```

## 复用点（现成）

| 环节 | 复用 | 位置 |
|---|---|---|
| WebRTC 收流 | str0m（aerodesk fork，或官方） | aerodesk 依赖 |
| 视频解码 | ffmpeg-next（H264/HEVC/AV1/VP9，VideoToolbox 硬解） | aerodesk-codec/decode.rs |
| 音频解码 | ffmpeg Opus → PCM | aerodesk-codec/audio.rs |
| 推摄像头 | FrameChannel TCP 协议（36 字节头 + BGRA） | vdev-app/frame.rs、camera-ext/frame_channel.rs |
| 写声卡 | AudioUnit HALOutput → vdev-audio 设备 | vdev-app/audio.rs |

## 滤镜管线（新增，纯 Rust 像素处理）

在 BGRA 帧上做可插拔滤镜链，实时无分配：

1. 基础：亮度/对比度/饱和度、色调、锐化/模糊、裁剪/缩放、水印
2. 进阶：绿幕抠像、背景模糊（人脸分割）、美颜（磨皮+美白）

## 实现进度（全部完成，代码编译通过）

- ✅ vdev-filter：滤镜管线（亮度/对比度/饱和度/绿幕/锐化 + 4 单测）
- ✅ 滤镜接入 vdev-app 视频推流（VDEV_FILTER）
- ✅ vdev-bridge 视频桥：str0m 收流 → NAL 组装 → FfmpegDecoder → RGBA→BGRA 缩放 → 滤镜 → FrameChannel
- ✅ vdev-bridge 音频桥：Opus 解码 → SPSC ring → AudioUnit → vdev-audio 声卡
- ✅ 端到端验证通过：aerodesk publisher(--noisy x264 + --audio-opus) → SFU → vdev-bridge
  → H264 解码 420+ 帧(1920x1080) 推摄像头、Opus 解码 700+ 帧(960s/帧) 出声卡

## 用法

```bash
# 1. 起 aerodesk SFU + signal（aerodesk 仓库）
# 2. 发布端推流
# 3. 本机运行桥
vdev-bridge <signal_server> <room> [auth]
# 环境变量：
#   VDEV_FILTER="brightness,contrast,saturation,green,sharpen"
#   VDEV_WIDTH / VDEV_HEIGHT（默认 1920x1080）
```

## 技术选型

- WebRTC：str0m（Rust，纯软件，无 GStreamer/libwebrtc 依赖）
- 解码：ffmpeg-next（软解起步，后续接 VideoToolbox 硬解）
- 滤镜：自研像素管线（BGRA，零依赖，实时无分配）；进阶可用 CoreImage/CoreML
