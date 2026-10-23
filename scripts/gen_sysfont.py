#!/usr/bin/env python3
"""Bake the desktop's system fonts into anti-aliased glyph bitmaps for the kernel and user space.

Usage:
    python3 scripts/gen_sysfont.py --fetch DIR   download the font files below into DIR (an
                                                 empty directory), check them, then bake
    python3 scripts/gen_sysfont.py DIR           bake from files already in DIR

Needs Pillow with FreeType (tested with Pillow 11.3, FreeType 2.13). Writes
kernel/src/sysfont/<family>.font (the glyphs, one self-describing blob per family, plus
symbols.font), kernel/src/sysfont_data.rs (the kernel's index of them) and
user/fontkit/src/generated.rs (family keys, the fallback order and the fold-to-ASCII table).
The .ttf files are not part of the repository: every file is listed below with the URL it
comes from (the Google Fonts repository, pinned to one commit) and its sha256, and the
script refuses files whose hash differs, so a regenerated bake matches the committed one.

Figtree, IBM Plex Sans, Fira Sans and Atkinson Hyperlegible Next are under the SIL Open Font
License 1.1; Ubuntu Sans (the 2023 family) is under the Ubuntu Font Licence 1.0. The licence
texts and copyright lines are in assets/fonts/ (and in the image, /System/Fonts/). The symbols
blob is drawn by this script (simple shapes for the symbols none of the families has).

What gets baked (sizes are the em in pixels, chosen to sit where the 8x16 Unifont cells sat):
    body  13 px in a 16 px line, baseline 13: Regular, Medium, Bold, the TEXT set
    head  26 px in a 32 px line, baseline 26: Regular, Bold, the TEXT set
    big   52 px in a 64 px line, baseline 52: Bold, letters, digits, ':' (clock, tiles, splash)
    huge 104 px in a 128 px line, baseline 104: Regular, digits and ':' (the lock screen clock)
The TEXT set is all of Latin-1 and U+FFFD in every family (stand-ins or a drawn replacement
where a font lacks one), then whatever each font has of: Latin Extended-A and -B, Latin Extended
Additional, spacing modifiers and combining marks, Greek, Cyrillic, general punctuation, super-
and subscripts, currency, letterlike symbols, number forms, arrows, mathematical operators,
technical symbols, geometric shapes, miscellaneous symbols, dingbats and the Latin ligatures.
U+FFFF is a drawn box, the glyph for "no font has this". The coverage of each family is printed.
Glyphs are FreeType's hinted grey coverage (no subpixel), cropped to their ink box and
quantised to 4 bits, stored either as packed nibbles or as nibble runs (value << 4 | len-1),
whichever is smaller. Advances are FreeType's hinted advances, whole pixels. No kerning.

Blob layout (little endian), per family:
    0   "MILKFONT", u16 version (1), u8 faces, u8 key length, u8 name length, 3 bytes 0,
        u32 record count, u32 offset of the records
    24  the key, the name, zero-padded to a multiple of 4
        faces, 16 bytes each: u8 em, u8 line box, u8 baseline, u8 0, u16 weight, u16 0,
        u32 first record, u32 records
    then glyph records, 12 bytes each, sorted by code point within a face:
        u16 code point, u32 data offset from the blob start (bit 31 set = run-length coded),
        u8 width, u8 height, i8 left bearing, i8 top (pixels above the baseline), u8 advance, u8 0
    then the coverage data.
"""
import hashlib
import pathlib
import struct
import sys
import unicodedata
import urllib.parse
import urllib.request

from PIL import Image, ImageDraw, ImageFont

GF = "https://raw.githubusercontent.com/google/fonts/45e3d5269e4a17d823f6d84470bc084dc5ae46f6/"

OFL = "SIL Open Font License 1.1"
UFL = "Ubuntu Font Licence 1.0"

