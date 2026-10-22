#!/usr/bin/env python3
"""Write the PNG test images for user/imgdec (tests/fixtures/NAME.png plus NAME.argb, the
pixels the decoder must produce: one big-endian u32 0xAARRGGBB per pixel). Standard library
only; the PNG writer is here so the fixtures cover things Python's own tools don't make
(every filter type, 1/2/4/16-bit, palettes with tRNS, Adam7, split IDATs, stored/fixed/dynamic
deflate). Re-run it to regenerate: python3 -I scripts/gen_png_fixtures.py"""
import os, struct, sys, zlib

OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "user", "imgdec", "tests", "fixtures")


def chunk(kind, data):
    return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else (b if pb <= pc else c)


def filter_row(ftype, row, prev, bpp):
    out = bytearray()
    for i, x in enumerate(row):
        a = row[i - bpp] if i >= bpp else 0
        b = prev[i]
        c = prev[i - bpp] if i >= bpp else 0
        pred = (0, a, b, (a + b) // 2, paeth(a, b, c))[ftype]
        out.append((x - pred) & 255)
    return bytes([ftype]) + bytes(out)


def pack_row(samples, depth):
    """samples: list of ints, `depth` bits each, MSB first."""
    if depth == 8:
        return bytes(samples)
    if depth == 16:
        return b"".join(struct.pack(">H", s) for s in samples)
    out, acc, n = bytearray(), 0, 0
    for s in samples:
        acc = (acc << depth) | s
        n += depth
        if n == 8:
            out.append(acc); acc = 0; n = 0
    if n:
        out.append(acc << (8 - n))
    return bytes(out)


ADAM7 = [(0, 0, 8, 8), (4, 0, 8, 8), (0, 4, 4, 8), (2, 0, 4, 4), (0, 2, 2, 4), (1, 0, 2, 2), (0, 1, 1, 2)]


def make_png(w, h, ctype, depth, rows, plte=None, trns=None, interlace=False, filters=(0, 1, 2, 3, 4),
             level=9, strategy=zlib.Z_DEFAULT_STRATEGY, idat_split=0):
    """rows[y] = list of samples (channels interleaved) for row y."""
    chans = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype]
    bpp = max(1, chans * depth // 8)
    raw = bytearray()
    if not interlace:
        prev = bytes(len(pack_row(rows[0], depth)))
        for y in range(h):
            row = pack_row(rows[y], depth)
            raw += filter_row(filters[y % len(filters)], row, prev, bpp)
            prev = row
    else:
        for (x0, y0, dx, dy) in ADAM7:
            ys = range(y0, h, dy)
            xs = list(range(x0, w, dx))
            if not xs or not ys:
                continue
            prev = None
            for n, y in enumerate(ys):
                samples = []
                for x in xs:
                    samples += rows[y][x * chans:(x + 1) * chans]
                row = pack_row(samples, depth)
                if prev is None:
                    prev = bytes(len(row))
                raw += filter_row(filters[n % len(filters)], row, prev, bpp)
                prev = row
    co = zlib.compressobj(level, zlib.DEFLATED, 15, 8, strategy)
    z = co.compress(bytes(raw)) + co.flush()
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, depth, ctype, 0, 0, 1 if interlace else 0))
    if plte:
        png += chunk(b"PLTE", bytes(v for c in plte for v in c))
    if trns is not None:
        png += chunk(b"tRNS", trns)
    if idat_split:
        for i in range(0, len(z), idat_split):
            png += chunk(b"IDAT", z[i:i + idat_split])
    else:
        png += chunk(b"IDAT", z)
    return png + chunk(b"IEND", b"")


def write(name, png, argb):
    with open(os.path.join(OUT, name + ".png"), "wb") as f:
        f.write(png)
    with open(os.path.join(OUT, name + ".argb"), "wb") as f:
        f.write(b"".join(struct.pack(">I", p) for p in argb))
    print(f"{name}: {len(png)} bytes, {len(argb)} px")


def argb(a, r, g, b):
    return (a << 24) | (r << 16) | (g << 8) | b


def scale(v, depth):
    """A sample of `depth` bits to 8 bits, as the decoder does it."""
    if depth == 16:
        return v >> 8
    return v * 255 // ((1 << depth) - 1)


def sample(x, y, i, maxv):
    return (x * 7 + y * 13 + i * 61 + (x * y) % 11) % (maxv + 1)


def pic_rgb(w, h, depth, ctype=2, alpha=False):
    chans = 4 if alpha else 3
    maxv = (1 << depth) - 1
    rows = [[sample(x, y, i, maxv) for x in range(w) for i in range(chans)] for y in range(h)]
    px = []
    for y in range(h):
        for x in range(w):
            s = rows[y][x * chans:(x + 1) * chans]
            px.append(argb(scale(s[3], depth) if alpha else 255, *[scale(v, depth) for v in s[:3]]))
    return rows, px


