#!/usr/bin/env python3
"""Generate the two tunes milkOS seeds ~/Music with (python3 stdlib only). The outputs are
committed in rootfs/System/Music/; the desktop copies them into ~/Music on first
boot. Rerun after changing anything here:

    python3 scripts/gen_music.py [OUTDIR]

24 kHz mono 16-bit PCM (the kernel's WAV reader resamples to 48 kHz stereo; 24 kHz keeps
the initramfs small and everything here sits well under 12 kHz).

  morning-pasture.wav  C major, 100 bpm, 4/4: C G Am F twice. A soft pad, a round bass,
                       a plucked melody on top. (~22 s)
  night-milk.wav       F major, 3/4 lullaby: music-box arpeggios over F Dm Bb C, a slow
                       melody, a long fade. (~22 s)

Same voice style as gen_sounds.py: sines plus a little second harmonic, soft attacks,
exponential decays, nothing clicks. Normalised to a -6 dBFS peak.
"""
import math
import os
import struct
import sys
import wave

RATE = 24000
TAU = 2 * math.pi


def hz(name):
    """'C4', 'F#3', 'Bb5' -> frequency (A4 = 440)."""
    steps = {"C": -9, "D": -7, "E": -5, "F": -4, "G": -2, "A": 0, "B": 2}
    n = steps[name[0]]
    rest = name[1:]
    if rest[0] == "#":
        n, rest = n + 1, rest[1:]
    elif rest[0] == "b":
        n, rest = n - 1, rest[1:]
    return 440.0 * 2 ** ((n + 12 * (int(rest) - 4)) / 12)


def add(buf, start, length, fn, attack=0.01, release=0.05):
    """Mix fn(t) into buf from `start` (s) for `length` (s), with linear attack / release."""
    s0, n = int(start * RATE), int(length * RATE)
    a, r = max(1, int(attack * RATE)), max(1, int(release * RATE))
    for i in range(n):
        k = s0 + i
        if k >= len(buf):
            break
        env = min(1.0, i / a, (n - i) / r)
        buf[k] += env * fn(i / RATE)


def pluck(f, vol, tau=0.35):
    return lambda t: vol * math.exp(-t / tau) * (
        math.sin(TAU * f * t) + 0.3 * math.sin(2 * TAU * f * t) * math.exp(-t / 0.1)
    )


def pad(f, vol):
    # Two slightly detuned sines: a slow, warm beat.
    return lambda t: vol * 0.5 * (math.sin(TAU * f * t) + math.sin(TAU * f * 1.003 * t))


def bass(f, vol, tau=0.6):
    return lambda t: vol * math.exp(-t / tau) * (
        math.sin(TAU * f * t) + 0.15 * math.sin(2 * TAU * f * t)
    )


def bell(f, vol, tau=0.7):
    # Music box: the fundamental plus a quickly fading high partial.
    return lambda t: vol * (
        math.exp(-t / tau) * math.sin(TAU * f * t)
        + 0.25 * math.exp(-t / 0.12) * math.sin(TAU * f * 3.01 * t)
    )


