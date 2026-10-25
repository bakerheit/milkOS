#!/usr/bin/env python3
"""Recompress an 8-bit RGB PNG (stdlib only): png_pack.py IN OUT.

Reads a PNG whose scanlines are unfiltered (what the host tests write, stored uncompressed), applies the Paeth
filter, deflates at level 9 and writes OUT. Used by scripts/lcd-shots.sh so screenshots in the repository are small.
"""
import struct
import sys
import zlib


def chunks(d):
    i = 8
    while i < len(d):
        n, = struct.unpack(">I", d[i:i + 4])
        yield d[i + 4:i + 8], d[i + 8:i + 8 + n]
        i += 12 + n


def write_chunk(kind, body):
    c = kind + body
    return struct.pack(">I", len(body)) + c + struct.pack(">I", zlib.crc32(c) & 0xFFFFFFFF)


def paeth(a, b, c):
    p = a + b - c
    pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
    return a if pa <= pb and pa <= pc else (b if pb <= pc else c)


def main():
    src = open(sys.argv[1], "rb").read()
    assert src[:8] == b"\x89PNG\r\n\x1a\n", "not a PNG"
    w = h = 0
    idat = b""
    for kind, body in chunks(src):
        if kind == b"IHDR":
            w, h = struct.unpack(">II", body[:8])
            assert body[8:] == b"\x08\x02\x00\x00\x00", "expects 8-bit RGB, not interlaced"
        elif kind == b"IDAT":
            idat += body
    raw = zlib.decompress(idat)
    stride = w * 3
    out = bytearray()
    prev = bytes(stride)
    for y in range(h):
        row = raw[y * (stride + 1):(y + 1) * (stride + 1)]
        assert row[0] == 0, "expects unfiltered rows"
        row = row[1:]
        out.append(4)
        for x in range(stride):
            a = row[x - 3] if x >= 3 else 0
            c = prev[x - 3] if x >= 3 else 0
            out.append((row[x] - paeth(a, prev[x], c)) & 255)
        prev = row
    png = b"\x89PNG\r\n\x1a\n" + write_chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
    png += write_chunk(b"IDAT", zlib.compress(bytes(out), 9)) + write_chunk(b"IEND", b"")
    open(sys.argv[2], "wb").write(png)


main()
