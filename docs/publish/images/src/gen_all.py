#!/usr/bin/env python3
"""为社区系列其余 8 篇生成配图。事实取自对应正文，图随正文一起改。"""
import sys, pathlib
sys.path.insert(0, str(pathlib.Path(__file__).parent))
from figlib import *

# ---------------------------------------------------------------- macOS 摄像头
def macos_camera():
    W, H = 1200, 820
    img, d = canvas(W, H)
    title(d, W, "macOS 虚拟摄像头：四个角色与两条通道", "宿主 App 负责激活，扩展进程负责出帧，cmiod 对系统暴露设备")

    # 三个角色
    card(d, 70, 130, 330, 200, "宿主 App（VDCamera.app）",
         "Rust + Slint，沙盒\n· OSSystemExtensionRequest\n  激活 / 停用扩展\n· usage description",
         BLUE_BG, BLUE)
    card(d, 435, 130, 330, 200, "扩展进程（vdev-camera-ext）",
         "沙盒，100% Rust\n· CMIOExtensionProvider 单例\n· device.addStream 先于 addDevice\n· 帧管线 + 设备侧滤镜",
         GREEN_BG, GREEN)
    card(d, 800, 130, 330, 200, "cmiod（系统守护进程）",
         "· 记住扩展的「坏状态」\n· 把设备暴露给 AVFoundation\n· 扩展崩溃后可能不再拉起",
         AMBER_BG, AMBER)

    # 两条通道
    rbox(d, (70, 380, 570, 600), BLUE_BG, BLUE, 2)
    d.text((98, 402), "通道一：激活链路", font=font(23, True), fill=INK, anchor="la")
    d.text((98, 446), "宿主 — OSSystemExtensionRequest → sysextd\n"
                      "        │  四道校验：bundle 名 / usage desc /\n"
                      "        │  application-groups / Mach service 前缀\n"
                      "        ▼\n"
                      "      系统设置里用户手动批准 → 扩展进程启动",
           font=font(17), fill=INK, anchor="la", spacing=9)

    rbox(d, (630, 380, 1130, 600), GREEN_BG, GREEN, 2)
    d.text((658, 402), "通道二：帧链路", font=font(23, True), fill=INK, anchor="la")
    d.text((658, 446), "外部推流端 — TCP 127.0.0.1:27890 → 扩展\n"
                       "        36 字节头(VDFR) + BGRA32 整帧\n"
                       "              │  超 2s 无新帧\n"
                       "              ▼\n"
                       "        CVPixelBuffer → CMSampleBuffer → send",
           font=font(17), fill=INK, anchor="la", spacing=9)

    # 铁律
    rbox(d, (70, 630, 1130, 760), RED_BG, RED, 2)
    d.text((98, 652), "两条铁律（都是踩出来的）", font=font(22, True), fill=RED, anchor="la")
    d.text((98, 692), "① 扩展代码不变就别升扩展版本号——升级会触发 launchd 替换竞态，扩展显示已启用但进程不启动。\n"
                      "② 扩展进程一旦异常退出，cmiod 会记住这个 bundle id 的坏状态，之后换版本号甚至换 id 都不再拉起。",
           font=font(18), fill=INK, anchor="la", spacing=10)
    save(img, "macos-camera-01-architecture.png")


