# vdev-hid-driver

vdev 虚拟键盘 / 虚拟鼠标（KMDF 内核 HID minidriver，路线 B）。

- 注册到 HIDCLASS：设备管理器 HID 类出现「vdev 虚拟键盘」（`Root\vdev-hid`）与
  「vdev 虚拟鼠标」（`Root\vdev-hid-mouse`），共用 `vdev_hid.sys`。
- 报告描述符含厂商输出管道：用户态 `WriteFile` 即注入——
  - 键盘：8 字节报告（1 修饰键 + 1 保留 + 6 按键）
  - 鼠标：4 字节报告（1 键位 + X + Y + 滚轮，相对值）
- 驱动经 manual 队列把报告投递给 hidclass，被系统作为真实 HID 设备消费。

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
