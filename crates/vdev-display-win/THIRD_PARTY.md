# 第三方代码归属（THIRD PARTY NOTICES）

`crates/vdev-display-win` 移植/改编自以下 MIT 许可项目，按许可要求保留版权声明与许可文本：

## MolotovCherry/virtual-display-rs (MIT)
Copyright (c) 2024 Cherry
- 仓库：https://github.com/MolotovCherry/virtual-display-rs
- 原样复制的 crate：`wdf-umdf-sys`（UMDF+IddCx bindgen 绑定）、`wdf-umdf`（WDF/IddCx 安全封装）、
  `driver-ipc`（驱动 IPC 协议）、`driver-logger`（事件日志）。
- 改编的 crate：`driver`（vdev 命名/EDID/管道名/INF）、`cli`（vdev 风格子命令 + SetupAPI 安装）。
- 许可文本：见下方 MIT License。

## MIT License
Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