# ---------------------------------------------------------------- macOS 声卡
def macos_audio():
    W, H = 1200, 820
    img, d = canvas(W, H)
    title(d, W, "CoreAudio HAL 插件：vdev-audio 的核心机制", "两台虚拟设备，各自输出环回输入；时钟与位置全靠自己演")

    steps(d, 70, 140, 470, 88, 30, [
        ("coreaudiod 加载 HAL 插件", "产物必须是 MH_BUNDLE + Developer ID 签名"),
        ("宿主 App 写输出流", "kAudioServerPlugInIOOperationWriteMix ('rite')"),
        ("环形缓冲按 sample time 定位", "不是 FIFO——按播放位置写入 / 读取"),
        ("会议软件读输入流", "从同一缓冲读出，得到环回音频"),
    ], [(BLUE_BG, BLUE)]*4)

    card(d, 610, 140, 520, 250, "GetZeroTimeStamp：不能只累加",
         "BlackHole 同款：锚定 → 环缓冲量化 → 追赶推进\n\n"
         "· host time 用 mach_absolute_time()（ticks）\n"
         "· sample time 用 timebase 换算\n"
         "· 用纳秒会被判为时钟异常，IO 跑几个周期就停\n"
         "· 每拍 16384 帧量化", GRAY_BG, BORDER)

    card(d, 610, 420, 520, 250, "StartIO / StopIO 与 DSP",
         "· 客户端计数：start 累加、stop 饱和递减（不回绕 u32）\n"
         "· 两台设备各持一份 DSP 与 scratch\n"
         "  （曾因全局单例跨设备打架）\n"
         "· DSP：三段 EQ + 总增益 + tanh 软限幅\n"
         "  由自定义属性 'vdsp' 控制",
         PURPLE_BG, PURPLE)

    rbox(d, (70, 690, 1130, 790), AMBER_BG, AMBER, 2)
    d.text((98, 708), "vtable 上的三个反直觉点", font=font(21, True), fill=AMBER, anchor="la")
    d.text((98, 742),
           "driver ref 要传 &interface_ptr（指针的指针）；QueryInterface 的 REFIID\n"
           "是 CFUUIDBytes 按值传 x1:x2；数组属性 inDataSize 不足要截断返回，而非报错",
           font=font(17), fill=INK, anchor="la", spacing=7)
    save(img, "macos-audio-01-core.png")


# ---------------------------------------------------------------- macOS 键鼠
def macos_hid():
    W, H = 1200, 760
    img, d = canvas(W, H)
    title(d, W, "macOS 虚拟键鼠：事件注入与监听", "注入无需授权，监听是 TCC 管制的敏感能力")

    card(d, 70, 140, 500, 230, "注入：CGEventPost（无需授权）",
         "type / key / move / click / scroll / down / up\n\n"
         "· 合成事件与物理事件在字段上结构完全相同，下游难以区分\n"
         "· CGEventPost 返回 void——发出去之后有没有 App 接收，\n"
         "  调用方一概不知", BLUE_BG, BLUE)

    card(d, 630, 140, 500, 230, "监听：CGEventTap（需「辅助功能」）",
         "CGPreflightListenEventAccess() 预检\n"
         "→ 失败则 CGRequestListenEventAccess() 并报错退出\n\n"
         "· 无权限必须返回 Err（非零退出码），不能只打印错误\n"
         "· tap 的 run loop 归属是最大陷阱", AMBER_BG, AMBER)

    rbox(d, (70, 410, 1130, 690), GRAY_BG, BORDER, 2)
    d.text((98, 432), "监听层级：挂在哪一层决定你能看到什么", font=font(23, True), fill=INK, anchor="la")
    d.text((98, 486),
           "kCGHIDEventTap      硬件层，最早看到事件（含驱动注入的事件）\n"
           "        │\n"
           "kCGSessionEventTap  登录会话层，同一用户会话内的事件流；事件尚未被打上「合成」注记\n"
           "        │\n"
           "        ▼   vdev 挂在这里：既能看物理输入，也能看自己注入的事件（自收验证）",
           font=font(18), fill=INK, anchor="la", spacing=10)
    d.text((98, 626), "掩码用 CG_EVENT_MASK_FOR_ALL_EVENTS 收全部事件类型；权限预检走错误路径是硬要求。",
           font=font(17), fill=MUTED, anchor="la")
    save(img, "macos-hid-01-events.png")


