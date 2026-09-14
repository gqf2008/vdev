"""文档体检：代码围栏配对/缩进、相对链接、以及发布稿里 KMDF 残留。

用法：python scripts/check-docs.py [仓库根目录]
（默认取本文件所在目录的上一级 = 仓库根；返回码非 0 表示有问题）

围栏判定按 CommonMark 的"相对缩进"来算：围栏最多比所在列表项的内容缩进多 3 空格，
再多就会被当成缩进代码块（围栏不生效、整篇文档被吞）——PR #14 的 README 事故就是这一类。
列表里的合法缩进围栏（如 `1. …` 下的 ```rust）不再误报。

跳过 vendored/第三方文档（vendor/**、third_party/**、node_modules/**）：它们的相对链接
是相对上游仓库根的，在本仓库里必然"断链"。
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(sys.argv[1]) if len(sys.argv) > 1 else pathlib.Path(__file__).resolve().parent.parent
FENCE = "`" * 3
SKIP_PARTS = {"vendor", "third_party", "node_modules", "target"}
LIST_ITEM = re.compile(r"^(\s*)([-*+]|\d+[.)])\s+")


def fence_issues(text: str) -> list[str]:
    """返回围栏问题列表（行号从 1 开始）。"""
    issues: list[str] = []
    list_indent = None       # 当前列表项的"内容缩进"
    open_indent = None
    open_line = 0
    for i, ln in enumerate(text.split("\n"), 1):
        if not ln.strip():
            continue
        indent = len(ln) - len(ln.lstrip(" "))
        m = LIST_ITEM.match(ln)
        if m:
            list_indent = len(m.group(1)) + len(m.group(2)) + 1
        elif list_indent is not None and indent < list_indent:
            list_indent = None
        if not ln.lstrip().startswith(FENCE):
            continue
        base = list_indent or 0
        rel = indent - base
        if open_indent is None:
            if rel > 3:
                issues.append(
                    f"第 {i} 行围栏缩进 {indent} 空格（列表内容缩进 {base}）→ 会被当成缩进代码，围栏不生效"
                )
                continue
            open_indent, open_line = indent, i
        elif rel <= 3 and indent <= open_indent:
            open_indent = None
    if open_indent is not None:
        issues.append(f"第 {open_line} 行的围栏未闭合")
    return issues


def main() -> int:
    files = (
        sorted((ROOT / "docs").rglob("*.md"))
        + [ROOT / "README.md"]
        + sorted((ROOT / "crates").rglob("*.md"))
    )
    files = [f for f in files if not (SKIP_PARTS & set(f.relative_to(ROOT).parts))]
    fence_problems: list[str] = []
    link_problems: list[str] = []
    kmdf_in_publish: list[str] = []
    for f in files:
        try:
            t = f.read_text(encoding="utf-8")
        except Exception:
            continue
        rel = f.relative_to(ROOT)
        fence_problems += [f"{rel}: {msg}" for msg in fence_issues(t)]
        for m in re.finditer(r"\[[^\]]*\]\(([^)]+)\)", t):
            link = m.group(1).split("#")[0].strip()
            if not link or link.startswith(("http", "mailto")):
                continue
            if not (f.parent / link).resolve().exists():
                link_problems.append(f"{rel}: {link}")
        if "public" in str(rel) and "publish" in str(rel) and "KMDF" in t:
            kmdf_in_publish.append(str(rel))
    print(f"检查文件数：{len(files)}")
    print("围栏问题：", fence_problems or "无")
    print("断链：", link_problems or "无")
    print("发布稿含 KMDF：", kmdf_in_publish or "无")
    return 1 if (fence_problems or link_problems) else 0


if __name__ == "__main__":
    sys.exit(main())
