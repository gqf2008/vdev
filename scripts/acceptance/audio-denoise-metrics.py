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
Requires numpy (the cross-correlation is FFT-based; the naive loop over the
full search range takes minutes per file).
"""
import pathlib
import sys
import wave

try:
    import numpy as np
except ImportError:  # pragma: no cover - environment guard
    raise SystemExit("this tool needs numpy (the cross-correlation is FFT-based): pip install numpy")

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


def lag_samples(a, b, max_ms=200.0, sr=SR_REF):
    """Delay of `b` relative to `a`: (lag, peak normalized correlation).

    The peak of the *raw-waveform* cross-correlation, searched around the
    loudest 2 s of `a`. Raw waveform and not an energy envelope: a 5 ms shift
    costs ~0.5 of correlation here, so the peak resolves single frames -- on an
    envelope it is smeared by the syllable rate and only gives +/-10 ms.
    """
    n = min(len(a), len(b))
    a = a[:n] - a[:n].mean()
    b = b[:n] - b[:n].mean()
    win = min(sr * 2, n // 3)
    if n > win:
        cs = np.concatenate([[0.0], np.cumsum(a[:n] ** 2)])
        start = int(np.argmax(cs[win:] - cs[:-win]))
    else:
        start = 0
    ref = a[start : start + win]
    max_lag = int(round(max_ms / 1000.0 * sr))
    max_lag = max(0, min(max_lag, n - start - win))
    seg = b[start : start + win + max_lag]
    nfft = 1 << int(np.ceil(np.log2(len(ref) + len(seg))))
    # c[k] = sum_t ref[t] * seg[t + k]; no wrap-around, len(ref)+len(seg) <= nfft
    corr = np.fft.irfft(np.fft.rfft(seg, nfft) * np.conj(np.fft.rfft(ref, nfft)), nfft)
    lags = np.arange(0, max_lag + 1)
    cs = np.concatenate([[0.0], np.cumsum(seg**2)])
    denom = np.sqrt((cs[lags + len(ref)] - cs[lags]) * np.sum(ref**2)) + 1e-20
    vals = corr[: max_lag + 1] / denom
    i = int(np.argmax(vals))
    return int(lags[i]), float(vals[i])


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


def cmd_lag(argv):
    a, sr_a = read_wav16(argv[0])
    b, sr_b = read_wav16(argv[1])
    if sr_a != sr_b:
        raise SystemExit(f"sample rates differ: {sr_a} vs {sr_b}")
    max_ms = float(argv[2]) if len(argv) > 2 else 200.0
    if sr_a != SR_REF:  # search resolution is fixed at the reference rate
        t = np.arange(int(round(len(a) * SR_REF / sr_a))) / SR_REF
        src = np.arange(len(a)) / sr_a
        a = np.interp(t, src, a)
        b = np.interp(t, src, b)
    l, c = lag_samples(a, b, max_ms)
    print(f"lag = {l} samples ({l / SR_REF * 1000:.2f} ms)  peak corr = {c:.4f}")
    return 0


def cmd_sisnr(argv):
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
    clean, sr_c = read_wav16(argv[0])
    noisy, sr_n = read_wav16(argv[1])
    out, sr_o = read_wav16(argv[2])
    if not sr_c == sr_n == sr_o:
        raise SystemExit(f"rate mismatch {sr_c}/{sr_n}/{sr_o}")
    l, c = lag_samples(noisy, out)
    den = si_sdr(clean, undo_lag(out, l))
    base = si_sdr(clean, noisy)
    print(f"sample rate      : {sr_c} Hz")
    print(f"algorithmic lag  : {l} samples = {l / sr_c * 1000:.2f} ms (corr {c:.4f})")
    print(f"SI-SDR noisy     : {base:+.2f} dB")
    print(f"SI-SDR denoised  : {den:+.2f} dB   (improvement {den - base:+.2f} dB)")
    print(f"residual rms     : {rms_dbfs(undo_lag(out, l)):.2f} dBFS   (clean {rms_dbfs(clean):.2f})")
    return 0


COMMANDS = {"lag": cmd_lag, "sisnr": cmd_sisnr, "rms": cmd_rms, "report": cmd_report}


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in COMMANDS:
        print(__doc__)
        return 2
    return COMMANDS[sys.argv[1]](sys.argv[2:])


if __name__ == "__main__":
    sys.exit(main())
