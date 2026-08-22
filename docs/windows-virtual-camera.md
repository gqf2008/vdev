# Windows 虚拟摄像头 vdev-camera-win（DirectShow）

用 Rust 在 Windows 上实现的虚拟摄像头：一个 **DirectShow 源过滤器**（用户态 COM 组件，
无需内核驱动、无需驱动签名），把跨进程推送的 BGRA 帧变成系统里的一个「视频捕获源」，
任意 App（ffmpeg / OBS / Zoom / Teams / 微信等）都能把它当摄像头选。

对应 macOS 版（`vdev-camera`，CMIOExtension）在 `docs/README.md` / `crates/vdev-camera`。

## 为什么选 DirectShow（而不是写驱动）

| 路线 | 说明 | 门槛 |
|---|---|---|
| **DirectShow 源过滤器（本方案）** | 用户态 COM 过滤器，注册到「视频捕获源」类别 | 无签名、无驱动，regsvr32/自注册即可 |
| AVStream 内核过滤器 | 内核虚拟摄像头（OBS Virtual Camera 走这条路） | 驱动签名（测试签名/EV/WHQL）门槛高 |
| IddCx 虚拟显示器 | 屏幕类虚拟设备 | 驱动签名门槛高 |

DirectShow 是老 API 但有「零签名门槛 + 生态兼容最广」的优势，且 Windows 自带
`quartz.dll` 提供全部基础设施，Rust 侧用微软官方 `windows` crate 0.62 的完整绑定 +
`implement` 宏即可 100% Rust 实现。

## 架构（安全封装优先）

所有 Windows 系统 API 先收敛到带 `SAFETY` 注释的安全封装模块，业务逻辑只调安全接口：

```
crates/vdev-camera-win (独立 workspace，不影响 macOS 主仓库)
  com/
    mod.rs        COM 初始化（ComInit RAII）/ 类工厂 / DLL 导出（DllGetClassObject/
                  DllRegisterServer/DllCanUnloadNow/DllUnregisterServer）
    registry.rs   注册表安全封装（RegKey RAII：create/set_string/set_binary/delete_tree）
    shm.rs        跨进程共享帧通道（命名 SHM + 命名事件 + 双缓冲 + 序号，无锁）
  dshow/
    media_type.rs   AM_MEDIA_TYPE / VIDEOINFOHEADER 安全封装（CoTaskMem 分配/释放/深拷贝）
    filter.rs       VirtualCameraFilter（IBaseFilter/IMediaFilter/IPersist/IAMFilterMiscFlags）
    pin.rs          OutputPin（IPin 全 15 方法 + IAMStreamConfig + IKsPropertySet）
    device.rs       视频捕获源枚举安全封装（ICreateDevEnum + IPropertyBag）
    selftest.rs     进程内自测图安全封装（源 → NullRenderer）
    enum_pins.rs / enum_media_types.rs
    streaming.rs    推流线程（取最新帧 → 填 IMediaSample → IMemInputPin::Receive；
                     无帧回退棋盘格；生产者帧尺寸≠协商尺寸时最近邻缩放）
  camera.rs       面向宿主的高层安全 API：register/unregister/push_frame
  main.rs         CLI：install / uninstall / selftest / push / list（纯业务层，零 unsafe）
```

关键 COM 接口：

- `VirtualCameraFilter`：`IBaseFilter` + 继承链（`IMediaFilter`/`IPersist`）+ `IAMFilterMiscFlags`（标识为源过滤器）。
- `OutputPin`：`IPin` 全部方法 + `IAMStreamConfig`（消费方查询/设置输出格式）+
  `IKsPropertySet`（返回 `PIN_CATEGORY_CAPTURE`，消费方据此识别「捕获」pin）。

## 构建

```powershell
cd crates/vdev-camera-win
cargo build --release
```

产物：`target\release\vdev_camera_win.dll`（64 位过滤器 DLL）+ `vdev_camera_win32.dll`（32 位）+ `vdev-camera-win.exe`（CLI）。
（crate 名 `vdev-camera-win` 的默认 lib target 名带下划线，DLL 就叫 `vdev_camera_win.dll`。）

32 位 DLL 编译（32 位应用如 32 位 VLC 需要，走 WOW6432Node 视图）：

```powershell
rustup target add i686-pc-windows-msvc
cargo build --release --target i686-pc-windows-msvc
Copy-Item target\i686-pc-windows-msvc\release\vdev_camera_win.dll target\release\vdev_camera_win32.dll
```

## 安装 / 卸载

