# 第三方组件与许可证

## RNNoise（降噪后端）

`vdev-mic-agent` 在运行时通过 `dlopen`/`LoadLibrary` 加载 `librnnoise`，**不静态链接**，
但发布包会随附其二进制，因此必须随附许可证与版权声明（BSD-3-Clause 第 2 条要求二进制再分发
复现版权声明、条件与免责声明）。

| 平台 | 来源 | 固定值 |
|---|---|---|
| macOS | xiph/rnnoise 源码构建，commit `70f1d256acd4b34a572f999a05c87bf00b67730d`（tag v0.2 在 arm64 编不过：`vec_neon.h` include 了仓库中不存在的 `os_support.h`） | 模型 `rnnoise_data-0a8755f8e2d834eff6a54714ecc7d75f9932e845df35f8b59bc52a7cfe6e8b37.tar.gz`，sha256 同名，58,603,099 B |
| Windows | MSYS2 `mingw-w64-ucrt-x86_64-rnnoise-0.2-2-any.pkg.tar.zst`（上游 v0.2） | 包 sha256 `6892ed69c436e9d5f006df8b3fcf7efb2f57d556338db545eb8e2d6e55a820cf`；`librnnoise-0.dll` sha256 `860d319b2e45e68c66b0d3eed680dd8c7a0887d4e1854c734202a20083ee56b7` |

许可证：**BSD-3-Clause**。原文随包分发：

- macOS 包：`licenses/rnnoise-COPYING.txt`（构建时从上游 `COPYING` 原样拷出）
- Windows 包：`librnnoise-LICENSE.txt`（从 MSYS2 包的 `share/licenses/rnnoise/LICENSE` 拷出）

### 许可证原文

```
Copyright (c) 2007-2017, 2024 Jean-Marc Valin
Copyright (c) 2023 Amazon
Copyright (c) 2017, Mozilla
Copyright (c) 2005-2017, Xiph.Org Foundation
Copyright (c) 2003-2004, Mark Borgerding

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions
are met:

- Redistributions of source code must retain the above copyright
notice, this list of conditions and the following disclaimer.

- Redistributions in binary form must reproduce the above copyright
notice, this list of conditions and the following disclaimer in the
documentation and/or other materials provided with the distribution.

- Neither the name of the Xiph.Org Foundation nor the names of its
contributors may be used to endorse or promote products derived from
this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
``AS IS'' AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED.  IN NO EVENT SHALL THE FOUNDATION
OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```