# ---------------------------------------------------------------- macOS 虚拟屏
def macos_display():
    W, H = 1200, 760
    img, d = canvas(W, H)
    title(d, W, "CGVirtualDisplay：四步造一块假屏", "除了创建这一步走私有入口，其余交互全部用公开 API")

    cards = [
        (70, "① mode", "CGVirtualDisplayMode\n宽 × 高 × 刷新率"),
        (330, "② descriptor", "CGVirtualDisplayDescriptor\n身份 + 物理尺寸 + 色域"),
        (590, "③ settings", "CGVirtualDisplaySettings\n模式列表 + HiDPI"),
        (850, "④ display", "CGVirtualDisplay\ninitWithDescriptor\n→ applySettings"),
    ]
    for x, head, body in cards:
        card(d, x, 150, 240, 170, head, body, BLUE_BG, BLUE, hs=22, bs=17)
    for x in (310, 570, 830):
        arrow(d, x, 235, x + 20, 235)

    card(d, 70, 360, 520, 190, "私有面：只有创建瞬间",
         "类型编码 / selector 存在性都要显式假设\n"
         "· 查不到类 → 直接报错，不静默降级\n"
         "· setRotation: 可能不存在 → respondsToSelector 先探测\n"
         "· ABI 宽度以证据为准（不赌调用约定）", RED_BG, RED)

    card(d, 630, 360, 500, 190, "公开面：创建之后",
         "CGGetOnlineDisplayList        枚举到它\n"
         "CGConfigureDisplayMirror...   让物理屏镜像它\n"
         "CGDisplayStream / SCK         直接当采集源",
         GREEN_BG, GREEN)

    rbox(d, (70, 580, 1130, 700), AMBER_BG, AMBER, 2)
    d.text((98, 602), "两个已知不确定", font=font(22, True), fill=AMBER, anchor="la")
    d.text((98, 640), "· 无互斥：CLI 与宿主 App 各自创建，同时建第二块可能失败、也可能得到两块\n"
                      "· 无稳定性承诺：私有 API 随大版本可能改名 / 改签名 / 移除，实测基线 macOS 26.5",
           font=font(18), fill=INK, anchor="la", spacing=9)
    save(img, "macos-display-01-create.png")




# ---------------------------------------------------------------- Windows 摄像头
def windows_camera():
    W, H = 1200, 800
    img, d = canvas(W, H)
    title(d, W, "DirectShow 虚拟摄像头：两个进程 + 一块共享内存",
          "用户态 COM 组件，免签名；推流与取流是两个独立进程")

    card(d, 70, 140, 500, 210, "推流端（任意进程）",
         "vdev-camera-win.exe push --width --height --fps\n"
         "· 把 BGRA 帧写进命名共享内存\n"
         "· 双缓冲 + 序号，无锁\n"
         "· 尺寸不一致时自动最近邻缩放", BLUE_BG, BLUE)
    card(d, 630, 140, 500, 210, "消费端（任意 App 的图里）",
         "ffmpeg / OBS / Zoom / Teams / 微信\n"
         "· 经 DirectShow 图拿到捕获源\n"
         "· 固定输出 YUY2，三档分辨率 @30fps\n"
         "· 无新帧回退棋盘格", GREEN_BG, GREEN)
    arrow(d, 580, 245, 620, 245)

    rbox(d, (70, 380, 1130, 620), GRAY_BG, BORDER, 2)
    d.text((98, 402), "COM 侧：四个标准导出 + 一个类工厂", font=font(23, True), fill=INK, anchor="la")
    d.text((98, 452),
           "DllGetClassObject → IClassFactory → CreateInstance 出过滤器对象\n"
           "DllCanUnloadNow 恒返回 S_FALSE（拒绝卸载，避免 DLL 在使用中被拔掉）\n"
           "所有线程入口用 ComInit RAII 守卫初始化 COM；宿主已以别的模式初始化过时复用而不卸载\n"
           "输出 pin 必须实现 IKsPropertySet 返回 PIN_CATEGORY_CAPTURE（否则 ffmpeg 报找不到输出 pin）",
           font=font(18), fill=INK, anchor="la", spacing=10)

    rbox(d, (70, 650, 1130, 760), AMBER_BG, AMBER, 2)
    d.text((98, 668), "最容易翻车的三处（都已修）", font=font(21, True), fill=AMBER, anchor="la")
    d.text((98, 702),
           "Instance 键必须带 FriendlyName（否则枚举不到，但 CoCreateInstance 能成功）　·　样本时间戳用流时间域 SetTime\n"
           "（从 0 起 + SetSyncPoint，不设媒体时间）　·　用 YUY2 别用 RGB32（VLC 无法提取 fourcc）",
           font=font(17), fill=INK, anchor="la", spacing=8)
    save(img, "windows-camera-01-architecture.png")


