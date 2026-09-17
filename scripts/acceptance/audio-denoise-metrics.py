#!/usr/bin/env python3
"""Denoiser metrics: algorithmic lag, SI-SDR, residual level.

The `vdev-mic-agent` acceptance line for a denoiser has three parts, and only
the third is about the model being "good":

* **algorithmic lag** -- how far the model's output trails its input. This is
  *not* the same as "how fast the model runs": a model can be cheaper than real
  time and still delay the audio by two frames (RNNoise does exactly that, see
  `crates/vdev-mic-agent/README.md`). It has to be measured through a *streaming*
  (frame-by-frame, stateful) implementation -- an offline `model(waveform)` pass
  reports the model's quality ceiling, not the latency of the live chain.
* **SI-SDR** -- scale-invariant SDR against the clean reference, before and
  after. Unlike SNR it does not reward a model for simply turning the volume
  down, and unlike PESQ it needs no reference implementation we do not ship.
* **residual level** -- RMS of the model's output on noise-only input. A model
  that scores well on SI-SDR but leaves the noise floor at -20 dBFS is not
  usable in a call.

Usage
-----
  audio-denoise-metrics.py lag    <in.wav> <out.wav> [max_ms]
  audio-denoise-metrics.py sisnr  <clean.wav> <test.wav> [undo_lag_ms]
  audio-denoise-metrics.py rms    <wav> [start_s] [end_s]
  audio-denoise-metrics.py report <clean.wav> <noisy.wav> <denoised.wav>

WAVs must be mono PCM16; `lag`/`report` need matching sample rates.
numpy makes the cross-correlation FFT-based; without it the search falls back to a
naive loop, which is fine for short files and unusable for long ones.

`--self-test` checks this tool's own arithmetic (a known delay must come back as
that delay, and a suspicious peak must not exit 0). It is stdlib-only so the
acceptance smoke can run it without installing anything.

Exit codes: 0 ok, 2 usage/input error, 3 measurement suspect (peak too flat, or
peak pinned to the edge of the search range -- e.g. the real delay is larger
than max_ms).
"""
import math
import pathlib
import random
import sys
import wave

try:
    import numpy as np
except ImportError:  # pragma: no cover - `lag --self-test` still works without it
    np = None


def require_numpy():
    if np is None:
        raise SystemExit("这个子命令需要 numpy（SI-SDR / RMS / 重采样都要它）：pip install numpy")

MIN_PEAK_CORR = 0.5  # below this the "peak" is not a measurement, see warn_reason()

SR_REF = 48000


def read_wav16(path):
    if not pathlib.Path(path).is_file():
        raise SystemExit(f"no such WAV: {path}")
    with wave.open(path, "rb") as w:
        if w.getsampwidth() != 2:
            raise SystemExit(f"{path}: PCM16 only (got {w.getsampwidth() * 8}-bit)")
        n, ch, sr = w.getnframes(), w.getnchannels(), w.getframerate()
        raw = w.readframes(n)
    x = np.frombuffer(raw, dtype="<i2").astype(np.float64) / 32768.0
    if ch > 1:
        x = x.reshape(-1, ch).mean(axis=1)
    return x, sr


