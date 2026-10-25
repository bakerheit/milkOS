#!/usr/bin/env python3
"""Build and take apart Android boot images, header version 0 (what the Pixel 2's bootloader
reads for `fastboot boot`).

  Build:   python3 -I scripts/android-bootimg.py --kernel Image --dtb walleye.dtb -o milkos-pixel2.img
  Unpack:  python3 -I scripts/android-bootimg.py --unpack boot.img [--out DIR]

Build puts the kernel and the device tree into one "kernel" section of the image the way the
phone's own `Image.lz4-dtb` does: the tree appended to the kernel. For an uncompressed arm64
`Image` the bootloader finds the tree after the end of the image as the Image header's `image_size`
field says (that field counts the zeroed memory after the file, .bss and the boot stack, so the
tree goes at that offset and the bytes in between are zero); `--dtb-at end` puts it right after
the file instead. `--compress gzip` is the other layout the bootloader knows from `Image.gz-dtb` (the
Image gzip-compressed, the tree right after the compressed stream), for the case that it refuses a
plain Image. The command line defaults to the stock Pixel 2 one; `--cmdline` replaces it
and `--add-cmdline` appends to it (kernel options of ours look like `msm.fmt=rgbx`). The
ramdisk is none (`--ramdisk none`), a tiny empty cpio archive (`tiny`, the default: the bootloader
sees an ordinary image with a ramdisk) or a file.

The header layout is the one in rpikit/src/bootimg.rs; `--unpack` reads the fields back, checks
the SHA-1 id the way mkbootimg computes it, finds device trees in the kernel section and, for an
arm64 `Image`, prints its header (text_offset, image_size). Nothing is written to a device, the
tool only makes and reads files. Python 3 standard library only.
"""

import argparse
import gzip
import hashlib
import os
import struct
import sys

MAGIC = b"ANDROID!"
HEADER_FMT = "<8s10I16s512s32s1024s"
HEADER_SIZE = struct.calcsize(HEADER_FMT)
assert HEADER_SIZE == 1632

# What the Pixel 2's own image uses (base 0: the bootloader adds the start of RAM).
KERNEL_ADDR = 0x00008000
RAMDISK_ADDR = 0x01000000
SECOND_ADDR = 0x00F00000
TAGS_ADDR = 0x00000100
PAGE_SIZE = 4096
STOCK_CMDLINE = (
    "androidboot.hardware=walleye androidboot.console=ttyMSM0 lpm_levels.sleep_disabled=1 "
    "user_debug=31 msm_rtb.filter=0x37 ehci-hcd.park=3 service_locator.enable=1 swiotlb=2048 "
    "firmware_class.path=/vendor/firmware loop.max_part=7 raid=noautodetect usbcore.autosuspend=7 "
    "androidboot.dtbo_idx=3 buildvariant=user"
)
FDT_MAGIC = b"\xd0\x0d\xfe\xed"
ARM64_MAGIC = 0x644D5241


def split_cmdline(c):
    """(cmdline, extra_cmdline) as bytes; the same cut as rpikit's bootimg::split_cmdline."""
    c = c[: 511 + 1023]
    if len(c) < 512:
        return c, b""
    cut = 511
    lo = max(0, cut - 64)
    sp = c[lo:cut].rfind(b" ")
    if sp >= 0:
        cut = lo + sp
    rest = c[cut:]
    if rest.startswith(b" "):
        rest = rest[1:]
    return c[:cut], rest


def pack_header(f):
    """The 1632 header bytes for a dict of fields (as unpack_header returns them)."""
    cmd, extra = split_cmdline(f["cmdline"])
    return struct.pack(
        HEADER_FMT,
        MAGIC,
        f["kernel_size"],
        f["kernel_addr"],
        f["ramdisk_size"],
        f["ramdisk_addr"],
        f["second_size"],
        f["second_addr"],
        f["tags_addr"],
        f["page_size"],
        f["header_version"],
        f["os_version"],
        f.get("name", b""),
        cmd,
        f["id"],
        extra,
    )


def unpack_header(b):
    if len(b) < HEADER_SIZE:
        raise ValueError("shorter than a boot image header")
    (magic, ks, ka, rs, ra, ss, sa, ta, ps, hv, osv, name, cmd, ident, extra) = struct.unpack_from(HEADER_FMT, b, 0)
    if magic != MAGIC:
        raise ValueError("not an Android boot image (no ANDROID! magic)")
    if ps < 2048 or ps & (ps - 1) or ps > 1 << 24:
        raise ValueError(f"bad page size {ps}")
    if hv != 0:
        raise ValueError(f"header version {hv}: this tool only handles version 0")
    cstr = lambda x: x[: x.index(b"\0")] if b"\0" in x else x
    return {
        "kernel_size": ks,
        "kernel_addr": ka,
        "ramdisk_size": rs,
        "ramdisk_addr": ra,
        "second_size": ss,
        "second_addr": sa,
        "tags_addr": ta,
        "page_size": ps,
        "header_version": hv,
        "os_version": osv,
        "name": cstr(name),
        "cmdline": cstr(cmd) + (b" " + cstr(extra) if cstr(extra) else b""),
        "id": ident,
        "_cmd_raw": cmd,
        "_extra_raw": extra,
    }