# ---------------------------------------------------------------- Windows 显示器
def windows_display():
    W, H = 1200, 820
    img, d = canvas(W, H)
    title(d, W, "IddCx UMDF 虚拟显示器：三层对象与一个上下文",
          "驱动是 DLL，跑在 WUDFHost 进程里；崩溃不影响内核")

    steps(d, 70, 140, 470, 84, 28, [
        ("IDDCX_ADAPTER", "由 EvtIddCxDeviceInitConfig 初始化，代表「适配器」"),
        ("IDDCX_MONITOR", "EvtIddCxMonitor* 回调；解析 EDID 报告支持的模式"),
        ("IDDCX_SWAPCHAIN", "EvtIddCxMonitorAssignSwapChain 起专用线程处理"),
    ], [(BLUE_BG, BLUE), (GREEN_BG, GREEN), (AMBER_BG, AMBER)])

    card(d, 610, 140, 520, 300, "上下文生命周期：一份 Arc，两种引用",
         "WDF_DECLARE_CONTEXT_TYPE! 生成上下文类型\n\n"
         "内部是 Arc<RwLock<T>>：\n"
         "· 设备对象 init 时存 Strong\n"
         "· 显示器 / 适配器对象 clone_into 存 Weak\n\n"
         "→ 整个上下文只有一份堆分配；框架销毁设备对象时\n"
         "   drop 掉那个 Strong，其余引用全部失效。", PURPLE_BG, PURPLE)

    card(d, 610, 470, 520, 210, "Swap chain 处理线程",
         "· 先给线程挂 MMCSS（Distribution）优先级\n"
         "· IddCxSwapChainReleaseAndAcquireBuffer 取帧\n"
         "· E_PENDING 时 WaitForSingleObject 最多 16 ms\n"
         "· 归还后 FinishedProcessingFrame\n"
         "· 当前是直通，画面注入是留给推流的扩展点", GRAY_BG, BORDER)

    rbox(d, (70, 660, 1130, 780), RED_BG, RED, 2)
    d.text((98, 678), "与 macOS 版的语义差异", font=font(21, True), fill=RED, anchor="la")
    d.text((98, 712),
           "macOS 由宿主 App 往虚拟屏里推内容；Windows 是 OS 直接渲染进虚拟屏。\n"
           "绑定的权威在 WDK 头文件——用 bindgen 直取，不再手写 ABI（同仓声卡的手写绑定踩过整套坑）。",
           font=font(17), fill=INK, anchor="la", spacing=8)
    save(img, "windows-display-01-objects.png")


# ---------------------------------------------------------------- Windows HID
def windows_hid():
    W, H = 1200, 800
    img, d = canvas(W, H)
    title(d, W, "KMDF HID minidriver：三重契约", "编译器看不见的那部分，才是内核驱动的正确性大头")

    cards = [
        (70, "① INF 接线", BLUE_BG, BLUE,
         "Include/Needs 把 hidclass 与\nmshidkmdf 接进驱动栈\n\nAddService flag 0 让关联\n服务唯一\n\nAddFilter + FilterPosition=\nLower 挂对层级\n\n接错了，代码再对也是死代码"),
        (435, "② IOCTL 契约", GREEN_BG, GREEN,
         "初版自造功能码，真实 hidport.h\n编号是 0/1/2/3/4/7/8/9/10\n（5/6 保留）\n\n4/5 恰好互换、6–9 段不存在\n→ HidD_GetAttributes 必失败\n\nfeature/报表类还在另一个头文件"), 
        (800, "③ 结构布局", AMBER_BG, AMBER,
         "_HID_DESCRIPTOR 被 pshpack1.h\n包裹（1 字节对齐）\n\nRust 侧初版用 #[repr(C)]\n自然对齐，偏移 7 处插了填充，\nsize_of 变 10\n\nhidclass 错位读，协商直接崩"),
    ]
    for x, head, bg, oc, body in cards:
        card(d, x, 140, 330, 340, head, body, bg, oc, hs=23, bs=16)

    rbox(d, (70, 500, 1130, 620), GRAY_BG, BORDER, 2)
    d.text((98, 518), "两个 crate：CLI + 驱动", font=font(22, True), fill=INK, anchor="la")
    d.text((98, 556),
           "用户态 CLI vdev-hid-win（SetupAPI 装驱动、注入命令）\n"
           "内核驱动 vdev-hid-driver（独立 workspace，绑由 bindgen 生成的 vendored wdk-sys）\n"
           "两侧经 #[path] 共享同一份契约定义（contract.rs / report.rs）——单一事实来源",
           font=font(18), fill=INK, anchor="la", spacing=9)

    rbox(d, (70, 650, 1130, 760), GREEN_BG, GREEN, 2)
    d.text((98, 668), "宿主可测的部分", font=font(21, True), fill=GREEN, anchor="la")
    d.text((98, 702),
           "纯逻辑层 windows-free：键名→HID usage、8 字节报告布局、IOCTL 常量、描述符字节，15 个单测在 macOS 上直接跑。\n"
           "数字键 '0' 是 0x27 而非 0x1E+(c-'0')——初版整体偏移 +1，还被一个「把错误当期望值」的单测固化了。",
           font=font(17), fill=INK, anchor="la", spacing=8)
    save(img, "windows-hid-01-contracts.png")


