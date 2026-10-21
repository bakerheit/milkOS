#!/usr/bin/env python3
"""Check a recording of the Music app playing a tune (python3 stdlib only).

  music_check.py RECORDING.wav SOURCE.wav [--min-secs S] [--max-secs S]

The recording is what qemu's wav audiodev captured (48 kHz stereo; the boot chime may come
first). Finds where SOURCE starts in it (the first window, after any leading sound and a
gap, whose level jumps), lines the two up to the sample (least squares over a 40 ms
search), then compares: the match has to be close (SNR >= 25 dB over everything recorded
of the tune, at least --min-secs, at most --max-secs: a pause has to stop it), nothing
clipped, and the recording 48 kHz stereo.
Prints PASS / FAIL lines; exits 1 on failure.
"""
import math
import struct
import sys
import wave


def load(path):
    with wave.open(path, "rb") as w:
        ch, width, rate, n = w.getnchannels(), w.getsampwidth(), w.getframerate(), w.getnframes()
        raw = w.readframes(n)
    if width != 2:
        sys.exit(f"{path}: not 16-bit")
    vals = struct.unpack(f"<{len(raw) // 2}h", raw)
    mono = [sum(vals[i : i + ch]) / ch for i in range(0, len(vals) - ch + 1, ch)]
    return rate, ch, vals, mono


def to48k(rate, mono):
    """Linear resampling to 48 kHz, like the kernel's WAV reader."""
    if rate == 48000:
        return mono
    out = []
    step = rate / 48000
    pos = 0.0
    while pos < len(mono) - 1:
        i = int(pos)
        f = pos - i
        out.append(mono[i] * (1 - f) + mono[i + 1] * f)
        pos += step
    return out


def rms(seg):
    return math.sqrt(sum(x * x for x in seg) / max(1, len(seg)))


def main():
    args = sys.argv[1:]
    opt = {"--min-secs": 2.0, "--max-secs": 1e9}
    for name in opt:
        if name in args:
            k = args.index(name)
            opt[name] = float(args[k + 1])
            del args[k : k + 2]
    min_secs, max_secs = opt["--min-secs"], opt["--max-secs"]
    rec_path, src_path = args
    ok = True

    def check(cond, msg):
        nonlocal ok
        print(("PASS " if cond else "FAIL ") + msg)
        ok = ok and cond

    rate, ch, vals, rec = load(rec_path)
    check(rate == 48000 and ch == 2, f"recording is {rate} Hz, {ch} ch (want 48000 Hz stereo)")
    clipped = sum(1 for v in vals if v in (32767, -32768))
    check(clipped == 0, f"{clipped} clipped samples")
    srate, _, _, src = load(src_path)
    src = to48k(srate, src)

    # Where does the tune start? 10 ms windows; the source's own first loud window tells us
    # what its start looks like. Skip whatever plays first (the boot chime) up to a gap.
    win = 480
    levels = [rms(rec[i : i + win]) for i in range(0, len(rec) - win, win)]
    quiet = 200.0
    i = 0
    if levels and levels[0] > quiet:
        while i < len(levels) and levels[i] > quiet:
            i += 1
    while i < len(levels) and levels[i] <= quiet:
        i += 1
    check(i < len(levels), f"something plays after the first gap (window {i})")
    if i >= len(levels):
        sys.exit(1)
    rough = i * win
    s0 = next(k for k, x in enumerate(src) if abs(x) > quiet)

    # Line up to the sample: the offset with the least squared error over 2000 samples.
    n = 2000
    best = None
    for off in range(rough - 960, rough + 960):
        if off < 0 or off + n > len(rec):
            continue
        a = rec[off : off + n]
        b = src[s0 : s0 + n]
        err = sum((x - y) * (x - y) for x, y in zip(a, b))
        if best is None or err < best[0]:
            best = (err, off)
    check(best is not None, "found an alignment")
    if best is None:
        sys.exit(1)
    off = best[1] - s0
    length = min(len(rec) - max(off, 0), len(src))
    a = rec[max(off, 0) : max(off, 0) + length]
    b = src[max(-off, 0) : max(-off, 0) + length]
    # The tune may have been cut off when qemu stopped, or paused: compare while it plays.
    sig = sum(y * y for y in b) or 1.0
    noise = sum((x - y) * (x - y) for x, y in zip(a, b)) or 1e-9
    snr = 10 * math.log10(sig / noise)
    secs = length / 48000
    print(f"     tune starts at {off / 48000:.3f} s in the recording, {secs:.2f} s of it recorded")
    check(min_secs <= secs <= max_secs, f"{secs:.2f} s recorded (want {min_secs} .. {max_secs})")
    check(snr >= 25, f"recording matches the source: SNR {snr:.1f} dB (want >= 25)")
    print("music-check: " + ("PASS" if ok else "FAIL"))
    sys.exit(0 if ok else 1)


if __name__ == "__main__":
    main()
