#!/usr/bin/env python3
"""Generate the screenshot shutter sound (python3 stdlib only), committed as
rootfs/System/Library/Sounds/shutter.wav (crate::audio::Sound::Shutter):

    python3 scripts/gen_shutter.py [OUT.wav]

48 kHz mono 16-bit, ~0.22 s: two soft filtered-noise clicks (the shutter opening and
closing) over a short low thump, quick decays, nothing harsh. Deterministic (fixed seed).
"""
import math
import os
import random
import struct
import sys
import wave

RATE = 48000


def shutter():
    rnd = random.Random(2026)
    n = int(0.22 * RATE)
    buf = [0.0] * n
    for start, vol, tau in [(0.000, 0.55, 0.012), (0.075, 0.45, 0.016)]:
        s0 = int(start * RATE)
        lp = 0.0
        for i in range(int(0.08 * RATE)):
            k = s0 + i
            if k >= n:
                break
            t = i / RATE
            # One-pole low-pass on white noise: a paper-ish click, not a hiss.
            lp += 0.35 * (rnd.uniform(-1, 1) - lp)
            env = min(1.0, i / 24) * math.exp(-t / tau)
            thump = 0.5 * math.sin(2 * math.pi * 180 * t) * math.exp(-t / 0.02)
            buf[k] += vol * env * lp * 2.2 + vol * thump * min(1.0, i / 48)
    peak = max(abs(x) for x in buf) or 1.0
    return [x * 0.5 / peak for x in buf]


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        here, "..", "rootfs", "System", "Library", "Sounds", "shutter.wav")
    buf = shutter()
    with wave.open(out, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(b"".join(struct.pack("<h", int(max(-1.0, min(1.0, x)) * 32767)) for x in buf))
    print(f"{out}: {len(buf) / RATE:.2f} s")


if __name__ == "__main__":
    main()
