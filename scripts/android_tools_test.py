#!/usr/bin/env python3
"""Tests of scripts/android-bootimg.py and scripts/dtb_build.py with small made-up inputs (no
phone, no downloaded or copied vendor file). Run by scripts/pixel2-tools-test.sh; prints
"android-tools: N passed" and exits non-zero on a failure.

If MILK_PIXEL2_STOCK names a folder holding the phone's own `boot.img` and `dtbo.img` as
`boot.img` and `dtbo.img` (files you extracted from your own device or image; they are never
part of the repository), two more checks run: the stock header is unpacked and packed again to
the identical 1632 bytes (and its id is reproduced), and the dtbo table is listed.
"""

import importlib.util
import os
import random
import re
import struct
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


ab = load("android_bootimg", "android-bootimg.py")
db = load("dtb_build", "dtb_build.py")


def fake_image(image_size=0x3000, text_offset=0x80000, file_size=0x1234):
    """A made-up arm64 Image: the 64-byte header and some bytes."""
    h = bytearray(file_size)
    struct.pack_into("<I", h, 0, 0x14000010)  # b +0x40
    struct.pack_into("<QQQ", h, 8, text_offset, image_size, 0xA)
    struct.pack_into("<I", h, 56, ab.ARM64_MAGIC)
    for i in range(64, file_size):
        h[i] = (i * 7) & 0xFF
    return bytes(h)