def morning_pasture():
    beat = 60 / 100
    bar = 4 * beat
    chords = [
        ("C3", ["C4", "E4", "G4"]),
        ("G2", ["B3", "D4", "G4"]),
        ("A2", ["C4", "E4", "A4"]),
        ("F2", ["C4", "F4", "A4"]),
    ] * 2
    # Melody: (beat offset in the whole piece, note, beats).
    tune = [
        (0, "E5", 1), (1, "G5", 1), (2, "C6", 1.5), (3.5, "B5", 0.5),
        (4, "D5", 1), (5, "G5", 1), (6, "B5", 2),
        (8, "C5", 1), (9, "E5", 1), (10, "A5", 1.5), (11.5, "G5", 0.5),
        (12, "F5", 1), (13, "E5", 1), (14, "C5", 2),
        (16, "E5", 0.5), (16.5, "F5", 0.5), (17, "G5", 1), (18, "C6", 1), (19, "D6", 1),
        (20, "B5", 1), (21, "G5", 1), (22, "D5", 2),
        (24, "C5", 1), (25, "E5", 1), (26, "A5", 1), (27, "C6", 1),
        (28, "A5", 1), (29, "G5", 1), (30, "C5", 2.5),
    ]
    total = len(chords) * bar + 2.5
    buf = [0.0] * int(total * RATE)
    for i, (root, triad) in enumerate(chords):
        t0 = i * bar
        last = i == len(chords) - 1
        length = bar + (2.0 if last else 0.15)
        for n in triad:
            add(buf, t0, length, pad(hz(n), 0.07), attack=0.25, release=0.6 if last else 0.3)
        for b in range(2):
            add(buf, t0 + b * 2 * beat, 2 * beat + (1.5 if last and b else 0), bass(hz(root), 0.22))
    for (b, n, d) in tune:
        add(buf, b * beat, d * beat + 0.6, pluck(hz(n), 0.20), attack=0.004, release=0.2)
    return buf


def night_milk():
    beat = 60 / 76
    bar = 3 * beat
    chords = [
        ("F3", ["F4", "A4", "C5"]),
        ("D3", ["D4", "F4", "A4"]),
        ("Bb2", ["D4", "F4", "Bb4"]),
        ("C3", ["E4", "G4", "C5"]),
        ("F3", ["F4", "A4", "C5"]),
        ("D3", ["D4", "F4", "A4"]),
        ("Bb2", ["D4", "F4", "Bb4"]),
        ("F3", ["F4", "A4", "C5"]),
    ]
    tune = [
        (0, "C6", 2), (2, "A5", 1), (3, "D6", 2), (5, "C6", 1),
        (6, "Bb5", 1.5), (7.5, "A5", 0.5), (8, "G5", 1), (9, "E5", 2), (11, "G5", 1),
        (12, "A5", 2), (14, "F5", 1), (15, "A5", 1), (16, "D6", 1), (17, "C6", 1),
        (18, "Bb5", 1), (19, "A5", 1), (20, "G5", 1), (21, "F5", 3),
    ]
    total = len(chords) * bar + 3.0
    buf = [0.0] * int(total * RATE)
    for i, (root, triad) in enumerate(chords):
        t0 = i * bar
        last = i == len(chords) - 1
        add(buf, t0, bar + (2.5 if last else 0.3), bass(hz(root), 0.16, tau=1.2), release=0.8 if last else 0.3)
        # Arpeggio: up the triad in eighths, then back down.
        arp = triad + [triad[2].replace("4", "5") if "4" in triad[2] else triad[2]] + triad[::-1][1:]
        for k, n in enumerate(arp[:6]):
            add(buf, t0 + k * beat / 2, 1.4, bell(hz(n), 0.07, tau=0.5), attack=0.003, release=0.3)
    for (b, n, d) in tune:
        add(buf, b * beat, d * beat + 0.8, bell(hz(n), 0.16, tau=0.9), attack=0.004, release=0.4)
    # A long fade over the last 4 s.
    n = len(buf)
    fade = int(4.0 * RATE)
    for i in range(fade):
        buf[n - fade + i] *= 1 - i / fade
    return buf


def write(path, buf):
    peak = max(abs(x) for x in buf) or 1.0
    g = 0.5 / peak  # -6 dBFS
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(b"".join(struct.pack("<h", int(x * g * 32767)) for x in buf))


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    out = sys.argv[1] if len(sys.argv) > 1 else os.path.join(here, "..", "rootfs", "System", "Library", "Music")
    os.makedirs(out, exist_ok=True)
    for name, fn in [("morning-pasture.wav", morning_pasture), ("night-milk.wav", night_milk)]:
        buf = fn()
        path = os.path.join(out, name)
        write(path, buf)
        print(f"{path}: {len(buf) / RATE:.2f} s")


if __name__ == "__main__":
    main()
