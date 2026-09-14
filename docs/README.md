# vdev 文档索引

本仓库文档分两层，读之前先分清自己要看哪一层：

| 层 | 目录 | 是什么 | 面向谁 |
|---|---|---|---|
| **对外发布** | [`community/`](community/README.md) | 虚拟设备驱动开发系列 **9 篇** + AI 虚拟麦克风[发布公告](community/announcement-ai-mic.md)。**路径已对外发布过，保持稳定**，改动需同步所有引用 | 想了解项目、想自己写同类驱动的开发者 |
| **内部开发笔记** | [`dev/`](#dev--内部开发笔记) | 技术路线调研、设计稿、选题线索。**反映当时的决策过程，可能滞后于实现**；实现以代码为准 | 参与开发的协作者 |

> 代码/文档/宣发的一致性由 CI 与发布前冒烟把关，但**内部笔记不保证与最新实现逐字对齐**——冲突时看代码。

---

## community/ — 对外发布的社区系列

系列总目录与阅读建议见 [`community/README.md`](community/README.md)。概览：

| # | 文档 | 平台 | 技术 |
|---|---|---|---|
| 1 | [macOS 虚拟摄像头](community/macos-virtual-camera.md) | macOS 26+ | CMIOExtension + 手写 objc2 绑定（100% Rust） |
| 2 | [macOS 虚拟声卡](community/macos-virtual-audio.md) | macOS 26+ | AudioServerPlugIn（HAL 插件） |
| 3 | [macOS 虚拟键盘/鼠标](community/macos-virtual-hid.md) | macOS 26+ | CGEventPost / EventTap（用户态） |
| 4 | [macOS 虚拟显示器](community/macos-virtual-display.md) | macOS 26+ | CGVirtualDisplay 私有 API |
| 5 | [Windows 虚拟摄像头](community/windows-virtual-camera.md) | Windows x64 | DirectShow Source Filter（用户态 COM） |
| 6 | [Windows 虚拟显示器](community/windows-virtual-display.md) | Windows x64 | IddCx UMDF 间接显示驱动 |
| 7 | [Windows 虚拟声卡](community/windows-virtual-audio.md) | Windows x64 | PortCls WaveRT（WDM 内核驱动） |
| 8 | [Windows 虚拟 HID](community/windows-virtual-hid.md) | Windows x64 | Virtual HID Framework（VHF，Win10/11 通用） |
| 9 | [AI 虚拟麦克风](community/ai-virtual-mic.md) | macOS + Windows | RNNoise + CoreAudio / WASAPI（用户态） |
| — | [发布公告：AI 虚拟麦克风](community/announcement-ai-mic.md) | — | 速览篇：只讲结果与上手；机制见第 9 篇 |

---

## dev/ — 内部开发笔记

| 文档 | 是什么 | 现状 |
|---|---|---|
| [macos-route-survey.md](dev/macos-route-survey.md) | macOS 三条虚拟设备路线（HID / 摄像头 / 屏幕）的技术调研与参考项目 | 路线决策已落地，结论仍有效 |
| [macos-route-ideas.md](dev/macos-route-ideas.md) | macOS 侧"值得写"的内容线索（选题阶段） | 选题笔记，非约定 |
| [windows-route-ideas.md](dev/windows-route-ideas.md) | Windows 侧"值得写"的内容线索（选题阶段） | 选题笔记，非约定 |
| [windows-camera-design.md](dev/windows-camera-design.md) | Windows 虚拟摄像头（DirectShow）设计与踩坑 | 实现已合入，文中为设计/联调过程 |
| [windows-display-audio-design.md](dev/windows-display-audio-design.md) | Windows 虚拟显示器 + 虚拟声卡（驱动路线）设计 | 实现已合入并真机验证（2026-09-14）；文中为设计过程 |

> Windows 四类设备（摄像头 / 显示器 / 声卡 / 键鼠）均已在 Win10 19045 x64 上装机验证，
> 但`dev/` 下的笔记**保持写作时的口径**（反映当时的决策与踩坑），不随验证结论回改；
> 要最新状态请看根 [`README.md`](../README.md) 与 `community/` 各篇的"现状与局限"章节。

此外，组件级的构建/签名/验收说明放在各自 crate 内：

- [`crates/vdev-audio-win/README.md`](../crates/vdev-audio-win/README.md) — 虚拟声卡驱动 / CLI / GUI / 验收
- [`crates/vdev-display-win/README.md`](../crates/vdev-display-win/README.md) — 虚拟显示器驱动
- [`crates/vdev-hid-win/kernel/driver/README.md`](../crates/vdev-hid-win/kernel/driver/README.md) — 内核 HID（VHF）驱动
- [`crates/vdev-mic-agent/README.md`](../crates/vdev-mic-agent/README.md) — AI 虚拟麦克风
- 其余 crate 的说明见根 [`README.md`](../README.md) 的"仓库结构"与"文档导航"

---

## 维护约定

1. **新增对外文章 → `community/`**；内部设计/调研/决策记录 → `dev/`。别放顶层。
2. `community/` 下的文件名与路径视为**已发布契约**，重命名等于对外死链——要改必须同步更新本索引、`community/README.md` 与所有引用。
3. 对外文章发布前跑一遍命令冒烟（见 `~/.agents/rules/RULE_技能文档命令须有可执行实现且发布前冒烟.md`）；内部笔记不强制冒烟，但**不得与代码冲突到误导实现**。
4. 本目录下若出现 `._*`（macOS 生成的 AppleDouble 元数据），是垃圾文件——`.gitignore` 已忽略，可直接 `find docs -name '._*' -delete`。