class Fdt(unittest.TestCase):
    def test_small_tree_is_the_rust_writers_golden_bytes(self):
        src = open(os.path.join(ROOT, "rpikit/src/fdtwrite.rs")).read()
        want = re.search(r'SMALL_TREE_HEX: &str = "([0-9a-f]+)"', src).group(1)
        self.assertEqual(db.small_tree().hex(), want)

    def test_round_trip_of_random_trees(self):
        rng = random.Random(7)
        for _ in range(60):
            root = db.Node("")
            names = ["a", "bb", "ccc", "reg", "x,y", "#cells"]

            def grow(n, depth):
                for _ in range(rng.randint(0, 4)):
                    n.prop(rng.choice(names) + str(rng.randint(0, 3)), bytes(rng.randrange(256) for _ in range(rng.randint(0, 13))))
                if depth < 3:
                    for i in range(rng.randint(0, 3)):
                        grow(n.child(f"n{i}@{rng.randint(0, 99):x}"), depth + 1)

            grow(root, 0)
            rsv = [(rng.randrange(1 << 40), rng.randrange(1, 1 << 20)) for _ in range(rng.randint(0, 3))]
            blob = db.write_fdt(root, rsv)
            total, got_rsv, nodes = db.parse_fdt(blob)
            self.assertEqual(total, len(blob))
            self.assertEqual(blob[:4], b"\xd0\x0d\xfe\xed")
            self.assertEqual(got_rsv, rsv)

            def walk(n, key):
                self.assertEqual(nodes[key], dict(n.props))
                for c in n.children:
                    walk(c, ("" if key == "/" else key) + "/" + c.name)

            walk(root, "/")

    def test_the_walleye_tree_has_what_the_bootloader_and_the_kernel_look_for(self):
        total, rsv, nodes = db.parse_fdt(db.minimal_tree())
        root = nodes["/"]
        self.assertEqual(root["qcom,msm-id"], struct.pack(">II", 0x124, 0x20001))
        self.assertEqual(root["compatible"], b"qcom,msm8998-mtp\0qcom,msm8998\0qcom,mtp\0")
        self.assertEqual(root["#address-cells"], struct.pack(">I", 2))
        self.assertEqual(nodes["/memory"]["device_type"], b"memory\0")
        self.assertEqual(len(nodes["/memory"]["reg"]), 16)
        self.assertIn("bootargs", nodes["/chosen"])
        cpus = sorted(p for p in nodes if p.startswith("/cpus/cpu@"))
        self.assertEqual([p.split("@")[1] for p in cpus], sorted(["0", "1", "2", "3", "100", "101", "102", "103"]))
        self.assertEqual(nodes["/psci"]["method"], b"smc\0")
        # Every fixed reserved region is no-map with a two-cell address and size; the splash is plain.
        fixed = [p for p in nodes if p.startswith("/reserved-memory/") and "reg" in nodes[p]]
        self.assertEqual(len(fixed), len(db.RESERVED_FIXED) + 1)
        for p in fixed:
            base, size = struct.unpack(">QQ", nodes[p]["reg"])
            if "splash" in p:
                self.assertEqual((base, size), (0x9D600000, 0x2400000))
                self.assertNotIn("no-map", nodes[p])
            else:
                self.assertIn("no-map", nodes[p], p)
        # No two reserved regions overlap.
        spans = sorted(struct.unpack(">QQ", nodes[p]["reg"]) for p in fixed)
        for (a, s), (b, _) in zip(spans, spans[1:]):
            self.assertLessEqual(a + s, b)
        gic = nodes["/soc/interrupt-controller@17a00000"]
        self.assertEqual(gic["compatible"], b"arm,gic-v3\0")
        self.assertEqual(struct.unpack(">4I", gic["reg"]), (0x17A00000, 0x10000, 0x17B00000, 0x100000))
        mdp = nodes["/soc/qcom,mdss_mdp@c900000"]
        self.assertEqual(struct.unpack(">II", mdp["reg"][:8]), (0xC900000, 0x90000))
        self.assertEqual(struct.unpack(">5I", mdp["qcom,mdss-ctl-off"])[0], 0x2000)
        self.assertEqual(db.msm_id_of(nodes), (0x124, 0x20001))
        self.assertEqual(rsv, [])
        self.assertEqual(total, len(db.minimal_tree()))

    def test_the_parser_refuses_damage(self):
        blob = db.minimal_tree()
        with self.assertRaises(ValueError):
            db.parse_fdt(b"\0" * 64)
        bad = bytearray(blob)
        struct.pack_into(">I", bad, 4, 100)  # total size too small for its blocks
        with self.assertRaises(ValueError):
            db.parse_fdt(bytes(bad))

    def test_the_qemu_test_tree_has_a_gic_and_a_timer_with_the_ppis_asked_for(self):
        _, _, nodes = db.parse_fdt(db.virt_test_tree())
        t = nodes["/timer"]
        self.assertEqual(struct.unpack(">12I", t["interrupts"]), (1, 13, 0x104, 1, 14, 0x104, 1, 11, 0x104, 1, 10, 0x104))
        self.assertIn(b"arm,armv8-timer", t["compatible"])
        self.assertIn(b"arm,gic-v3", nodes["/intc@8000000"]["compatible"])
        # A wrong virtual timer interrupt (the kernel has to find the right one by trying), or no node at all.
        _, _, nodes = db.parse_fdt(db.virt_test_tree(timer=(13, 14, 3, 10)))
        self.assertEqual(struct.unpack(">12I", nodes["/timer"]["interrupts"])[7], 3)
        _, _, nodes = db.parse_fdt(db.virt_test_tree(timer=None))
        self.assertNotIn("/timer", nodes)

    def test_dtbo_table(self):
        # A made-up table of two entries in the format of the dtbo partition.
        hdr = struct.pack(">8I", db.DTBO_MAGIC, 200, 32, 32, 2, 32, 2048, 0)
        e0 = struct.pack(">8I", 100, 96, 0x1BC, 3, 0, 0, 0, 0)
        e1 = struct.pack(">8I", 50, 196, 0x190, 0, 1, 2, 3, 4)
        got = db.dtbo_entries(hdr + e0 + e1)
        self.assertEqual([(e["id"], e["rev"]) for e in got], [(0x1BC, 3), (0x190, 0)])
        self.assertEqual(got[1]["custom"], (1, 2, 3, 4))
        with self.assertRaises(ValueError):
            db.dtbo_entries(b"\0" * 64)


