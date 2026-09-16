# vdev-audio-win

vdev 虚拟声卡（Windows）：**PortCls / WaveRT 内核驱动**（WDM，无 WDF）+ CLI + GUI 页。
控制面板/音频设置里会出现「vdev 扬声器」与「vdev 麦克风」两个端点：**播放到扬声器的声音
会被麦克风录到**（环回），宿主也可以从扬声器侧注入音频。

> 已在 **Win10 19045 x64（测试签名）** 真机验证通过（2026-09-14）：端点可开流、环回实测
> RMS −6.0 dBFS（0.5 幅度 1 kHz 正弦，理论值）、独占模式可开流。细节与踩坑见
> [`docs/community/windows-virtual-audio.md`](../../docs/community/windows-virtual-audio.md)。

## 目录结构

```
driver/    内核驱动（PortCls miniport + topology，no_std；kernel feature 开内核链接）
  vdev-audio.inf        设备/接口安装脚本（UTF-16 LE + BOM，SetupAPI 只认这个编码）
cli/       vdev-audio-win.exe：install/uninstall/status/inject/capture
scripts/stage-sign-audio.ps1   打包 + Inf2Cat + signtool
```

## 前置

- Windows x64 + Visual Studio 2022（C++ 桌面负载）+ **WDK 10.0.26100**
- Rust stable（MSVC target）：驱动必须 `--target x86_64-pc-windows-msvc`
- libclang 18（pip 装的即可）：bindgen 0.71 与 libclang 22 不兼容
- 测试签名：`bcdedit /set testsigning on`（重启生效）或已签名证书

## 构建

```powershell
$env:LIBCLANG_PATH = "$env:APPDATA\Python\Python312\site-packages\clang\native"
cd crates\vdev-audio-win

# 内核驱动（产出 vdev_audio.dll → 打包时改名 vdev_audio.sys）
cargo build --release --features kernel -p vdev-audio-driver --target x86_64-pc-windows-msvc
cargo clippy --release --features kernel -p vdev-audio-driver --target x86_64-pc-windows-msvc --no-deps -- -D warnings
cargo test -p vdev-audio-driver          # 宿主单测（环形缓冲/布局/描述符/GUID/INF 契约）

# CLI（含 inject/capture）
cargo build --release --target x86_64-pc-windows-msvc -p vdev-audio-win
```

## 打包与签名

```powershell
powershell -ExecutionPolicy Bypass -File scripts\stage-sign-audio.ps1
# 输出 target\dist\：vdev_audio.sys + vdev-audio.inf + vdev-audio.cat + vdev-audio-win.exe
```

脚本从 `Cert:\CurrentUser\My` 里按 **FriendlyName=vdev-driver** 选证书（用指纹 `/sha1` 签名；
`/n` 匹配的是 CN，传完整 DN 会直接报找不到证书）。自签证书需装进 `TrustedPublisher` 与 `Root`。

> 每次改驱动都要**升 INF 的 `DriverVer`**：版本没变时 `pnputil /add-driver /install` 会报
> `Driver package is up-to-date on device`，`System32\drivers\vdev_audio.sys` 时间戳不变——
> 看着装上了，跑的还是旧驱动。

## 安装 / 卸载 / 状态

```powershell
cd crates\vdev-audio-win
.\target\x86_64-pc-windows-msvc\release\vdev-audio-win.exe install --inf-dir target\dist   # 需管理员（自动 UAC）
.\target\x86_64-pc-windows-msvc\release\vdev-audio-win.exe status                          # --json 出机器可读格式
.\target\x86_64-pc-windows-msvc\release\vdev-audio-win.exe uninstall
```

## 注入 / 采集（宿主侧验证环回）

```powershell
$exe = ".\target\x86_64-pc-windows-msvc\release\vdev-audio-win.exe"

& $exe inject --tone 1000 --amplitude 0.5 --duration 4   # 向「vdev 扬声器」推流（可 --wav f.wav 循环播放）
& $exe capture --duration 6 --skip 4 --json              # 从「vdev 麦克风」采集，报 RMS/峰值 dBFS
```

