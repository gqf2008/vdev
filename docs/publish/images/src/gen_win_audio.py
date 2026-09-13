#!/usr/bin/env python3
"""生成 windows-virtual-audio 篇的配图（Pillow 直绘，中文字体）。"""
from PIL import Image, ImageDraw, ImageFont
import pathlib

FONT = "/System/Library/Fonts/Hiragino Sans GB.ttc"
OUT = pathlib.Path(__file__).resolve().parents[1]

def f(sz, bold=False):
    # Hiragino Sans GB.ttc: index 0=W3, 1=W6
    return ImageFont.truetype(FONT, sz, index=1 if bold else 0)

BG, INK, MUTED = "#ffffff", "#1f2329", "#6b7280"
BLUE, BLUE_BG = "#2563eb", "#eef4ff"
GREEN, GREEN_BG = "#15803d", "#ecfdf3"
GRAY_BG, BORDER = "#f6f7f9", "#d0d5dd"
AMBER_BG, AMBER = "#fff7e6", "#b45309"

def rbox(d, xy, r, fill, outline=BORDER, w=2):
    d.rounded_rectangle(xy, radius=r, fill=fill, outline=outline, width=w)

def arrow(d, x1, y1, x2, y2, color=MUTED, w=3, head=9):
    d.line([(x1, y1), (x2, y2)], fill=color, width=w)
    if y1 == y2:      # 水平
        s = 1 if x2 > x1 else -1
        d.polygon([(x2, y2), (x2 - s*head*1.6, y2 - head), (x2 - s*head*1.6, y2 + head)], fill=color)
    else:             # 垂直
        s = 1 if y2 > y1 else -1
        d.polygon([(x2, y2), (x2 - head, y2 - s*head*1.6), (x2 + head, y2 - s*head*1.6)], fill=color)

def center(d, cx, y, text, size=26, color=INK, bold=False):
    d.text((cx, y), text, font=f(size, bold), fill=color, anchor="mm")

# ---------- 图 1：三条路线 ----------
W, H = 1200, 760
img = Image.new("RGB", (W, H), BG); d = ImageDraw.Draw(img)
center(d, W//2, 46, "Windows 虚拟声卡：三条路线的取舍", 34, INK, True)
center(d, W//2, 88, "目标：把系统播放环回成麦克风输入，供会议 / 录制软件抓取", 20, MUTED)

routes = [
    ("1  用户态虚拟设备", "Windows 音频没有此选项", "摄像头有 DirectShow、显示器有\nIddCx UMDF，唯独声卡与内核 HID\n没有官方用户态捷径", "X  走不通", GRAY_BG, MUTED),
    ("2  AVStream 通用流框架", "什么媒体类型都能做", "但音频的 KS 语义、时钟、DMA\n要全部自己拼，胶水代码量反而\n比 PortCls 更大", "-  更费力", AMBER_BG, AMBER),
    ("3  PortCls / WaveRT   ← 本文选择", "微软为音频定制的端口类模型", "系统自带 portcls.sys 负责 KS 自动化、\n格式协商、位置跟踪；驱动只写 miniport\n回答“我有什么 pin / 格式 / 缓冲在哪”", "本文选择 · 现代 WDM 音频事实标准", GREEN_BG, GREEN),
]
y0, bh, gap = 130, 180, 18
for i, (title, sub, body, tag, bg, tagc) in enumerate(routes):
    y = y0 + i*(bh+gap)
    rbox(d, (60, y, W-60, y+bh), 16, bg, tagc if i == 2 else BORDER, 3 if i == 2 else 2)
    d.text((92, y+24), title, font=f(26, True), fill=INK, anchor="la")
    d.text((92, y+62), sub, font=f(20), fill=MUTED, anchor="la")
    d.multiline_text((92, y+96), body, font=f(19), fill=INK, anchor="la", spacing=8)
    d.text((W-92, y+bh-38), tag, font=f(21, True), fill=tagc, anchor="ra")
center(d, W//2, y0+3*(bh+gap)+20, "内核路线的代价：一次野指针 / 池越界 / IRQL 误判 = BSOD，不是段错误", 21, "#b91c1c", True)
img.quantize(colors=64, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(OUT/"win-audio-01-routes.png", optimize=True)

# ---------- 图 2：数据流 + PortCls 分工 ----------
W, H = 1200, 980
img = Image.new("RGB", (W, H), BG); d = ImageDraw.Draw(img)
center(d, W//2, 44, "PortCls / WaveRT 虚拟声卡：输出环回输入", 34, INK, True)
center(d, W//2, 86, "扬声器（render）写入，麦克风（capture）读出，共享同一块内核环形缓冲", 20, MUTED)

LX, LW = 90, 620          # 左列：数据流
RX, RW = 760, 350         # 右列：PortCls 负责什么

steps = [
    ("会议 / 播放软件", "把音频写到「vdev 扬声器」", BLUE_BG, BLUE),
    ("KS Filter · WaveRender-0", "render pin（SINK）", BLUE_BG, BLUE),
    ("内核环形缓冲 · 1 MB 非分页池", "SPSC：read / write / count 三个原子索引", AMBER_BG, AMBER),
    ("KS Filter · WaveCapture-0", "capture pin（SOURCE）", GREEN_BG, GREEN),
    ("会议软件选「vdev 麦克风」", "从同一块缓冲读出降噪/环回后的音频", GREEN_BG, GREEN),
]
sy, sh, sg = 130, 96, 26
for i, (title, sub, bg, oc) in enumerate(steps):
    y = sy + i*(sh+sg)
    rbox(d, (LX, y, LX+LW, y+sh), 14, bg, oc, 2)
    d.text((LX+28, y+22), title, font=f(24, True), fill=INK, anchor="la")
    d.text((LX+28, y+56), sub, font=f(18), fill=MUTED, anchor="la")
    if i < len(steps)-1:
        arrow(d, LX+LW//2, y+sh, LX+LW//2, y+sh+sg, MUTED, 3)

# 右侧：PortCls 的职责
rbox(d, (RX, 130, RX+RW, 640), 16, GRAY_BG, BORDER, 2)
d.text((RX+28, 152), "系统侧（portcls.sys）负责", font=f(23, True), fill=INK, anchor="la")
duties = ["KS 自动化与过滤器注册", "音频格式协商", "位置跟踪与时钟", "DMA 缓冲映射给 AudioEng",
          "PnP / 电源管理", "IRP 分发与同步"]
for i, t in enumerate(duties):
    yy = 204 + i*52
    d.ellipse((RX+30, yy+8, RX+42, yy+20), fill=BLUE)
    d.text((RX+58, yy), t, font=f(19), fill=INK, anchor="la")
d.text((RX+28, 536), "驱动侧（我们写的 miniport）", font=f(23, True), fill=INK, anchor="la")
d.text((RX+28, 578), "GetDescription · AllocateAudioBuffer\nGetPosition · 格式交集", font=f(19), fill=MUTED, anchor="la", spacing=8)

# 底部：关键契约
rbox(d, (LX, 760, LX+LW, 856), 14, "#fff1f2", "#e11d48", 2)
d.text((LX+24, 782), "最容易违反的契约", font=f(21, True), fill="#be123c", anchor="la")
d.text((LX+24, 818), "GetPosition 必须返回环形缓冲内偏移（对 dma_size 取模），不能是累计字节数", font=f(18), fill=INK, anchor="la")
img.quantize(colors=64, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(OUT/"win-audio-02-datapath.png", optimize=True)
print("生成:", OUT/"win-audio-01-routes.png", OUT/"win-audio-02-datapath.png")
