# vdev AI 虚拟麦克风：端侧降噪，macOS 与 Windows 双平台可用

> 发布公告 · 2026-09 · [vdev](https://github.com/gqf2008/vdev) · 关联 issue [#10](https://github.com/gqf2008/vdev/issues/10)

**一句话：物理麦克风 → RNNoise 端侧降噪 → 自适应干湿 → 注入虚拟麦克风端点。开会的另一端听到的，是降噪后的你——全程本地推理，不上传一个字节。**

## 这是什么

[.vdev](https://github.com/gqf2008/vdev) 是一个用 Rust 造虚拟设备的开源项目（摄像头/显示器/声卡/HID，macOS + Windows 双栈）。这次发布的是它缺的半条链——**AI 虚拟麦克风 `vdev-mic-agent`**：

```
物理麦克风 ──▶ 480 样本重排 ──▶ RNNoise 降噪 ──▶ 自适应干湿 ──▶ 虚拟声卡 ──▶ Zoom/腾讯会议/QuickTime
                （帧管线）        （libloading 动态加载）   （VAD 门控迟滞闸门）    （内核/插件环回）
```

三个设计决定，让它与其他"AI 降噪麦克风"方案不同：

1. **纯用户态，驱动零改动**。降噪后的音频作为普通客户端写进虚拟声卡的输出端点——macOS 走 CoreAudio HAL 插件客户端，Windows 走 WASAPI 写渲染端点 + 内核环回。虚拟设备那一侧一行代码没动。
2. **RNNoise 运行时加载，免链接依赖**。`librnnoise` 用 libloading 按五级候选路径解析，没有就明确报错告诉你怎么装；构建产物不携带任何推理框架。
3. **自适应闸门是"节流阀"不是噱头**。RNNoise 对干净语音也一律衰减（实测干净输入被干跑会损失 SI-SDR）。VAD 门控 + 25 dB 迟滞闸门让干净麦克风**绕过模型**，只有真正需要时才全湿处理。

## 平台支持

| 平台 | live 降噪 | 探针 | 状态 |
|---|---|---|---|
| macOS 26+（Apple Silicon） | ✅ CoreAudio 回调 | 数字 + 声学 | **可用（实测）** |
| Windows x64 | ✅ WASAPI 轮询 + 内核环回 | 数字 + 声学 | 门禁全绿，**真机音频验证进行中** |

两侧共享同一个平台无关 DSP 核心（帧重排 / 混音 / 无锁环 / 延迟测量），差异只在系统音频栈的接线层。

## 数字（i7-11700 / Win10，48 kHz，480 样本帧）

| 指标 | 值 |
|---|---|
| RNNoise 单帧耗时 | 平均 0.078 ms，p99 0.135 ms |
| 占帧预算（10 ms） | p99 约 **1.4%** |
| 实时运行 CPU | **0.78% 单核**（单核约可带 128 路） |
| 算法延迟 | 960 样本 = **20.0 ms**（互相关实测） |
| 干净输入自适应闸门 | SI-SDR 14.72 → **30.57 dB**（湿比 0.06，几乎全旁路） |
| 与 C/Python 参考实现 | **100% 采样误差 ≤ 1 LSB**（逐采样一致） |

## 快速上手

```bash
# macOS：装虚拟声卡 + agent
make -C crates/vdev-audio install          # HAL 插件虚拟声卡
cargo build --release -p vdev-mic-agent
target/release/vdev-mic-agent live --adaptive --seconds 20 \
    --record-in /tmp/heard.wav --record-out /tmp/sent.wav --report /tmp/live.json
# 然后在会议软件里把「麦克风」选成 vdev-audio A

# Windows x64：装 vdev-audio-win 虚拟声卡（测试签名）后
vdev-mic-agent live --adaptive --seconds 20 --report live.json
vdev-mic-agent live --probe digital --seconds 30   # 注入链路自检：无麦克风、无 DLL 也能跑
```

降噪后端从 [xiph/rnnoise](https://github.com/xiph/rnnoise) 自建或取 MSYS2 包，放进可执行文件旁或 `third_party/` 即可（详见 crate README）。

## 延迟到底多少

如实口径：**算法 20 ms + 缓冲**。macOS 回调路径缓冲近似为零；Windows 轮询路径为 40 ms 端点缓冲（抗调度抖动的折中，报告中如实标注）。两条探针（数字环回/声学环回）随包提供，端到端延迟自己测，不听我们口头承诺。

## 想深挖实现？

九篇驱动开发系列（全部基于真实源码，"踩坑实录"均为开发中真实修复的问题）：

- [AI 虚拟麦克风](ai-virtual-mic.md)（本功能：SPSC 环、自适应干湿、chirp+NCC 延迟探针）
- [macOS 虚拟声卡](macos-virtual-audio.md) / [Windows 虚拟声卡](windows-virtual-audio.md)（注入所依赖的两块虚拟设备）
- 其余七篇：macOS 摄像头/键鼠/虚拟屏、Windows 摄像头/显示器/HID —— 见[系列索引](README.md)

## 状态与边界（如实）

- Windows 真机音频行为验证进行中（编译/门禁/单测已绿；驱动侧装机需测试签名）。
- v1 边界：WASAPI 端点需 48 kHz（shared 模式不重采样，非 48k 明确报错给指引）；无设备热拔插自动恢复。
- 全部代码与文档：[github.com/gqf2008/vdev](https://github.com/gqf2008/vdev)，MIT 路线的实验性项目，欢迎 issue。
