#!/usr/bin/env python3
"""Tests of the Mac-side log reader (scripts/pi_readlog.py, run through scripts/pi-readlog.sh) on made-up files.

    python3 -I scripts/pi_readlog_test.py        (scripts/pi-readlog-test.sh runs this)

The files are built here by a small writer that follows the format in rpikit/src/milklog.rs (the Rust host tests
check the kernel's writer against the same golden numbers): healthy logs of one and many boots, a wrapped ring, torn
and lost headers, damaged sectors, an old format 1 file, blank, truncated and garbage files.
"""
import os
import random
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
READER = os.path.join(HERE, "pi_readlog.py")
SECTOR, PAYLOAD = 512, 500


def seal(payload, abs_, boot):
    sec = bytearray(SECTOR)
    sec[: len(payload)] = payload
    struct.pack_into("<IHH", sec, PAYLOAD, abs_, len(payload), boot)
    struct.pack_into("<I", sec, PAYLOAD + 8, zlib.crc32(bytes(sec[: PAYLOAD + 8])))
    return bytes(sec)


def header(seq, file_sectors, boot_count, cursor, boots):
    h = bytearray(SECTOR)
    h[:8] = b"MILKLOG2"
    struct.pack_into("<IIIIII", h, 8, 2, seq, file_sectors, file_sectors - 2, PAYLOAD, boot_count)
    struct.pack_into("<QI", h, 32, cursor, len(boots))
    for i, (b, st, start) in enumerate(boots):
        struct.pack_into("<IIQ", h, 48 + 16 * i, b, st, start)
    struct.pack_into("<I", h, SECTOR - 4, zlib.crc32(bytes(h[: SECTOR - 4])))
    return bytes(h)


class Writer:
    """Just enough of the kernel's writer to make files."""

    def __init__(self, sectors):
        self.sectors = sectors
        self.ring = sectors - 2
        blank = b"MILKLOG.TXT: the milkOS log goes here (the kernel overwrites this file in place; nothing below yet).\n"
        self.data = bytearray((blank + b" " * (sectors * SECTOR))[: sectors * SECTOR])
        self.pos = 0
        self.boot = 0
        self.boots = []
        self.seq = 0

    def begin_boot(self):
        self.boot += 1
        self.boots.append((self.boot, 0, self.pos))
        self.boots = self.boots[-28:]
        self.write_header()
        return self.boot

    def end_boot(self, state):
        self.boots[-1] = (self.boots[-1][0], state, self.boots[-1][2])
        self.write_header()

    def write_header(self, slot=None):
        self.seq += 1
        at = self.seq & 1 if slot is None else slot
        self.data[at * SECTOR : (at + 1) * SECTOR] = header(self.seq, self.sectors, self.boot, self.pos, self.boots)

    def write(self, text):
        if isinstance(text, str):
            text = text.encode()
        while text:
            abs_ = self.pos // PAYLOAD
            off = self.pos % PAYLOAD
            old = b""
            if off:
                at = (2 + abs_ % self.ring) * SECTOR
                old = bytes(self.data[at : at + off])
            take = text[: PAYLOAD - off]
            payload = old + take
            at = (2 + abs_ % self.ring) * SECTOR
            self.data[at : at + SECTOR] = seal(payload, abs_, self.boot)
            self.pos += len(take)
            text = text[len(take) :]
            if self.pos % (16 * PAYLOAD) == 0:
                self.write_header()


def lines(boot, n, base=0):
    out = [f"=== milkOS boot {boot} === milkOS 0.2.0-dev | build abc1234 | git abc1234 | log format 2\n"]
    for i in range(n):
        out.append(f"[{(base + i) // 10:5}.{(base + i) % 10 * 100:03}] boot {boot} line {i}: the quick brown fox jumps over the lazy dog\n")
    return "".join(out)


