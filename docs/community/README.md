# vdev 虚拟设备驱动开发系列

本目录是 **vdev**（[github.com/gqf2008/vdev](https://github.com/gqf2008/vdev)）的驱动开发系列文档，面向想自己写同类驱动的开发者，可直接作为社区发文使用。全部内容基于仓库真实代码写成：所有 API、常量、命令均取自源码与 README，"踩坑实录"章节的每个案例都是本项目开发/审查过程中真实出现并修复过的问题。

| # | 文档 | 平台 | 技术 | 一句话 |
|---|------|------|------|--------|
| 1 | [macOS 虚拟摄像头](macos-virtual-camera.md) | macOS 26+ | CMIOExtension + 100% Rust 手写 objc2 绑定 | 不用 Xcode/Swift 壳，纯 Rust 造一个 QuickTime 可见的摄像头 |
| 2 | [macOS 虚拟声卡](macos-virtual-audio.md) | macOS 26+ | AudioServerPlugIn（HAL 插件） | 跑在 coreaudiod 里的"驱动"：sample-time 环形缓冲 + 追赶时钟，对齐 BlackHole |
| 3 | [Windows 虚拟摄像头](windows-virtual-camera.md) | Windows x64 | DirectShow Source Filter（用户态 COM） | 免签名的虚拟摄像头：纯 Rust 写 COM、共享内存双缓冲防撕裂 |
| 4 | [Windows 虚拟显示器](windows-virtual-display.md) | Windows x64 | IddCx UMDF 间接显示驱动 | bindgen 直取 WDK 头做绑定层、零初始化上下文的 UB 防御 |
| 5 | [Windows 虚拟声卡](windows-virtual-audio.md) | Windows x64 | PortCls WaveRT（WDM 内核驱动） | no_std 内核音频驱动：7 个 BSOD 级踩坑实录与"编译过≠能跑"方法论 |
| 6 | [Windows 虚拟 HID](windows-virtual-hid.md) | Windows x64 | KMDF HID minidriver | 硬件级键鼠注入：HID 三重契约（INF 接线 / IOCTL 契约 / 结构布局） |

## 阅读建议

- **想快速判断各条路线的难度**：先读每篇第 1-2 节的"路线选型"，六篇合起来是一张跨平台的虚拟设备路线图。
- **只想看踩坑**：每篇都有独立"踩坑实录"章节，可跳跃阅读；其中 Windows 声卡篇（环形缓冲栈悬垂、回绕公式、NTSTATUS 手算错）与 HID 篇（三重契约）案例最密集。
- **想动手复现**：每篇"构建与运行"的命令都与仓库 README 一致；注意 Windows 三个内核驱动需要 WDK + 测试签名（各篇现状章节有如实说明）。

## 相关仓库内文档

- `README.md` — 项目总览、构建矩阵与各驱动状态框（✅ 已可用 / 🔧 构建与自测通过、真机验证进行中）
- `docs/windows-virtual-camera.md`、`docs/windows-virtual-display-audio.md` — Windows 侧内部设计/联调笔记
- `docs/macOS驱动开发有趣方向.md`、`docs/Windows驱动开发有趣方向.md` — 选题阶段的研究笔记
