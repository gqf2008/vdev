# vdev-hid-win

vdev 虚拟键鼠（Windows）：**VHF（Virtual HID Framework）内核驱动** + CLI。
设备管理器 HID 类下会出现两个节点——「vdev 虚拟键盘」（`Root\vdev-hid`）与
「vdev 虚拟鼠标」（`Root\vdev-hid-mouse`），键盘/鼠标事件由 CLI 经 HID 报告真正注入系统输入栈
（不是 `SendInput`，所以对游戏/远控那类"只认 HID"的程序同样有效）。

> 已在 **Win10 19045 x64（测试签名）** 真机验证（2026-09-14）：两个节点 `Status=OK`；
> 键盘注入 `a/b/1/Enter` → 观察窗 8 条 `KeyDown/KeyPress`；鼠标 `click / wheel 120 / move 20 0`
> → `MouseDown Left` / `MouseWheel delta=120` / 光标 `dx≠0`。原理与踩坑见
> [`docs/community/windows-virtual-hid.md`](../../docs/community/windows-virtual-hid.md)。

## 目录结构

```
kernel/            内核侧工作区（Rust + WDK）
  driver/          vdev-hid-driver：VHF 虚拟 HID 驱动（产出 vdev_hid.dll → 打包改名 vdev_hid.sys）
  vendor/wdk-sys/  vendored wdk-sys（bindgen 绑定，需要 libclang 18）
src/               vdev-hid-win.exe：kernel install/uninstall/status + key/mouse 注入
scripts/stage-sign-hid.ps1   打包 + Inf2Cat + signtool
```

## 前置

- Windows x64 + Visual Studio 2022（C++ 桌面负载）+ **WDK 10.0.26100**
- Rust stable（MSVC target）：驱动必须 `--target x86_64-pc-windows-msvc`
- libclang 18（pip 装的即可）：bindgen 0.71 与 libclang 22 不兼容
- 测试签名：`bcdedit /set testsigning on`（重启生效）或已签名证书

## 构建

```powershell
$env:LIBCLANG_PATH = "$env:APPDATA\Python\Python312\site-packages\clang\native"

# 内核驱动（在 kernel 子工作区；产出 kernel\target\x86_64-pc-windows-msvc\release\vdev_hid.dll）
cd crates\vdev-hid-win\kernel
cargo build --release
cargo clippy -p vdev-hid-driver --no-deps -- -D warnings
cargo fmt --check

# CLI（在仓库根）
cd ..\..
cargo build --release -p vdev-hid-win
```

## 打包与签名

```powershell
powershell -ExecutionPolicy Bypass -File crates\vdev-hid-win\scripts\stage-sign-hid.ps1
# 输出 crates\vdev-hid-win\target\dist\：vdev_hid.sys + vdev-hid.inf + vdev-hid.cat + vdev-test-signing.cer
```

> 改驱动后必须**升 INF 的 `DriverVer`**（或先 `pnputil /delete-driver` 删包）：
> 同版本包会被 Windows 视为"已装过"，`System32\drivers\vdev_hid.sys` 不会替换——
> 看着装上了，跑的仍是旧构建。装完请比对 sys 的 SHA256/时间戳。

## 安装 / 卸载 / 状态

```powershell
$exe = ".\crates\vdev-hid-win\target\x86_64-pc-windows-msvc\release\vdev-hid-win.exe"

& $exe kernel install --inf-dir crates\vdev-hid-win\target\dist   # 需管理员（自动 UAC）
& $exe kernel status
& $exe kernel uninstall
```

## 注入

```powershell
& $exe kernel key a                       # 单键 tap
& $exe kernel key ctrl --action down      # 修饰键按住 / 抬起（--action up）
& $exe kernel key a --modifiers ctrl      # 组合键（Ctrl+A）
& $exe kernel mouse move 20 0             # 相对移动（±127 上限）
& $exe kernel mouse click                 # 左键单击（也支持 down/up 与右键）
& $exe kernel mouse wheel 120             # 滚轮（120 的倍数）
```

边界：**只支持相对移动**，没有绝对坐标模式；滚轮按 120 的倍数给。

## 验收