class Case(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.mkdtemp(prefix="milk-readlog.")

    def tearDown(self):
        shutil.rmtree(self.dir, ignore_errors=True)

    def path(self, name="MILKLOG.TXT"):
        return os.path.join(self.dir, name)

    def put(self, data, name="MILKLOG.TXT"):
        with open(self.path(name), "wb") as f:
            f.write(bytes(data))
        return self.path(name)

    def run_reader(self, path, *args):
        p = subprocess.run([sys.executable, "-I", READER, path, *args], capture_output=True)
        return p.returncode, p.stdout.decode(errors="replace"), p.stderr.decode(errors="replace")

    def three_boots(self, sectors=2048):
        w = Writer(sectors)
        w.begin_boot()
        w.write(lines(1, 30))
        w.begin_boot()
        w.write(lines(2, 40, 100))
        w.write("*** KERNEL PANIC ***\nled: crash code stage=7 ec=0o75 pc=0o1 far=0o0 [panic]\n=== milkOS boot 2 ended: crash ===\n")
        w.end_boot(3)
        w.begin_boot()
        w.write(lines(3, 20, 300))
        w.write("=== milkOS boot 3 ended: shutdown ===\n")
        w.end_boot(1)
        return w


class Healthy(Case):
    def test_golden_numbers_match_the_kernel_tests(self):
        sec = seal(b"hello milk\n", 7, 3)
        self.assertEqual(sec[500:508], bytes([7, 0, 0, 0, 11, 0, 3, 0]))
        self.assertEqual(struct.unpack_from("<I", sec, 508)[0], 0x17FBA60E)
        h = header(5, 130, 3, 12345, [(1, 1, 0), (2, 3, 500), (3, 0, 9000)])
        self.assertEqual(struct.unpack_from("<I", h, 508)[0], 0x91F9E996)

    def test_all_boots_in_order(self):
        w = self.three_boots()
        rc, out, err = self.run_reader(self.put(w.data))
        self.assertEqual(rc, 0, err)
        want = lines(1, 30) + lines(2, 40, 100)
        self.assertIn(want, out)
        # Boot 1 comes before boot 2 before boot 3.
        i1, i2, i3 = (out.index(f"=== milkOS boot {n} === ") for n in (1, 2, 3))
        self.assertTrue(i1 < i2 < i3)
        self.assertIn("3 boot(s) in the file", err)
        self.assertIn("the newest boot is 3: shutdown", err)

    def test_list_says_how_each_boot_ended(self):
        w = self.three_boots()
        rc, out, err = self.run_reader(self.put(w.data), "--list")
        self.assertEqual(rc, 0, err)
        rows = [l for l in out.splitlines() if l.strip()[:1].isdigit()]
        self.assertEqual(len(rows), 3, out)
        self.assertIn("no end line", rows[0])
        self.assertIn("crash", rows[1])
        self.assertIn("shutdown", rows[2])
        self.assertIn("abc1234", rows[0])
        # Lines and uptime come from the stamps.
        self.assertRegex(rows[0], r"\s31\s")

    def test_one_boot_and_the_last_boot(self):
        w = self.three_boots()
        p = self.put(w.data)
        rc, out, err = self.run_reader(p, "--boot", "2")
        self.assertEqual(rc, 0, err)
        self.assertTrue(out.startswith("=== milkOS boot 2 === "))
        self.assertIn("KERNEL PANIC", out)
        self.assertNotIn("boot 1 line", out)
        self.assertNotIn("boot 3 line", out)
        rc, out, err = self.run_reader(p, "--last")
        self.assertTrue(out.startswith("=== milkOS boot 3 === "))
        self.assertTrue(out.rstrip().endswith("ended: shutdown ==="))
        rc, out, err = self.run_reader(p, "--boot", "9")
        self.assertEqual(rc, 2)
        self.assertIn("no boot 9", err)

    def test_tail(self):
        p = self.put(self.three_boots().data)
        rc, out, err = self.run_reader(p, "--tail", "3")
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(out.splitlines()), 3)
        self.assertTrue(out.rstrip().endswith("ended: shutdown ==="))
        rc, out, err = self.run_reader(p, "--boot", "1", "--tail", "2")
        self.assertEqual(len(out.splitlines()), 2)
        self.assertIn("boot 1 line 29", out)
        rc, out, err = self.run_reader(p, "--tail", "0")
        self.assertEqual(out, "")

    def test_a_blank_file_is_nothing_logged_yet(self):
        rc, out, err = self.run_reader(self.put(Writer(64).data))
        self.assertEqual((rc, out), (0, ""))
        self.assertIn("blank", err)

    def test_nothing_after_the_end_is_printed(self):
        # Old text of an earlier lap, left behind the end, is not shown.
        w = Writer(40)
        w.begin_boot()
        for i in range(500):
            w.write(f"[{i:5}.000] lap one line {i}\n")
        w.begin_boot()
        w.write("=== milkOS boot 2 === x\nnew\n")
        rc, out, err = self.run_reader(self.put(w.data))
        self.assertTrue(out.rstrip().endswith("new"))
        self.assertEqual(out.count("=== milkOS boot 2 ==="), 1)


