#!/usr/bin/env python3
"""Check a qemu screendump of the Raspberry Pi kernel against an independent model of the present transform
(docs/raspberry-pi.md, "HDMI displays"; the kernel's version is rpikit::fbpresent, in fixed point; this is a
floating point re-statement written from the description, so the two can disagree).

  fbpresent_check.py DUMP.png --logical 800x480 --phys 1024x768 --rotate 90 [--step 3] [--tol 6]
                              [--logical-out A.png] [--raster-out B.png] [--glass-out C.png --glass 480x800]
                              [--seen-out D.png]

The kernel was booted with `milk.fbtest=1`, which fills the logical screen with the calibration pattern. The model
draws the same pattern (pattern() below, the spec in rpikit::fbpresent::pattern_pixel), turns it clockwise by
--rotate, stretches it to --phys with an area-weighted (box) filter, and compares every --step'th pixel of the
dump (plus the whole border) channel by channel: the largest and the mean difference are printed, and the exit status
is 1 if any is over --tol or the dump is not --phys. The optional outputs are PNGs: the logical pattern, the raster as
the dump has it, and the glass: the dump stretched over a panel of --glass pixels the way a scaler board does
(the same box filter, non-uniformly: what the glass shows), and --seen-out is that glass turned back by --rotate, the
way a viewer holds the panel: it should look like the logical pattern.

python3 standard library only. Prints `fbpresent-check: OK ...` or `fbpresent-check: FAIL ...` last.
"""
import argparse
import os
import struct
import sys
import zlib

sys.dont_write_bytecode = True  # no __pycache__ next to the scripts
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from png_check import decode  # noqa: E402

CORNERS = [(0xe0, 0x20, 0x20), (0x20, 0xc0, 0x40), (0x30, 0x60, 0xf0), (0xf0, 0xd0, 0x20)]
BARS = [(255, 255, 255), (255, 255, 0), (0, 255, 255), (0, 255, 0), (255, 0, 255), (255, 0, 0), (0, 0, 255), (0, 0, 0)]
GRID = 80


