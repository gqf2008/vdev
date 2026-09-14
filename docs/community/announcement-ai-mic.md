# vdev AI 虚拟麦克风：端侧降噪，macOS 与 Windows 双平台可用

> 发布公告 · 2026-09 · [vdev](https://github.com/gqf2008/vdev) · 关联 issue [#10](https://github.com/gqf2008/vdev/issues/10)
> **速览篇**：只答"能干什么、怎么上手"。[深度拆解](ai-virtual-mic.md)答"为什么这么写、怎么复现"。

**物理麦克风 → RNNoise 端侧降噪 → 自适应干湿 → 注入虚拟麦克风端点。开会的另一端听到的，是降噪后的你——全程本地推理，不上传一个字节。**

`vdev-mic-agent` 是 [vdev](https://github.com/gqf2008/vdev)（用 Rust 造虚拟设备，摄像头/显示器/声卡/HID，macOS + Windows 双栈）缺的那半条链：

```
物理麦克风 ──▶ 480 样本重排 ──▶ RNNoise 降噪 ──▶ 自适应干湿 ──▶ 虚拟声卡 ──▶ Zoom/腾讯会议/QuickTime
                （帧管线）      （libloading 动态加载）  （VAD 门控迟滞闸门）   （内核/插件环回）
```

三点不同，都落在**使用**上：**纯用户态、驱动零改动**（降噪后按普通客户端写进虚拟声卡输出端点）；**免链接依赖**（`librnnoise` 运行时加载，缺库明确报错给装法）；**干净麦克风不被拖累**（RNNoise 对干净语音也一律衰减，自适应闸门让干净输入绕过模型，这就是下表那行 SI-SDR 的由来）。

## 平台支持与状态

| 平台 | live 降噪 | 探针 | 状态 |
|---|---|---|---|
| macOS 26+（Apple Silicon） | ✅ CoreAudio 回调 | 数字 + 声学 | **可用（实测）** |
| Windows x64 | ✅ WASAPI 轮询 + 内核环回 | 数字 + 声学 | 门禁全绿；依赖的 `vdev-audio-win` 环回驱动**已真机验证**，mic-agent 自身的 Windows live 行为待实测 |

v1 边界（如实）：WASAPI 端点需 48 kHz（shared 模式不重采样，非 48k 明确报错给指引）；无设备热拔插自动恢复。Windows 驱动侧装机需测试签名。

两侧共享同一个平台无关 DSP 核心，差异只在系统音频栈的接线层——**所以实现拆解只写了一篇，Windows 是其中一节**。

## 数字（i7-11700 / Win10，48 kHz，480 样本帧）

| 指标 | 值 |
|---|---|
| RNNoise 单帧耗时 | 平均 0.078 ms，p99 0.135 ms |
| 占帧预算（10 ms） | p99 约 **1.4%** |
| 实时运行 CPU | **0.78% 单核**（单核约可带 128 路） |
| 算法延迟 | 960 样本 = **20.0 ms**（互相关实测） |
| 干净输入自适应闸门 | SI-SDR 14.72 → **30.57 dB**（湿比 0.06，几乎全旁路） |
| 与 C/Python 参考实现 | **100% 采样误差 ≤ 1 LSB** |
| 端到端延迟 | **算法 20 ms + 缓冲**：macOS 回调路径近似为零；Windows 轮询路径 40 ms 端点缓冲（抗抖动折中，报告中标注） |

## 快速上手

```bash
# macOS：装虚拟声卡 + agent，然后会议软件里把「麦克风」选成 vdev-audio A
make -C crates/vdev-audio install
cargo build --release -p vdev-mic-agent
target/release/vdev-mic-agent live --adaptive --seconds 20 \
    --record-in /tmp/heard.wav --record-out /tmp/sent.wav --report /tmp/live.json

# Windows x64：装 vdev-audio-win 虚拟声卡（测试签名）后
vdev-mic-agent live --adaptive --seconds 20 --report live.json
vdev-mic-agent live --probe digital --seconds 30   # 注入链路自检：无麦克风、无 DLL 也能跑
```

降噪后端从 [xiph/rnnoise](https://github.com/xiph/rnnoise) 自建或取 MSYS2 包，放进可执行文件旁或 `third_party/` 即可；参数与解析顺序见 [crate README](https://github.com/gqf2008/vdev/blob/main/crates/vdev-mic-agent/README.md)。端到端延迟不要听口头承诺：两条探针随包提供，自己测。

## 想深挖实现？

[**《给会议软件一支 AI 麦克风：vdev-mic-agent 端侧降噪链路拆解》**](ai-virtual-mic.md)——无锁 SPSC 环、VAD 门控自适应干湿、chirp+NCC 双探针，以及五个"编译过、单测绿、跑起来静默错"的真实踩坑；构建参数与平台差异也在那里。

系列其余篇目：注入所依赖的 [macOS 虚拟声卡](macos-virtual-audio.md) / [Windows 虚拟声卡](windows-virtual-audio.md)，以及 macOS 摄像头/键鼠/虚拟屏、Windows 摄像头/显示器/HID 六篇——见[系列索引](README.md)。

全部代码与文档：[github.com/gqf2008/vdev](https://github.com/gqf2008/vdev)，MIT 路线的实验性项目，欢迎 issue。