class Header(unittest.TestCase):
    def fields(self, ks=0xE427AA, rs=0x9E2830):
        return {
            "kernel_size": ks, "kernel_addr": ab.KERNEL_ADDR, "ramdisk_size": rs, "ramdisk_addr": ab.RAMDISK_ADDR,
            "second_size": 0, "second_addr": ab.SECOND_ADDR, "tags_addr": ab.TAGS_ADDR, "page_size": 4096,
            "header_version": 0, "os_version": 0x14000143, "name": b"", "cmdline": ab.STOCK_CMDLINE.encode(),
            "id": bytes(32),
        }

    def test_the_header_layout_byte_for_byte(self):
        b = ab.pack_header(self.fields())
        self.assertEqual(len(b), 1632)
        w = lambda o: struct.unpack_from("<I", b, o)[0]
        self.assertEqual(b[:8], b"ANDROID!")
        self.assertEqual([w(o) for o in range(8, 48, 4)], [0xE427AA, 0x8000, 0x9E2830, 0x1000000, 0, 0xF00000, 0x100, 0x1000, 0, 0x14000143])
        self.assertEqual(b[48:64], bytes(16))
        self.assertTrue(b[64:].startswith(b"androidboot.hardware=walleye "))
        n = b[64:576].index(0)
        self.assertEqual(n, 306)
        self.assertEqual(b[64 + n : 576], bytes(512 - n))
        self.assertEqual(b[576:], bytes(32 + 1024))

    def test_unpack_pack_is_the_identity(self):
        f = self.fields()
        f["id"] = ab.image_id(b"k" * 10, b"r" * 5, b"")
        b = ab.pack_header(f)
        g = ab.unpack_header(b)
        self.assertEqual(ab.pack_header(g), b)
        for k in ("kernel_size", "ramdisk_addr", "tags_addr", "page_size", "os_version", "cmdline", "id"):
            self.assertEqual(g[k], f[k], k)

    def test_long_command_lines_go_in_the_extra_field_and_come_back(self):
        c = b" ".join(b"opt%d=value%d" % (i, i) for i in range(80))
        f = self.fields()
        f["cmdline"] = c
        g = ab.unpack_header(ab.pack_header(f))
        self.assertTrue(g["cmdline"].startswith(b"opt0=value0 opt1=value1 "))
        self.assertLessEqual(len(g["_cmd_raw"].rstrip(b"\0")), 511)
        a, b = ab.split_cmdline(c)
        self.assertEqual(a + b" " + b, g["cmdline"])
        # The same cuts as rpikit's bootimg::split_cmdline tests.
        self.assertEqual(ab.split_cmdline(b"a b c"), (b"a b c", b""))
        a, b = ab.split_cmdline(b"x" * 700)
        self.assertEqual((len(a), len(b)), (511, 189))
        a, b = ab.split_cmdline(b"y" * 4000)
        self.assertEqual((len(a), len(b)), (511, 1023))

    def test_refusals(self):
        b = ab.pack_header(self.fields())
        for bad, why in ((b"X" + b[1:], "magic"), (b[:100], "short")):
            with self.assertRaises(ValueError, msg=why):
                ab.unpack_header(bad)
        v = bytearray(b)
        struct.pack_into("<I", v, 40, 2)
        with self.assertRaises(ValueError):
            ab.unpack_header(bytes(v))
        v = bytearray(b)
        struct.pack_into("<I", v, 36, 3000)
        with self.assertRaises(ValueError):
            ab.unpack_header(bytes(v))

    def test_the_id_is_the_mkbootimg_one(self):
        import hashlib
        k, r = b"kernel", b"ramdisk"
        want = hashlib.sha1(k + struct.pack("<I", 6) + r + struct.pack("<I", 7) + struct.pack("<I", 0)).digest()
        self.assertEqual(ab.image_id(k, r, b""), want + bytes(12))

    def test_tiny_ramdisk_is_a_valid_empty_cpio(self):
        c = ab.tiny_cpio()
        self.assertTrue(c.startswith(b"070701"))
        self.assertIn(b"TRAILER!!!", c)
        self.assertEqual(len(c) % 512, 0)
        self.assertEqual(int(c[6 + 8 * 11 : 6 + 8 * 12], 16), len(b"TRAILER!!!\0"))


