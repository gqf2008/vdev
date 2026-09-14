"""在采回的 WAV 里找标记音的**后沿**（tone → silence），算出环回积压。

用法: python ring-delay-analysis.py <wav> <tag> [--expect-tail <秒>]
后沿用"从有声变无声"的第一个 10ms 窗判定：前沿可能被环形缓冲的 drop-oldest 吃掉，
后沿一定落在最新数据里，所以用它算积压最稳。
"""
import math
import pathlib
import struct
import sys


def read_wav16(path: pathlib.Path):
    b = path.read_bytes()
    assert b[:4] == b"RIFF" and b[8:12] == b"WAVE", "不是 WAV"
    pos, fmt, data = 12, None, None
    while pos + 8 <= len(b):
        cid = b[pos : pos + 4]
        size = struct.unpack_from("<I", b, pos + 4)[0]
        body = b[pos + 8 : pos + 8 + size]
        if cid == b"fmt ":
            fmt = struct.unpack_from("<HHIIHH", body, 0)
        elif cid == b"data":
            data = body
        pos += 8 + size + (size & 1)
    assert fmt and data is not None
    return fmt, data


def main() -> int:
    wav = pathlib.Path(sys.argv[1])
    tag = sys.argv[2]
    expect = None
    if "--expect-tail" in sys.argv:
        expect = float(sys.argv[sys.argv.index("--expect-tail") + 1])

    (tagf, ch, rate, _avg, align, bits), data = read_wav16(wav)
    frames = len(data) // align
    peaks = struct.unpack_from(f"<{len(data) // 2}h", data)
    gmax = max(abs(v) for v in peaks) if peaks else 0
    gdb = 20 * math.log10(gmax / 32768) if gmax else -100.0
    win = max(1, rate // 100)  # 10 ms
    thr = max(200, int(gmax * 0.25))

    def loud(i: int) -> bool:
        return max(abs(peaks[(i + k) * ch]) for k in range(win)) > thr

    head = tail = None
    for i in range(0, frames - win, win):
        if loud(i):
            if head is None:
                head = i / rate
        elif head is not None and tail is None:
            tail = i / rate  # 第一个"由响转静"的窗 = 后沿

    print(f"==== {tag}: {rate} Hz / {ch} ch / {frames} 帧 = {frames / rate:.2f}s；全段峰值 {gmax} ({gdb:.1f} dBFS)")
    if head is None:
        print("  全段静音，没找到标记音")
        return 1
    print(f"  标记音区间 ≈ {head:.2f}s → {tail if tail is not None else frames / rate:.2f}s")
    if expect is not None and tail is not None:
        print(f"  后沿实测 {tail:.2f}s，源侧应在 {expect:.2f}s → **环回积压 ≈ {tail - expect:.2f}s**")
    return 0


if __name__ == "__main__":
    sys.exit(main())