def loudest_window(a, sr):
    """Index of the loudest `2 s` (least `2 s`/`n//3`) inside `a`.

    Working on the loudest stretch instead of the whole file is what keeps a
    quiet lead-in from winning the correlation.
    """
    n = len(a)
    win = min(sr * 2, n // 3)
    if win < 64:
        raise SystemExit(f"素材太短：{n} 样本（至少需要几百样本才谈得上测延迟）")
    if n <= win or np is None:
        running, best, start = 0.0, -1.0, 0
        e = [x * x for x in a]
        for i in range(n):
            running += e[i]
            if i >= win:
                running -= e[i - win]
            if i >= win - 1 and running > best:
                best, start = running, i - win + 1
        return start, win
    e = np.asarray(a, dtype=np.float64) ** 2
    cs = np.concatenate([[0.0], np.cumsum(e)])
    running = cs[win:] - cs[:-win]
    return int(np.argmax(running)), win


def _as_list(x):
    return [float(v) for v in x]


def _search_window(a, b, sr, max_ms):
    """Loudest window of `a` plus the matching segment of `b` to slide over."""
    n = min(len(a), len(b))
    a, b = _as_list(a[:n]), _as_list(b[:n])
    mean_a = sum(a) / n if n else 0.0
    mean_b = sum(b) / n if n else 0.0
    a = [x - mean_a for x in a]
    b = [x - mean_b for x in b]
    start, win = loudest_window(a, sr)
    ref = a[start : start + win]
    max_lag = int(round(max_ms / 1000.0 * sr))
    max_lag = max(0, min(max_lag, n - start - win))
    seg = b[start : start + win + max_lag]
    return ref, seg, max_lag


def _lag_py(ref, seg, max_lag):
    """Naive `Σ ref[t]·seg[t+k]` over k=0..max_lag; same convention as the FFT path."""
    e_ref = sum(x * x for x in ref)
    cs = [0.0] * (len(seg) + 1)
    for i, x in enumerate(seg):
        cs[i + 1] = cs[i] + x * x
    best_c, best_k = -2.0, 0
    for k in range(max_lag + 1):
        num = 0.0
        for t in range(len(ref)):
            num += ref[t] * seg[t + k]
        den = math.sqrt(e_ref * (cs[k + len(ref)] - cs[k]))
        c = num / den if den else 0.0
        if c > best_c:
            best_c, best_k = c, k
    return best_k, best_c


def _lag_np(ref, seg, max_lag):
    ref = np.asarray(ref, dtype=np.float64)
    seg = np.asarray(seg, dtype=np.float64)
    nfft = 1 << int(np.ceil(np.log2(len(ref) + len(seg))))
    # c[k] = Σ_t ref[t]·seg[t+k]; len(ref)+len(seg) <= nfft, so there is no wrap
    corr = np.fft.irfft(np.fft.rfft(seg, nfft) * np.conj(np.fft.rfft(ref, nfft)), nfft)
    lags = np.arange(0, max_lag + 1)
    cs = np.concatenate([[0.0], np.cumsum(seg**2)])
    denom = np.sqrt((cs[lags + len(ref)] - cs[lags]) * np.sum(ref**2)) + 1e-20
    vals = corr[: max_lag + 1] / denom
    i = int(np.argmax(vals))
    return int(lags[i]), float(vals[i])


def lag_samples(a, b, max_ms=200.0, sr=SR_REF):
    """Delay of `b` relative to `a`: (lag, peak normalized correlation).

    The peak of the *raw-waveform* cross-correlation, searched around the
    loudest 2 s of `a`. Raw waveform and not an energy envelope: a 5 ms shift
    costs ~0.5 of correlation here, so the peak resolves single frames -- on an
    envelope it is smeared by the syllable rate and only gives +/-10 ms.
    """
    ref, seg, max_lag = _search_window(a, b, sr, max_ms)
    if np is None:
        return _lag_py(ref, seg, max_lag)
    return _lag_np(ref, seg, max_lag)


def warn_reason(lag, corr, max_lag):
    """Why this reading should not be trusted, or None.

    A "peak" at the edge of the search range means the real delay is probably
    larger than `max_ms`; a flat peak means the two signals are not the same
    audio. Both used to be printed as if they were measurements.
    """
    if max_lag == 0:
        return "搜索范围是 0（max_ms 太小）"
    if lag >= max_lag:
        return f"峰值贴在搜索边界（{lag}/{max_lag}）：真实延迟可能大于 max_ms"
    if corr < MIN_PEAK_CORR:
        return f"峰值相关系数只有 {corr:.3f}（< {MIN_PEAK_CORR}）：两段音频很可能不是同一路信号"
    return None


def si_sdr(ref, est):
    """Scale-invariant SDR in dB (Le Roux et al., 2019)."""
    n = min(len(ref), len(est))
    ref = ref[:n].astype(np.float64)
    est = est[:n].astype(np.float64)
    ref = ref - ref.mean()
    est = est - est.mean()
    alpha = np.dot(est, ref) / (np.dot(ref, ref) + 1e-20)
    target = alpha * ref
    noise = est - target
    return 10 * np.log10((np.sum(target**2) + 1e-20) / (np.sum(noise**2) + 1e-20))


def rms_dbfs(x):
    return 20 * np.log10(np.sqrt(np.mean(x**2)) + 1e-20)


def undo_lag(test, lag_samples_):
    if lag_samples_ > 0:
        return test[lag_samples_:]
    if lag_samples_ < 0:
        return np.concatenate([np.zeros(-lag_samples_), test])
    return test


def _resample(x, sr_from, sr_to):
    if sr_from == sr_to:
        return x
    if np is None:
        raise SystemExit(f"输入是 {sr_from} Hz，需要重采样到 {sr_to} Hz：请装 numpy")
    out = np.interp(
        np.arange(int(round(len(x) * sr_to / sr_from))) / sr_to,
        np.arange(len(x)) / sr_from,
        np.asarray(x, dtype=np.float64),
    )
    return list(out)


def cmd_lag(argv):
    a, sr_a = read_wav16(argv[0])
    b, sr_b = read_wav16(argv[1])
    if sr_a != sr_b:
        raise SystemExit(f"sample rates differ: {sr_a} vs {sr_b}")
    max_ms = float(argv[2]) if len(argv) > 2 else 200.0
    # search resolution is fixed at the reference rate; report ms alongside so the
    # number is comparable across models, whatever the model's own rate is
    if sr_a != SR_REF:
        a = _resample(a, sr_a, SR_REF)
        b = _resample(b, sr_b, SR_REF)
    l, c = lag_samples(a, b, max_ms)
    note = " (numpy 缺失，走的朴素实现)" if np is None else ""
    print(f"lag = {l} samples @48k ({l / SR_REF * 1000:.2f} ms)  peak corr = {c:.4f}{note}")
    why = warn_reason(l, c, int(round(max_ms / 1000.0 * SR_REF)))
    if why:
        print(f"WARN: {why}", file=sys.stderr)
        return 3
    return 0


def cmd_sisnr(argv):
    require_numpy()
    clean, sr_c = read_wav16(argv[0])
    test, sr_t = read_wav16(argv[1])
    if sr_c != sr_t:
        raise SystemExit(f"sample rates differ: {sr_c} vs {sr_t}")
    if len(argv) > 2:
        test = undo_lag(test, int(round(float(argv[2]) * sr_c / 1000.0)))
    print(f"SI-SDR = {si_sdr(clean, test):.2f} dB")
    return 0


def cmd_rms(argv):
    x, sr = read_wav16(argv[0])
    s = int(float(argv[1]) * sr) if len(argv) > 1 else 0
    e = int(float(argv[2]) * sr) if len(argv) > 2 else len(x)
    print(f"rms = {rms_dbfs(x[s:e]):.2f} dBFS  ({e - s} samples)")
    return 0


def cmd_report(argv):
    require_numpy()
    clean, sr_c = read_wav16(argv[0])
    noisy, sr_n = read_wav16(argv[1])
    out, sr_o = read_wav16(argv[2])
    if not sr_c == sr_n == sr_o:
        raise SystemExit(f"rate mismatch {sr_c}/{sr_n}/{sr_o}")
    l, c = lag_samples(noisy, out)
    why = warn_reason(l, c, int(round(200.0 / 1000.0 * sr_c)))  # report 固定搜 200 ms
    den = si_sdr(clean, undo_lag(out, l))
    base = si_sdr(clean, noisy)
    print(f"sample rate      : {sr_c} Hz")
    print(f"algorithmic lag  : {l} samples = {l / sr_c * 1000:.2f} ms (corr {c:.4f})")
    print(f"SI-SDR noisy     : {base:+.2f} dB")
    print(f"SI-SDR denoised  : {den:+.2f} dB   (improvement {den - base:+.2f} dB)")
    print(f"residual rms     : {rms_dbfs(undo_lag(out, l)):.2f} dBFS   (clean {rms_dbfs(clean):.2f})")
    if why:
        print(f"WARN: {why}", file=sys.stderr)
        return 3
    return 0


def _synthetic(n, seed):
    """Deterministic broadband signal with a speech-like envelope (no numpy)."""
    rng = random.Random(seed)
    out, env = [], 0.0
    for i in range(n):
        # slow envelope so the loudest-window search has something to find
        env = 0.995 * env + 0.005 * rng.random()
        out.append((rng.random() * 2.0 - 1.0) * (0.2 + env))
    return out


def self_test():
    """Guard this tool's own arithmetic. Runs without numpy on purpose: the
    acceptance smoke must be able to check the ruler on any machine."""
    fails = []
    ref = _synthetic(1500, 7)
    shift, max_lag = 37, 200
    # `_search_window` always hands over a `seg` that is `max_lag` longer than `ref`
    seg = [0.0] * shift + ref + [0.0] * (max_lag - shift + 8)
    k, c = _lag_py(ref, seg, max_lag)
    if (k, round(c, 3)) != (shift, 1.0):
        fails.append(f"已知延迟 {shift} 样本，朴素互相关报的是 {k}（corr {c:.4f}）")
    k0, c0 = _lag_py(ref, ref + [0.0] * max_lag, max_lag)
    if (k0, round(c0, 3)) != (0, 1.0):
        fails.append(f"零延迟应当报 0 / corr 1.000，实际 {k0} / {c0:.4f}")
    if warn_reason(200, 0.99, 200) is None:
        fails.append("峰值贴在搜索边界时必须给 warn_reason（真实延迟可能被 max_ms 截断）")
    if warn_reason(10, 0.2, 200) is None:
        fails.append("峰值相关系数很低时必须给 warn_reason（两段很可能不是同一路信号）")
    if warn_reason(10, 0.99, 200) is not None:
        fails.append("正常的读数不该被 warn_reason 拦下")
    try:
        loudest_window([0.0] * 40, 48000)
        fails.append("过短素材应当直接报错，而不是给一个假读数")
    except SystemExit:
        pass
    if np is not None:
        kn, cn = _lag_np(ref, seg, max_lag)
        if kn != k or abs(cn - c) > 1e-9:
            fails.append(f"numpy 路径与朴素路径不一致：{kn}/{cn:.6f} vs {k}/{c:.6f}")
    if fails:
        for f in fails:
            print(f"FAIL: {f}", file=sys.stderr)
        return 1
    which = "numpy + 朴素" if np is not None else "仅朴素"
    print(f"audio-denoise-metrics self-test: PASS（已知延迟 {shift} 样本回读一致；"
          f"边界/低相关/过短素材三种可疑读数都被拦下；{which}）")
    return 0


COMMANDS = {"lag": cmd_lag, "sisnr": cmd_sisnr, "rms": cmd_rms, "report": cmd_report}


def main():
    if len(sys.argv) >= 2 and sys.argv[1] == "--self-test":
        return self_test()
    if len(sys.argv) < 2 or sys.argv[1] not in COMMANDS:
        print(__doc__)
        return 2
    cmd, argv = COMMANDS[sys.argv[1]], sys.argv[2:]
    if len(argv) < {"lag": 2, "sisnr": 2, "rms": 1, "report": 3}[sys.argv[1]]:
        print(f"{sys.argv[1]} 参数不足\n", file=sys.stderr)
        print(__doc__)
        return 2
    return cmd(argv)


if __name__ == "__main__":
    sys.exit(main())