class Build(unittest.TestCase):
    def run_build(self, tmp, image, dtb, *extra):
        k, d, o = (os.path.join(tmp, n) for n in ("Image", "x.dtb", "out.img"))
        open(k, "wb").write(image)
        open(d, "wb").write(dtb)
        rc = ab.main(["--kernel", k, "--dtb", d, "-o", o, *extra])
        self.assertEqual(rc, 0)
        return o

    def test_an_image_with_the_tree_where_the_image_header_says(self):
        dtb = db.minimal_tree()
        image = fake_image()
        with tempfile.TemporaryDirectory() as tmp:
            o = self.run_build(tmp, image, dtb)
            b = open(o, "rb").read()
            f = ab.unpack_header(b)
            lay = ab.layout(f)
            self.assertEqual(len(b), lay["total"])
            self.assertEqual(len(b) % 4096, 0)
            kernel = b[lay["kernel"] : lay["kernel"] + f["kernel_size"]]
            # The Image, zeros up to image_size, the tree right there.
            self.assertEqual(kernel[: len(image)], image)
            self.assertEqual(kernel[len(image) : 0x3000], bytes(0x3000 - len(image)))
            self.assertEqual(kernel[0x3000:], dtb)
            self.assertEqual(f["kernel_addr"], 0x8000)
            self.assertEqual(f["cmdline"].decode(), ab.STOCK_CMDLINE)
            ram = b[lay["ramdisk"] : lay["ramdisk"] + f["ramdisk_size"]]
            self.assertEqual(ram, ab.tiny_cpio())
            self.assertEqual(f["id"], ab.image_id(kernel, ram, b""))
            self.assertEqual(f["tags_addr"], 0x100)
            # And the unpack command reads it.
            self.assertEqual(ab.main(["--unpack", o]), 0)
            out = os.path.join(tmp, "parts")
            self.assertEqual(ab.main(["--unpack", o, "--out", out]), 0)
            self.assertEqual(open(os.path.join(out, "kernel"), "rb").read(), kernel)
            self.assertEqual(open(os.path.join(out, "header"), "rb").read(), b[:1632])

    def test_options(self):
        dtb = db.small_tree()
        with tempfile.TemporaryDirectory() as tmp:
            o = self.run_build(tmp, fake_image(), dtb, "--dtb-at", "end", "--ramdisk", "none", "--cmdline", "a=b", "--add-cmdline", "msm.fmt=rgbx")
            b = open(o, "rb").read()
            f = ab.unpack_header(b)
            self.assertEqual(f["ramdisk_size"], 0)
            self.assertEqual(f["cmdline"], b"a=b msm.fmt=rgbx")
            lay = ab.layout(f)
            kernel = b[lay["kernel"] : lay["kernel"] + f["kernel_size"]]
            self.assertEqual(len(kernel), 0x1238 + len(dtb))  # 0x1234 rounded up to 8, then the tree
            self.assertEqual(kernel[0x1238:], dtb)

    def test_gzip_layout_is_a_plain_gz_dtb(self):
        import zlib
        dtb = db.minimal_tree()
        image = fake_image()
        with tempfile.TemporaryDirectory() as tmp:
            o = self.run_build(tmp, image, dtb, "--compress", "gzip")
            b = open(o, "rb").read()
            f = ab.unpack_header(b)
            lay = ab.layout(f)
            kernel = b[lay["kernel"] : lay["kernel"] + f["kernel_size"]]
            self.assertEqual(kernel[:2], b"\x1f\x8b")
            d = zlib.decompressobj(31)
            self.assertEqual(d.decompress(kernel), image)
            self.assertEqual(d.unused_data, dtb)
            # The same input gives the same bytes (no timestamp in the stream).
            o2 = self.run_build(tmp, image, dtb, "--compress", "gzip")
            self.assertEqual(open(o2, "rb").read(), b)

    def test_not_a_tree_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            k, d, o = (os.path.join(tmp, n) for n in ("Image", "x.dtb", "out.img"))
            open(k, "wb").write(fake_image())
            open(d, "wb").write(b"not a tree at all")
            with self.assertRaises(SystemExit):
                ab.main(["--kernel", k, "--dtb", d, "-o", o])
            open(d, "wb").write(db.small_tree()[:30])
            with self.assertRaises(SystemExit):
                ab.main(["--kernel", k, "--dtb", d, "-o", o])
            self.assertFalse(os.path.exists(o))

    def test_arm64_header_is_read(self):
        self.assertEqual(ab.arm64_header(fake_image(0x4000, 0x80000)), (0x80000, 0x4000, 0xA))
        self.assertIsNone(ab.arm64_header(b"\0" * 100))


