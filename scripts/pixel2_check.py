#!/usr/bin/env python3
"""Checks on qemu screendumps (PPM) of the Pixel 2 test build (scripts/pixel2-qemu-test.sh):

  pixel2_check.py pattern FILE.ppm [FMT]       the colour test pattern, as qemu shows it for the given pixel order
  pixel2_check.py console FILE1.ppm FILE2.ppm  the boot log console and the live counter (two dumps a while apart)
  pixel2_check.py text FILE.ppm ROW0 ROW1 N    at least N light pixels in rows ROW0..ROW1
  pixel2_check.py red FILE.ppm ROW0 ROW1 N     at least N red-text pixels in rows ROW0..ROW1
  pixel2_check.py desktop FILE1.ppm FILE2.ppm [free]  the desktop on the 1080x1920 glass, a 540x960 picture shown doubled ("free": any size): it is
                                               bright (not the dark boot log), the menu bar is there, every 2x2 block is one
                                               colour, and the clock in the menu bar changed between the two dumps
  pixel2_check.py turned FILE.ppm              the desktop turned a quarter turn clockwise: the menu bar is along the right edge

qemu's ramfb reads the buffer as B, G, R, x; a guest that writes another 4-byte order shows red and
blue (or more) changed, which is exactly what the pattern checks expect for FMT. Prints `ok:` / `FAIL:` lines
and exits 1 on any failure. Python 3 standard library only.
"""

import sys

ORDERS = {  # the bytes a guest writes for (r, g, b)
    "bgrx": lambda r, g, b: (b, g, r, 255),
    "rgbx": lambda r, g, b: (r, g, b, 255),
    "xrgb": lambda r, g, b: (255, r, g, b),
    "xbgr": lambda r, g, b: (255, b, g, r),
}
bad = 0


def say(ok, what):
    global bad
    print(("  ok: " if ok else "  FAIL: ") + what)
    if not ok:
        bad = 1


def load(path):
    d = open(path, "rb").read()
    toks, i = [], 0
    while len(toks) < 4:
        while d[i : i + 1].isspace():
            i += 1
        j = i
        while not d[j : j + 1].isspace():
            j += 1
        toks.append(d[i:j])
        i = j
    assert toks[0] == b"P6" and toks[3] == b"255", "not an 8-bit PPM"
    w, h = int(toks[1]), int(toks[2])
    data = d[i + 1 : i + 1 + w * h * 3]
    assert len(data) == w * h * 3, "short PPM"
    return w, h, data


def px(img, x, y):
    w, _, data = img
    o = (y * w + x) * 3
    return tuple(data[o : o + 3])


def seen(fmt, rgb):
    """What qemu shows (read as B, G, R) when the guest wrote `rgb` in `fmt`."""
    b = ORDERS[fmt](*rgb)
    return (b[2], b[1], b[0])


def light(p):
    return p[0] > 150 and p[1] > 150 and p[2] > 140


def count(img, x0, y0, x1, y1, pred):
    n = 0
    for y in range(y0, y1):
        for x in range(x0, x1):
            n += pred(px(img, x, y))
    return n


def pattern(path, fmt="bgrx"):
    img = load(path)
    say(img[0] == 1080 and img[1] == 1920, f"size {img[0]}x{img[1]}")
    for name, at, rgb in (
        ("red block", (100, 600), (255, 0, 0)),
        ("green block", (1000, 300), (0, 255, 0)),
        ("blue block", (100, 1500), (0, 0, 255)),
        ("white block", (1000, 1500), (255, 255, 255)),
        ("gray gap", (540, 300), (0x60, 0x60, 0x60)),
        ("black border", (30, 500), (0, 0, 0)),
        ("frame left", (0, 100), (255, 255, 255)),
        ("frame right", (1079, 100), (255, 255, 255)),
        ("frame top", (500, 0), (255, 255, 255)),
        ("frame bottom", (500, 1919), (255, 255, 255)),
        ("black diagonal on white", (1012, 1800), (0, 0, 0)),
        ("white diagonal on blue", (67, 1800), (255, 255, 255)),
    ):
        got = px(img, *at)
        want = seen(fmt, rgb)
        say(got == want, f"{name} at {at}: {got}, want {want}")
    # The label plates: black, with white letters.
    for name, (x0, y0, x1, y1) in (("RED", (188, 422, 363, 549)), ("GREEN", (668, 422, 940, 549)), ("BLUE", (164, 1370, 388, 1497)), ("WHITE", (668, 1370, 940, 1497))):
        white = count(img, x0, y0, x1, y1, lambda p: p == (255, 255, 255))
        black = count(img, x0, y0, x1, y1, lambda p: p == (0, 0, 0))
        say(white > 300 and black > 8000, f"label {name}: {white} letter pixels on {black} black")


