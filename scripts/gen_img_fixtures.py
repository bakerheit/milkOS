#!/usr/bin/env python3
"""Write the JPEG, GIF and WebP test images for user/imgdec (tests/fixtures/NAME.jpg|gif|webp plus
NAME.argb, the pixels a reference decoder produced: one big-endian u32 0xAARRGGBB per pixel,
and for animations NAME.N.argb for frame N). The reference decoders are Pillow's libjpeg-turbo,
GIF and libwebp; libjpeg-turbo's cjpeg makes the JPEGs Pillow can't (restart markers, 1x2
sampling). The pictures are drawn here, deterministic, so a re-run writes the same files.

Needs Pillow (tested with 11.3: libjpeg-turbo 3.1, libwebp 1.5) and cjpeg on PATH. Regenerate:
    python3 scripts/gen_img_fixtures.py
"""
import io
import os
import pathlib
import struct
import subprocess
import sys
import tempfile

from PIL import Image, ImageDraw, ImageOps

OUT = pathlib.Path(__file__).resolve().parent.parent / "user/imgdec/tests/fixtures"


def picture(w, h, alpha=False, seed=1):
    """Gradients, shapes, text-like stripes and some noise: something with edges and colour."""
    im = Image.new("RGBA", (w, h))
    px = im.load()
    s = seed
    for y in range(h):
        for x in range(w):
            s = (s * 1103515245 + 12345) & 0x7FFFFFFF
            n = (s >> 16) % 24
            r = (x * 255 // max(1, w - 1) + n) % 256
            g = (y * 255 // max(1, h - 1) + n // 2) % 256
            b = ((x + y) * 128 // max(1, w + h) + 60) % 256
            a = 255 if not alpha else (255 if (x // 8 + y // 8) % 3 else (x * 255 // max(1, w - 1)))
            px[x, y] = (r, g, b, a)
    d = ImageDraw.Draw(im)
    d.ellipse((w // 5, h // 5, w * 3 // 5, h * 4 // 5), fill=(230, 60, 80, 255))
    d.rectangle((w // 2, h // 8, w * 7 // 8, h // 2), fill=(30, 90, 200, 200 if alpha else 255))
    for i in range(0, w, 6):
        d.line((i, h - 6, i + 3, h - 2), fill=(250, 250, 250, 255), width=1)
    return im if alpha else im.convert("RGB")


def argb_bytes(im):
    im = im.convert("RGBA")
    out = bytearray()
    for r, g, b, a in im.getdata():
        out += struct.pack(">I", a << 24 | r << 16 | g << 8 | b)
    return bytes(out)


def write(name, ext, data, decoded):
    (OUT / f"{name}.{ext}").write_bytes(data)
    (OUT / f"{name}.argb").write_bytes(argb_bytes(decoded))


def jpeg(name, im, **kw):
    buf = io.BytesIO()
    im.save(buf, "JPEG", **kw)
    data = buf.getvalue()
    write(name, "jpg", data, Image.open(io.BytesIO(data)))


def cjpeg(name, im, args):
    with tempfile.TemporaryDirectory() as t:
        src = os.path.join(t, "in.ppm")
        im.convert("RGB").save(src)
        data = subprocess.run(["cjpeg", *args, src], check=True, capture_output=True).stdout
    write(name, "jpg", data, Image.open(io.BytesIO(data)))


def exif_with_orientation(o):
    """A minimal EXIF block: IFD0 with one entry, Orientation = o."""
    tiff = b"MM\x00\x2a\x00\x00\x00\x08" + struct.pack(">H", 1)
    tiff += struct.pack(">HHIHH", 0x0112, 3, 1, o, 0) + struct.pack(">I", 0)
    return b"Exif\x00\x00" + tiff


def jpegs():
    p = picture(61, 45)
    jpeg("jpeg-444", p, quality=92, subsampling=0)
    jpeg("jpeg-422", p, quality=92, subsampling=1)
    jpeg("jpeg-420", p, quality=85, subsampling=2)
    jpeg("jpeg-gray", p.convert("L"), quality=90)
    jpeg("jpeg-progressive", p, quality=88, subsampling=2, progressive=True)
    jpeg("jpeg-progressive-444", p, quality=95, subsampling=0, progressive=True)
    jpeg("jpeg-cmyk", p.convert("CMYK"), quality=90)
    jpeg("jpeg-tiny", picture(3, 2), quality=90, subsampling=2)
    jpeg("jpeg-big", picture(320, 200, seed=7), quality=80, subsampling=2, progressive=True)
    cjpeg("jpeg-restart", p, ["-quality", "90", "-restart", "2B"])
    cjpeg("jpeg-progressive-restart", p, ["-quality", "90", "-progressive", "-restart", "1"])
    cjpeg("jpeg-121", p, ["-quality", "90", "-sample", "1x2"])
    cjpeg("jpeg-411", p, ["-quality", "90", "-sample", "4x1"])
    for o in range(1, 9):
        buf = io.BytesIO()
        p.save(buf, "JPEG", quality=90, exif=exif_with_orientation(o))
        data = buf.getvalue()
        shown = ImageOps.exif_transpose(Image.open(io.BytesIO(data)))
        write(f"jpeg-orient{o}", "jpg", data, shown)


def gifs():
    p = picture(40, 30)
    q = p.quantize(colors=64, dither=Image.Dither.NONE)
    buf = io.BytesIO()
    q.save(buf, "GIF", interlace=False)
    data = buf.getvalue()
    write("gif-still", "gif", data, Image.open(io.BytesIO(data)))
    buf = io.BytesIO()
    q.save(buf, "GIF", interlace=True)
    data = buf.getvalue()
    write("gif-interlaced", "gif", data, Image.open(io.BytesIO(data)))
    # Transparency: index 0 is see-through.
    t = q.copy()
    t.info["transparency"] = 0
    buf = io.BytesIO()
    t.save(buf, "GIF", transparency=0)
    data = buf.getvalue()
    write("gif-transparent", "gif", data, Image.open(io.BytesIO(data)))
    # A two-colour picture (minimum code size 2) and a 256-colour one (code size 8, many clears).
    two = Image.new("1", (33, 17))
    d = ImageDraw.Draw(two)
    d.ellipse((2, 2, 30, 15), fill=1)
    buf = io.BytesIO()
    two.save(buf, "GIF")
    data = buf.getvalue()
    write("gif-bilevel", "gif", data, Image.open(io.BytesIO(data)))
    big = picture(160, 120, seed=3).quantize(colors=256, dither=Image.Dither.FLOYDSTEINBERG)
    buf = io.BytesIO()
    big.save(buf, "GIF")
    data = buf.getvalue()
    write("gif-256", "gif", data, Image.open(io.BytesIO(data)))
    anim_gif()
    # A slow, big one (Preview's desktop test watches it change), written by Pillow.
    frames = [picture(120, 90, seed=s).quantize(colors=32, dither=Image.Dither.NONE) for s in (4, 5)]
    frames[1] = frames[1].transpose(Image.Transpose.ROTATE_180)
    buf = io.BytesIO()
    frames[0].save(buf, "GIF", save_all=True, append_images=frames[1:], duration=1500, loop=0)
    data = buf.getvalue()
    (OUT / "gif-anim-slow.gif").write_bytes(data)
    a = Image.open(io.BytesIO(data))
    for i in range(a.n_frames):
        a.seek(i)
        (OUT / f"gif-anim-slow.{i}.argb").write_bytes(argb_bytes(a))


def anim_gif():
    """Four frames on a 32x24 canvas: a full first frame, then small moving squares with
    the three disposal methods, written by hand so the frames are exactly these."""
    w, h = 32, 24
    pal = [(255, 255, 255), (220, 40, 40), (40, 160, 60), (40, 60, 200)] + [(0, 0, 0)] * 4

    def lzw(indices, min_size):
        # Plain LZW with a clear code at the start and no table growth past 12 bits.
        clear, end = 1 << min_size, (1 << min_size) + 1
        size, nxt = min_size + 1, end + 1
        codes = [(clear, size)]
        table = {(i,): i for i in range(clear)}
        cur = ()
        for v in indices:
            if cur + (v,) in table:
                cur = cur + (v,)
                continue
            codes.append((table[cur], size))
            if nxt < 4096:
                table[cur + (v,)] = nxt
                nxt += 1
                if nxt == (1 << size) + 1 and size < 12:
                    size += 1
            cur = (v,)
        codes.append((table[cur], size))
        # The decoder adds one more entry after the last code, which may widen the end code.
        if nxt == 1 << size and size < 12:
            size += 1
        codes.append((end, size))
        out, acc, n = bytearray(), 0, 0
        for c, s in codes:
            acc |= c << n
            n += s
            while n >= 8:
                out.append(acc & 255)
                acc >>= 8
                n -= 8
        if n:
            out.append(acc & 255)
        blocks = bytearray()
        for i in range(0, len(out), 255):
            part = out[i:i + 255]
            blocks += bytes([len(part)]) + part
        return bytes([min_size]) + blocks + b"\x00"

    def frame(x, y, fw, fh, pixels, dispose, delay, transparent=None):
        gce = b"\x21\xF9\x04" + bytes([(dispose << 2) | (1 if transparent is not None else 0)])
        gce += struct.pack("<H", delay) + bytes([transparent or 0]) + b"\x00"
        desc = b"\x2C" + struct.pack("<HHHH", x, y, fw, fh) + b"\x00"
        return gce + desc + lzw(pixels, 3)

    data = b"GIF89a" + struct.pack("<HH", w, h) + bytes([0xF2, 0, 0])
    data += b"".join(bytes(c) for c in pal)
    data += b"\x21\xFF\x0BNETSCAPE2.0\x03\x01" + struct.pack("<H", 0) + b"\x00"
    # Frame 0: stripes over the whole canvas, kept.
    data += frame(0, 0, w, h, [(x // 4 + y // 4) % 2 for y in range(h) for x in range(w)], 1, 20)
    # Frame 1: a red square, cleared to transparent afterwards (disposal 2).
    data += frame(4, 4, 8, 8, [1] * 64, 2, 15)
    # Frame 2: a green square with a see-through hole (index 7), put back afterwards (3).
    hole = [7 if 2 <= x < 6 and 2 <= y < 6 else 2 for y in range(8) for x in range(8)]
    data += frame(12, 8, 8, 8, hole, 3, 0, transparent=7)
    # Frame 3: a blue square, partly off the canvas (clipped).
    data += frame(26, 18, 10, 10, [3] * 100, 1, 50)
    data += b"\x3B"
    (OUT / "gif-anim.gif").write_bytes(data)
    # What browsers show (computed here, not by a library): frame by frame.
    canvas = [[0] * w for _ in range(h)]
    shown = []

    def put(x0, y0, fw, fh, pixels, transparent=None):
        for yy in range(fh):
            for xx in range(fw):
                v = pixels[yy * fw + xx]
                if v == transparent or x0 + xx >= w or y0 + yy >= h:
                    continue
                r, g, b = pal[v]
                canvas[y0 + yy][x0 + xx] = 0xFF000000 | r << 16 | g << 8 | b

    def snap():
        shown.append(b"".join(struct.pack(">I", canvas[y][x]) for y in range(h) for x in range(w)))

    put(0, 0, w, h, [(x // 4 + y // 4) % 2 for y in range(h) for x in range(w)])
    snap()
    put(4, 4, 8, 8, [1] * 64)
    snap()
    for yy in range(4, 12):
        for xx in range(4, 12):
            canvas[yy][xx] = 0
    saved = [row[:] for row in canvas]
    put(12, 8, 8, 8, hole, transparent=7)
    snap()
    canvas = saved
    put(26, 18, 10, 10, [3] * 100)
    snap()
    for i, s in enumerate(shown):
        (OUT / f"gif-anim.{i}.argb").write_bytes(s)


def webp(name, im, **kw):
    buf = io.BytesIO()
    im.save(buf, "WEBP", **kw)
    data = buf.getvalue()
    write(name, "webp", data, Image.open(io.BytesIO(data)))


def webps():
    p = picture(61, 45)
    pa = picture(61, 45, alpha=True)
    webp("webp-lossy", p, quality=80)
    webp("webp-lossy-q20", p, quality=20, method=0)
    webp("webp-lossy-q100", p, quality=100, method=6)
    webp("webp-lossy-alpha", pa, quality=75)
    webp("webp-lossy-alpha-raw", pa, quality=75, alpha_quality=100, alpha_filtering=0)
    webp("webp-lossy-big", picture(333, 211, seed=5), quality=70)
    webp("webp-lossless", p, lossless=True)
    webp("webp-lossless-alpha", pa, lossless=True, exact=True)
    webp("webp-lossless-small-palette", p.quantize(colors=4).convert("RGB"), lossless=True)
    webp("webp-lossless-palette16", p.quantize(colors=16).convert("RGB"), lossless=True)
    webp("webp-lossless-big", picture(300, 190, seed=9), lossless=True, quality=100, method=6)
    webp("webp-tiny", picture(1, 1), quality=90)
    # Animated: three frames of different sizes and blending.
    frames = [picture(40, 30, seed=s) for s in (1, 2, 3)]
    frames[1] = picture(40, 30, alpha=True, seed=2)
    buf = io.BytesIO()
    frames[0].save(buf, "WEBP", save_all=True, append_images=frames[1:], duration=[80, 120, 60],
                   loop=0, lossless=True)
    data = buf.getvalue()
    (OUT / "webp-anim.webp").write_bytes(data)
    a = Image.open(io.BytesIO(data))
    for i in range(a.n_frames):
        a.seek(i)
        (OUT / f"webp-anim.{i}.argb").write_bytes(argb_bytes(a))


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    jpegs()
    gifs()
    webps()
    print("wrote", OUT)


if __name__ == "__main__":
    sys.exit(main())