class BuildScript(unittest.TestCase):
    """scripts/android-build.sh with a made-up Image (--image: no cargo): the gzip image is the default, the plain
    one is a variant, every variant is gzip, the README says what the phone showed."""

    @classmethod
    def setUpClass(cls):
        import subprocess
        import zlib
        cls.zlib = zlib
        cls.tmp = tempfile.TemporaryDirectory()
        t = cls.tmp.name
        cls.image = fake_image(0x5000, 0x80000, 0x2345)
        cls.stock = db.minimal_tree()
        open(os.path.join(t, "fake-image"), "wb").write(cls.image)
        open(os.path.join(t, "stock-base.dtb"), "wb").write(cls.stock)
        cls.out = os.path.join(t, "out")
        r = subprocess.run(
            ["bash", os.path.join(HERE, "android-build.sh"), cls.out, "--image", os.path.join(t, "fake-image"),
             "--stock-dtb", os.path.join(t, "stock-base.dtb")],
            capture_output=True, text=True, timeout=120,
        )
        cls.result = r

    @classmethod
    def tearDownClass(cls):
        cls.tmp.cleanup()

    def kernel_of(self, name):
        b = open(os.path.join(self.out, name), "rb").read()
        f = ab.unpack_header(b)
        lay = ab.layout(f)
        return f, b[lay["kernel"] : lay["kernel"] + f["kernel_size"]]

    def test_the_build_worked(self):
        self.assertEqual(self.result.returncode, 0, self.result.stderr + self.result.stdout)
        self.assertTrue(os.path.exists(os.path.join(self.out, "README.txt")))

    def test_the_default_image_is_the_gzip_one_with_the_stock_tree(self):
        f, k = self.kernel_of("milkos-pixel2.img")
        self.assertEqual(k[:2], b"\x1f\x8b")
        d = self.zlib.decompressobj(31)
        self.assertEqual(d.decompress(k), self.image)
        self.assertEqual(d.unused_data, self.stock)
        self.assertEqual(f["cmdline"].decode(), ab.STOCK_CMDLINE)

    def test_the_plain_image_is_only_a_variant(self):
        f, k = self.kernel_of("milkos-pixel2-plain.img")
        self.assertEqual(k[: len(self.image)], self.image)
        self.assertNotEqual(k[:2], b"\x1f\x8b")
        self.assertEqual(f["cmdline"].decode(), ab.STOCK_CMDLINE)

    def test_every_variant_is_gzip_and_adds_its_option(self):
        want = {
            "stage1": "msm.stage=1", "log30": "msm.logsecs=30", "smp": "msm.smp=1", "turn": "msm.rotate=90",
            "phystimer": "msm.timer=phys", "desk720": "msm.desk=720x1280", "pattern": "msm.hold=inf", "rgbx": "msm.fmt=rgbx", "xrgb": "msm.fmt=xrgb",
            "xbgr": "msm.fmt=xbgr", "kick": "msm.kick=ctl0 msm.hold=3", "mdp": "msm.mdp=dump msm.hold=3",
            "wdt": "msm.wdt=pet",
        }
        for name, opt in want.items():
            f, k = self.kernel_of(f"milkos-pixel2-{name}.img")
            self.assertEqual(k[:2], b"\x1f\x8b", name)
            self.assertTrue(f["cmdline"].decode().endswith(" " + opt), (name, f["cmdline"]))
        # The old name of the gzip variant is gone: the default is that now.
        self.assertFalse(os.path.exists(os.path.join(self.out, "milkos-pixel2-gz.img")))

    def test_the_minimal_tree_variant_is_gzip_with_the_small_tree(self):
        f, k = self.kernel_of("milkos-pixel2-minimal.img")
        d = self.zlib.decompressobj(31)
        self.assertEqual(d.decompress(k), self.image)
        self.assertEqual(d.unused_data, db.minimal_tree())

    def test_unpack_reads_into_the_gzip_stream(self):
        import contextlib
        import io

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.assertEqual(ab.main(["--unpack", os.path.join(self.out, "milkos-pixel2.img")]), 0)
        text = buf.getvalue()
        self.assertIn("(gzip)", text)
        self.assertIn(f"{len(self.image)} bytes uncompressed, {len(self.stock)} bytes after it", text)
        self.assertIn("arm64 Image   text_offset 0x80000, image_size 0x5000", text)

    def test_the_readme_says_what_the_phone_showed(self):
        t = open(os.path.join(self.out, "README.txt")).read()
        for needle in ("FIRST boot", "Load Error", "STILL ANSWERS", "no longer answers", "android-try.sh",
                       "GZIP", "10 to 15 seconds", "adb reboot bootloader", "fastboot reboot bootloader",
                       "milkos-pixel2-plain.img", "milkos-pixel2-minimal.img", "second boot"):
            self.assertIn(needle, t)


