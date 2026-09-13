> 本文是 [vdev](https://github.com/gqf2008/vdev) 虚拟设备驱动开发系列之一。全套含 macOS 摄像头/声卡/键鼠/虚拟屏与 Windows 摄像头/显示器/声卡/HID 九篇。

# 在 macOS 上造一块"假显示器"：CGVirtualDisplay 私有 API 的 Rust 封装实录

> 对应仓库：[`crates/vdev-screen`](https://github.com/gqf2008/vdev/tree/main/crates/vdev-screen)（约 400 行）。文中所有代码片段与 file:line 均来自 main 分支当前状态（commit `2385ebe`），所有命令与根 README 逐字一致。
> 代码引用约定：本文所有 `文件:行号` 均相对**仓库根**（如 `crates/.../foo.rs:12`），行号为写作时基线；代码演进后行号会漂移，按符号名搜索为准。

## 一、引言：为什么想造一块假显示器

虚拟显示器是一块"操作系统认为存在、但没有物理面板"的屏幕。它值得做的场景比想象中多：

- **无头渲染**：Mac mini 当构建机/渲染节点用，没插显示器，但某些软件（浏览器、DCC 工具链、UI 自动化）必须有桌面才能跑；
- **远控扩展屏**：把一台 Mac 的"第二块屏"推流到 iPad 或另一台电脑，分辨率由你定义，不受物理屏限制；
- **投屏/推流画布**：虚拟屏是一块系统级画布——往上面画任何内容（视频、幻灯片、远端桌面），再用采集器当作推流源，下游拿到的是一条干净的显示流；
- **给 AI Agent 当屏幕**：让依赖窗口系统的自动化流程在独立分辨率的"沙箱屏"里跑，不干扰主屏。

Windows 上有官方路线 IddCx（间接显示驱动，系列另一篇写过）。macOS 呢？把三条历史路线排一下，答案会很清楚：

| 路线 | 现状 |
|---|---|
| kext（`IOFramebuffer` 子类） | macOS 10.13 起失效，Big Sur 之后第三方 kext 基本死亡 |
| DriverKit（dext） | 从未开放显示类 family——没有 `IOUserDisplay`，只覆盖 USB/PCI/网络/HID 等 |
| CoreGraphics 私有 API `CGVirtualDisplay` | 无公开文档、无稳定性承诺，但**一直可用** |

第三条就是唯一的用户态路线。BetterDisplay、force-hidpi、node-mac-virtual-display，以及 DisplayLink 这类商业产品的驱动，走的都是它。这就是选型的全部逻辑：**官方没留门，但门没锁**。风险同样明确——私有 API 没有任何跨版本兼容承诺，符号可能随大版本漂移，所以本项目把它封装成独立的小 crate（`vdev-screen`），把"可能变化的部分"隔离在 400 行里。

## 二、CGVirtualDisplay 最小知识

这套 API 由四个 ObjC 类组成，全部藏在 `CoreGraphics.framework` 里——注意，framework 二进制里有这些类，但公开头文件里没有它们的声明：

- `CGVirtualDisplayDescriptor`：显示器的"身份证"。vendor ID / product ID / 序列号 / 名称 / 物理尺寸（毫米）/ 最大像素边界，外加色域三原色与白点坐标；
- `CGVirtualDisplayMode`：一个显示模式，即 宽 × 高 × 刷新率；
- `CGVirtualDisplaySettings`：一组设置的容器——模式列表 + HiDPI 开关（部分系统版本还有 rotation）；
- `CGVirtualDisplay`：显示器本体。`initWithDescriptor:` 创建，`applySettings:` 生效，之后通过 `displayID` 拿到一块真正的 `CGDirectDisplayID`。

创建流程是严格的四步：**mode → descriptor → settings → display**。创建成功后，这块屏就进入系统正常的显示配置体系：`CGGetOnlineDisplayList` 能枚举到它，可以用 `CGConfigureDisplayMirrorOfDisplay` 让物理屏镜像它，采集 API（`CGDisplayStream` / ScreenCaptureKit）能直接把它当源。换句话说，**除了创建这一步走的是私有入口，其余一切交互都用公开 API**——这也是整套方案工程上可接受的根本原因：不稳定面被压缩到了创建瞬间。

关于它在驱动栈里的位置，按本仓库能验证的事实说：它工作在 CoreGraphics / WindowServer 这一层——你的进程创建的只是一批普通 ObjC 对象，"接显示器"这件事由 WindowServer 完成。至于更底层接的是哪个 framebuffer 用户态组件，仓库代码与所参考资料都没有给出可验证的结论，这里不下断言。

两条必须刻在脑子里的生命周期事实：

1. **进程必须一直持有 `CGVirtualDisplay` 对象**，进程退出，虚拟屏即刻消失；
2. **固定 vendor/product/serial 三元组**，macOS 才能在重启后记住这块屏的排列布局——`CGDirectDisplayID` 本身每次都会变。

## 三、Rust 封装：手写 objc2 消息发送

一个常见的误解是"调私有 API 得先 dlopen"。对 C 符号确实可以 `-Wl,-U,xxx` 直接链接（这些符号就在 framework 里）；但 `CGVirtualDisplay` 是 **ObjC 类**，更自然的做法是走 ObjC 运行时按名字查类、直接发消息。`vdev-screen` 用 `objc2` 0.6 手写全部绑定，没有 bindgen，也没有 dlopen：

```rust
// crates/vdev-screen/src/private.rs:14
fn class(name: &str) -> Result<&'static AnyClass> {
    let cname = CString::new(name).expect("class name has no NUL");
    AnyClass::get(&cname).ok_or_else(|| anyhow!("private ObjC class not found: {name}"))
}
```

`AnyClass::get` 查不到类时（比如某天 Apple 改名）返回 `Err` 而不是崩溃——这是私有 API 封装的第一条自保原则：**所有"存在性假设"都要变成显式错误**。

四个类的构造各封装成一个小函数。以最简单的 mode 为例：

```rust
// crates/vdev-screen/src/private.rs:26
pub fn create_mode(width: u32, height: u32, refresh_rate: f64) -> Result<Retained<AnyObject>> {
    let cls = class("CGVirtualDisplayMode")?;
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    let mode: *mut AnyObject = unsafe {
        msg_send![
            alloc,
            initWithWidth: u64::from(width),
            height: u64::from(height),
            refreshRate: refresh_rate
        ]
    };
    unsafe { retained(mode, "CGVirtualDisplayMode init") }
}
```

注意 `u64::from(width)`——这里有个血泪故事，第四节细说。`alloc` 返回 +1 未初始化对象，`init` 返回 +1 对象，`Retained::from_raw` 把它接进 Rust 的所有权体系（init 返回 nil 时报错），此后 ARC 接管释放，不需要手写 `release`。

顶层 API 只有三个函数：`list_displays()`（枚举）、`create(opts)`（创建）、`VirtualDisplay`（句柄）。`create` 把四步串起来：

```rust
// crates/vdev-screen/src/lib.rs:75
pub fn create(opts: CreateOptions) -> Result<VirtualDisplay> {
    let mode = private::create_mode(opts.width, opts.height, opts.refresh_rate)?;
    let descriptor = private::create_descriptor(/* ... */)?;
    let settings = private::create_settings(&mode)?;
    let display = private::create_display(&descriptor, &settings)?;
    let display_id = display.display_id;
    Ok(VirtualDisplay { display_id, _mode: mode, _descriptor: descriptor,
                        _settings: settings, _display: display.obj })
}
```

RAII 的关键在 `VirtualDisplay` 的字段设计（`crates/vdev-screen/src/lib.rs:49`）：**四个 `Retained` 全部保存在结构体里，而不是只留 display 本体**。Drop 时按声明序逆序释放：先释放 display（触发 WindowServer 拆屏），再释放 settings、descriptor、mode——与创建顺序严格互逆。如果只保存 display 而让中间对象提前析构，等于把生命周期赌在 CG 内部是否持有拷贝上，没必要冒这个险。

`create` 的默认参数也值得抄走（`crates/vdev-screen/src/lib.rs:30`）：1920×1080@60，vendor `0x05AC`（Apple）、product `0x1111`、物理尺寸 597×336 mm、maxPixels 3840×2160。物理尺寸不是摆设——系统用它推算 DPI，尺寸瞎填会得到诡异的缩放行为。

## 四、关键实现解析

### 4.1 描述符：身份 + 色域

`create_descriptor` 除了逐项填入 `DescriptorOptions` 的八个字段，还硬编码了 Display P3 色域主色（`crates/vdev-screen/src/private.rs:102`）——红 `(0.680, 0.320)`、绿 `(0.265, 0.690)`、蓝 `(0.150, 0.060)`、白点 `(0.3127, 0.3290)`，与大多数现代 Mac 显示器一致。不填色域某些版本也能工作，但填了可以让"显示器 EDID 信息"看起来像一块正经的 Apple 屏。

### 4.2 模式设置与 HiDPI

`create_settings` 把 mode 装进 `NSArray` 后 `setModes:`，然后 `setHiDPI: 1u32`（`crates/vdev-screen/src/private.rs:139`）。HiDPI 开启后，系统会在 1920×1080 的模式上呈现"Retina 逻辑分辨率"，窗口渲染按 2x 走，采集到的帧更细腻。对推流场景，这就是"虚拟屏出 1080p 高清画面"的开关。

### 4.3 应用设置与拿 ID

```rust
// crates/vdev-screen/src/private.rs:165
pub fn create_display(descriptor: &AnyObject, settings: &AnyObject) -> Result<VirtualDisplay> {
    let cls = class("CGVirtualDisplay")?;
    let alloc: *mut AnyObject = unsafe { msg_send![cls, alloc] };
    let display: *mut AnyObject = unsafe { msg_send![alloc, initWithDescriptor: descriptor] };
    let display = unsafe { retained(display, "CGVirtualDisplay init")? };
    let applied: bool = unsafe { msg_send![&*display, applySettings: settings] };
    if !applied {
        return Err(anyhow!("CGVirtualDisplay applySettings failed"));
    }
    let display_id: u32 = unsafe { msg_send![&*display, displayID] };
    Ok(VirtualDisplay { obj: display, display_id })
}
```

`applySettings:` 返回 `BOOL`，**必须检查**——创建成功但模式被拒是真实会发生的路径。`displayID` 返回的 `u32` 就是 `CGDirectDisplayID`，从这一刻起，这块虚拟屏与物理屏在所有公开 CG API 面前完全平权。

### 4.4 镜像：把画面"落"到物理屏

虚拟屏本身没有面板，想"看见"它有两条路：让物理屏镜像它（演示/扩展屏场景），或者直接采集它（推流场景）。前者用公开 C API：

```rust
// crates/vdev-screen/src/ffi.rs:85
pub fn mirror(source: u32, target: u32) -> Result<()> {
    let mut config: *mut c_void = ptr::null_mut();
    check(unsafe { CGBeginDisplayConfiguration(&raw mut config) })?;
    check(unsafe { CGConfigureDisplayMirrorOfDisplay(config, target, source) })?;
    check(unsafe { CGConfigureDisplayOrigin(config, source, 0, 0) })?;
    check(unsafe { CGCompleteDisplayConfiguration(config, KCG_CONFIGURE_FOR_SESSION) })
}
```

begin/configure/complete 三段式，其中每个 `CGConfigure*` 都返回 `CGError`，**必须逐个检查后才允许 complete**——否则残缺配置会被整体应用出去（`crates/vdev-screen/src/ffi.rs:91` 的注释就是这个教训）。

### 4.5 与采集 API 配合：虚拟屏作为推流源

虚拟屏真正的杀手锏是当推流画布。仓库里有两条已验证的链路：

- **Rust 侧 `CGDisplayStream`**（`crates/vdev-app/src/screen.rs`）：C API + CFRunLoop + block2，把指定 `display_id` 的帧回调成 `IOSurface`，编码后推给虚拟摄像头扩展或网络；
- **Swift 侧 ScreenCaptureKit**（`crates/vdev-camera/tools/push_frames.swift`）：`swift push_frames.swift screen --display <虚拟屏ID> --fps 30`，用 SCStream 采集虚拟屏，把帧注入 CMIOExtension 虚拟摄像头。

组合玩法实测通过（README 有完整命令）：`vdev screen create` 建屏 → push_frames 采集该屏注入虚拟摄像头 → 任何会议软件里就多了一个"正在播放虚拟屏内容的摄像头"，再接 SFU 即可远程串流。端到端链路（虚拟屏 → HEVC 1080p 采集 → SFU → 观看端解码）在 macOS 26.5 上验证无误。

## 五、踩坑实录

以下四个坑全部在本项目开发/审查过程中真实出现并修复。

### 坑 1：私有 API 的 ABI 宽度三连

审查发现三类"恰好能跑"的宽度错误：

1. `initWithWidth:height:refreshRate:` 的 width/height 真实类型是 `NSUInteger`（64 位）。DeskPad 的私有头 dump 是证据来源。传 `u32` 时 x86_64/arm64 的调用约定恰好零扩展高位，于是"一直没炸"——直到有人按规范把参数当 32 位读取。修法就是代码里的 `u64::from(width)`（`crates/vdev-screen/src/private.rs:30` 的注释完整记录了推理）；
2. `setHiDPI:` 的类型编码是 `I`（unsigned int），**不是 `BOOL`**。go-macos/virtualdisplay 项目实测编码为 `"v20@0:8I16"`——凭"布尔开关传 bool"的直觉反而错，`1u32` 才是对的；
3. `CGDisplayIsBuiltin` 等函数的返回值 `boolean_t` 在 MacTypes.h 里是 **4 字节 int**，FFI 声明曾误写为 `u8`，仅因小端读低位恰好可用（`crates/vdev-screen/src/ffi.rs:11` 注释）。

教训浓缩成一句：**手写私有 API 绑定时，逐参数对照运行时类型编码或多份独立 dump 交叉验证，宽度一律按声明传，绝不靠"实践中恰好没炸"兜底**。

### 坑 2：`setRotation:` 这个 selector 可能不存在

`setRotation:` 不在 DeskPad 私有头 dump 的 `CGVirtualDisplaySettings` 属性列表里，多个 dump 之间互相互斥；实参类型也没有权威出处。致命的是 ObjC 的语义：**无条件向不存在 selector 发消息会抛 `NSInvalidArgumentException`，进程直接 abort**——不是返回错误，是崩溃。修法是先探测再调用（`crates/vdev-screen/src/private.rs:148`）：

```rust
let has_rotation: bool =
    unsafe { msg_send![&*settings, respondsToSelector: sel!(setRotation:)] };
if has_rotation {
    let _: () = unsafe { msg_send![&*settings, setRotation: 0u32] };
}
```

macOS 26.5 实测探测为真、调用可用。对私有 API，任何"不确定是否存在"的 selector 都该过这道闸。

### 坑 3：静态画面的 CGDisplayStream 几乎不回调

虚拟屏推流后画面静止（比如停在一张幻灯片上），下游虚拟摄像头开始出彩条。第一版修复在 CGDisplayStream 回调里等 `kCGDisplayStreamFrameStatusIdle` 再重发最后一帧——实测 macOS 26 上**画面完全静止后连 IDLE 都几乎不回调**（静态虚拟屏 145 秒只收到 20 帧），“回调驱动保活”整体失效。最终方案是独立保活线程：每 200ms 检查最后发送时间，超过 500ms 无新帧就重发最后一帧（带新时间戳），实测静态虚拟屏稳定 2fps 出帧（`crates/vdev-app/src/screen.rs:108`，修复记录在 README:107）。

### 坑 4：第二块虚拟屏的不确定性，与一个槽位泄漏

README:94 如实记录：CLI 的 `vdev screen create` 与宿主 App 各自创建虚拟屏、彼此不加锁，**同时创建第二块虚拟屏可能失败、也可能得到两块**——取决于 macOS 版本与当时的系统状态，私有 API 没有给出任何契约。工程上的对策不是加锁假装互斥，而是：App 侧把"同一时刻只有一块"写死在实现里，且二次创建先销毁旧实例（`crates/vdev-app/src/vscreen.rs:15` 的 `destroy()` 前置调用）——审查曾发现直接覆盖槽位指针会泄漏整个旧 `VirtualDisplay`。

顺带一个 CLI 层的小坑：`--width 0` 或 `--refresh NaN` 直达私有 API 只会得到 CG 的笼统报错，其中 NaN 尤其阴险——它同时不满足 `>= 1.0` 和 `< 1.0` 两个比较，必须在入口显式 `is_nan()` 拦截（`crates/vdev-host/src/main.rs:146` 的 `validate_screen_geometry`，尺寸上限 8192、刷新率须为有限值且 ≥ 1）。

## 六、构建与运行

```bash
git clone https://github.com/gqf2008/vdev && cd vdev
cargo build --release

# 虚拟屏幕（私有 API，仅供学习）
vdev screen list
vdev screen create --width 1920 --height 1080 --name vdev-demo
```

`screen list` 输出每块在线屏的 displayID（十六进制）、内外置、分辨率、物理尺寸与 vendor/product。`screen create` 的完整参数（`crates/vdev-host/src/main.rs:67`）：`--width`、`--height`、`--name`、`--refresh`（默认 60）、`--mirror <displayID>`（创建后让某块物理屏镜像虚拟屏）、`--hold <秒>`（默认 10，进程保持存活多久；虚拟屏随进程退出而销毁，Ctrl-C 提前拆除）。创建成功输出：

```text
virtual display created: 0x00120b98 (1920x1080 @ 60Hz)
holding for 10s (Ctrl-C to destroy) ...
```

注意平台语义差异：macOS 由**宿主进程向虚拟屏推内容**（采集/镜像），Windows 的 IddCx 由 **OS 直接渲染进虚拟屏**——同系列两篇文章正好覆盖两端。

## 七、现状与局限

如实交代：

- **无稳定性承诺**：私有 API 不在 Apple 的兼容性契约内，任何大版本都可能改名、改签名或移除。本项目在 macOS 26.5（Apple Silicon）实测可用，其余版本未验证；
- **符号漂移防御很薄**：`AnyClass::get` 查不到类会返回明确错误，`respondsToSelector:` 能挡住 selector 消失，但签名变化（比如参数类型改了）运行时无从感知；
- **单实例假设**：CLI 与 App 无互斥，同时建第二块屏的行为依系统状态而定；
- **无自动测试**：这类代码离开真实 ObjC 运行时与 WindowServer 无法回归，仓库中的单测只覆盖校验函数与错误码路径；
- **仅供学习研究**：不要把它放进要分发给别人的产品里——DisplayLink 们的用法有商业授权与系统演进的双重背景，个人项目仿写前先想清楚；
- **macOS 升级后的第一件事**：跑一遍 `vdev screen list` 和 `vdev screen create` 做冒烟。类还在、selector 还在、`applySettings` 还返回 `YES`，三关都过，基本就能继续用；哪天挂了，通常也是这三关之一先亮红灯。

低成本替代方案也值得一提：HDMI EDID 模拟器（dummy plug，几十块钱的"假显示器"插头）零驱动零代码，很多远控场景它就够了。

## 八、写在最后

这个 crate 的全部代码不到 400 行，但它把 macOS 虚拟屏幕这件事从"逆向 speculate"变成了"照着 dump 写绑定 + 交叉验证 + 显式失败"。私有 API 开发的全部方法论其实就三条：**存在性假设显式化**（查不到类就报错、selector 先探测）、**ABI 宽度以证据为准**（类型编码/dump 交叉验证，不赌调用约定）、**生命周期自己兜底**（RAII 配对创建销毁，进程退出即拆屏）。

如果这篇文档帮你省掉了一次 `NSInvalidArgumentException` 的抓瞎，欢迎到 [vdev 仓库](https://github.com/gqf2008/vdev) 点个 star；系列的其余八篇（macOS 摄像头/声卡/键鼠，Windows 摄像头/显示器/声卡/HID）见 [`docs/community/`](https://github.com/gqf2008/vdev/tree/main/docs/community)。