def image_id(kernel, ramdisk, second):
    """mkbootimg's id for a version 0 image: SHA-1 over each section followed by its size."""
    h = hashlib.sha1()
    for blob in (kernel, ramdisk, second):
        h.update(blob)
        h.update(struct.pack("<I", len(blob)))
    return h.digest().ljust(32, b"\0")


def pages(n, page):
    return -(-n // page) * page


def layout(f):
    p = f["page_size"]
    kernel = p
    ramdisk = kernel + pages(f["kernel_size"], p)
    second = ramdisk + pages(f["ramdisk_size"], p)
    return {"kernel": kernel, "ramdisk": ramdisk, "second": second, "total": second + pages(f["second_size"], p)}


def tiny_cpio():
    """An empty newc cpio archive (just the trailer), padded to 512 bytes like cpio does."""
    name = b"TRAILER!!!\0"
    hdr = b"070701" + b"".join(b"%08x" % v for v in (0, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, len(name), 0))
    blob = hdr + name
    blob += b"\0" * (-len(blob) % 4)
    return blob + b"\0" * (-len(blob) % 512)


def arm64_header(image):
    """(text_offset, image_size, flags) of an arm64 Image, or None when it has no such header."""
    if len(image) < 64 or struct.unpack_from("<I", image, 56)[0] != ARM64_MAGIC:
        return None
    text_offset, image_size, flags = struct.unpack_from("<QQQ", image, 8)
    return text_offset, image_size, flags


def build_kernel_section(image, dtb, dtb_at="image-size", compress=None):
    """The kernel + appended tree section: see the module doc."""
    if compress == "gzip":
        # Reproducible (no timestamp); the tree follows the end of the compressed stream.
        return gzip.compress(image, 9, mtime=0) + (dtb or b"")
    blob = bytearray(image)
    hdr = arm64_header(image)
    if dtb is not None:
        if dtb_at == "image-size" and hdr is not None and hdr[1] > len(blob):
            blob.extend(b"\0" * (hdr[1] - len(blob)))
        elif len(blob) % 8:
            blob.extend(b"\0" * (-len(blob) % 8))
        blob.extend(dtb)
    return bytes(blob)


def build(args):
    image = open(args.kernel, "rb").read()
    dtb = open(args.dtb, "rb").read() if args.dtb else None
    if dtb is not None:
        if dtb[:4] != FDT_MAGIC:
            raise SystemExit("android-bootimg: --dtb is not a device tree (no d00dfeed magic)")
        (total,) = struct.unpack_from(">I", dtb, 4)
        if total > len(dtb):
            raise SystemExit("android-bootimg: --dtb is shorter than its own header says")
        dtb = dtb[:total]
    if args.ramdisk == "none":
        ramdisk = b""
    elif args.ramdisk == "tiny":
        ramdisk = tiny_cpio()
    else:
        ramdisk = open(args.ramdisk, "rb").read()
    kernel = build_kernel_section(image, dtb, args.dtb_at, args.compress)
    cmdline = (args.cmdline if args.cmdline is not None else STOCK_CMDLINE).encode()
    if args.add_cmdline:
        cmdline += b" " + args.add_cmdline.encode()
    fields = {
        "kernel_size": len(kernel),
        "kernel_addr": args.kernel_addr,
        "ramdisk_size": len(ramdisk),
        "ramdisk_addr": args.ramdisk_addr,
        "second_size": 0,
        "second_addr": args.second_addr,
        "tags_addr": args.tags_addr,
        "page_size": args.page_size,
        "header_version": 0,
        "os_version": args.os_version,
        "name": b"",
        "cmdline": cmdline,
        "id": image_id(kernel, ramdisk, b""),
    }
    out = bytearray(pack_header(fields))
    out.extend(b"\0" * (-len(out) % args.page_size))
    for blob in (kernel, ramdisk):
        out.extend(blob)
        out.extend(b"\0" * (-len(out) % args.page_size))
    with open(args.output, "wb") as fh:
        fh.write(out)
    hdr = arm64_header(image)
    print(f"android-bootimg: {args.output}: {len(out)} bytes")
    print(f"  kernel section {len(kernel)} bytes (Image {len(image)}" + (f", tree {len(dtb)} at {len(kernel) - len(dtb)}" if dtb else "") + ")")
    print(f"  ramdisk {len(ramdisk)} bytes, page size {args.page_size}")
    if args.compress:
        print(f"  compressed with {args.compress}")
    if hdr:
        print(f"  Image header: text_offset {hdr[0]:#x}, image_size {hdr[1]:#x}, flags {hdr[2]:#x}")
    else:
        print("  note: the kernel file has no arm64 Image header (no 'ARM\\x64' magic at offset 56)")
    print(f"  command line: {cmdline.decode()}")
    return 0


def unpack(args):
    b = open(args.unpack, "rb").read()
    f = unpack_header(b)
    lay = layout(f)
    print(f"{args.unpack}: {len(b)} bytes, header version {f['header_version']}, page size {f['page_size']}")
    for k in ("kernel_size", "kernel_addr", "ramdisk_size", "ramdisk_addr", "second_size", "second_addr", "tags_addr", "os_version"):
        print(f"  {k:13} {f[k]:#x}")
    print(f"  name          {f['name'].decode(errors='replace')!r}")
    print(f"  cmdline       {f['cmdline'].decode(errors='replace')}")
    print(f"  sections      kernel at {lay['kernel']:#x}, ramdisk at {lay['ramdisk']:#x}, second at {lay['second']:#x}, end {lay['total']:#x}")
    if lay["total"] > len(b):
        print(f"  WARNING: the header describes {lay['total']} bytes, the file has {len(b)}")
    kernel = b[lay["kernel"] : lay["kernel"] + f["kernel_size"]]
    ramdisk = b[lay["ramdisk"] : lay["ramdisk"] + f["ramdisk_size"]]
    second = b[lay["second"] : lay["second"] + f["second_size"]]
    want = image_id(kernel, ramdisk, second)
    print(f"  id            {f['id'][:20].hex()} ({'matches' if want == f['id'] else 'does NOT match'} the sha1 of the contents)")
    print(f"  kernel starts {kernel[:8].hex()}" + ("  (LZ4 frame)" if kernel[:4] == b"\x04\x22\x4d\x18" else "  (gzip)" if kernel[:2] == b"\x1f\x8b" else ""))
    h = arm64_header(kernel)
    if h:
        print(f"  arm64 Image   text_offset {h[0]:#x}, image_size {h[1]:#x}, flags {h[2]:#x}")
    at = 0
    while True:
        at = kernel.find(FDT_MAGIC, at)
        if at < 0:
            break
        (tot,) = struct.unpack_from(">I", kernel, at + 4)
        print(f"  device tree   at kernel+{at:#x}, {tot} bytes")
        at += 4
    if args.out:
        os.makedirs(args.out, exist_ok=True)
        for name, blob in (("kernel", kernel), ("ramdisk", ramdisk), ("second", second)):
            if blob:
                open(os.path.join(args.out, name), "wb").write(blob)
        open(os.path.join(args.out, "header"), "wb").write(b[:HEADER_SIZE])
        print(f"  sections written to {args.out}")
    return 0


def main(argv):
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--unpack", metavar="IMG", help="print the fields of an existing boot image instead of building one")
    ap.add_argument("--out", metavar="DIR", help="with --unpack: write the kernel, ramdisk and second sections here")
    ap.add_argument("--kernel", help="the kernel: a raw arm64 Image")
    ap.add_argument("--dtb", help="a device tree to append to the kernel")
    ap.add_argument("--dtb-at", choices=("image-size", "end"), default="image-size")
    ap.add_argument("--compress", choices=("gzip",), help="compress the kernel (the tree stays plain, after it)")
    ap.add_argument("--ramdisk", default="tiny", help="none, tiny (default) or a file")
    ap.add_argument("--cmdline", help="replace the command line (default: the stock Pixel 2 one)")
    ap.add_argument("--add-cmdline", help="append to the command line")
    ap.add_argument("--kernel-addr", type=lambda v: int(v, 0), default=KERNEL_ADDR)
    ap.add_argument("--ramdisk-addr", type=lambda v: int(v, 0), default=RAMDISK_ADDR)
    ap.add_argument("--second-addr", type=lambda v: int(v, 0), default=SECOND_ADDR)
    ap.add_argument("--tags-addr", type=lambda v: int(v, 0), default=TAGS_ADDR)
    ap.add_argument("--page-size", type=lambda v: int(v, 0), default=PAGE_SIZE)
    ap.add_argument("--os-version", type=lambda v: int(v, 0), default=0)
    ap.add_argument("-o", "--output")
    args = ap.parse_args(argv)
    try:
        if args.unpack:
            return unpack(args)
        if not args.kernel or not args.output:
            ap.error("building needs --kernel and -o (or use --unpack IMG)")
        return build(args)
    except (ValueError, OSError) as e:
        print(f"android-bootimg: {e}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