def main():
    os.makedirs(OUT, exist_ok=True)
    # RGB 8-bit, every filter type cycling, dynamic Huffman (level 9), odd size.
    rows, px = pic_rgb(37, 29, 8)
    write("rgb8-filters", make_png(37, 29, 2, 8, rows), px)
    # Same pixels, Adam7 interlaced.
    write("rgb8-interlaced", make_png(37, 29, 2, 8, rows, interlace=True), px)
    # Same pixels, stored blocks (level 0) and fixed Huffman only, and split over many IDATs.
    write("rgb8-stored", make_png(37, 29, 2, 8, rows, level=0), px)
    write("rgb8-fixed", make_png(37, 29, 2, 8, rows, strategy=zlib.Z_FIXED), px)
    write("rgb8-idat-split", make_png(37, 29, 2, 8, rows, idat_split=50), px)
    # RGBA 8-bit and 16-bit.
    rows, px = pic_rgb(21, 17, 8, 6, alpha=True)
    write("rgba8", make_png(21, 17, 6, 8, rows), px)
    rows, px = pic_rgb(19, 11, 16, 2)
    write("rgb16", make_png(19, 11, 2, 16, rows), px)
    rows, px = pic_rgb(19, 11, 16, 6, alpha=True)
    write("rgba16-interlaced", make_png(19, 11, 6, 16, rows, interlace=True), px)
    # Greyscale at every depth, grey+alpha, and a grey colour key.
    for depth in (1, 2, 4, 8, 16):
        maxv = (1 << depth) - 1
        w, h = 13 + depth, 9
        rows = [[sample(x, y, 0, maxv) for x in range(w)] for y in range(h)]
        px = [argb(255, *[scale(v, depth)] * 3) for r in rows for v in r]
        write(f"gray{depth}", make_png(w, h, 0, depth, rows), px)
    rows = [[sample(x, y, i, 255) for x in range(15) for i in range(2)] for y in range(7)]
    px = [argb(r[2 * x + 1], r[2 * x], r[2 * x], r[2 * x]) for r in rows for x in range(15)]
    write("graya8", make_png(15, 7, 4, 8, rows), px)
    rows = [[((x + y) % 8) * 36 for x in range(12)] for y in range(6)]
    write("gray8-colorkey", make_png(12, 6, 0, 8, rows, trns=struct.pack(">H", 2 * 36)),
          [argb(0 if v == 2 * 36 else 255, v, v, v) for r in rows for v in r])
    # RGB colour key.
    rows = [[((x + y) % 3) * 100 + i for x in range(10) for i in range(3)] for y in range(5)]
    key = (100, 101, 102)
    px = []
    for r in rows:
        for x in range(10):
            c = tuple(r[3 * x:3 * x + 3])
            px.append(argb(0 if c == key else 255, *c))
    write("rgb-colorkey", make_png(10, 5, 2, 8, rows, trns=struct.pack(">HHH", *key)), px)
    # Palettes at 1/2/4/8 bits, with tRNS on the 4- and 8-bit ones, one interlaced.
    for depth in (1, 2, 4, 8):
        n = min(1 << depth, 17)
        pal = [((i * 53) % 256, (i * 97 + 20) % 256, (i * 29 + 200) % 256) for i in range(n)]
        w, h = 16 + depth, 10
        rows = [[(x * 3 + y) % n for x in range(w)] for y in range(h)]
        alpha = [(i * 40 + 15) % 256 for i in range(max(1, n // 2))] if depth >= 4 else None
        trns = bytes(alpha) if alpha else None
        px = []
        for r in rows:
            for v in r:
                a = alpha[v] if alpha and v < len(alpha) else 255
                px.append(argb(a, *pal[v]))
        write(f"pal{depth}", make_png(w, h, 3, depth, rows, plte=pal, trns=trns, interlace=(depth == 4)), px)
    # A bigger, compressible image (long matches, dynamic Huffman with real length/distance codes).
    w, h = 64, 48
    rows = []
    for y in range(h):
        r = []
        for x in range(w):
            v = 255 if (x // 16 + y // 16) % 2 else 40
            r += [v, (v + x) % 256 if y % 7 == 0 else v, 90]
        rows.append(r)
    px = [argb(255, r[3 * x], r[3 * x + 1], r[3 * x + 2]) for r in rows for x in range(w)]
    write("checker", make_png(w, h, 2, 8, rows, filters=(1, 0, 4)), px)
    # 1x1 and a single row/column (interlace passes with empty rows).
    write("one-px", make_png(1, 1, 6, 8, [[1, 2, 3, 4]]), [argb(4, 1, 2, 3)])
    rows, px = pic_rgb(5, 1, 8)
    write("row-interlaced", make_png(5, 1, 2, 8, rows, interlace=True), px)
    rows, px = pic_rgb(1, 6, 8)
    write("col-interlaced", make_png(1, 6, 2, 8, rows, interlace=True), px)


main()
