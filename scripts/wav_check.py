#!/usr/bin/env python3
"""Analyse a 16-bit PCM WAV (python3 stdlib only): what got played, when, at what pitch.

  wav_check.py FILE [--probe F1,F2,...] [--json]

Prints the format and duration, then a timeline of "runs": stretches of 20 ms windows that
are not silent and have a steady pitch (zero-crossing estimate) and level, with start time, length,
frequency and level. --probe adds, per run, the Goertzel level of each given frequency, to
tell whether two tones were mixed. --json prints the same as JSON (for scripts).
"""
import json
import math
import struct
import sys
import wave

WIN = 0.020
SILENCE_DBFS = -50.0


def load(path):
    with wave.open(path, "rb") as w:
        ch, width, rate, n = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes()
        raw = w.readframes(n)
    if width != 2:
        sys.exit(f"{path}: {8 * width}-bit samples, want 16")
    vals = struct.unpack(f"<{len(raw) // 2}h", raw)
    mono = [sum(vals[i : i + ch]) / ch for i in range(0, len(vals) - ch + 1, ch)]
    return rate, ch, mono


def dbfs(x):
    return 20 * math.log10(x / 32768) if x > 0 else -120.0


def zc_freq(seg, rate):
    """Frequency from rising zero crossings (with interpolation)."""
    xs = []
    for i in range(1, len(seg)):
        a, b = seg[i - 1], seg[i]
        if a < 0 <= b:
            xs.append(i - 1 + (-a / (b - a) if b != a else 0))
    if len(xs) < 2:
        return 0.0
    return (len(xs) - 1) * rate / (xs[-1] - xs[0])


def goertzel(seg, rate, f):
    w = 2 * math.pi * f / rate
    c = 2 * math.cos(w)
    s1 = s2 = 0.0
    for x in seg:
        s1, s2 = x + c * s1 - s2, s1
    p = s1 * s1 + s2 * s2 - c * s1 * s2
    return dbfs(2 * math.sqrt(max(p, 0)) / len(seg))


def analyse(path, probes):
    rate, ch, mono = load(path)
    win = int(rate * WIN)
    windows = []
    for k in range(0, len(mono) - win + 1, win):
        seg = mono[k : k + win]
        rms = math.sqrt(sum(x * x for x in seg) / win)
        level = dbfs(rms)
        f = zc_freq(seg, rate) if level > SILENCE_DBFS else 0.0
        windows.append((k / rate, level, f))
    runs = []
    for t, level, f in windows:
        silent = level <= SILENCE_DBFS
        if runs and not silent and not runs[-1]["silent"]:
            prev = runs[-1]["freq"]
            prev_level = runs[-1]["levels"][-1]
            if prev and abs(f - prev) / prev < 0.04 and abs(level - prev_level) < 2.5:
                r = runs[-1]
                r["dur"] += WIN
                r["levels"].append(level)
                r["freqs"].append(f)
                continue
        if runs and silent and runs[-1]["silent"]:
            runs[-1]["dur"] += WIN
            continue
        runs.append({"start": t, "dur": WIN, "silent": silent, "freq": f, "levels": [level], "freqs": [f]})
    out = []
    for r in runs:
        if r["silent"]:
            out.append({"start": round(r["start"], 3), "dur": round(r["dur"], 3), "silent": True})
            continue
        fs = sorted(r["freqs"])
        item = {
            "start": round(r["start"], 3),
            "dur": round(r["dur"], 3),
            "freq": round(fs[len(fs) // 2], 1),
            "dbfs": round(max(r["levels"]), 1),
        }
        if probes:
            a = int(r["start"] * rate)
            seg = mono[a : a + int(r["dur"] * rate)]
            item["probe"] = {str(f): round(goertzel(seg, rate, f), 1) for f in probes}
        out.append(item)
    total = len(mono) / rate
    peak = max((abs(x) for x in mono), default=0)
    return {
        "file": path,
        "rate": rate,
        "channels": ch,
        "seconds": round(total, 3),
        "peak_dbfs": round(dbfs(peak), 1),
        "nonsilent_seconds": round(sum(r["dur"] for r in out if not r.get("silent")), 3),
        "runs": out,
    }


def main():
    args = sys.argv[1:]
    probes = []
    as_json = "--json" in args
    if "--probe" in args:
        i = args.index("--probe")
        probes = [float(x) for x in args[i + 1].split(",")]
        del args[i : i + 2]
    args = [a for a in args if a != "--json"]
    if len(args) != 1:
        sys.exit(__doc__)
    res = analyse(args[0], probes)
    if as_json:
        print(json.dumps(res))
        return
    print(f"{res['file']}: {res['rate']} Hz, {res['channels']} ch, {res['seconds']} s, "
          f"peak {res['peak_dbfs']} dBFS, {res['nonsilent_seconds']} s not silent")
    for r in res["runs"]:
        if r.get("silent"):
            print(f"  {r['start']:7.3f} s  {r['dur']:6.3f} s  (silence)")
        else:
            p = ""
            if "probe" in r:
                p = "  " + " ".join(f"{k}Hz:{v}dB" for k, v in r["probe"].items())
            print(f"  {r['start']:7.3f} s  {r['dur']:6.3f} s  ~{r['freq']:7.1f} Hz  {r['dbfs']:6.1f} dBFS{p}")


if __name__ == "__main__":
    main()
