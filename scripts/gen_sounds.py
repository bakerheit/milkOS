#!/usr/bin/env python3
"""Generate milkOS's system sounds (python3 stdlib only). The outputs are committed in
rootfs/System/Library/Sounds/; rerun after changing anything here:

    python3 scripts/gen_sounds.py [OUTDIR]

All 48 kHz, mono, 16-bit PCM (the kernel mixer converts to stereo). Soft sines with a bit
of second harmonic, quick attack, exponential decay, so nothing clicks or blares:
  notify.wav  A5 then E6, a light "ding-ding"           (~0.4 s)
  error.wav   G4 then Eb4, a gentle descending pair     (~0.45 s)
  chime.wav   G5 then C6, the boot chime (same as `play chime`) (~0.95 s)
"""
import math
import os
import struct
import sys
import wave

RATE = 48000


def note(buf, start, freq, length, vol, tau=0.18):
    s0 = int(start * RATE)
    n = int(length * RATE)
    attack = int(0.012 * RATE)
    for i in range(n):
        k = s0 + i
        if k >= len(buf):
            break
        a = min(1.0, i / attack)
        fade = min(1.0, (n - i) / attack)
        t = i / RATE
        v = math.sin(2 * math.pi * freq * t) + 0.25 * math.sin(4 * math.pi * freq * t)
        buf[k] += vol * a * fade * math.exp(-t / tau) * v / 1.25


def write(path, buf):
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(b"".join(struct.pack("<h", int(max(-1.0, min(1.0, x)) * 32767)) for x in buf))


def sounds():
    notify = [0.0] * int(0.40 * RATE)
    note(notify, 0.00, 880.00, 0.14, 0.25, tau=0.08)
    note(notify, 0.10, 1318.51, 0.30, 0.22, tau=0.12)
    error = [0.0] * int(0.45 * RATE)
    note(error, 0.00, 392.00, 0.20, 0.30, tau=0.12)
    note(error, 0.17, 311.13, 0.28, 0.30, tau=0.14)
    chime = [0.0] * int(0.95 * RATE)
    note(chime, 0.00, 783.99, 0.60, 0.22)
    note(chime, 0.16, 1046.50, 0.78, 0.22)
    return {"notify.wav": notify, "error.wav": error, "chime.wav": chime}


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "rootfs", "System", "Library", "Sounds")
    os.makedirs(out, exist_ok=True)
    for name, buf in sounds().items():
        path = os.path.join(out, name)
        write(path, buf)
        print(f"{path}: {len(buf) / RATE:.2f} s")


if __name__ == "__main__":
    main()
