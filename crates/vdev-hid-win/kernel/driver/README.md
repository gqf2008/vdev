# vdev-hid-driver

vdev 虚拟键盘（KMDF 内核 HID minidriver，路线 B）。

- 注册到 HIDCLASS：设备管理器 HID 类出现「vdev 虚拟键盘」（`Root\vdev-hid`）。
- 键盘 HID 报告描述符含一个 8 字节厂商输出管道：用户态 `WriteFile` 8 字节键盘报告
  （1 修饰键 + 1 保留 + 6 按键）即注入，驱动经 manual 队列把报告投递给 hidclass，
  被系统作为真实 HID 键盘消费。

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
vdev-hid-win kernel install     # 需管理员；默认找 exe 同目录的 inf/sys
vdev-hid-win kernel status
vdev-hid-win kernel key a       # 注入按键 a（tap）
vdev-hid-win kernel key ctrl    # 注入修饰键（down 用 --action down）
vdev-hid-win kernel uninstall
```

内核驱动需开启测试签名（`bcdedit /set testsigning on` 后重启）或已签名证书。