仓库根 `scripts\acceptance\` 下有成套真机验收脚本（枚举、键鼠观察窗、注入端到端、卸载回归、
干净重装、通道探针）。**判据是被注入端的事件/位移，不是 CLI 打印的成功**：

```powershell
python scripts\acceptance\hid-enum.py                 # 接口在不在、能不能只写打开
python scripts\acceptance\hid-channel-probe.py        # 三条注入通道各自的结果（含 errno 与光标位移）
powershell -File scripts\acceptance\hid-keyboard-verify.ps1   # 观察窗记录 KeyDown/KeyPress，空日志 exit 1
powershell -File scripts\acceptance\hid-mouse-verify.ps1      # 记录 MouseDown/MouseWheel 与光标位移
```

注入类脚本会抢前台/光标，**必须串行跑**。

## 已知限制

- 键盘报告 8 字节（1 修饰键 + 1 保留 + 6 按键）、鼠标 4 字节（键位 + X + Y + 滚轮），都走
  **厂商 Feature 报告**注入；`WriteFile`/输出报告在这条链路上不可用（`ERROR_INVALID_FUNCTION`）；
- 需要测试签名（或已签名证书 + TrustedPublisher/Root）；纯内核驱动，用户态不装服务；
- Driver Verifier（special pool + DDI compliance）专项尚未跑。

## 排查

| 现象 | 先看 |
|---|---|
| `install` 报签名错误 | testsigning 是否生效（`bcdedit /enum` 看 `testsigning Yes`，改完要重启）；证书是否进了 TrustedPublisher/Root |
| 设备管理器里没有两个节点 | `Get-PnpDevice -Class HIDClass`；INF 是否同时含 `Root\vdev-hid` 与 `Root\vdev-hid-mouse` 模型节；`C:\Windows\INF\setupapi.dev.log` |
| 鼠标被识别成键盘 | 历史 bug（HWID 前缀误匹配），当前代码是逐段精确比较；如复现请带 `InstanceId + HardwareID` 反馈 |
| 装了但行为没变 | INF `DriverVer` 是否升过；`C:\Windows\System32\drivers\vdev_hid.sys` 的时间戳与 SHA256 是否等于新构建 |
| 节点越卸越多（幽灵/重复节点） | `scripts\acceptance\hid-converge.ps1` 收敛；收敛不掉再 `hid-reinstall.ps1`（删包重装） |

### 注入不生效怎么查（2026-09-14 实机跑过一遍，按这个顺序走）

1. **接口层**：`python scripts\acceptance\hid-enum.py`
   期望看到 `vid=0x5644 pid=0x4849`（键盘）与 `pid=0x484D`（鼠标），且 `w=ok`（只写打开成功）。
2. **通道层**：`python scripts\acceptance\hid-channel-probe.py`。实机基准：

   | 通道 | 结果 |
   |---|---|
   | `HidD_SetFeature`（帧化 5B，带 1 字节 Report ID） | `api_ok=True err=0`，光标 **dx=+22** ← 唯一可用通道 |
   | `HidD_SetFeature`（裸 4B） | `api_ok=False err=87`（`FeatureReportByteLength` 含 Report ID） |
   | `WriteFile`（裸/帧化） | `api_ok=False err=1`（描述符里没有 Output 报告） |

   若第 1 行"成功但光标不动"，说明写入被受理却没进系统——**九成是装置状态脏**，见下一步。
3. **装置层**：反复 install/uninstall 会留下幽灵/重复节点（`ROOT\HIDCLASS\000X` 无驱动条目、
   或两个都挂 `Root\vdev-hid`），写入就落到**无效实例**上；`pnputil /restart-device` 救不回来。
   依次用 `scripts\acceptance\hid-converge.ps1`（清节点 → 重装一对）、不行再用 `hid-reinstall.ps1`
   （`pnputil /delete-driver` 删包 → 重装，顺带把旧构建的 sys 换掉），两者都会断言"恰好一对节点"。
4. **判定**：`hid-keyboard-verify.ps1` / `hid-mouse-verify.ps1` 必须 **exit=0** 才算过
   （键盘脚本在拿不到前台焦点时会中止并报红，避免把按键打进你正在用的窗口）。
5. **要不要怀疑驱动/CLI 代码**：先做第 2、3 步。历史上出现过"新旧两个 CLI 都报成功但零副作用"，
   最后定位是装置状态，代码没改一行。若确实要验证"VHF 回调有没有被调用"，给回调临时塞一个特征
   返回码即可（例如让 `EvtVhfAsyncOperationSetFeature` 立刻用 `STATUS_NOT_SUPPORTED(0xC00000BB)` 完成，
   用户态拿到 `ERROR_NOT_SUPPORTED(50)` 就说明回调被执行——测完记得删探针重建）。