class Damaged(Case):
    def test_the_ring_wrapped_and_says_so(self):
        w = Writer(128)
        for b in range(1, 7):
            w.begin_boot()
            w.write(lines(b, 150, b * 1000))
        rc, out, err = self.run_reader(self.put(w.data))
        self.assertEqual(rc, 1, err)
        self.assertIn("the ring has wrapped", err)
        self.assertTrue(out.rstrip().endswith(f"boot 6 line 149: the quick brown fox jumps over the lazy dog"))
        self.assertIn("=== milkOS boot 6 === ", out)
        self.assertNotIn("=== milkOS boot 1 === ", out)
        rc, lst, err = self.run_reader(self.put(w.data), "--list")
        self.assertIn("(cut)", lst)
        self.assertIn("    6 ", lst)

    def test_a_torn_header_copy_changes_nothing(self):
        w = self.three_boots()
        good = self.run_reader(self.put(w.data))[1]
        for at in (0, 1):
            d = bytearray(w.data)
            d[at * SECTOR : (at + 1) * SECTOR] = os.urandom(SECTOR)
            rc, out, err = self.run_reader(self.put(d))
            self.assertEqual(out, good, f"copy {at}")

    def test_no_header_at_all_is_found_by_scanning(self):
        w = self.three_boots()
        good = self.run_reader(self.put(w.data))[1]
        d = bytearray(w.data)
        d[: 2 * SECTOR] = b"\0" * (2 * SECTOR)
        rc, out, err = self.run_reader(self.put(d))
        self.assertEqual(out, good)
        self.assertIn("scanning the whole ring", err)

    def test_a_header_that_lies_is_not_believed(self):
        w = self.three_boots()
        good = self.run_reader(self.put(w.data))[1]
        for cursor in (0, 1, 499, 500, 10**6, w.pos + 1, w.pos + 5000, 2**40, 2**63):
            d = bytearray(w.data)
            d[0:SECTOR] = header(w.seq + 10, w.sectors, w.boot, cursor, w.boots)
            rc, out, err = self.run_reader(self.put(d))
            self.assertEqual(out, good, f"cursor {cursor}")

    def test_a_damaged_sector_is_marked_and_the_rest_read(self):
        w = self.three_boots()
        d = bytearray(w.data)
        at = (2 + 5) * SECTOR
        d[at + 100] ^= 0xFF
        rc, out, err = self.run_reader(self.put(d))
        self.assertEqual(rc, 1)
        self.assertIn("1 sector(s) of the log are missing or damaged here", out)
        self.assertIn("boot 1 line 29", out)
        self.assertIn("damaged", err)

    def test_a_cut_end_is_still_read(self):
        # The last sector never landed (a torn flush): the text ends at the sector before.
        w = Writer(256)
        w.begin_boot()
        w.write(lines(1, 60))
        d = bytearray(w.data)
        last = 2 + (w.pos // PAYLOAD) % w.ring
        d[last * SECTOR : (last + 1) * SECTOR] = b" " * SECTOR
        rc, out, err = self.run_reader(self.put(d))
        self.assertEqual(rc, 0, err)
        self.assertTrue(out.startswith("=== milkOS boot 1 === "))
        self.assertLess(len(out), w.pos)

    def test_a_ghost_after_a_gap_is_not_text(self):
        # Sectors 6 and 7 of a flush landed but 5 did not: the text ends before the gap.
        w = Writer(256)
        w.begin_boot()
        w.write("x" * (5 * PAYLOAD))
        for n in (6, 7):
            at = (2 + n) * SECTOR
            w.data[at : at + SECTOR] = seal(b"g" * PAYLOAD, n, 1)
        rc, out, err = self.run_reader(self.put(w.data))
        self.assertNotIn("gggggggggg", out)
        self.assertIn("xxxxxxxxxx", out)


class Odd(Case):
    def test_old_format_one_files(self):
        head = b"MILKLOG 1 boot=0000abcd\nmilkOS test\n--------\nline one\nline two\n"
        mk = lambda boot, fl, n: b"\n<<milklog end boot=%s flush=%d bytes=%d>>\n" % (boot, fl, n)
        rc, out, err = self.run_reader(self.put(head[:-1] + mk(b"0000abcd", 3, len(head) - 1) + b" " * 3000))
        self.assertEqual(rc, 0, err)
        self.assertIn("line two\n", out)
        self.assertNotIn("milklog end", out)
        self.assertIn("format 1", err)
        rc, out, err = self.run_reader(self.put(head + b"line thr" + b" " * 3000))
        self.assertEqual(rc, 1)
        self.assertIn("line thr\n", out)
        rc, out, err = self.run_reader(self.put(head + b"line thr" + b" " * 100 + mk(b"00001111", 9, 5) + b" " * 100))
        self.assertEqual(rc, 1)

    def test_not_a_log(self):
        for data in (b"", b"hello", b"\0" * 100, os.urandom(40000), b"\xff" * 20000, b"MILKLOG2" * 3000):
            rc, out, err = self.run_reader(self.put(data))
            self.assertEqual(rc, 2, (data[:20], err))
            self.assertNotIn("Traceback", err)

    def test_random_damage_never_crashes_the_reader(self):
        rng = random.Random(7)
        w = self.three_boots(512)
        for round_ in range(60):
            d = bytearray(w.data)
            for _ in range(1 + round_ % 10):
                d[rng.randrange(len(d))] = rng.randrange(256)
            if round_ % 5 == 0:
                d = d[: rng.randrange(len(d))]
            rc, out, err = self.run_reader(self.put(d), rng.choice(["--all", "--list", "--last"]))
            self.assertIn(rc, (0, 1, 2), err)
            self.assertNotIn("Traceback", err)

    def test_options(self):
        for args in (["--nope"], ["--boot"], ["--boot", "x"], ["--tail"], ["--tail", "-1"], [], ["a", "b"]):
            p = subprocess.run([sys.executable, "-I", READER, *args], capture_output=True)
            self.assertEqual(p.returncode, 2, args)
            self.assertIn(b"usage", p.stderr)
            self.assertNotIn(b"Traceback", p.stderr)
        rc, out, err = self.run_reader(self.path("missing.txt"))
        self.assertEqual(rc, 2)
        self.assertIn("cannot read", err)


if __name__ == "__main__":
    unittest.main(verbosity=1)