def _gen_windows():
    print("生成中（Windows）：")
    windows_camera(); windows_display(); windows_hid()




# ---------------------------------------------------------------- AI 虚拟麦克风
def ai_virtual_mic():
    from figlib import canvas as _cv
    W, H = 1200, 820
    img, d = _cv(W, H)
    title(d, W, "端侧 AI 麦克风：一条全程本地的降噪链路",
          "物理麦克风 → 端侧降噪 → 注入虚拟麦克风端点；全程本地推理，不上传一个字节")

    steps(d, 70, 140, 620, 82, 26, [
        ("物理麦克风（48 kHz）", "任意会议软件的输入源"),
        ("FrameAssembler：重分帧到 480 样本", "设备块长任意，模型只认 480"),
        ("RNNoise 降噪（libloading 运行时加载）", "单流状态仅 32 688 B"),
        ("自适应干湿混合（VAD 门控 + 迟滞闸门）", "干净麦克风绕过模型，不被模型伤害"),
        ("虚拟麦克风端点（vdev-audio / vdev-audio-win）", "会议软件把它当选麦克风"),
    ], [(BLUE_BG, BLUE), (BLUE_BG, BLUE), (GREEN_BG, GREEN), (AMBER_BG, AMBER), (GREEN_BG, GREEN)])

    card_fit(d, 730, 140, 400, 300, "为什么干净麦克风要绕过模型",
         "RNNoise 对每一帧都做衰减，包括本来就很干净的帧。\n\n"
         "把一段干净录音全湿推过去，SI-SDR 反而从 329 dB 掉到 14.7 dB——"
         "听感上是淡淡的金属染色，收益为零。\n\n"
         "所以加一条闸门：噪声底与语音电平两个慢估计器算出长期 SNR，"
         "高于闸门就旁路模型，低于闸门才全湿。",
         RED_BG, RED, bs=17)

    card_fit(d, 730, 460, 400, 250, "延迟怎么测：不靠加总和",
         "两条探针把标记埋进音频，用归一化互相关（NCC）在整个链路里找它：\n\n"
         "· 数字探针：注进虚拟输出流，在输入流里找（不含模型）\n"
         "· 声学探针：物理扬声器播 chirp，房间 + 物理麦当信道\n\n"
         "阈值：数字 0.60、声学 0.35。",
         PURPLE_BG, PURPLE, bs=17)

    rbox(d, (70, 700, 1130, 790), GRAY_BG, BORDER, 2)
    d.text((98, 716), "实测口径（i7-11700 / Win10，48 kHz，480 样本帧）", font=font(21, True), fill=INK, anchor="la")
    d.multiline_text((98, 748), wrap(
        "单帧 0.078 ms（p99 0.135 ms，占 10 ms 预算 1.4%）　·　实时 CPU 0.78% 单核\n"
        "算法延迟 960 样本 = 20.0 ms　·　与 C/Python 参考实现 1 LSB 内一致", 17, 1032),
        font=font(17), fill=INK, anchor="la", spacing=7)
    save(img, "macos-mic-01-chain.png")


if __name__ == "__main__":
    print("生成中：")
    macos_camera(); macos_audio(); macos_hid(); macos_display()
    _gen_windows()
    ai_virtual_mic()
