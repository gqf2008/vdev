"""配图公共库：扁平色块风格的架构/流程图（Pillow 直绘，中文走 Hiragino Sans GB）。

用法：在同目录的生成脚本里
    import sys, pathlib
    sys.path.insert(0, str(pathlib.Path(__file__).parent))
    from figlib import *

约定：色板见下；正文事实与图必须一致，图随正文一起改。
"""
from PIL import Image, ImageDraw, ImageFont
import pathlib

FONT_PATH = "/System/Library/Fonts/Hiragino Sans GB.ttc"
OUT_DIR = pathlib.Path(__file__).resolve().parents[1]

BG, INK, MUTED, BORDER = "#ffffff", "#1f2329", "#6b7280", "#d0d5dd"
BLUE, BLUE_BG = "#2563eb", "#eef4ff"
GREEN, GREEN_BG = "#15803d", "#ecfdf3"
AMBER, AMBER_BG = "#b45309", "#fff7e6"
GRAY_BG = "#f6f7f9"
RED, RED_BG = "#be123c", "#fff1f2"
PURPLE, PURPLE_BG = "#7c3aed", "#f5f3ff"


# ---------------------------------------------------------------------------
# 字形安全：Hiragino Sans GB 缺 ▶ ◀ ✓ ✕ 等符号，画上去就是豆腐块。
# 每次 canvas() 时校验待绘文本（这里用一条"禁用字符"清单 + 可选白名单断言）。
# ---------------------------------------------------------------------------
_FORBIDDEN = set("\u25b6\u25c0\u2713\u2715\u2717\u26a0")  # ▶ ◀ ✓ ✕ ✗ ⚠

def assert_glyphs(s):
    """待绘字符串若含已知缺字形字符则直接报错。"""
    bad = sorted({c for c in s if c in _FORBIDDEN})
    if bad:
        raise ValueError(
            "配图含缺字形字符 %r（Hiragino Sans GB 无此字形，会渲染成豆腐块）；"
            "请改用 ASCII 或 →/←/↓/▼。" % bad
        )


def font(size, bold=False):
    return ImageFont.truetype(FONT_PATH, size, index=1 if bold else 0)


def canvas(w=1200, h=800):
    img = Image.new("RGB", (w, h), BG)
    return img, ImageDraw.Draw(img)


def rbox(d, xy, fill, outline=BORDER, w=2, r=14):
    d.rounded_rectangle(xy, radius=r, fill=fill, outline=outline, width=w)


def center(d, cx, y, s, size=26, color=INK, bold=False):
    assert_glyphs(s)
    d.text((cx, y), s, font=font(size, bold), fill=color, anchor="mm")


def text(d, x, y, s, size=19, color=INK, bold=False, anchor="la", spacing=8):
    assert_glyphs(s)
    d.multiline_text((x, y), s, font=font(size, bold), fill=color, anchor=anchor, spacing=spacing)


def arrow(d, x1, y1, x2, y2, color=MUTED, w=3, head=9):
    d.line([(x1, y1), (x2, y2)], fill=color, width=w)
    if y1 == y2:
        s = 1 if x2 > x1 else -1
        d.polygon([(x2, y2), (x2 - s * head * 1.6, y2 - head), (x2 - s * head * 1.6, y2 + head)], fill=color)
    else:
        s = 1 if y2 > y1 else -1
        d.polygon([(x2, y2), (x2 - head, y2 - s * head * 1.6), (x2 + head, y2 - s * head * 1.6)], fill=color)


def title(d, w, main, sub=None, y=44):
    center(d, w // 2, y, main, 34, INK, True)
    if sub:
        center(d, w // 2, y + 42, sub, 20, MUTED)


def card(d, x, y, w, h, heading, body, bg=GRAY_BG, oc=BORDER, hs=24, bs=18, pad=28, hcolor=INK):
    rbox(d, (x, y, x + w, y + h), bg, oc, 2)
    d.text((x + pad, y + 22), heading, font=font(hs, True), fill=hcolor, anchor="la")
    if body:
        d.multiline_text((x + pad, y + 22 + hs + 14), body, font=font(bs), fill=INK, anchor="la", spacing=8)


def steps(d, x, y, w, h, gap, items, colors=None):
    for i, (t, sub) in enumerate(items):
        bg, oc = (colors[i] if colors else (BLUE_BG, BLUE))
        yy = y + i * (h + gap)
        rbox(d, (x, yy, x + w, yy + h), bg, oc, 2)
        d.text((x + 26, yy + 18), t, font=font(23, True), fill=INK, anchor="la")
        if sub:
            d.text((x + 26, yy + 50), sub, font=font(17), fill=MUTED, anchor="la")
        if i < len(items) - 1:
            arrow(d, x + w // 2, yy + h, x + w // 2, yy + h + gap)
    return y + len(items) * (h + gap) - gap


def save(img, name):
    p = OUT_DIR / name
    img.quantize(colors=64, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE).save(p, optimize=True)
    print("  %s  %.0f KB" % (name, p.stat().st_size / 1024))

def wrap(s, size, max_w, bold=False):
    """按渲染宽度折行。中文可逐字断行；英文单词不拆，但超长单词（路径等）按字符断。"""
    f = font(size, bold)

    def emit(para):
        out, cur = [], ""
        for ch in para:
            if f.getlength(cur + ch) <= max_w:
                cur += ch
                continue
            if cur:
                out.append(cur)
                cur = ""
            # 单字符已超宽（几乎不会发生）→ 直接放
            if f.getlength(ch) > max_w:
                out.append(ch)
            else:
                cur = ch
        out.append(cur)
        return out

    lines = []
    for para in s.split("\n"):
        if not para:
            lines.append("")
            continue
        lines.extend(emit(para))
    return "\n".join(l.rstrip() for l in lines)


def card_fit(d, x, y, w, h, heading, body, bg=GRAY_BG, oc=BORDER, hs=24, bs=18, pad=28, hcolor=INK):
    """同 card()，但正文按卡片内宽自动折行，避免溢出。"""
    rbox(d, (x, y, x + w, y + h), bg, oc, 2)
    d.text((x + pad, y + 22), heading, font=font(hs, True), fill=hcolor, anchor="la")
    if body:
        wrapped = wrap(body, bs, w - 2 * pad)
        assert_glyphs(wrapped)
        d.multiline_text((x + pad, y + 22 + hs + 14), wrapped, font=font(bs), fill=INK,
                         anchor="la", spacing=8)
