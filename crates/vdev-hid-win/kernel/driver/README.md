# vdev-hid-driver

vdev 虚拟键盘 / 虚拟鼠标（**Virtual HID Framework（VHF）**内核驱动，路线 B）。
已在 Win10 19045 x64 真机验证：设备管理器出现两个节点 + 实弹注入生效（见 `docs/community/windows-virtual-hid.md`）。

- 设备管理器 HID 类出现「vdev 虚拟键盘」（`Root\vdev-hid`）与「vdev 虚拟鼠标」
  （`Root\vdev-hid-mouse`），共用 `vdev_hid.sys`；两者都是本驱动在
  `EvtDevicePrepareHardware` 里用 `VhfCreate/VhfStart` 建的虚拟 HID 设备。
- 报告描述符由本驱动提供：
  - 键盘：8 字节报告（1 修饰键 + 1 保留 + 6 按键）
  - 鼠标：4 字节报告（1 键位 + X + Y + 滚轮，相对值）
- 注入链路：用户态 `HidD_SetFeature`（厂商 Feature 报告）→ 驱动
  `EvtVhfAsyncOperationSetFeature` → 转成输入报告 `VhfReadReportSubmit` → 系统输入栈。
  `WriteFile`（输出报告）在这条链路上不可用（返回 `ERROR_INVALID_FUNCTION`）。
- INF 里必须有 `HKR,,LowerFilters,0x00010000,"vhf"`：`vhf.sys` 不在本设备栈里时
  `VhfCreate` 直接返回 `STATUS_INVALID_DEVICE_REQUEST(0xC0000010)`。

> 早期实现是 KMDF HID minidriver（`Include=MsHidKmdf.inf`），该文件只随 Windows 11
> （build 22000+）提供，Win10 上安装报 `0xE0000219`，因此改用 VHF（Win10 1607 起随系统提供）。

## 构建

```powershell
$env:LIBCLANG_PATH = "$env:APPDATA\..\Roaming\Python\Python312\site-packages\clang\native"
cd crates\vdev-hid-win\kernel
cargo build --release     # 产出 target\...\release\vdev_hid.dll（拷贝为 vdev_hid.sys）
cargo clippy -p vdev-hid-driver --no-deps -- -D warnings
cargo fmt --check
```

> LIBCLANG_PATH 必须指向 pip 装的 libclang 18（bindgen 0.71 与 libclang 22 不兼容）。

## 打包 / 签名

```powershell
powershell -ExecutionPolicy Bypass -File crates\vdev-hid-win\scripts\stage-sign-hid.ps1
```

输出 `crates\vdev-hid-win\target\dist\`（vdev_hid.sys + vdev-hid.inf + vdev-hid.cat）。

## 安装 / 注入

```powershell
vdev-hid-win kernel install                    # 需管理员；默认找 exe 同目录的 inf/sys
vdev-hid-win kernel status
vdev-hid-win kernel key a                      # 注入按键 a（tap）
vdev-hid-win kernel key ctrl --action down     # 注入修饰键
vdev-hid-win kernel mouse move 20 0            # 鼠标相对移动
vdev-hid-win kernel mouse click                # 鼠标左键点击
vdev-hid-win kernel mouse wheel 120            # 滚轮向上
vdev-hid-win kernel uninstall
```

内核驱动需开启测试签名（`bcdedit /set testsigning on` 后重启）或已签名证书。

> CLI 用法、验收脚本与「注入不生效怎么查」的完整排查顺序见 crate README
> [`../../README.md`](../../README.md)。
