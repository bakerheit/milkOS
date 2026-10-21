#!/usr/bin/env python3
"""Decode a PNG with the python3 stdlib (CRC-checked chunks, zlib, all five row filters) and
optionally compare it with another PNG.

  png_check.py A.png                     -> size, colour type, OK
  png_check.py A.png B.png [--at X,Y] [--skip X,Y,W,H ...]
                                         -> also: how many pixels of A differ from B's
                                            pixels at offset X,Y (default 0,0), not
                                            counting the --skip rects (B's coordinates)

Used by scripts/shot-test.sh to check milkOS screenshots against qemu's own screendump.
"""
import struct
import sys
import zlib


def decode(path):
    d = open(path, "rb").read()
    if d[:8] != b"\x89PNG\r\n\x1a\n":
        sys.exit(f"{path}: not a PNG")
    i, idat, hdr = 8, b"", None
    while i < len(d):
        n, = struct.unpack(">I", d[i:i + 4])
        t, body = d[i + 4:i + 8], d[i + 8:i + 8 + n]
        crc, = struct.unpack(">I", d[i + 8 + n:i + 12 + n])
        if zlib.crc32(t + body) & 0xFFFFFFFF != crc:
            sys.exit(f"{path}: bad CRC in {t}")
        if t == b"IHDR":
            hdr = struct.unpack(">IIBBBBB", body)
        elif t == b"IDAT":
            idat += body
        i += 12 + n
    w, h, depth, ctype = hdr[:4]
    if depth != 8 or ctype not in (2, 6):
        sys.exit(f"{path}: depth {depth} colour type {ctype} not handled")
    bpp = 3 if ctype == 2 else 4
    raw = zlib.decompress(idat)
    stride = w * bpp
    rows, prev, k = [], bytearray(stride), 0
    for _ in range(h):
        f = raw[k]
        line = bytearray(raw[k + 1:k + 1 + stride])
        k += 1 + stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 255
        rows.append(bytes(line[i:i + 3]) for i in range(0, stride, bpp))
        rows[-1] = list(rows[-1])
        prev = line
    return w, h, rows


def main():
    args = sys.argv[1:]
    at = (0, 0)
    if "--at" in args:
        k = args.index("--at")
        at = tuple(int(v) for v in args[k + 1].split(","))
        del args[k:k + 2]
    skips = []
    while "--skip" in args:
        k = args.index("--skip")
        skips.append(tuple(int(v) for v in args[k + 1].split(",")))
        del args[k:k + 2]

    def skipped(x, y):
        return any(sx <= x < sx + sw and sy <= y < sy + sh for sx, sy, sw, sh in skips)
    w, h, a = decode(args[0])
    print(f"{args[0]}: {w}x{h} RGB, CRCs and zlib OK")
    if len(args) > 1:
        W, H, b = decode(args[1])
        diff = sum(1 for y in range(h) for x in range(w)
                   if a[y][x] != b[y + at[1]][x + at[0]]
                   and not skipped(x + at[0], y + at[1]))
        print(f"differs from {args[1]} at {at}: {diff} of {w * h} pixels ({100 * diff / (w * h):.2f}%)")
        print(f"DIFF {diff} {w * h}")


if __name__ == "__main__":
    main()
