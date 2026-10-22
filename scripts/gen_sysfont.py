#!/usr/bin/env python3
"""Bake the desktop's system fonts into anti-aliased glyph bitmaps for the kernel.

Usage:
    python3 scripts/gen_sysfont.py --fetch DIR   download the font files below into DIR (an
                                                 empty directory), check them, then bake
    python3 scripts/gen_sysfont.py DIR           bake from files already in DIR

Needs Pillow with FreeType (tested with Pillow 11.3, FreeType 2.13). Writes
kernel/src/sysfont_data.rs (the index) and kernel/src/sysfont/<family>.bin (the glyphs).
The .ttf files are not part of the repository: every file is listed below with the URL it
comes from (the Google Fonts repository, pinned to one commit) and its sha256, and the
script refuses files whose hash differs, so a regenerated bake matches the committed one.

Figtree, IBM Plex Sans, Fira Sans and Atkinson Hyperlegible Next are under the SIL Open Font
License 1.1; Ubuntu Sans (the 2023 family) is under the Ubuntu Font Licence 1.0. The licence
texts and copyright lines are in assets/fonts/ (and in the image, /System/Library/Fonts/).

What gets baked (sizes are the em in pixels, chosen to sit where the 8x16 Unifont cells sat):
    body  13 px in a 16 px line, baseline 13: Regular, Medium, Bold, all of Latin-1 + U+FFFD
    head  26 px in a 32 px line, baseline 26: Regular, Bold, all of Latin-1 + U+FFFD
    big   52 px in a 64 px line, baseline 52: Bold, letters, digits, ':' (clock, tiles, splash)
    huge 104 px in a 128 px line, baseline 104: Regular, digits and ':' (the lock screen clock)
Glyphs are FreeType's hinted grey coverage (no subpixel), cropped to their ink box and
quantised to 4 bits, stored either as packed nibbles or as nibble runs (value << 4 | len-1),
whichever is smaller. Advances are FreeType's hinted advances, whole pixels. No kerning.

Blob layout (little endian), per family:
    glyph records, 12 bytes each, sorted by code point within a face:
        u16 code point, u32 data offset (bit 31 set = run-length coded), u8 width,
        u8 height, i8 left bearing, i8 top (pixels above the baseline), u8 advance, u8 0
    then the coverage data.
"""
import hashlib
import pathlib
import sys
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

# name, em px, line box, baseline, weights, code points. Order matters: sysfont.rs indexes it.
FACES = [
    ("body", 13, 16, 13, (400, 500, 700), LATIN1),
    ("head", 26, 32, 26, (400, 700), LATIN1),
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


def bake_family(d, key, files):
    """(blob, [(face name, weight, size, line, ascent, first record, count)])."""
    records = []
    data = bytearray()
    faces = []
    for name, size, line, ascent, weights, cps in FACES:
        for wt in weights:
            f = load(d, files, wt, size)
            bold = load(d, files, 700, size)
            first = len(records)
            for cp in cps:
                src = cp
                if not has_glyph(f, cp):
                    src = STAND_INS.get(cp, 0xFFFD)
                if src == 0xFFFD and not has_glyph(f, 0xFFFD):
                    im, left, top, adv = replacement(f, bold, size)
                elif not has_glyph(f, src):
                    sys.exit(f"{key}: no glyph for U+{cp:04X}")
                else:
                    im, left, top, adv = render(f, src, size)
                if im is None:
                    w = h = 0
                    enc, rle = b"", False
                else:
                    w, h = im.size
                    enc, rle = encode(quant(im))
                assert w < 256 and h < 256 and -128 <= left < 128 and -128 <= top < 128
                assert 0 <= adv < 256, (key, cp, adv)
                records.append((cp, len(data), rle, w, h, left, top, adv))
                data += enc
            faces.append((name, wt, size, line, ascent, first, len(records) - first))
    blob = bytearray()
    base = 12 * len(records)
    for cp, off, rle, w, h, left, top, adv in records:
        o = (base + off) | (1 << 31 if rle else 0)
        blob += cp.to_bytes(2, "little") + o.to_bytes(4, "little")
        blob += bytes([w, h, left & 0xFF, top & 0xFF, adv, 0])
    blob += data
    return bytes(blob), faces


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
    rs = [
        "// @generated by scripts/gen_sysfont.py. Do not edit by hand.",
        "// Baked from fonts under the SIL OFL 1.1 and the Ubuntu Font Licence 1.0, see assets/fonts/.",
        "",
        "use crate::sysfont::{Face, Family};",
        "",
        f"pub static FAMILIES: [Family; {len(FAMILIES)}] = [",
    ]
    total = 0
    for key, name, files, lic, licence, copyright in FAMILIES:
        blob, faces = bake_family(d, key, files)
        (outdir / f"{key}.bin").write_bytes(blob)
        total += len(blob)
        print(f"{key}: {len(blob)} bytes")
        cr = (copyright or copyright_line(d, lic[0])).replace("\\", "\\\\").replace('"', '\\"')
        rs.append("    Family {")
        rs.append(f'        key: "{key}",')
        rs.append(f'        name: "{name}",')
        rs.append(f'        copyright: "{cr}",')
        rs.append(f'        licence: "{licence}",')
        rs.append(f'        blob: include_bytes!("sysfont/{key}.bin"),')
        rs.append("        faces: &[")
        for fname, wt, size, line, ascent, first, n in faces:
            # Laid out the way rustfmt wants it, so `cargo fmt` leaves the file alone.
            rs.append("            Face {")
            for k, v in (("size", size), ("line", line), ("ascent", ascent), ("weight", wt),
                         ("first", first), ("count", n)):
                rs.append(f"                {k}: {v},")
            rs.append(f"            }}, // {fname}")
        rs.append("        ],")
        rs.append("    },")
    rs += ["];", ""]
    (ROOT / "kernel/src/sysfont_data.rs").write_text("\n".join(rs))
    print(f"total {total} bytes; wrote kernel/src/sysfont_data.rs and kernel/src/sysfont/*.bin")


main()
