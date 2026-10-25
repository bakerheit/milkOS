#!/usr/bin/env python3
"""Tests of scripts/led-decode.py (run by scripts/led-decode-test.sh): the typed forms, the pulse list
decoder against an encoder written here from the documented pattern (and against the Rust golden
cycle), jitter, truncation, and the symbol lookup when a toolchain is there."""
import importlib.util
import os
import random
import subprocess
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("led_decode", os.path.join(HERE, "led-decode.py"))
ld = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ld)


def encode(stage, ec, pc, far):
    """The documented cycle as [(on, ms)]."""
    d = [stage] + [(ec >> (3 * (1 - k))) & 7 for k in range(2)] + [(pc >> (3 * (11 - k))) & 7 for k in range(12)] \
        + [(far >> (3 * (7 - k))) & 7 for k in range(8)]
    out = []
    for i in range(10):
        out += [(True, 100), (False, 2500 if i == 9 else 100)]
    for i, x in enumerate(d):
        n = max(x, 1)
        for k in range(n):
            out += [(True, 600 if x == 0 else 150), (False, 250 if k < n - 1 else (2500 if i in (0, 2, 14, 22) else 700))]
    return out


EXAMPLE = (4, 0o45, 0x9A3C4, 0x2000A0)


class Typed(unittest.TestCase):
    def test_forms(self):
        want = EXAMPLE
        self.assertEqual(ld.parse_typed("stage=4 ec=0o45 pc=0o000002321704 far=0o10000240"), want)
        self.assertEqual(ld.parse_typed("stage=4, ec=45, pc=2321704, far=10000240"), want)
        self.assertEqual(ld.parse_typed("4 45 000002321704 10000240"), want)
        self.assertEqual(ld.parse_typed("4-45-000002321704-10000240"), want)
        self.assertEqual(ld.parse_typed("4,4,5,0,0,0,0,0,2,3,2,1,7,0,4,1,0,0,0,0,2,4,0"), want)
        self.assertEqual(ld.parse_typed("0" * 23), (0, 0, 0, 0))
        self.assertEqual(ld.parse_typed("8 77 777777777777 77777777"), (8, 0o77, (1 << 36) - 1, (1 << 24) - 1))

    def test_bad_input(self):
        for bad in ["", "4 45", "9 00 000000000000 00000000", "4 48 000000000000 00000000", "stage=4 ec=45",
                    "4 45 00000232170 10000240", "stage=4 ec=0o45 pc=0o9 far=0"]:
            with self.assertRaises(ValueError, msg=bad):
                ld.parse_typed(bad)


class Pulses(unittest.TestCase):
    def test_golden_matches_rust(self):
        v = encode(*EXAMPLE)
        s = 0
        for i, (on, ms) in enumerate(v):
            s = (s * 31 + (i + 1) * ms + on) % 1_000_000_007
        self.assertEqual((len(v), s), (120, 925_751_336))  # GOLDEN_LEN / GOLDEN_SUM in rpikit/src/tests.rs

    def test_round_trip(self):
        r = random.Random(5)
        cases = [(0, 0, 0, 0), (8, 0o77, (1 << 36) - 1, (1 << 24) - 1), EXAMPLE]
        for _ in range(2000):
            cases.append((r.randint(0, 8), r.randint(0, 63), r.getrandbits(36), r.getrandbits(24)))
        for c in cases:
            v = encode(*c)
            self.assertEqual(ld.decode_intervals(v), c)
            self.assertLess(sum(ms for _, ms in v), 90_000)

    def test_middle_of_a_repeat_and_jitter(self):
        r = random.Random(6)
        for _ in range(300):
            c = (r.randint(0, 8), r.randint(0, 63), r.getrandbits(36), r.getrandbits(24))
            v = encode(*c)
            cut = r.randrange(len(v))
            if cut % 2:
                cut += 1
            v = (v[cut:] + encode(*c))
            j = []
            for i, (on, ms) in enumerate(v):
                e = r.randint(0, 33)
                j.append((on, ms + (e if on else -e)))
            self.assertEqual(ld.decode_intervals(j), c)

    def test_no_cycle(self):
        v = encode(*EXAMPLE)
        self.assertIsNone(ld.decode_intervals(v[:-2]))
        self.assertIsNone(ld.decode_intervals(v[:30]))
        self.assertIsNone(ld.decode_intervals([]))
        r = random.Random(7)
        for _ in range(200):
            ld.decode_intervals([(i % 2 == 0, r.randint(0, 4000)) for i in range(200)])

    def test_text_forms(self):
        v = encode(*EXAMPLE)
        plain = "\n".join(str(ms) for _, ms in v)
        self.assertEqual(ld.decode_intervals(ld.parse_pulses(plain)), EXAMPLE)
        labelled = "# a video\n" + "\n".join(("on " if on else "off ") + str(ms) + " ms" for on, ms in v)
        self.assertEqual(ld.decode_intervals(ld.parse_pulses(labelled)), EXAMPLE)
        with self.assertRaises(ValueError):
            ld.parse_pulses("banana")


class Words(unittest.TestCase):
    def test_panic_names_its_file(self):
        h = ld.file_hash24("kernel/src/rpi/led.rs")
        out = "\n".join(ld.describe(5, ld.EC_PANIC, 123, h, src=os.path.join(HERE, "..")))
        self.assertIn("panicked at kernel/src/rpi/led.rs:123", out)
        self.assertEqual(ld.file_hash24(""), 0x811C9DC5 & 0xFFFFFF)

    def test_exception_words(self):
        out = "\n".join(ld.describe(*EXAMPLE))
        self.assertIn("0xffff00000009a3c4", out)
        self.assertIn("data abort at EL1", out)
        self.assertIn("0x2000a0", out)
        out = "\n".join(ld.describe(8, 0x24, 0x400000, 0x10))
        self.assertIn("user code at EL0", out)
        self.assertIn("0xffff", "\n".join(ld.describe(2, 0x25, 0, 0)))

    def test_cli_and_symbol(self):
        exe = os.path.join(HERE, "led-decode.py")
        r = subprocess.run([sys.executable, "-I", exe, "4", "45", "000002321704", "10000240"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertIn("stage:   4, interrupts", r.stdout)
        r = subprocess.run([sys.executable, "-I", exe, "banana"], capture_output=True, text=True)
        self.assertEqual(r.returncode, 2)
        # With a kernel image around: some symbol comes back for the kernel's entry point.
        elf = os.environ.get("LED_DECODE_ELF")
        if elf and os.path.isfile(elf):
            r = subprocess.run([sys.executable, "-I", exe, "--elf", elf, "0", "45", "000002000000", "00000000"], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertNotIn("nothing found", r.stdout)

    def test_demangle(self):
        self.assertEqual(ld.demangle("_ZN6kernel3rpi3led13blink_forever17h0123456789abcdefE"), "kernel::rpi::led::blink_forever")
        self.assertEqual(ld.demangle("main"), "main")


if __name__ == "__main__":
    unittest.main(verbosity=1)