def console(p1, p2):
    a, b = load(p1), load(p2)
    say((a[0], a[1]) == (1080, 1920), "size 1080x1920")
    green = (0x30, 0xD0, 0x60)
    say(px(a, 300, 2) in (green, (255, 255, 255)) and px(a, 600, 2) in (green, (255, 255, 255)), f"green band on top: {px(a, 300, 2)}")
    say(px(a, 500, 1200) == (0x16, 0x15, 0x13), f"console background {px(a, 500, 1200)}")
    n = count(a, 0, 8, 1080, 700, light)
    say(n > 3000, f"{n} light text pixels in the log area")
    n1 = count(a, 0, 1860, 600, 1900, light)
    say(n1 > 80, f"{n1} light pixels in the counter row")
    # Over time: the counter row and the band's marker change.
    rowa = [px(a, x, 1880) for x in range(0, 500)]
    rowb = [px(b, x, 1880) for x in range(0, 500)]
    say(any(rowa[i] != rowb[i] for i in range(500)) or count(b, 0, 1860, 600, 1900, light) != n1, "the counter row changed between the dumps")
    ma = [x for x in range(1080) if px(a, x, 2) == (255, 255, 255)]
    mb = [x for x in range(1080) if px(b, x, 2) == (255, 255, 255)]
    say(bool(ma) and bool(mb) and ma[0] != mb[0], f"the marker moved: {ma[:1]} -> {mb[:1]}")


def desktop(p1, p2, doubled=True):
    a, b = load(p1), load(p2)
    say((a[0], a[1]) == (1080, 1920), "size 1080x1920")
    say(px(a, 500, 1200) != (0x16, 0x15, 0x13), f"not the boot log's dark background: {px(a, 500, 1200)}")
    n = count(a, 0, 200, 1080, 1700, light)
    say(n > 400000, f"{n} light pixels in the desktop area (the wallpaper and a window), want 400000+")
    bar = count(a, 0, 0, 1080, 50, light)
    say(bar > 40000, f"{bar} light pixels in the menu bar rows (want 40000+ of 54000)")
    dock = count(a, 0, 1830, 1080, 1900, lambda p: p[0] < 120 or p[1] < 120)
    say(dock > 3000, f"{dock} dark or coloured pixels in the dock rows (the icons): want 3000+")
    # The desktop is 540x960 and the glass shows every pixel as a 2x2 block (the exact 2x of the area filter).
    bad_blocks = 0
    total = 0
    for k in range(1, 1200):
        x, y = (k * 37) % 540, (k * 61) % 960
        o = [px(a, 2 * x + dx, 2 * y + dy) for dy in (0, 1) for dx in (0, 1)]
        total += 1
        bad_blocks += len(set(o)) != 1
    if doubled:
        say(bad_blocks == 0, f"{total} sampled 2x2 blocks, {bad_blocks} not one colour")
    changed = sum(1 for y in range(8, 52) for x in range(880, 1080) if px(a, x, y) != px(b, x, y))
    say(changed > 20, f"the clock changed between the dumps: {changed} pixels differ")


def turned(path):
    img = load(path)
    say((img[0], img[1]) == (1080, 1920), "size 1080x1920")
    right = count(img, 1020, 100, 1070, 1800, light)
    left = count(img, 5, 100, 55, 1800, light)
    top = count(img, 100, 0, 1000, 40, light)
    say(right > 0.5 * 50 * 1700, f"the right edge is the menu bar: {right} light pixels in a strip of 85000")
    say(left < right and top < right, f"the left edge ({left}) and the top ({top}) are not the menu bar")


def text(path, r0, r1, n):
    img = load(path)
    c = count(img, 0, r0, 1080, r1, light)
    say(c >= n, f"{c} light pixels in rows {r0}..{r1} (want {n}+)")


def red(path, r0, r1, n):
    img = load(path)
    c = count(img, 0, r0, 1080, r1, lambda p: p[0] > 200 and p[1] < 160 and p[2] < 160)
    say(c >= n, f"{c} red pixels in rows {r0}..{r1} (want {n}+)")


if __name__ == "__main__":
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "pattern":
        pattern(args[0], args[1] if len(args) > 1 else "bgrx")
    elif cmd == "console":
        console(args[0], args[1])
    elif cmd == "desktop":
        desktop(args[0], args[1], len(args) < 3 or args[2] != "free")
    elif cmd == "turned":
        turned(args[0])
    elif cmd == "text":
        text(args[0], int(args[1]), int(args[2]), int(args[3]))
    elif cmd == "red":
        red(args[0], int(args[1]), int(args[2]), int(args[3]))
    else:
        sys.exit(f"unknown check {cmd}")
    sys.exit(bad)