class TryScript(unittest.TestCase):
    """scripts/android-try.sh against stand-ins for adb and fastboot: a phone that jumps, one that stays in the
    bootloader, one that refuses a second boot in a session, and no phone at all."""

    STUB_FASTBOOT = r"""#!/bin/bash
D="$STUB_DIR"
echo "$@" >> "$D/fastboot.calls"
case "$1" in
  devices) [ -f "$D/present" ] && printf 'FAKE123\tfastboot\n'; exit 0 ;;
  getvar)
    if [ -f "$D/present" ]; then
      if [ "$2" = unlocked ]; then echo "unlocked: yes" >&2; else echo "$2: walleye" >&2; fi
      exit 0
    fi
    exec sleep 30 ;;
  reboot) rm -f "$D/used"; echo "rebooting into bootloader" >&2; exit 0 ;;
  boot)
    if [ -f "$D/used" ]; then
      echo "Sending 'boot.img' (1 KB) OKAY" >&2
      echo "FAILED (remote: 'Failed to load/authenticate boot image: Load Error')" >&2
      exit 1
    fi
    touch "$D/used"
    echo "Sending 'boot.img' (1 KB) OKAY [ 0.1s]" >&2
    echo "Booting OKAY [ 0.0s]" >&2
    if [ "$STUB_MODE" = jump ]; then rm -f "$D/present"; fi
    exit 0 ;;
esac
exit 1
"""
    STUB_ADB = r"""#!/bin/bash
D="$STUB_DIR"
echo "$@" >> "$D/adb.calls"
case "$1" in
  get-state) if [ -f "$D/android" ]; then echo device; exit 0; else echo "error: no devices/emulators found" >&2; exit 1; fi ;;
  reboot) rm -f "$D/android" "$D/used"; touch "$D/present"; exit 0 ;;
esac
exit 1
"""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        t = self.tmp.name
        self.stub = os.path.join(t, "stub")
        os.makedirs(self.stub)
        for name, body in (("fastboot", self.STUB_FASTBOOT), ("adb", self.STUB_ADB)):
            p = os.path.join(t, name)
            open(p, "w").write(body)
            os.chmod(p, 0o755)
        self.img = os.path.join(t, "good.img")
        open(self.img, "wb").write(b"")
        # A real (tiny) boot image from the builder.
        k = os.path.join(t, "Image")
        open(k, "wb").write(fake_image())
        ab.main(["--kernel", k, "--dtb", self._dtb(t), "-o", self.img, "--compress", "gzip"])

    def tearDown(self):
        self.tmp.cleanup()

    def _dtb(self, t):
        p = os.path.join(t, "x.dtb")
        open(p, "wb").write(db.small_tree())
        return p

    def run_try(self, *args, mode="jump", phone="android", img=None):
        import subprocess
        t = self.tmp.name
        for f in ("present", "android", "used"):
            try:
                os.remove(os.path.join(self.stub, f))
            except FileNotFoundError:
                pass
        if phone == "android":
            open(os.path.join(self.stub, "android"), "w").close()
        elif phone == "bootloader":
            open(os.path.join(self.stub, "present"), "w").close()
        elif phone == "used":
            open(os.path.join(self.stub, "present"), "w").close()
            open(os.path.join(self.stub, "used"), "w").close()
        env = dict(os.environ, STUB_DIR=self.stub, STUB_MODE=mode, ADB=os.path.join(t, "adb"),
                   FASTBOOT=os.path.join(t, "fastboot"), ANDROID_TRY_SETTLE="0", ANDROID_TRY_POLLS="2",
                   ANDROID_TRY_TICK="0.2", ANDROID_TRY_ASK="1")
        r = subprocess.run(["bash", os.path.join(HERE, "android-try.sh"), "--yes", "--wait", "1", *args, img or self.img],
                           capture_output=True, text=True, env=env, timeout=60, stdin=subprocess.DEVNULL)
        return r

    def calls(self, name):
        try:
            return open(os.path.join(self.stub, name)).read().splitlines()
        except FileNotFoundError:
            return []

    def test_from_android_a_jump_is_reported_as_a_jump(self):
        r = self.run_try(mode="jump", phone="android")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("JUMPED", r.stdout)
        self.assertIn("reboot bootloader", self.calls("adb.calls"))
        self.assertTrue(any(c.startswith("boot ") for c in self.calls("fastboot.calls")))

    def test_a_bootloader_that_still_answers_means_not_started(self):
        r = self.run_try(mode="stay", phone="bootloader")
        self.assertEqual(r.returncode, 3, r.stdout + r.stderr)
        self.assertIn("STILL ANSWERS", r.stdout)
        # In fastboot already: a fresh session by `fastboot reboot bootloader`, not adb.
        self.assertIn("reboot bootloader", self.calls("fastboot.calls"))
        self.assertEqual(self.calls("adb.calls"), [])

    def test_a_second_boot_in_a_session_is_refused_and_explained(self):
        r = self.run_try("--no-reboot", mode="jump", phone="used")
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertIn("REFUSED", r.stdout)
        self.assertIn("Load Error", r.stdout)
        # The same state with the default (a reboot first) works: every try starts in a fresh session.
        r = self.run_try(mode="jump", phone="used")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertIn("JUMPED", r.stdout)

    def test_no_phone_is_reported(self):
        r = self.run_try(mode="jump", phone="none")
        self.assertEqual(r.returncode, 4, r.stdout + r.stderr)
        self.assertIn("no phone found", r.stderr)
        r = self.run_try("--no-reboot", mode="jump", phone="none")
        self.assertEqual(r.returncode, 4)
        self.assertFalse(any(c.startswith("boot ") for c in self.calls("fastboot.calls")))

    def test_a_file_that_is_not_a_boot_image_is_refused_before_anything_is_run(self):
        bad = os.path.join(self.tmp.name, "bad.img")
        open(bad, "wb").write(b"not an android boot image" * 100)
        r = self.run_try(mode="jump", phone="android", img=bad)
        self.assertEqual(r.returncode, 2)
        self.assertIn("not an Android boot image", r.stderr)
        self.assertEqual(self.calls("adb.calls"), [])
        self.assertEqual(self.calls("fastboot.calls"), [])