# key, display name, files, (licence file, sha256), licence name, copyright line (None: the
# licence file's first line). Files are {"var": (path, sha256)} for a variable font with a
# wght axis, or {weight: (path, sha256)} for static files. Paths are relative to GF. The
# order is the Settings order; prefs store the key, so new families go at the end.
FAMILIES = [
    (
        "figtree",
        "Figtree",
        {"var": ("ofl/figtree/Figtree[wght].ttf",
                 "26ad3db9b31ff7dde67a91ff515d022d2f495cd506590699cf264f0bfe6fb714")},
        ("ofl/figtree/OFL.txt", "140d37233e7f3ce7313798befa9600893bcceaf41a55fa0fa5ad52f7f657a268"),
        OFL,
        None,
    ),
    (
        "plex",
        "IBM Plex Sans",
        {"var": ("ofl/ibmplexsans/IBMPlexSans[wdth,wght].ttf",
                 "3b031aa4216174205bd8471f88a49b91f093169e9e87bd5262242bc5967fe2e3")},
        ("ofl/ibmplexsans/OFL.txt", "7e6b2818edbd8f6a01ae80641cc8f16a51080d08fb4e532be3a0b6f74adb07da"),
        OFL,
        None,
    ),
    (
        "fira",
        "Fira Sans",
        {
            400: ("ofl/firasans/FiraSans-Regular.ttf",
                  "c29556a2719bf613ef3d5e070e40d903a8965d9c081beca1375dc1e6e0f93c23"),
            500: ("ofl/firasans/FiraSans-Medium.ttf",
                  "cbc1842cbed8c1d1146ba7c9db97d8f28c9bedfd25f41c5b0e1259ca48622328"),
            700: ("ofl/firasans/FiraSans-Bold.ttf",
                  "a4d8e149ecdd4874a0726eb0af894488b3b31c423d6b0017c8f415ed1b795b45"),
        },
        ("ofl/firasans/OFL.txt", "8f24842e9174beda18a556c2ae7d54f5dc444340c19a3a9ef77e23bca366adbd"),
        OFL,
        None,
    ),
    (
        "atkinson",
        "Atkinson Hyperlegible Next",
        {"var": ("ofl/atkinsonhyperlegiblenext/AtkinsonHyperlegibleNext[wght].ttf",
                 "5a455d1cfa099b601ab70751bb9673e8fe1854dc4500c80e1a220d0d75e31745")},
        ("ofl/atkinsonhyperlegiblenext/OFL.txt",
         "aca6a428580965d2297d1b718042dd427c2a9443ece3b0d02d758e161e0c4030"),
        OFL,
        None,
    ),
    (
        "ubuntu",
        "Ubuntu Sans",
        {"var": ("ufl/ubuntusans/UbuntuSans[wdth,wght].ttf",
                 "37e90bba45a2a1172d9f6078e45daebaa1b4e537943205cc1309f142a254fe40")},
        ("ufl/ubuntusans/LICENCE.txt",
         "2f0015108d68627bd788d313f529c21ff4da2c2c42a5e1f3883acc83480f9002"),
        UFL,
        # From the family's METADATA.pb in the same directory (the licence has none).
        "Copyright 2011, 2022, 2023 Canonical Ltd.",
    ),
]

LATIN1 = list(range(0x20, 0x7F)) + list(range(0xA0, 0x100)) + [0xFFFD]
BIG = [ord(c) for c in " 0123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"]
DIGITS = [ord(c) for c in " 0123456789:"]
BOX = 0xFFFF

# Unicode blocks baked wherever a family has the character (name, first, last).
BLOCKS = [
    ("Latin Extended-A", 0x0100, 0x017F),
    ("Latin Extended-B", 0x0180, 0x024F),
    ("Spacing modifiers", 0x02B0, 0x02FF),
    ("Combining marks", 0x0300, 0x036F),
    ("Greek", 0x0370, 0x03FF),
    ("Cyrillic", 0x0400, 0x04FF),
    ("Latin Extended Additional", 0x1E00, 0x1EFF),
    ("General punctuation", 0x2000, 0x206F),
    ("Super- and subscripts", 0x2070, 0x209F),
    ("Currency", 0x20A0, 0x20CF),
    ("Letterlike", 0x2100, 0x214F),
    ("Number forms", 0x2150, 0x218F),
    ("Arrows", 0x2190, 0x21FF),
    ("Mathematical operators", 0x2200, 0x22FF),
    ("Technical", 0x2300, 0x23FF),
    ("Geometric shapes", 0x25A0, 0x25FF),
    ("Miscellaneous symbols", 0x2600, 0x26FF),
    ("Dingbats", 0x2700, 0x27BF),
    ("Ligatures", 0xFB00, 0xFB06),
]
# Characters that take no room and are never drawn (fontkit::ignorable has the same list).
IGNORABLE = set(range(0x200B, 0x2010)) | set(range(0x2028, 0x202F)) | set(range(0x2060, 0x2070)) | {0xFEFF}

# name, em px, line box, baseline, weights, code points (None: the TEXT set). Order matters.
FACES = [
    ("body", 13, 16, 13, (400, 500, 700), None),
    ("head", 26, 32, 26, (400, 700), None),
    ("big", 52, 64, 52, (700,), BIG),
    ("huge", 104, 128, 104, (400,), DIGITS),
]

# Characters a font lacks, drawn with a look-alike it has.
STAND_INS = {0xB5: 0x3BC}  # micro sign -> Greek mu

ROOT = pathlib.Path(__file__).resolve().parent.parent


def sha256(p):
    return hashlib.sha256(p.read_bytes()).hexdigest()


def all_files():
    for _, _, files, lic, _, _ in FAMILIES:
        yield from files.values()
        yield lic


def fetch(d):
    d.mkdir(parents=True, exist_ok=True)
    for path, _ in all_files():
        dst = d / path
        dst.parent.mkdir(parents=True, exist_ok=True)
        url = GF + urllib.parse.quote(path)
        print(f"fetch {url}")
        with urllib.request.urlopen(url) as r:
            dst.write_bytes(r.read())


def check(d):
    for path, want in all_files():
        got = sha256(d / path)
        if got != want:
            sys.exit(f"{path}: sha256 {got}, expected {want}")