```powershell
# 安装：优先注册到 HKLM（需管理员），失败自动回退 HKCU（免管理员，HKCR 视图同样可见）
.\target\release\vdev-camera-win.exe install

# 验证枚举
.\target\release\vdev-camera-win.exe list
ffmpeg -f dshow -list_devices true -i dummy      # 应看到 "vdev-camera" (video)
ffmpeg -f dshow -list_options true -i video=vdev-camera   # 应列出 3 档格式

# 卸载（清理 HKLM 与 HKCU 两个根）
.\target\release\vdev-camera-win.exe uninstall
```

注册结构（与 OBS Virtual Camera / ToDesk Camera 一致）：

```
HKCR\CLSID\{E4C01F0D-A9FC-4352-8590-F0E5AD2BFFCE}\InprocServer32   # DLL 路径 + ThreadingModel=Both
HKCR\CLSID\{860BB310-5D01-11D0-BD3B-00A0C911CE86}\Instance\{E4C01F0D-...}
    FriendlyName = vdev-camera     # 缺它设备枚举直接跳过（关键）
    CLSID        = {E4C01F0D-...}
    FilterData   = REG_BINARY（REGFILTER2 v2 序列化：单 RGB32 输出 pin、MERIT_DO_NOT_USE）
```

## 使用

```powershell
# 终端 1：推流（棋盘格测试画面，可指定分辨率/帧率/时长）
.\target\release\vdev-camera-win.exe push --width 640 --height 360 --fps 30 --seconds 120

# 终端 2：任意 App 摄像头列表选 vdev-camera，或用 ffmpeg 取流
ffmpeg -f dshow -i "video=vdev-camera" -frames:v 1 -update 1 out.png
ffmpeg -f dshow -i "video=vdev-camera" -c:v libx264 -f mp4 out.mp4
```

推流与取流是**两个独立进程**，通过命名共享内存通道（`com/shm.rs`）通信：
- 生产者（`push`）把 BGRA 帧写进 SHM，带 Release/Acquire 序号，无锁双缓冲。
- 过滤器（消费方，在目标 App 进程内）取最新帧，无新帧时回退棋盘格图案。
- 生产者帧尺寸与连接协商尺寸不一致时自动最近邻缩放，保证下游每帧大小一致。

## 自测

```powershell
# 进程内 DirectShow 图：源过滤器 → NullRenderer，验证连接/推流/帧计数
.\target\release\vdev-camera-win.exe selftest --seconds 3
# 期望：3s 内交付约 80 帧
```

单元/集成测试：`cargo test`（SHM 往返 3 例 + FilterData 布局 2 例）。

## 踩坑记录（详见 LESSON）

- **Instance 键必须带 `FriendlyName`**：只有 `CLSID` 时设备枚举器（ICreateDevEnum）直接跳过，
  枚举不到但 CoCreateInstance 又能成功，极具迷惑性。
- **输出 pin 必须实现 `IKsPropertySet` 并返回 `PIN_CATEGORY_CAPTURE`**：ffmpeg 的
  `dshow_cycle_pins` 要求 QI `IKsPropertySet` 成功 + `Get(AMPROPSETID_Pin,
  AMPROPERTY_PIN_CATEGORY)` 返回 `PIN_CATEGORY_CAPTURE`，否则报「Could not find output pin」。
- **推源必须自建并 `Commit` 内存分配器**：下游（ffmpeg sink）的 `GetAllocator` 返回
  `VFW_E_NO_ALLOCATOR`，且分配器不 `Commit` 时 `GetBuffer` 永远失败（表现为推流线程空转）。
  参照 `CBaseOutputPin::DecideAllocator`：先试下游分配器，失败回退 `CLSID_MemoryAllocator`，
  激活（Pause）时 `Commit`、停止（Stop）时 `Decommit`。
- **推流线程在 Pause 启动**（送预滚帧），Run 只更新 `tstart`；否则图一直 GetState=Paused 死锁。
- **不要 SetTime 样本时间戳**：基于 CBaseRenderer 的下游（NullRenderer 等）会按参考时钟等待，
  帧率掉到 ~0；去掉后 ~26fps。
- **FilterData 的媒体类型必须与 pin 实际输出一致**（本过滤器输出 RGB32/BGRA）。

## 当前限制

- 固定输出 RGB32（BGRA），3 档分辨率（1920x1080 / 1280x720 / 640x480）@ 30fps。
- 推流端是简单棋盘格图案 + 共享帧通道；音频、多分辨率动态协商、配置 UI 等后续再做。
- 同时注册 64 位视图与 32 位视图（WOW6432Node）：32 位进程（如 32 位 VLC）通过 WOW64 重定向只能看到 WOW6432Node 视图，install 时若同目录存在 `vdev_camera_win32.dll` 会自动注册 32 位视图。



