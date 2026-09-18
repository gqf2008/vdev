# 驱动验收脚本（Windows + macOS）

这些脚本是 Windows/macOS 虚拟设备在**真机**上做验收时用的工具，随 PR #12/#17/#18/#19 的验收过程沉淀下来：
它们不依赖 `SendInput`/测试桩，只走「设备节点 + 系统 API」这条真实路径，所以能真正回答"驱动装上了吗""注入生效了吗""链路通不通"。

原先这些脚本散落在验收机的临时目录里、路径写死成 `E:\vdev-hid-fix`；入库时统一改成：

- 仓库根默认按脚本位置推导（`scripts\acceptance\` 的上一级），需要时用 `-RepoRoot` 覆盖；
- 日志/录音等产物默认写 `%TEMP%\vdev-acceptance\`，用 `-OutDir` 覆盖；
- 需要管理员的脚本会**自动请求 UAC** 并把参数转发给提权后的自己。

## macOS 驱动验收（HID 文本注入）

`macos-hid-type-verify.sh` + `macos-hid-type-probe.swift` 验证 `vdev hid type` 是否逐字到达：

- 探针不再用裸二进制（macOS 26 上拿不到前台焦点），而是在临时目录组装最小
  `VdevHidProbe.app`，用 `open -W -n --args <out> 3` 启动；探针把
  `READY/SUMMARY/TYPED` 写进 out 文件。先跑一个 throwaway 实例预热 LaunchServices。
- **只有探针窗口拿到 `active=true key=true` 才注入**；拿不到前台焦点就 SKIP，绝不把合成键打进用户当前窗口。
- 判定：窗口 `TYPED == 输入` **且**（`tap_ok=false` 或 `tap_delta >= 字符数`）→ PASS。
  `.app` 身份通常拿不到辅助功能权限（`tap_ok=false`），此时以窗口逐字一致为准；
  `tap_ok=true` 时 EventTap 计数作为交叉校验。
- 假绿边界：`swiftc`/`open`/`python3` 缺失 → 立即 FAIL **exit 2**；探针未就绪、`tap_ok` 字段非法
  → FAIL（计入 fails，最终 exit 1）；所有用例都 SKIP / 没执行（`executed=0`）→ `NOT_RUN` exit 2；
  `TYPED` 不匹配且结束焦点已丢失 → 该用例按 SKIP 处理（`TYPED` 完全匹配时焦点在 SUMMARY
  时刻的变化不影响 PASS，因为注入已完成）。
- 注入窗口 3s，覆盖当前 ≤19 字符的用例（每字符约 2×12ms）；CASES 加长用例时需同步加大窗口。
- 阳性对照：修复前窗口只收到 **2/17**（EventTap 2/15）；修复后 8/8 PASS，
  15/17/19/9/4/4/16/10 字符逐字一致。对照 fake `vdev`（什么都不注入）→ 7 FAIL + 1 SKIP，
  `HID_TYPE_RESULT=FAIL`，无假绿。

```bash
cargo build -p vdev-host --release
./scripts/acceptance/macos-hid-type-verify.sh target/release/vdev
```

> 该脚本会**抢走前台焦点**并弹一个探针窗口，整轮约 40–60s；跑之前确认没有别人正在用这台机器。

## macOS 调研探针（蓝牙角色：能不能把 Mac 当手机可识别的耳麦 / 音箱）

`macos-bluetooth-role-probe.sh` + `macos-bluetooth-hfp-probe.m` + `macos-bluetooth-hci-probe.m`
是 2026-09-18 那轮蓝牙角色调研的产物，**结论与证据链见
[docs/dev/macos-bluetooth-role-survey.md](../../docs/dev/macos-bluetooth-role-survey.md)**。
一句话：控制面可用（SLC / 来电显示 / 从 Mac 拨号 / 通话状态），**通话音频拿不到**，
且 macOS 没有 A2DP Sink —— 「当耳麦」只到"被手机认出来"，「当音箱」完全不可行。

```bash
./scripts/acceptance/macos-bluetooth-role-probe.sh list                                    # 只读
./scripts/acceptance/macos-bluetooth-role-probe.sh sdp  "<地址|名字>"                       # 只读
./scripts/acceptance/macos-bluetooth-role-probe.sh hfp  "<地址|名字>" --audio --seconds 90  # 会连手机
./scripts/acceptance/macos-bluetooth-role-probe.sh hci  --addr <aa:bb:cc:dd:ee:ff>          # 控制器层
```

- **行为分级**：`list`/`sdp` 只读不改状态；`hfp` 会真的连手机（可逆，退出即断），
  叠加 `--dial` 会真的拨号、`--auto-accept` 会真的接听；`hci` 会向控制器发 HCI 命令（不做持久改动）。
- **产物**：默认编译到 `${TMPDIR:-/tmp}/vdev-bt-probe`（`-OutDir` 覆盖），源文件没变不重编
  （避免每次运行重写二进制导致该路径的身份漂移）。
- **两个必须记住的坑**：① 判据是"**通话中** SCO 是否 status=0"，无通话时 `connectSCO`
  返回 `kIOReturnUnsupported` 属正常，别据此推断"等有通话就好了"；② HCI 那条路上
  `handle=0` 调用会返回 success 却什么都不做，必须以 `out.connectionHandle != 0` 判成功。
- **前置条件**：手机需已与该 Mac 配对；若上次会话残留导致 SLC 建不起来，先用 `--reset`
  或把手机蓝牙关→开。

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

没有物理麦克风时用「立体声混音 / Stereo Mix」当信号源（`-Input`、`-ToneEndpoint` 可改）：往真实声卡播 1 kHz 正弦，由混音端采回，穿过整条链路。本机实测基线：

| 项目 | 实测 |
|---|---|
| 环回积压（1 MB 环 / 256 KB 环） | 5.14 s / **1.48 s**（理论 5.46 s / 1.37 s） |
| 环回链路的电平 | 峰值 **−6.02 dBFS**（0.5 幅度正弦理论值） |
| mic-agent：数字探针 p50 | **10.16 ms** |
| mic-agent：完整降噪 20 s 实时 | 帧时 p50 **26.8 µs**、CPU **0.156 %**（单核） |
| 直通 vs 降噪 A/B | **−5.8 → −34.1 dBFS** |

## 降噪模型：算法延迟 / 质量 / 残余（macOS + Windows 通用）

`audio-denoise-metrics.py` 是「模型到底给端到端加了多久」的唯一标尺——纯 WAV 工具，两个平台都能跑，需要 `numpy`：

| 子命令 | 作用 |
|---|---|
| `lag <in.wav> <out.wav> [max_ms]` | 输入→输出延迟：在 `in` 最响的 2 s 上做**原始波形**归一化互相关。不用能量包络——包络被音节率抹平，只能给到 ±10 ms；原始波形的峰是尖的（偏移 5 ms 掉约 0.5 相关），能分辨单帧 |
| `sisnr <clean.wav> <test.wav> [undo_lag_ms]` | SI-SDR（尺度不变，不会让「把音量调小」冒充降噪） |
| `rms <wav> [start_s] [end_s]` | 分段 RMS（dBFS） |
| `report <clean> <noisy> <denoised>` | 一次出齐：lag + SI-SDR 前后 + 残余电平 |

```bash
# RNNoise 基线：960 样本 = 20.00 ms（peak corr 0.99+）
cargo build -p vdev-mic-agent --release
target/release/vdev-mic-agent run clean48.wav --dll /path/to/librnnoise.dylib --mix 1 --out out.wav
python3 scripts/acceptance/audio-denoise-metrics.py lag clean48.wav out.wav 200
```

**必须用流式（逐帧、带状态）实现测**：整段 `model(waveform)` 得到的是质量上限，不是这条链路的延迟。
`--mix 1` 时 dry 完全不参与混音（`dry_delay_samples = 0`），测出来的 lag 就是模型自身的算法延迟；
`--mix 0 --lookahead-samples 0` 是零延迟对照，用来证明测量链自己不加延迟（实测 lag 0 / corr 1.0000）。

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