@unittest.skipUnless(os.environ.get("MILK_PIXEL2_STOCK"), "no stock images given")
class Stock(unittest.TestCase):
    def test_stock_header_repacks_identically(self):
        b = open(os.path.join(os.environ["MILK_PIXEL2_STOCK"], "boot.img"), "rb").read()
        f = ab.unpack_header(b)
        self.assertEqual(ab.pack_header(f), b[:1632])
        lay = ab.layout(f)
        k = b[lay["kernel"] : lay["kernel"] + f["kernel_size"]]
        r = b[lay["ramdisk"] : lay["ramdisk"] + f["ramdisk_size"]]
        self.assertEqual(ab.image_id(k, r, b""), f["id"])
        self.assertEqual(f["kernel_addr"], 0x8000)

    def test_stock_dtbo_table(self):
        b = open(os.path.join(os.environ["MILK_PIXEL2_STOCK"], "dtbo.img"), "rb").read()
        e = db.dtbo_entries(b)
        self.assertGreater(len(e), 0)
        self.assertIn(0x1BC, {x["id"] for x in e})


if __name__ == "__main__":
    r = unittest.main(exit=False, verbosity=1)
    if not r.result.wasSuccessful():
        sys.exit(1)
    n = r.result.testsRun - len(r.result.skipped)
    print(f"android-tools: {n} passed")