def pattern(w, h, x, y):
    """The calibration pattern's pixel (x, y) of a w x h screen, as (r, g, b)."""
    if w < 16 or h < 16 or not (0 <= x < w and 0 <= y < h):
        return (0, 0, 0)
    if x < 2 or y < 2 or x >= w - 2 or y >= h - 2:
        return (255, 255, 255)

    def marker(x0, y0):
        return x0 <= x < x0 + 40 and y0 <= y < y0 + 40
    if marker(6, 6):
        return CORNERS[0]
    if marker(w - 46, 6):
        return CORNERS[1]
    if marker(w - 46, h - 46):
        return CORNERS[2]
    if marker(6, h - 46):
        return CORNERS[3]
    if x in (w // 2 - 1, w // 2) or y in (h // 2 - 1, h // 2):
        return (0xd0, 0xd0, 0xd0)
    if abs(y * w - x * h) <= w:
        return (255, 0, 255)
    by0 = h // 2 + 24
    if by0 <= y < by0 + 48 and 16 <= x < w - 16:
        return BARS[min(7, (x - 16) * 8 // (w - 32))]
    if x % GRID == 0 or y % GRID == 0:
        return (0x60, 0x60, 0x60)
    return (0x18, 0x18, 0x18)


# The golden points the Rust test (rpikit::fbpresent, pattern_golden_points) checks too.
GOLDEN = [
    ((0, 0), (255, 255, 255)), ((799, 479), (255, 255, 255)), ((1, 300), (255, 255, 255)),
    ((10, 10), CORNERS[0]), ((790, 10), CORNERS[1]), ((790, 470), CORNERS[2]), ((10, 470), CORNERS[3]),
    ((400, 100), (0xd0, 0xd0, 0xd0)), ((100, 240), (0xd0, 0xd0, 0xd0)), ((150, 90), (255, 0, 255)),
    ((30, 200), (0x18, 0x18, 0x18)), ((160, 200), (0x60, 0x60, 0x60)),
    ((21, 280), BARS[0]), ((779, 280), BARS[7]), ((300, 280), BARS[2]),
]


def logical_of(rot, lw, lh, rx, ry):
    """The logical pixel at rotated pixel (rx, ry) when the picture was turned clockwise by rot degrees."""
    return {0: (rx, ry), 90: (ry, lh - 1 - rx), 180: (lw - 1 - rx, lh - 1 - ry), 270: (lw - 1 - ry, rx)}[rot]


def footprint(src, dst, d):
    """(first source index, [fractional weights]) of destination pixel d of dst over a source axis of src pixels."""
    lo, hi = d * src / dst, (d + 1) * src / dst
    first = int(lo)
    out = []
    i = first
    while i < src and i < hi:
        ov = min(hi, i + 1) - max(lo, i)
        if ov > 1e-12:
            out.append(ov / (hi - lo))
        i += 1
    return first, out


def model_pixel(lw, lh, pw, ph, rot, px, py, cache):
    """The model's physical pixel (px, py): area average of the turned pattern under its footprint."""
    rw, rh = (lh, lw) if rot in (90, 270) else (lw, lh)
    fx = cache.setdefault(("x", px), footprint(rw, pw, px))
    fy = cache.setdefault(("y", py), footprint(rh, ph, py))
    acc = [0.0, 0.0, 0.0]
    for j, wy in enumerate(fy[1]):
        for i, wx in enumerate(fx[1]):
            lx, ly = logical_of(rot, lw, lh, fx[0] + i, fy[0] + j)
            c = pattern(lw, lh, lx, ly)
            for k in range(3):
                acc[k] += wx * wy * c[k]
    return acc


def box_resample(rows, sw, sh, dw, dh):
    """rows[y][x] = (r, g, b), resampled to dw x dh with an area filter (separable), as ints."""
    fx = [footprint(sw, dw, x) for x in range(dw)]
    fy = [footprint(sh, dh, y) for y in range(dh)]
    mid = []
    for y in range(sh):
        r = rows[y]
        line = []
        for first, ws in fx:
            a = [0.0, 0.0, 0.0]
            for i, w in enumerate(ws):
                p = r[first + i]
                a[0] += w * p[0]
                a[1] += w * p[1]
                a[2] += w * p[2]
            line.append(a)
        mid.append(line)
    out = []
    for first, ws in fy:
        line = []
        for x in range(dw):
            a = [0.0, 0.0, 0.0]
            for j, w in enumerate(ws):
                p = mid[first + j][x]
                a[0] += w * p[0]
                a[1] += w * p[1]
                a[2] += w * p[2]
            line.append((int(a[0] + 0.5), int(a[1] + 0.5), int(a[2] + 0.5)))
        out.append(line)
    return out


def rotated_of(rot, lw, lh, lx, ly):
    """Logical pixel (lx, ly) as a pixel of the picture turned clockwise by rot degrees."""
    return {0: (lx, ly), 90: (lh - 1 - ly, lx), 180: (lw - 1 - lx, lh - 1 - ly), 270: (ly, lw - 1 - lx)}[rot]


def seen_from(glass, lw, lh, rot):
    """The glass (the turned picture) as a viewer sees it with the panel turned back the other way: the logical picture's orientation."""
    return [[glass[rotated_of(rot, lw, lh, lx, ly)[1]][rotated_of(rot, lw, lh, lx, ly)[0]] for lx in range(lw)] for ly in range(lh)]


def write_png(path, rows):
    h, w = len(rows), len(rows[0])
    raw = bytearray()
    for r in rows:
        raw.append(0)
        for p in r:
            raw += bytes(p[:3])

    def chunk(kind, body):
        c = kind + body
        return struct.pack(">I", len(body)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b"")
    open(path, "wb").write(png)


def size(s):
    w, h = s.lower().split("x")
    return int(w), int(h)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("dump")
    ap.add_argument("--logical", default="800x480")
    ap.add_argument("--phys", default="1024x768")
    ap.add_argument("--rotate", type=int, default=90, choices=[0, 90, 180, 270])
    ap.add_argument("--step", type=int, default=3)
    ap.add_argument("--tol", type=int, default=6)
    ap.add_argument("--logical-out")
    ap.add_argument("--raster-out")
    ap.add_argument("--glass-out")
    ap.add_argument("--glass", default="480x800")
    ap.add_argument("--seen-out")
    a = ap.parse_args()
    for (x, y), want in GOLDEN:
        if pattern(800, 480, x, y) != want:
            sys.exit(f"fbpresent-check: FAIL the checker's own pattern differs from the golden point ({x},{y})")
    lw, lh = size(a.logical)
    pw, ph = size(a.phys)
    w, h, rows = decode(a.dump)
    if (w, h) != (pw, ph):
        print(f"fbpresent-check: FAIL the dump is {w}x{h}, not {pw}x{ph}")
        sys.exit(1)
    cache = {}
    pts = {(x, y) for y in range(0, ph, a.step) for x in range(0, pw, a.step)}
    pts |= {(x, y) for y in (0, 1, ph - 2, ph - 1) for x in range(pw)}
    pts |= {(x, y) for x in (0, 1, pw - 2, pw - 1) for y in range(ph)}
    worst, total, over, where = 0, 0.0, 0, None
    for (x, y) in sorted(pts):
        exp = model_pixel(lw, lh, pw, ph, a.rotate, x, y, cache)
        got = rows[y][x]
        d = max(abs(exp[k] - got[k]) for k in range(3))
        total += sum(abs(exp[k] - got[k]) for k in range(3)) / 3
        if d > worst:
            worst, where = d, (x, y)
        if d > a.tol:
            over += 1
    n = len(pts)
    print(f"fbpresent-check: {n} points compared: mean error {total / n:.3f}, worst {worst:.1f} at {where}, {over} over {a.tol}")
    if a.logical_out:
        write_png(a.logical_out, [[pattern(lw, lh, x, y) for x in range(lw)] for y in range(lh)])
    if a.raster_out:
        write_png(a.raster_out, rows)
    if a.glass_out or a.seen_out:
        gw, gh = size(a.glass)
        glass = box_resample(rows, pw, ph, gw, gh)
        if a.glass_out:
            write_png(a.glass_out, glass)
        if a.seen_out:
            rw, rh = (lh, lw) if a.rotate in (90, 270) else (lw, lh)
            if (gw, gh) != (rw, rh):
                sys.exit(f"fbpresent-check: FAIL --glass {gw}x{gh} is not the turned picture's {rw}x{rh}")
            write_png(a.seen_out, seen_from(glass, lw, lh, a.rotate))
    if over:
        print(f"fbpresent-check: FAIL {over} points differ by more than {a.tol} from the model")
        sys.exit(1)
    print(f"fbpresent-check: OK the dump is {pw}x{ph} and equals the model of rotate {a.rotate} of the {lw}x{lh} pattern within {a.tol}")


if __name__ == "__main__":
    main()
