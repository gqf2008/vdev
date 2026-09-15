# Windows 驱动验收脚本

这些脚本是 Windows 虚拟设备（HID / 声卡 / 显示器）在**真机**上做验收时用的工具，随 PR #12/#17/#18/#19 的验收过程沉淀下来：
它们不依赖 `SendInput`/测试桩，只走「设备节点 + 系统 API」这条真实路径，所以能真正回答"驱动装上了吗""注入生效了吗""链路通不通"。

原先这些脚本散落在验收机的临时目录里、路径写死成 `E:\vdev-hid-fix`；入库时统一改成：

- 仓库根默认按脚本位置推导（`scripts\acceptance\` 的上一级），需要时用 `-RepoRoot` 覆盖；
- 日志/录音等产物默认写 `%TEMP%\vdev-acceptance\`，用 `-OutDir` 覆盖；
- 需要管理员的脚本会**自动请求 UAC** 并把参数转发给提权后的自己。

## 前置条件

- Windows 10/11 x64；安装自签内核驱动需要 `bcdedit /set testsigning on` + 重启
- Rust（与 CI 同版本，当前 **1.98.0**）+ MSVC target；`LIBCLANG_PATH` 指向 **libclang 18**（bindgen 0.71 与 libclang 22 不兼容）
- 已构建产物：

  ```powershell
  cd crates\vdev-hid-win; cargo build --release      # → target\x86_64-pc-windows-msvc\release\vdev-hid-win.exe
  cd ..\vdev-audio-win;   cargo build --release      # → …\vdev-audio-win.exe
  cd ..\..;               cargo build --release -p vdev-mic-agent
  ```

  （装机包由 `scripts\stage-sign*.ps1` 生成到各 crate 的 `target\dist\`；`scripts\verify-dist.ps1` 可自检）

## HID（虚拟键鼠）

| 脚本 | 作用 | 权限 |
|---|---|---|
| `hid-enum.py` | 枚举 HID 设备接口，打印 VID/PID 与「读写打开 / 只写打开」结果（VID 0x5644 = vdev，键盘 PID 0x4849、鼠标 0x484D） | 普通 |
| `hid-keyboard-verify.ps1` | 开置顶观察窗，注入 `a/b/1/Enter`，记录 `KeyDown/KeyPress` 到 `kbd-events.txt` | 普通 |
| `hid-mouse-verify.ps1` | 注入 `click / wheel 120 / move 20 0`，记录 `MouseDown/MouseWheel` 与光标位移 | 普通 |
| `hid-injection-verify.ps1` | 记事本实弹打字 + `WM_GETTEXT` 读回文本，再验鼠标三件事 | 普通 |
| `hid-uninstall-regression.ps1` | 故意造重复节点 → `uninstall` → 断言 0 残留 → 装回一对 | **管理员** |
| `hid-converge.ps1` | 把幽灵节点收敛成"键盘/鼠标各一个"（CLI uninstall + `pnputil /remove-device` 兜底 + 重装 + 注入冒烟） | **管理员** |
| `hid-reinstall.ps1` | **干净重装**：删掉 store 里的 `vdev-hid.inf` 包（同版本不删不会替换文件）→ 重新安装 → 断言恰好一对 | **管理员** |
| `hid-channel-probe.py` | 逐条通道试写鼠标报告（SetFeature 帧化/裸、WriteFile），打印 API 结果、`errno` 与光标位移，用来区分"写没到驱动"与"到了没进系统" | 普通 |
| `drivers-cleanup.ps1` | 清理 driver store 里不再被 vdev 设备使用的历史驱动包（自动保留现役包；`-DryRun` 只列不删） | **管理员**（`-DryRun` 不需要） |

```powershell
powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-keyboard-verify.ps1
powershell -ExecutionPolicy Bypass -File scripts\acceptance\hid-uninstall-regression.ps1   # 会弹 UAC
```

边界：键盘/鼠标是**相对**注入——鼠标移动上限 ±127、滚轮按 120 的倍数、没有绝对坐标模式。

### 注入不生效时的排查顺序（2026-09-14 实机踩过一遍）

1. `python scripts\acceptance\hid-enum.py`：vdev 的接口在不在、能不能只写打开（`vid=0x5644`）；
2. `python scripts\acceptance\hid-channel-probe.py`：Feature 通道是否"受理且光标动"。
   实机基准：`SetFeature(帧化 5B) → api_ok=True, err=0, dx=+22`；裸 4B 报 `err=87`；`WriteFile` 报 `err=1`；
3. 若通道"err=0 但光标不动"：多半是**装置状态脏**（反复装卸留下的幽灵/重复节点让写入落到无效实例）——
   先 `hid-converge.ps1` 清节点，不行再 `hid-reinstall.ps1`（删包重装，能同时刷新驱动文件）；
4. 复盘判据：`hid-keyboard-verify.ps1` / `hid-mouse-verify.ps1` 必须 exit=0 才算过；
   键盘脚本在拿不到前台焦点时会**中止并报红**（避免把按键打进用户的其它窗口）。

## 声卡（环回 / 时延 / KS 属性）

| 脚本 | 作用 | 权限 |
|---|---|---|
| `audio-loopback` 相关：`audio-ring-delay.ps1` + `audio-ring-delay-analysis.py` | 灌满驱动环形缓冲后打标记音，量「渲染写入→采集读出」积压上限（**用后沿**算，前沿会被 drop-oldest 吃掉） | 普通 |
| `audio-mic-live-e2e.ps1` | mic-agent 端到端 A/B：`--mix 0` 直通 vs `--adaptive` 降噪，输出 vdev 麦克风 RMS/峰值 | 普通 |
| `audio-flow-probe.ps1` | 单端点"流是否真的在动"：render 看 `GetCurrentPadding` 下降、capture 看 `GetNextPacketSize` 出包 | 普通 |
| `audio-ks-probe.ps1` | 用 `IOCTL_KS_PROPERTY` 逐格读 pin 的 DATAFLOW/COMMUNICATION/CATEGORY/DATARANGES/DATAINTERSECTION…并与同机可用设备对照 | 普通 |
| `audio-ks-set-probe.ps1` | 对照 `KSPROPERTY_PIN_PROPOSEDATAFORMAT(14)` 的 **SET** 语义（该不该校验/回填） | 普通 |

```powershell
# 环回积压（默认 256 KB 环 ≈ 1.37 s）
powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-ring-delay.ps1 -Tag before
# 降噪 A/B
powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-mic-live-e2e.ps1
# KS 逐格对照（实例号会变，先 pnputil /enum-devices /class MEDIA）
powershell -ExecutionPolicy Bypass -File scripts\acceptance\audio-ks-probe.ps1 -VdevInstance 0001 -RefInstance 0000
```

没有物理麦克风时用「立体声混音 / Stereo Mix」当信号源（`-InputDevice`、`-ToneEndpoint` 可改）：往真实声卡播 1 kHz 正弦，由混音端采回，穿过整条链路。本机实测基线：

| 项目 | 实测 |
|---|---|
| 环回积压（1 MB 环 / 256 KB 环） | 5.14 s / **1.48 s**（理论 5.46 s / 1.37 s） |
| 环回链路的电平 | 峰值 **−6.02 dBFS**（0.5 幅度正弦理论值） |
| mic-agent：数字探针 p50 | **10.16 ms** |
| mic-agent：完整降噪 20 s 实时 | 帧时 p50 **26.8 µs**、CPU **0.156 %**（单核） |
| 直通 vs 降噪 A/B | ~~−5.8 → −34.1 dBFS~~（见下方修正：该数是自反馈环，不是物理链路） |

### ⚠ 2026-09-15 复测修正（两条真 bug）

1. **脚本参数名踩了 PowerShell 自动变量。** `audio-mic-live-e2e.ps1` / `audio-ring-delay.ps1` 的
   `-Input` 与自动变量 `$Input` 同名，在函数作用域里它求值为「空枚举」，于是传给 agent 的是
   `--input ""`；空串被任何设备名 `contains` 命中 ⇒ 选到枚举出的第一个采集端点，本机正是
   `Microphone (vdev 虚拟声卡)` **本身**。也就是说上表最后一行量的是 **agent 的自反馈环**，
   不是「物理声源 → agent → 虚拟麦克风」。已改名为 `-InputDevice`，并加了硬断言
   （采集端点 == vdev 设备即判不通过）。同时修了带空格取值的引号问题
   （`Start-Process -ArgumentList` 不会替我们加引号，`--input Stereo Mix` 会被切出多余的 `Mix`）。
2. **agent 的 Windows live 通路样本标度是错的（已修）。** crate 内部约定样本是 **int16 标度**
   （±32768，`wavio` / `metrics` / RNNoise 的口径），而 WASAPI 共享模式交付/消费的是
   `[-1,1]` float；`drain_capture` / `fill_render_ring` 在边界两侧都没换算，于是喂给模型的信号
   比训练分布低约 90 dB（VAD 形同失效）。实测症状：`--record-in` / `--record-out` 两个 WAV
   **全是数字静音**，注入电平同错。另有一处编码校验缺口：`check_mix_format` 只查了
   `wBitsPerSample == 32`（容器位数），而 32bit 容器也可能是 32bit **PCM 整型**。
   均已在 main `179480e` 修掉（入口 ×32768、上环前 ÷32768；按 `wFormatTag` / `SubFormat`
   拒绝非 IEEE float 的端点）。
   ⚠ **更正**：这两条症状**不是**"端点走 16bit PCM"——`GetMixFormat` 实测就是 IEEE float
   32bit / 2ch / 48k（注册表里的 16bit 是设备格式，引擎自己会转）。
   帧时/CPU 与标度无关，一直有效。

设备侧单独复测（2026-09-15，这三条可信）：

| 项目 | 实测 |
|---|---|
| 驱动状态 | `testsigning Yes`、服务 `vdev_audio` RUNNING、设备 `ROOT\MEDIA\0001` Started（0.3.10.0） |
| vdev 端点 | `Speakers (vdev 虚拟声卡)` + `Microphone (vdev 虚拟声卡)` 均 Active |
| 驱动环回电平（`inject --endpoint vdev` + `capture --endpoint vdev`，0.5 幅度 1 kHz） | peak **−6.0201 dBFS**（理论 −6.0206），rms −13.06 |
| agent live 20 s（真实 WASAPI 设备） | 帧 2000、**drop 0**、帧时 p99 **45 µs**、CPU **0.078 %**（单核） |

### 2026-09-16 复跑：agent 修好了，瓶颈转到驱动的环回

**agent 侧（M-g / L9）已验证**：`cargo test -p vdev-mic-agent` **46 passed / 0 failed**
（原 27 + 新增 19），`cargo check -p vdev-mic-agent --target x86_64-apple-darwin` 通过。
标度不再是问题 —— 用 vdev 环回当已知源（0.5 幅度 1 kHz，理论 peak −6.02 dBFS）跑 agent 直通，
`--record-in` 与 `--record-out` 两路一致：peak **−4.97** / rms **−10.83 dBFS**
（不再是数字静音，也没有削顶）；环回干净的轮次里，设备读数与源逐 dB 对齐
（源 peak −30.81 → vdev 麦克风 peak −30.82）。数字探针 p50 **10.22 ms**（复现 10.16 ms 基线）。

**但 `vdev-audio-win` 的环回本身不稳定，且只用 CLI 就能复现**（`vdev-mic-agent` 完全不参与）：

| `inject --endpoint vdev` + `capture --endpoint vdev`，连跑 5 轮 | 1 | 2 | 3 | 4 | 5 |
|---|---|---|---|---|---|
| 实测 peak | −6.02 ✅ | −6.02 ✅ | **−0.06** ❌ | **−0.06** ❌ | **−0.06** ❌ |

- 坏读数**与注入幅度无关**：amplitude 0.10 / 0.25 / 0.50 都得到同一个坏电平（−1.8 / −0.27 / −0.06 dBFS），
  说明不是"增益偏大"，是信号被破坏；
- 频谱上是被**硬削顶的 1 kHz**（基频 −20 dB、3 kHz 分量 −17 dB 更高，过零率 ≈7 kHz）；
- **一旦翻坏就持续坏**；驱动服务与设备都还是 RUNNING/Started。

所以这条链路上现在的瓶颈是**驱动环回**，不是 agent。在它修好之前，
`audio-mic-live-e2e.ps1` 输出的「vdev 麦克风」列不可信（本脚本已加 `Assert-Loopback`：
自检偏差 >3 dB 即判不通过）。

## 文档体检

```powershell
python scripts\check-docs.py            # 代码围栏配对/缩进、相对链接、发布稿 KMDF 残留
```

## 说明

- **退出码即结论**：`hid-keyboard-verify.ps1` / `hid-mouse-verify.ps1` 在没记录到任何真实输入事件时 **exit 1**——
  "CLI 打印已注入"不等于生效，空日志必须红（本仓库其他验收点也是这个口径）。
  需要管理员的两个脚本会自己请求 UAC，并用 `-Wait` 透传**提权子进程的退出码**；UAC 被取消时包装脚本
  同样 `exit 1`（不会把"没跑"报成"通过"）。
- 这些脚本会**真实操作设备**（安装/卸载驱动节点、注入按键、开音频流）：跑之前确认当前没有别人在用这台机器；
- **注入类脚本必须串行跑**：`hid-keyboard-verify` / `hid-mouse-verify` / `hid-injection-verify` 都会抢前台窗口和光标，
  并行执行会互相破坏证据（实测：键盘脚本把光标移走后，鼠标脚本的滚轮事件就落到别的窗口上，出现假红）；
- `-OutDir` 里的日志/录音就是验收证据，贴 PR/issue 时直接引用；
- 只做验收用，不进产品包（`scripts\stage-sign*.ps1` 不会打包本目录）。
