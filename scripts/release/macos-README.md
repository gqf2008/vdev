# vdev macOS 包

命令行工具 + CoreAudio HAL 虚拟声卡。**不含** `VDCamera.app`（虚拟摄像头需要
Apple Development 证书与 embedded provisioning profile，CI 没有这些材料，见
walgit 线程 `release-macos-camera-1`）。

## 包内容

| 路径 | 说明 |
|---|---|
| `bin/vdev-mic-agent` | AI 虚拟麦克风：物理麦克风 → 降噪 → 注入虚拟麦克风 |
| `bin/librnnoise.dylib` | 降噪后端（xiph/rnnoise，运行时 `dlopen` 加载；**必须与 mic-agent 放一起**） |
| `bin/vdev` | 宿主 CLI：虚拟键鼠注入（`vdev hid`）、虚拟屏（`vdev screen`）、摄像头滤镜（`vdev camera`） |
| `bin/vdev-audio-ctl` | 虚拟声卡控制/自测 |
| `vdev-audio.driver/` | CoreAudio HAL 插件（安装到 `/Library/Audio/Plug-Ins/HAL/`） |

## 安装与使用

```bash
# 1) 先看虚拟声卡在不在（需要 root 安装，见下）
./bin/vdev-audio-ctl --help

# 2) 装 HAL 插件（需要管理员密码；装完重启 coreaudiod）
read -r -p "install driver to /Library/Audio/Plug-Ins/HAL? [y/N] " y
[ "$y" = y ] && sudo cp -R vdev-audio.driver /Library/Audio/Plug-Ins/HAL/ && sudo killall coreaudiod

# 3) 降噪虚拟麦克风：注入 vdev-audio A，会议软件里把麦克风选成「vdev-audio A」
./bin/vdev-mic-agent live --adaptive --seconds 30
```

`librnnoise.dylib` 就在 `bin/` 里，mic-agent 会自己找到它（显式指定用 `--dll`）。

## 架构

本包是 **Apple Silicon（arm64）** 构建，macOS 26+。Intel 机器请自行从源码构建。

## ⚠️ 签名：CI 出的包是 **adhoc 签名**

CI 里没有 Developer ID 证书，所以 `vdev-audio.driver` 是 `codesign --sign -` 的 adhoc 产物
（**如果这包是你自己在带 Developer ID 的机器上跑 `scripts/release/package-macos.sh` 生成的，
那它就是 Developer ID 签名，可以忽略下面这段**）。
**macOS 26 的 coreaudiod 可能拒绝加载 adhoc 签名的 HAL 插件**（本仓库 Makefile 里也写了这条警告）。
如果你有自己的证书，重签一次即可：

```bash
IDENTITY="Developer ID Application: Your Name (TEAMID)"   # 或 "Apple Development: ..."
codesign --force --sign "$IDENTITY" --identifier com.vdev.audio.driver vdev-audio.driver
codesign --verify --strict --verbose=2 vdev-audio.driver
```

`vdev` / `vdev-mic-agent` 是普通命令行工具，不需要签名；但
`vdev hid type` 注入键盘需要系统「辅助功能」权限（首次运行会提示，去
系统设置 → 隐私与安全性 → 辅助功能 里勾选）。

## 版本与来源

- HAL 插件 / `vdev` / `vdev-mic-agent`：本仓库 tag 对应源码构建；
- `librnnoise.dylib`：xiph/rnnoise 源码构建，commit 与模型哈希写在 `THIRD_PARTY.md` 与 Release 说明里；
  它的 sha256 同时出现在**包内** `SHA256SUMS.txt` 与 Release 的 `SHA256SUMS.txt`（后者是各 zip 的）；
- 许可证：RNNoise 为 BSD-3-Clause，原文见 `licenses/rnnoise-COPYING.txt`。

已知限制：虚拟摄像头（`VDCamera.app`）不在本包；
`vdev hid type` 的注入器 app 需要在有「辅助功能」权限的会话里首次准备。