**顺序执行读不到当次注入**：两个流共享非分页环形缓冲（0.3.9.0 = 1 MB，按设备格式
16bit/48k/2ch = 192000 B/s 约 **5.46 s** 积压；0.3.10.0 起 256 KB ≈ 1.37 s），
所以"先注入再采集"读到的是环里的历史数据。要量当次注入就**并发**跑：一个进程 `capture`、
另一个进程 `inject`（`--skip` 用来跳过起播瞬间）。参考实测：

```
基线（先注 4s 静音冲满环）: rms -96.8 dBFS
并发 inject(4s 正弦 0.5) ‖ capture(6s, skip 4): rms -9.1 dBFS / peak -6.02 dBFS
  （0.5 幅度正弦理论值 -9.03 / -6.02）
```

## GUI

`crates/vdev-app-win` 的「虚拟声卡」页有安装/卸载/刷新 + 「注入音频」/「环回自测」：
后者并发跑 `capture ‖ inject` 并把 RMS/峰值写回面板与日志。GUI 委托本 crate 的 exe
（找不到时设 `VDEV_AUDIO_WIN_EXE` 或把 exe 放到 GUI 同目录）。

## 已知限制

- 格式固定 **48 kHz / 16 bit / 双声道 PCM**（引擎负责混音格式↔设备格式转换）；
- 环回积压：0.3.9.0 的 1 MB 环形缓冲 ≈ **5.46 s**；0.3.10.0 起 256 KB ≈ **1.37 s**（可按需再调）；
- 音量/静音只有采集 topology 有节点（"记事本"语义，无 DSP 效果）；渲染拓扑直通；
- 驱动不暴露 IOCTL/WriteFile 面；宿主注音走端点推流（`inject`）；
- 驱动侧设备适配器是**单例**（`adapter::create` 只允许一个实例）：设备树里若出现第二个 `Root\vdev-audio` 节点，第二个会在 `start_device` 里直接上报 `STATUS_INSUFFICIENT_RESOURCES`（`0xC000009A`）——`install` 现已幂等，正常路径不会产生这种节点；
- Driver Verifier（special pool + DDI compliance）专项尚未跑。

## 排查

| 现象 | 先看 |
|---|---|
| `install` 报签名错误 | testsigning 是否已生效（`bcdedit /enum` 看 `testsigning Yes`，改完要重启），或证书是否进了 TrustedPublisher/Root |
| 端点不存在 | `vdev-audio-win status`；KS 五类别符号链接是否存在；INF 是否含 `Root\vdev-audio` 模型节 |
| 端点存在但 `IAudioClient` 报 `0x80070491` | 开流 pin 的 `KSPROPERTY_PIN_INTERFACES` 是否给了 `KSINTERFACE_STANDARD_LOOPED_STREAMING(1)`；数据范围是否带 `KSDATARANGE_ATTRIBUTES` + 信号处理模式属性（详见社区文档"案例七～十一"） |
| 能开流但没声音 / 采集无包 | 流是否真的拿到格式（`NewStream` 里应用格式，`bytes_per_sec` 不能是 0）；`GetCurrentPadding` 是否下降 |
| 装了但行为没变 | INF `DriverVer` 是否升过；`C:\Windows\System32\drivers\vdev_audio.sys` 时间戳 |
| `install` 后出现多个 `ROOT\MEDIA\000N` 节点 / 设备报 `0xC000009A` | 该驱动的设备适配器是**单例**：一个系统只允许一个设备节点，多出来的节点必然 `CM_PROB_FAILED_START`。0.3.10.0 之前的 CLI 卸载是「假成功」，会留下这种幽灵节点；现版本 `install` 已幂等（有节点就地更新、多节点先清后建、装完断言恰好一个），遇到残留直接 `uninstall` 清干净重装 |