def cmap(path):
    """The code points a TrueType font maps (its format 4 or 12 cmap)."""
    b = path.read_bytes()
    n = struct.unpack(">H", b[4:6])[0]
    tabs = {}
    for i in range(n):
        tag, _, off, _ = struct.unpack(">4sIII", b[12 + 16 * i:28 + 16 * i])
        tabs[tag] = off
    c = tabs[b"cmap"]
    best = None
    for i in range(struct.unpack(">H", b[c + 2:c + 4])[0]):
        _, _, off = struct.unpack(">HHI", b[c + 4 + 8 * i:c + 12 + 8 * i])
        fmt = struct.unpack(">H", b[c + off:c + off + 2])[0]
        if fmt == 12 or (fmt == 4 and best is None):
            best = (fmt, c + off)
    fmt, o = best
    cps = set()
    if fmt == 12:
        for g in range(struct.unpack(">I", b[o + 12:o + 16])[0]):
            s, e, _ = struct.unpack(">III", b[o + 16 + 12 * g:o + 28 + 12 * g])
            cps.update(range(s, e + 1))
    else:
        seg = struct.unpack(">H", b[o + 6:o + 8])[0] // 2
        ends = struct.unpack(f">{seg}H", b[o + 14:o + 14 + 2 * seg])
        starts = struct.unpack(f">{seg}H", b[o + 16 + 2 * seg:o + 16 + 4 * seg])
        for s, e in zip(starts, ends):
            if s != 0xFFFF:
                cps.update(range(s, e + 1))
    return cps


def coverage(d, files):
    cov = None
    for path, _ in files.values():
        c = cmap(d / path)
        cov = c if cov is None else cov & c
    return cov


def load(d, files, weight, size):
    if "var" in files:
        f = ImageFont.truetype(str(d / files["var"][0]), size)
        axes = f.get_variation_axes()
        f.set_variation_by_axes([weight if a["name"] == b"Weight" else a["default"] for a in axes])
        return f
    return ImageFont.truetype(str(d / files[weight][0]), size)


def has_glyph(f, cp):
    notdef = f.getmask("\U000E0100")
    m = f.getmask(chr(cp))
    return cp in (0x20, 0xA0) or (m.size, bytes(m)) != (notdef.size, bytes(notdef))


