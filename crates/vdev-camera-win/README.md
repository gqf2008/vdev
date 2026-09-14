# vdev-camera-win

vdev 虚拟摄像头（Windows）：**DirectShow 源过滤器**，100% Rust（`windows` 官方绑定 + 安全封装），
用户态 COM，**免签名**。注册后任意 DirectShow 应用的摄像头列表里会出现 **vdev-camera**
（ffmpeg / OBS / 微信 / ToDesk…），画面来自 `push` 推的测试图案（或后续接入的真实帧）。

> 已在 **Win10 19045 x64** 真机验证（2026-09-14）：`list` 能看到 `vdev-camera`；
> `selftest` 进程内建图（源 → NullRenderer）3 s 交付 **62~75 帧**。

## 构建

本 crate 是独立 workspace（主 workspace 是 macOS-only）：

```powershell
cd crates\vdev-camera-win
cargo build --release
# 产出 target\release\：vdev_camera_win.dll（过滤器本体，cdylib）+ vdev-camera-win.exe（CLI）
```

## 安装 / 卸载 / 验证

```powershell
cd crates\vdev-camera-win
.\target\release\vdev-camera-win.exe install     # 注册（推荐管理员；失败自动回退当前用户）
.\target\release\vdev-camera-win.exe list        # 列出视频捕获源，应出现 vdev-camera
.\target\release\vdev-camera-win.exe selftest --seconds 3   # 进程内建图验证帧流动
.\target\release\vdev-camera-win.exe push --width 1280 --height 720 --fps 30
.\target\release\vdev-camera-win.exe uninstall   # 注销（同时清 HKLM/HKCU × WOW6432Node）
```

`install` 写 **`HKLM\Software\Classes\CLSID\{…}`（系统级）**，失败才回退 `HKCU`；
注册表里记的 DLL 路径 = **exe 同目录的 `vdev_camera_win.dll`**。
`push` 是"另一边"：它把画面写进共享内存，过滤器在被应用打开时读出来——要看画面得**另开一个程序**；
`selftest` 是"帧流动"的最短自证路径（不需要消费者）。

> **别从构建目录注册**：`target\release\` 会被 `cargo clean` / worktree 清理吃掉，
> 之后应用加载过滤器直接失败（本机踩过一次：注册指向了已删的 worktree）。
> 想让机器长期可用，先把 `vdev_camera_win.dll` + `vdev-camera-win.exe` 拷到固定目录
> （例如 `E:\vdev-dist\camera\`）再从那里 `install`；重建后重新 `install` 覆盖即可。

## 已知限制

- 只编 **64 位**过滤器：没构建 `vdev_camera_win32.dll` 时，**32 位应用**的摄像头列表看不到它（`install` 会 warn）；
- 免签名仅限用户态：注册表写入需要管理员，否则只能注册到当前用户；
- 画面来源目前是 `push` 的测试图案；真实帧（窗口/视频/屏幕）由宿主 `vdev-app-win` 那侧接。

## 排查

| 现象 | 先看 |
|---|---|
| `list` 里没有 vdev-camera | `install` 是否成功；`reg query HKCR\CLSID\{E4C01F0D-A9FC-4352-8590-F0E5AD2BFFCE} /s` 有没有 `InprocServer32` |
| 应用里选了摄像头却打不开/黑屏 | `InprocServer32` 指向的 DLL **文件是否还在**（常见：路径指向已删的构建目录） |
| `install` 打印成功但路径没变 | **HKCR 是合并视图、HKCU 优先**：旧注册可能留在 `HKCU\Software\Classes\CLSID\{…}`（无管理员时会回退写 HKCU），提权重装只写 HKLM、不覆盖它。分根查 `HKCU`/`HKLM`（含各自 `WOW6432Node`），删 HKCU 的 `CLSID\{…}`、`WOW6432Node\CLSID\{…}`、`CLSID\{860BB310-…}\Instance\{…}` 三处（不需要管理员），或直接 `uninstall` 再 `install` |
| 32 位程序看不到 | 需要 32 位过滤器 DLL（当前未构建） |

验收口径：**`list` 有设备名 + `selftest` 有帧数**才算通，不要只看 `install` 的成功输出。