def render(f, cp, size):
    """(coverage image cropped to ink or None, left, top, advance)."""
    pad = size * 2
    im = Image.new("L", (size * 5, size * 4), 0)
    ImageDraw.Draw(im).text((pad, pad * 3 // 2), chr(cp), font=f, fill=255, anchor="ls")
    adv = round(f.getlength(chr(cp)))
    box = im.getbbox()
    if box is None:
        return None, 0, 0, adv
    return im.crop(box), box[0] - pad, pad * 3 // 2 - box[1], adv


def replacement(f, bold, size):
    """U+FFFD for fonts that lack it: a diamond with a question mark knocked out."""
    _, _, _, qadv = render(f, ord("?"), size)
    cap = size * 7 // 10
    h = cap + size // 4
    w = h
    s = 4
    big = Image.new("L", (w * s, h * s), 0)
    ImageDraw.Draw(big).polygon(
        [(w * s / 2, 0), (w * s, h * s / 2), (w * s / 2, h * s), (0, h * s / 2)], fill=255)
    im = big.resize((w, h), Image.BOX)
    q, ql, qt, _ = render(bold, ord("?"), max(size * 3 // 4, 7))
    if q is not None:
        im.paste(0, ((w - q.width) // 2, (h - q.height) // 2), q)
    top = (cap + h) // 2
    return im, max(1, qadv // 6), top, w + 2 * max(1, qadv // 6)


# ---- drawn glyphs: the missing-glyph box and the symbols ----------------------------------

SS = 8  # supersampling


def stroke_for(size, weight):
    return max(1.0, size / 13 * (1.0 if weight < 500 else 1.35 if weight < 700 else 1.7))


def drawn(size, weight, w_em, paint):
    """A glyph `w_em` ems wide drawn by `paint(draw, S, cap, stroke)` on a supersampled canvas
    whose origin is the pen at the baseline (y grows down, so the cap height is at -cap)."""
    cap = size * 0.7
    adv = max(3, round(size * w_em))
    W, H = adv * SS, int(size * 1.4) * SS
    base = int(size * 1.1) * SS
    im = Image.new("L", (W, H), 0)
    dr = ImageDraw.Draw(im)
    paint(dr, lambda x, y: (x * SS, base - y * SS), cap, stroke_for(size, weight) * SS)
    small = im.resize((adv, H // SS), Image.BOX)
    box = small.getbbox()
    if box is None:
        return None, 0, 0, adv
    return small.crop(box), box[0], base // SS - box[1], adv


def box_glyph(size, weight):
    def paint(dr, P, cap, st):
        x0, x1 = size * 0.08, size * 0.08 + size * 0.48
        dr.rectangle([P(x0, cap), P(x1, 0)], outline=255, width=max(SS, round(st)))
    return drawn(size, weight, 0.64, paint)


def poly(dr, pts, fill=True, st=SS):
    if fill:
        dr.polygon(pts, fill=255)
    else:
        dr.polygon(pts, outline=255, width=max(SS, round(st)))


def symbols(size, weight):
    """Code point -> drawing function (draw, P, cap, stroke) and width in ems."""
    mid = 0.35  # the vertical centre of capitals, in ems

    def shape(cp, w_em, fn):
        return cp, w_em, fn

    def tri(direction, small, filled):
        def fn(dr, P, cap, st):
            r = size * (0.22 if small else 0.32)
            cx, cy = size * 0.4, size * mid
            pts = {
                "u": [(cx, cy + r), (cx + r, cy - r * 0.8), (cx - r, cy - r * 0.8)],
                "d": [(cx, cy - r), (cx + r, cy + r * 0.8), (cx - r, cy + r * 0.8)],
                "r": [(cx + r, cy), (cx - r * 0.8, cy + r), (cx - r * 0.8, cy - r)],
                "l": [(cx - r, cy), (cx + r * 0.8, cy + r), (cx + r * 0.8, cy - r)],
            }[direction]
            poly(dr, [P(x, y) for x, y in pts], filled, st)
        return fn

    def square(r_em, filled):
        def fn(dr, P, cap, st):
            r = size * r_em
            cx, cy = size * 0.4, size * mid
            poly(dr, [P(cx - r, cy + r), P(cx + r, cy + r), P(cx + r, cy - r), P(cx - r, cy - r)], filled, st)
        return fn

    def circle(r_em, filled):
        def fn(dr, P, cap, st):
            r = size * r_em
            cx, cy = size * 0.4, size * mid
            (x0, y0), (x1, y1) = P(cx - r, cy + r), P(cx + r, cy - r)
            if filled:
                dr.ellipse([x0, y0, x1, y1], fill=255)
            else:
                dr.ellipse([x0, y0, x1, y1], outline=255, width=max(SS, round(st)))
        return fn

    def diamond(filled):
        def fn(dr, P, cap, st):
            r = size * 0.32
            cx, cy = size * 0.4, size * mid
            poly(dr, [P(cx, cy + r), P(cx + r * 0.8, cy), P(cx, cy - r), P(cx - r * 0.8, cy)], filled, st)
        return fn

    def star(filled):
        def fn(dr, P, cap, st):
            import math
            R, r = size * 0.38, size * 0.16
            cx, cy = size * 0.42, size * (mid + 0.02)
            pts = []
            for i in range(10):
                a = math.pi / 2 + i * math.pi / 5
                rr = R if i % 2 == 0 else r
                pts.append(P(cx + rr * math.cos(a), cy + rr * math.sin(a)))
            poly(dr, pts, filled, st)
        return fn

    def heart(filled):
        def fn(dr, P, cap, st):
            import math
            pts = []
            for i in range(64):
                t = i / 64 * 2 * math.pi
                x = 16 * math.sin(t) ** 3
                y = 13 * math.cos(t) - 5 * math.cos(2 * t) - 2 * math.cos(3 * t) - math.cos(4 * t)
                pts.append(P(size * 0.42 + x * size * 0.021, size * (mid + 0.02) + y * size * 0.021))
            poly(dr, pts, filled, st)
        return fn

    def club(dr, P, cap, st):
        r = size * 0.13
        cx, cy = size * 0.42, size * mid
        for dx, dy in ((0, 0.13), (-0.14, -0.04), (0.14, -0.04)):
            (x0, y0), (x1, y1) = P(cx + dx * size - r, cy + dy * size + r), P(cx + dx * size + r, cy + dy * size - r)
            dr.ellipse([x0, y0, x1, y1], fill=255)
        poly(dr, [P(cx - size * 0.03, cy), P(cx + size * 0.03, cy), P(cx + size * 0.12, cy - size * 0.33), P(cx - size * 0.12, cy - size * 0.33)])

    def spade(dr, P, cap, st):
        import math
        pts = []
        for i in range(64):
            t = i / 64 * 2 * math.pi
            x = 16 * math.sin(t) ** 3
            y = 13 * math.cos(t) - 5 * math.cos(2 * t) - 2 * math.cos(3 * t) - math.cos(4 * t)
            pts.append(P(size * 0.42 + x * size * 0.019, size * (mid + 0.05) - y * size * 0.019))
        poly(dr, pts)
        cx, cy = size * 0.42, size * mid
        poly(dr, [P(cx - size * 0.03, cy), P(cx + size * 0.03, cy), P(cx + size * 0.12, cy - size * 0.33), P(cx - size * 0.12, cy - size * 0.33)])

    def lines(segs, w_em=0.8):
        def fn(dr, P, cap, st):
            for (x0, y0, x1, y1) in segs:
                dr.line([P(x0 * size, y0 * size), P(x1 * size, y1 * size)], fill=255, width=max(SS, round(st * 1.2)))
        return fn

    def check_mark(heavy):
        def fn(dr, P, cap, st):
            k = 1.6 if heavy else 1.15
            dr.line([P(0.1 * size, 0.33 * size), P(0.3 * size, 0.1 * size), P(0.72 * size, 0.66 * size)],
                    fill=255, width=max(SS, round(st * k)), joint="curve")
        return fn

    def cross(heavy):
        def fn(dr, P, cap, st):
            k = 1.7 if heavy else 1.15
            for a, b in (((0.14, 0.12), (0.62, 0.6)), ((0.14, 0.6), (0.62, 0.12))):
                dr.line([P(a[0] * size, a[1] * size), P(b[0] * size, b[1] * size)], fill=255, width=max(SS, round(st * k)))
        return fn

    def ballot(mark):
        def fn(dr, P, cap, st):
            x0, x1, y0, y1 = 0.08 * size, 0.7 * size, 0.0, 0.62 * size
            dr.rectangle([P(x0, y1), P(x1, y0)], outline=255, width=max(SS, round(st)))
            if mark == "v":
                dr.line([P(0.2 * size, 0.33 * size), P(0.34 * size, 0.16 * size), P(0.6 * size, 0.5 * size)],
                        fill=255, width=max(SS, round(st * 1.2)), joint="curve")
            elif mark == "x":
                for a, b in (((0.22, 0.12), (0.56, 0.5)), ((0.22, 0.5), (0.56, 0.12))):
                    dr.line([P(a[0] * size, a[1] * size), P(b[0] * size, b[1] * size)], fill=255, width=max(SS, round(st * 1.2)))
        return fn

    def trigram(dr, P, cap, st):
        for y in (0.1, 0.33, 0.56):
            dr.line([P(0.1 * size, y * size), P(0.7 * size, y * size)], fill=255, width=max(SS, round(st * 1.3)))

    def command(dr, P, cap, st):
        # Four rings joined by a square: the Mac's command key.
        r = size * 0.11
        a, b = size * 0.22, size * 0.52
        w = max(SS, round(st))
        for cx, cy in ((a, a - 0.06 * size), (b, a - 0.06 * size), (a, b - 0.06 * size), (b, b - 0.06 * size)):
            (x0, y0), (x1, y1) = P(cx - r, cy + r), P(cx + r, cy - r)
            dr.ellipse([x0, y0, x1, y1], outline=255, width=w)
        dr.rectangle([P(a, b - 0.06 * size), P(b, a - 0.06 * size)], outline=255, width=w)

    def option(dr, P, cap, st):
        w = max(SS, round(st * 1.1))
        dr.line([P(0.06 * size, 0.56 * size), P(0.26 * size, 0.56 * size), P(0.52 * size, 0.08 * size), P(0.74 * size, 0.08 * size)], fill=255, width=w)
        dr.line([P(0.46 * size, 0.56 * size), P(0.74 * size, 0.56 * size)], fill=255, width=w)

    def caret(dr, P, cap, st):
        dr.line([P(0.12 * size, 0.3 * size), P(0.4 * size, 0.6 * size), P(0.68 * size, 0.3 * size)], fill=255, width=max(SS, round(st * 1.2)))

    def arrow(direction, double=False):
        def fn(dr, P, cap, st):
            w = max(SS, round(st * 1.1))
            y, x0, x1, hd = mid * size, 0.08 * size, 0.82 * size, 0.2 * size
            if double:
                for dy in (-0.09, 0.09):
                    dr.line([P(x0 + (0.1 * size if direction != "r" else 0), y + dy * size), P(x1 - (0.1 * size if direction != "l" else 0), y + dy * size)], fill=255, width=w)
            else:
                dr.line([P(x0, y), P(x1, y)], fill=255, width=w)
            if direction in ("r", "lr"):
                dr.line([P(x1 - hd, y + hd), P(x1, y), P(x1 - hd, y - hd)], fill=255, width=w, joint="curve")
            if direction in ("l", "lr"):
                dr.line([P(x0 + hd, y + hd), P(x0, y), P(x0 + hd, y - hd)], fill=255, width=w, joint="curve")
        return fn

    table = [
        shape(0x21D0, 0.9, arrow("l", True)),
        shape(0x21D2, 0.9, arrow("r", True)),
        shape(0x21D4, 0.9, arrow("lr", True)),
        shape(0x2303, 0.8, caret),
        shape(0x2318, 0.78, command),
        shape(0x2325, 0.8, option),
        shape(0x25A0, 0.8, square(0.3, True)),
        shape(0x25A1, 0.8, square(0.3, False)),
        shape(0x25AA, 0.6, square(0.16, True)),
        shape(0x25AB, 0.6, square(0.16, False)),
        shape(0x25B2, 0.8, tri("u", False, True)),
        shape(0x25B3, 0.8, tri("u", False, False)),
        shape(0x25B4, 0.6, tri("u", True, True)),
        shape(0x25B5, 0.6, tri("u", True, False)),
        shape(0x25B6, 0.8, tri("r", False, True)),
        shape(0x25B7, 0.8, tri("r", False, False)),
        shape(0x25B8, 0.6, tri("r", True, True)),
        shape(0x25B9, 0.6, tri("r", True, False)),
        shape(0x25BA, 0.8, tri("r", False, True)),
        shape(0x25BC, 0.8, tri("d", False, True)),
        shape(0x25BD, 0.8, tri("d", False, False)),
        shape(0x25BE, 0.6, tri("d", True, True)),
        shape(0x25BF, 0.6, tri("d", True, False)),
        shape(0x25C0, 0.8, tri("l", False, True)),
        shape(0x25C1, 0.8, tri("l", False, False)),
        shape(0x25C2, 0.6, tri("l", True, True)),
        shape(0x25C3, 0.6, tri("l", True, False)),
        shape(0x25C4, 0.8, tri("l", False, True)),
        shape(0x25C6, 0.8, diamond(True)),
        shape(0x25C7, 0.8, diamond(False)),
        shape(0x25CB, 0.8, circle(0.3, False)),
        shape(0x25CF, 0.8, circle(0.3, True)),
        shape(0x25E6, 0.5, circle(0.12, False)),
        shape(0x2605, 0.84, star(True)),
        shape(0x2606, 0.84, star(False)),
        shape(0x2610, 0.8, ballot(None)),
        shape(0x2611, 0.8, ballot("v")),
        shape(0x2612, 0.8, ballot("x")),
        shape(0x2630, 0.8, trigram),
        shape(0x2660, 0.84, spade),
        shape(0x2663, 0.84, club),
        shape(0x2661, 0.84, heart(False)),
        shape(0x2665, 0.84, heart(True)),
        shape(0x2662, 0.8, diamond(False)),
        shape(0x2666, 0.8, diamond(True)),
        shape(0x2713, 0.8, check_mark(False)),
        shape(0x2714, 0.8, check_mark(True)),
        shape(0x2715, 0.76, cross(False)),
        shape(0x2716, 0.76, cross(True)),
        shape(0x2717, 0.76, cross(False)),
        shape(0x2718, 0.76, cross(True)),
        shape(0x2764, 0.84, heart(True)),
        shape(0x2794, 0.9, arrow("r")),
        shape(0x279C, 0.9, arrow("r")),
        shape(0x27A1, 0.9, arrow("r")),
    ]
    return {cp: (w, fn) for cp, w, fn in table}


# ---- encoding -------------------------------------------------------------------------------

def quant(im):
    return [(v * 15 + 127) // 255 for v in im.getdata()]


def encode(px):
    """(bytes, rle?) for a list of 4-bit values."""
    raw = bytearray()
    for i in range(0, len(px), 2):
        a = px[i]
        b = px[i + 1] if i + 1 < len(px) else 0
        raw.append(a << 4 | b)
    rle = bytearray()
    i = 0
    while i < len(px):
        j = i
        while j < len(px) and j - i < 16 and px[j] == px[i]:
            j += 1
        rle.append(px[i] << 4 | (j - i - 1))
        i = j
    return (bytes(rle), True) if len(rle) < len(raw) else (bytes(raw), False)


def blob(key, name, faces_out):
    """faces_out: [(em, line, ascent, weight, [(cp, im, left, top, adv)])] -> bytes."""
    kb, nb = key.encode(), name.encode()
    head_len = 24 + len(kb) + len(nb)
    head_len += (-head_len) % 4
    nrec = sum(len(g) for *_, g in faces_out)
    rec_off = head_len + 16 * len(faces_out)
    data_off = rec_off + 12 * nrec
    head = b"MILKFONT" + struct.pack("<HBBB3xII", 1, len(faces_out), len(kb), len(nb), nrec, rec_off)
    head += kb + nb
    head += b"\0" * (head_len - len(head))
    faces = bytearray()
    records = bytearray()
    data = bytearray()
    first = 0
    for em, line, ascent, weight, glyphs in faces_out:
        faces += struct.pack("<BBBxHxxII", em, line, ascent, weight, first, len(glyphs))
        first += len(glyphs)
        last = -1
        for cp, im, left, top, adv in glyphs:
            assert cp > last, (key, cp)
            last = cp
            if im is None:
                w = h = 0
                enc, rle = b"", False
            else:
                w, h = im.size
                enc, rle = encode(quant(im))
            assert w < 256 and h < 256 and -128 <= left < 128 and -128 <= top < 128, (key, cp)
            assert 0 <= adv < 256, (key, cp, adv)
            o = (data_off + len(data)) | (1 << 31 if rle else 0)
            records += struct.pack("<HIBBbbBx", cp, o, w, h, left, top, adv)
            data += enc
    out = head + bytes(faces) + bytes(records) + bytes(data)
    assert len(head) + len(faces) == rec_off
    return out


def text_set(cov):
    extra = []
    for _, lo, hi in BLOCKS:
        extra += [c for c in range(lo, hi + 1) if c in cov and c not in IGNORABLE]
    return sorted(set(LATIN1) | set(extra)) + [BOX]


def bake_family(d, key, name, files, cov):
    faces_out = []
    for fname, size, line, ascent, weights, cps in FACES:
        for wt in weights:
            f = load(d, files, wt, size)
            bold = load(d, files, 700, size)
            glyphs = []
            for cp in (cps if cps is not None else text_set(cov)):
                if cp == BOX:
                    glyphs.append((cp, *box_glyph(size, wt)))
                    continue
                src = cp
                if cp in LATIN1 and not has_glyph(f, cp):
                    src = STAND_INS.get(cp, 0xFFFD)
                if src == 0xFFFD and not has_glyph(f, 0xFFFD):
                    glyphs.append((cp, *replacement(f, bold, size)))
                elif cp in LATIN1 and not has_glyph(f, src):
                    sys.exit(f"{key}: no glyph for U+{cp:04X}")
                else:
                    glyphs.append((cp, *render(f, src, size)))
            faces_out.append((size, line, ascent, wt, glyphs))
    return blob(key, name, faces_out), sum(len(g) for *_, g in faces_out)


def bake_symbols(have):
    """The drawn symbols for code points no family has."""
    faces_out = []
    for fname, size, line, ascent, weights, cps in FACES[:2]:
        for wt in weights:
            table = symbols(size, wt)
            glyphs = []
            for cp in sorted(table):
                if cp in have:
                    continue
                w_em, fn = table[cp]
                glyphs.append((cp, *drawn(size, wt, w_em, fn)))
            glyphs.append((BOX, *box_glyph(size, wt)))
            faces_out.append((size, line, ascent, wt, glyphs))
    return blob("symbols", "milkOS Symbols", faces_out), [g[0] for g in faces_out[0][4]][:-1]


# ---- fold to ASCII --------------------------------------------------------------------------

MANUAL_FOLD = {
    0x2010: "-", 0x2011: "-", 0x2012: "-", 0x2013: "-", 0x2014: "-", 0x2015: "-",
    0x2016: "||", 0x2018: "'", 0x2019: "'", 0x201A: ",", 0x201B: "'", 0x201C: '"', 0x201D: '"',
    0x201E: '"', 0x201F: '"', 0x2020: "+", 0x2021: "+", 0x2022: "*", 0x2023: ">", 0x2026: "...",
    0x2027: "-", 0x2030: "%o", 0x2032: "'", 0x2033: '"', 0x2034: "'''", 0x2039: "<", 0x203A: ">",
    0x203B: "*", 0x2043: "-", 0x2044: "/", 0x204E: "*", 0x2052: "%", 0x2053: "~",
    0x20AC: "EUR", 0x20A4: "L", 0x20A9: "W", 0x20AA: "NIS", 0x20B9: "Rs", 0x20BD: "RUB",
    0x20BF: "BTC", 0x2116: "No", 0x2122: "TM", 0x2117: "(P)",
    0x2190: "<-", 0x2191: "^", 0x2192: "->", 0x2193: "v", 0x2194: "<->", 0x2195: "|",
    0x21D0: "<=", 0x21D2: "=>", 0x21D4: "<=>", 0x21B5: "<-'", 0x21E6: "<=", 0x21E8: "=>",
    0x2212: "-", 0x2215: "/", 0x2216: "\\", 0x2217: "*", 0x2219: "*", 0x221A: "v", 0x221E: "oo",
    0x2223: "|", 0x2225: "||", 0x2227: "^", 0x2228: "v", 0x2236: ":", 0x223C: "~", 0x2248: "~",
    0x2260: "!=", 0x2261: "==", 0x2264: "<=", 0x2265: ">=", 0x226A: "<<", 0x226B: ">>",
    0x22C5: "*", 0x22EF: "...", 0x2302: "^", 0x2303: "^", 0x2318: "Cmd", 0x2325: "Opt",
    0x2329: "<", 0x232A: ">", 0x2500: "-", 0x2502: "|", 0x250C: "+", 0x2510: "+", 0x2514: "+",
    0x2518: "+", 0x251C: "+", 0x2524: "+", 0x252C: "+", 0x2534: "+", 0x253C: "+",
    0x25A0: "#", 0x25A1: "[]", 0x25AA: "#", 0x25B2: "^", 0x25B6: ">", 0x25BA: ">", 0x25BC: "v",
    0x25C0: "<", 0x25C4: "<", 0x25C6: "*", 0x25CB: "o", 0x25CF: "*", 0x25E6: "o",
    0x2605: "*", 0x2606: "*", 0x2610: "[ ]", 0x2611: "[x]", 0x2612: "[x]", 0x2630: "=",
    0x2713: "v", 0x2714: "v", 0x2715: "x", 0x2716: "x", 0x2717: "x", 0x2718: "x",
    0x2764: "<3", 0x2794: "->", 0x279C: "->", 0x27A1: "->", 0x27E8: "<", 0x27E9: ">",
    0x3000: " ", 0x3001: ",", 0x3002: ".", 0xFEFF: "",
}


def fold_table(have):
    """cp -> string of characters some family has, for characters none has."""
    out = {}
    ranges = list(range(0xA0, 0x3000)) + list(range(0xFB00, 0xFB50)) + list(range(0xFF00, 0xFFF0))
    for cp in ranges:
        if cp in have or cp in IGNORABLE or 0xD800 <= cp < 0xE000:
            continue
        s = MANUAL_FOLD.get(cp)
        if s is None:
            dec = unicodedata.normalize("NFKD", chr(cp))
            dec = "".join(c for c in dec if not unicodedata.combining(c))
            if dec == chr(cp) or not dec:
                continue
            s = dec
        # Fold to what can be drawn: characters some family has (folding again if needed).
        t = ""
        for c in s:
            if ord(c) in have:
                t += c
            elif ord(c) in MANUAL_FOLD and all(ord(x) in have for x in MANUAL_FOLD[ord(c)]):
                t += MANUAL_FOLD[ord(c)]
            else:
                t = None
                break
        if t is not None and len(t.encode()) <= 255:
            out[cp] = t
    for cp, s in MANUAL_FOLD.items():
        if cp not in have and cp not in out and all(ord(c) in have for c in s):
            out[cp] = s
    return dict(sorted(out.items()))


def rust_str(s):
    return '"' + "".join(c if c.isprintable() and c not in '"\\' and ord(c) < 0x80 else f"\\u{{{ord(c):x}}}" for c in s) + '"'


def copyright_line(d, lic):
    for line in (d / lic).read_text(encoding="utf-8").splitlines():
        if line.strip():
            return line.strip()
    return ""


def main():
    args = sys.argv[1:]
    if len(args) == 2 and args[0] == "--fetch":
        d = pathlib.Path(args[1]).resolve()
        fetch(d)
    elif len(args) == 1:
        d = pathlib.Path(args[0]).resolve()
    else:
        sys.exit(__doc__)
    check(d)
    outdir = ROOT / "kernel/src/sysfont"
    outdir.mkdir(exist_ok=True)
    for old in outdir.glob("*.bin"):
        old.unlink()
    covs = {key: coverage(d, files) for key, _, files, *_ in FAMILIES}
    baked = {}
    sizes = {}
    have = set()
    for key, name, files, *_ in FAMILIES:
        b, n = bake_family(d, key, name, files, covs[key])
        (outdir / f"{key}.font").write_bytes(b)
        sizes[key] = len(b)
        baked[key] = set(text_set(covs[key]))
        have |= baked[key]
        print(f"{key}: {len(b)} bytes, {n} glyphs")
    sb, drawn_cps = bake_symbols(have)
    (outdir / "symbols.font").write_bytes(sb)
    sizes["symbols"] = len(sb)
    print(f"symbols: {len(sb)} bytes, {len(drawn_cps)} drawn symbols")
    have |= set(drawn_cps)
    have.discard(BOX)
    print(f"total {sum(sizes.values())} bytes")
    # Coverage per block.
    print(f"{'':28}" + "".join(f"{k:>10}" for k, *_ in FAMILIES) + f"{'any':>10}")
    for bname, lo, hi in [("Latin-1", 0xA0, 0xFF)] + BLOCKS:
        assigned = [c for c in range(lo, hi + 1) if unicodedata.category(chr(c)) != "Cn" and c not in IGNORABLE]
        row = f"{bname:28}"
        for key, *_ in FAMILIES:
            row += f"{sum(c in baked[key] for c in assigned):>6}/{len(assigned):<3}"
        row += f"{sum(c in have for c in assigned):>6}/{len(assigned):<3}"
        print(row)
    # Fallback: the family with the most glyphs first, the symbols last.
    order = sorted(range(len(FAMILIES)), key=lambda i: -len(baked[FAMILIES[i][0]]))
    fold = fold_table(have)
    print(f"fold table: {len(fold)} entries")
    # The kernel's index.
    rs = [
        "// @generated by scripts/gen_sysfont.py. Do not edit by hand.",
        "// Baked from fonts under the SIL OFL 1.1 and the Ubuntu Font Licence 1.0, see assets/fonts/.",
        "",
        "use crate::sysfont::Family;",
        "",
        f"pub static FAMILIES: [Family; {len(FAMILIES)}] = [",
    ]
    for key, name, files, lic, licence, copyright in FAMILIES:
        cr = (copyright or copyright_line(d, lic[0])).replace("\\", "\\\\").replace('"', '\\"')
        rs.append("    Family {")
        rs.append(f'        key: "{key}",')
        rs.append(f'        name: "{name}",')
        rs.append(f'        copyright: "{cr}",')
        rs.append(f'        licence: "{licence}",')
        rs.append(f'        font: fontkit::Family::baked(include_bytes!("sysfont/{key}.font")),')
        rs.append("    },")
    rs += [
        "];",
        "",
        "/// The drawn symbols no family has (fontkit::SYMBOLS in the fallback order).",
        'pub static SYMBOLS: fontkit::Family<\'static> =',
        '    fontkit::Family::baked(include_bytes!("sysfont/symbols.font"));',
        "",
    ]
    (ROOT / "kernel/src/sysfont_data.rs").write_text("\n".join(rs))
    # fontkit's tables.
    g = [
        "// @generated by scripts/gen_sysfont.py. Do not edit by hand.",
        "",
        "/// The families' keys in the Settings order (their files are /System/Fonts/<key>.font),",
        "/// then the drawn symbols.",
        f"pub const KEYS: [&str; {len(FAMILIES) + 1}] = [" + ", ".join(f'"{k}"' for k, *_ in FAMILIES) + ', "symbols"];',
        f"pub const NAMES: [&str; {len(FAMILIES) + 1}] = [" + ", ".join(f'"{n}"' for _, n, *_ in FAMILIES) + ', "milkOS Symbols"];',
        "/// Where to look for a character the chosen family lacks: the family with the most glyphs",
        "/// first, the drawn symbols last.",
        f"pub const FALLBACK: [usize; {len(FAMILIES) + 1}] = [" + ", ".join(str(i) for i in order) + f", {len(FAMILIES)}];",
        "",
        "/// Characters no family has, folded to ones that some family has (sorted by code point):",
        "/// `FOLD_CP[i]` becomes `FOLD_STR[FOLD_AT[i]..FOLD_AT[i + 1]]`.",
        f"pub const FOLD_CP: [u16; {len(fold)}] = [",
    ]
    cps = list(fold)
    for i in range(0, len(cps), 12):
        g.append("    " + ", ".join(f"0x{c:04X}" for c in cps[i:i + 12]) + ",")
    g.append("];")
    at = [0]
    allstr = ""
    for c in cps:
        allstr += fold[c]
        at.append(len(allstr.encode()))
    g.append(f"pub const FOLD_AT: [u16; {len(at)}] = [")
    for i in range(0, len(at), 16):
        g.append("    " + ", ".join(str(a) for a in at[i:i + 16]) + ",")
    g.append("];")
    g.append("pub const FOLD_STR: &str = concat!(")
    for i in range(0, len(cps), 16):
        g.append("    " + rust_str("".join(fold[c] for c in cps[i:i + 16])) + ",")
    g.append(");")
    g.append("")
    (ROOT / "user/fontkit/src/generated.rs").write_text("\n".join(g))
    print("wrote kernel/src/sysfont/*.font, kernel/src/sysfont_data.rs, user/fontkit/src/generated.rs")


main()
