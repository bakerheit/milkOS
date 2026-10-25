#!/usr/bin/env python3
"""Read a milkOS crash from the Raspberry Pi's green ACT LED (docs/raspberry-pi.md, "Reading the ACT LED").

Run with `python3 -I scripts/led-decode.py ...`. No dependencies.

  led-decode.py [--elf KERNEL.elf] stage=4 ec=0o45 pc=0o000002321704 far=0o10000240
  led-decode.py [--elf KERNEL.elf] 4 45 000002321704 10000240
  led-decode.py [--elf KERNEL.elf] 4-45-000002321704-10000240      (any separators; 1 + 2 + 12 + 8 digits)
  led-decode.py [--elf KERNEL.elf] --pulses FILE                   (a phone video, measured: see below)

The digits are what you counted, in octal (0 to 7; a long pulse is a 0); the stage is 0 to 8.

--elf is the kernel image's ELF from scripts/pi-build.sh (target/pi/aarch64-unknown-none-softfloat/release/milkos-kernel):
the faulting address is turned into a function and a source line with llvm-symbolizer / addr2line / nm,
whichever the machine has. --src DIR is the source tree (default: this repository), used to find the
file of a panic from the hash the LED carries.

--pulses FILE (or `-` for stdin): the LED's on and off times in milliseconds, one number per line, alternating
on, off, on, off... (a line may also say `on 150` or `off 250`; blank lines and # comments are ignored). The
first complete crash cycle in it is decoded. Measure them from a video by frame number x 1000 / fps.
"""
import glob
import os
import re
import shutil
import subprocess
import sys

KERNEL_BASE = 0xFFFF_0000_0000_0000

# The pattern (rpikit/src/ledcrash.rs has the same numbers; led_decode_test.py checks them against the
# Rust tests' golden cycle).
ALARM_PULSES, ALARM_MS = 10, 100
PULSE_MS, PULSE_GAP_MS, ZERO_MS, DIGIT_GAP_MS, GROUP_GAP_MS = 150, 250, 600, 700, 2500
DIGIT_GROUPS = (("stage", 1), ("ec", 2), ("pc", 12), ("far", 8))
DIGITS = 23
ALARM_OFF_MAX_MS = 175
LONG_ON_MIN_MS = 375
DIGIT_OFF_MIN_MS = 475
GROUP_OFF_MIN_MS = 1600
MAX_STAGE = 8
EC_PANIC, EC_STACK_OVERFLOW, EC_OTHER = 0o77, 0o76, 0o75

STAGES = ["before the first stage", "memory", "mailbox", "framebuffer", "interrupts", "sdcard", "scheduler",
          "userspace", "desktop up"]

EC_NAMES = {
    0x00: "unknown reason", 0x01: "trapped WFI/WFE", 0x0E: "illegal execution state", 0x15: "SVC (AArch64)",
    0x18: "trapped system register", 0x20: "instruction abort from a lower EL (user code)",
    0x21: "instruction abort at EL1 (kernel jumped to a bad address)", 0x22: "PC alignment fault",
    0x24: "data abort from a lower EL (user code)", 0x25: "data abort at EL1 (kernel touched a bad address)",
    0x26: "SP alignment fault", 0x2C: "floating point exception", 0x2F: "SError", 0x30: "breakpoint (lower EL)",
    0x31: "breakpoint (same EL)", 0x3C: "BRK instruction",
    EC_PANIC: "Rust panic (pc = source line, far = file hash)",
    EC_STACK_OVERFLOW: "kernel stack overflow",
    EC_OTHER: "unexpected interrupt vector, or no information",
}

DABT_FSC = {
    0x04: "translation fault, level 0", 0x05: "translation fault, level 1", 0x06: "translation fault, level 2",
    0x07: "translation fault, level 3", 0x09: "access flag fault, level 1", 0x0A: "access flag fault, level 2",
    0x0B: "access flag fault, level 3", 0x0D: "permission fault, level 1", 0x0E: "permission fault, level 2",
    0x0F: "permission fault, level 3", 0x10: "synchronous external abort", 0x21: "alignment fault",
}


def file_hash24(name):
    h = 0x811C9DC5
    for b in name.encode():
        h = ((h ^ b) * 0x01000193) & 0xFFFFFFFF
    return h & 0xFFFFFF


# ---- reading a typed code -------------------------------------------------------------------------

def _octal(text, what):
    t = text.strip().lower()
    if t.startswith("0o"):
        t = t[2:]
    if not t or any(c not in "01234567" for c in t):
        raise ValueError("%s must be octal digits (0 to 7), got %r" % (what, text))
    return t


def parse_typed(text):
    """-> (stage, ec, pc, far) from `stage=4 ec=0o45 pc=... far=...` or from the 23 digits."""
    named = dict(re.findall(r"\b(stage|ec|pc|far)\s*[=:]\s*(\S+)", text, re.I))
    if named:
        for k in ("stage", "ec", "pc", "far"):
            if k not in named:
                raise ValueError("missing %s=" % k)
        stage = int(named["stage"].strip(",;"), 10)
        digits = {k: _octal(named[k].strip(",;"), k) for k in ("ec", "pc", "far")}
    else:
        flat = re.sub(r"0o", "", text.lower())
        tokens = re.findall(r"[0-9]+", flat)
        flat = "".join(tokens)
        if len(flat) != DIGITS:
            raise ValueError("expected %d digits (1 stage + 2 + 12 + 8), got %d: %r" % (DIGITS, len(flat), flat))
        stage = int(flat[0])
        digits = {"ec": _octal(flat[1:3], "ec"), "pc": _octal(flat[3:15], "pc"), "far": _octal(flat[15:], "far")}
    if not 0 <= stage <= MAX_STAGE:
        raise ValueError("the stage is 0 to 8, got %d" % stage)
    ec, pc, far = (int(digits[k], 8) for k in ("ec", "pc", "far"))
    if ec > 0o77 or pc >= 1 << 36 or far >= 1 << 24:
        raise ValueError("a value is too big (ec is 2 digits, pc 12, far 8)")
    return stage, ec, pc, far


# ---- reading a video's pulse list -----------------------------------------------------------------

def decode_intervals(ivs):
    """ivs: [(on: bool, ms)] in time order. -> (stage, ec, pc, far) of the first complete cycle, or None.
    The same rules as rpikit::ledcrash::decode."""
    digits = []
    seen_alarm = False
    pulses = 0
    long = False
    prev_alarm = False
    i = 0
    n = len(ivs)
    while i < n:
        if not ivs[i][0]:
            i += 1
            continue
        on_ms = ivs[i][1]
        if i + 1 < n and not ivs[i + 1][0]:
            off_ms = ivs[i + 1][1]
            i += 2
        elif i + 1 < n:
            i += 1
            continue
        else:
            off_ms = GROUP_GAP_MS
            i += 1
        if off_ms < ALARM_OFF_MAX_MS or prev_alarm:
            prev_alarm = off_ms < ALARM_OFF_MAX_MS
            seen_alarm = True
            digits, pulses, long = [], 0, False
            continue
        if not seen_alarm:
            continue
        if on_ms >= LONG_ON_MIN_MS:
            long = True
        else:
            pulses += 1
        if off_ms < DIGIT_OFF_MIN_MS:
            continue
        d = 0 if (long and pulses == 0) else (255 if long else pulses)
        group_ends = off_ms >= GROUP_OFF_MIN_MS
        k = len(digits)
        pulses, long = 0, False
        if d > (MAX_STAGE if k == 0 else 7) or group_ends != (k + 1 in (1, 3, 15, 23)):
            seen_alarm, digits = False, []
            continue
        digits.append(d)
        if len(digits) == DIGITS:
            def oct_(a, b):
                v = 0
                for x in digits[a:b]:
                    v = v << 3 | x
                return v
            return digits[0], oct_(1, 3), oct_(3, 15), oct_(15, 23)
    return None


def parse_pulses(text):
    ivs = []
    on = True
    for line in text.splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        m = re.fullmatch(r"(?:(on|off|1|0)[\s:,=]+)?([0-9]+(?:\.[0-9]*)?)\s*(?:ms)?", line, re.I)
        if not m:
            raise ValueError("cannot read this line: %r" % line)
        if m.group(1):
            on = m.group(1).lower() in ("on", "1")
        ivs.append((on, int(float(m.group(2)))))
        on = not on
    return ivs


# ---- the numbers, in words ------------------------------------------------------------------------

def tool(names, extra_dirs=()):
    for n in names:
        for d in extra_dirs:
            p = os.path.join(d, n)
            if os.path.isfile(p) and os.access(p, os.X_OK):
                return p
        p = shutil.which(n)
        if p:
            return p
    return None


def toolchain_bins():
    dirs = glob.glob("/opt/homebrew/opt/rustup/bin")
    cargo_home = os.environ.get("CARGO_HOME", os.path.expanduser("~/.cargo"))
    dirs.append(os.path.join(cargo_home, "bin"))
    rustc = tool(["rustc"], dirs)
    if rustc:
        try:
            sysroot = subprocess.run([rustc, "--print", "sysroot"], capture_output=True, text=True, timeout=30).stdout.strip()
            dirs += glob.glob(os.path.join(sysroot, "lib", "rustlib", "*", "bin"))
        except (OSError, subprocess.SubprocessError):
            pass
    dirs += ["/opt/homebrew/opt/llvm/bin", "/usr/local/opt/llvm/bin"]
    return dirs


def demangle(name):
    """Legacy Rust symbols (_ZN...E): the path without the hash. Anything else is left alone."""
    if not name.startswith("_ZN"):
        return name
    parts, i = [], 3
    while i < len(name) and name[i].isdigit():
        j = i
        while name[j].isdigit():
            j += 1
        ln = int(name[i:j])
        parts.append(name[j:j + ln])
        i = j + ln
    if parts and re.fullmatch(r"h[0-9a-f]{16}", parts[-1]):
        parts.pop()
    return "::".join(p.replace("$LT$", "<").replace("$GT$", ">").replace("..", "::") for p in parts) or name


def short_path(text, src):
    """Paths in a symbolizer line: repository files relative to the repository, the standard library as <rust>."""
    if src:
        text = text.replace(os.path.abspath(src).rstrip("/") + "/", "")
    return re.sub(r"/[^ ()]*/lib/rustlib/src/rust/", "<rust>/", text)


def symbolize(elf, addr):
    """-> [str] lines describing the address, empty if nothing could be found."""
    bins = toolchain_bins()
    sym = tool(["llvm-symbolizer"], bins)
    if sym:
        r = subprocess.run([sym, "--obj=" + elf, "--inlining", "--demangle", "0x%x" % addr],
                           capture_output=True, text=True, timeout=120)
        out = [l for l in r.stdout.splitlines() if l.strip()]
        if out and not out[0].startswith("??"):
            return ["%s  (%s)" % (out[i], out[i + 1]) for i in range(0, len(out) - 1, 2)]
    a2l = tool(["llvm-addr2line", "addr2line", "aarch64-elf-addr2line", "aarch64-none-elf-addr2line"], bins)
    if a2l:
        r = subprocess.run([a2l, "-f", "-C", "-i", "-e", elf, "0x%x" % addr], capture_output=True, text=True, timeout=120)
        out = r.stdout.splitlines()
        res = ["%s  (%s)" % (out[i], out[i + 1]) for i in range(0, len(out) - 1, 2) if not out[i].startswith("??")]
        if res:
            return res
    nm = tool(["llvm-nm", "nm", "aarch64-elf-nm"], bins)
    if nm:
        r = subprocess.run([nm, "-n", elf], capture_output=True, text=True, timeout=120)
        best = None
        for line in r.stdout.splitlines():
            f = line.split()
            if len(f) == 3 and f[1] in "tTwW":
                try:
                    a = int(f[0], 16)
                except ValueError:
                    continue
                if a <= addr:
                    best = (a, f[2])
                else:
                    break
        if best:
            return ["%s + 0x%x  (no line info: nm only)" % (demangle(best[1]), addr - best[0])]
    return []


def find_source(src_root, h):
    """Source files (paths relative to src_root) whose name hashes to h; a panic's Location file is
    relative to the workspace root, so that is what is hashed."""
    hits = []
    for root, dirs, files in os.walk(src_root):
        dirs[:] = [d for d in dirs if d not in (".git", "target", "node_modules", "__pycache__")]
        for f in files:
            if f.endswith(".rs"):
                rel = os.path.relpath(os.path.join(root, f), src_root).replace(os.sep, "/")
                if file_hash24(rel) == h:
                    hits.append(rel)
    return sorted(hits)


def describe(stage, ec, pc, far, elf=None, src=None):
    out = []
    out.append("typed:   stage=%d ec=0o%02o pc=0o%012o far=0o%08o" % (stage, ec, pc, far))
    out.append("stage:   %d, %s" % (stage, STAGES[stage]))
    name = EC_NAMES.get(ec, "exception class %#04x" % ec)
    out.append("kind:    ec=%#04x  %s" % (ec, name))
    if ec == EC_PANIC:
        out.append("line:    %d" % pc)
        files = find_source(src, far) if src else []
        out.append("file:    hash %#08x -> %s" % (far, ", ".join(files) if files else "(no file in the source tree has this hash: probably the standard library)"))
        if files:
            out.append("         so: panicked at %s:%d" % (files[0], pc))
        return out
    kernel = ec not in (0x20, 0x24)
    if kernel:
        addr = KERNEL_BASE | pc
        out.append("pc:      %#018x  (kernel; low 36 bits %#011x)" % (addr, pc))
    else:
        addr = None
        out.append("pc:      low 36 bits %#011x  (user code at EL0: the kernel survived the process, not a kernel address)" % pc)
    if ec in (0x20, 0x21, 0x22, 0x24, 0x25, 0x26, 0x34, 0x35):
        out.append("far:     low 24 bits %#08x  (the address that was touched)" % far)
    elif far:
        out.append("far:     %#08x  (not an abort: ignore)" % far)
    if elf and addr is not None:
        if not os.path.isfile(elf):
            out.append("symbol:  %s is not a file" % elf)
        else:
            lines = symbolize(elf, addr)
            if lines:
                out.append("symbol:  " + short_path(lines[0], src))
                out += ["         inlined from " + short_path(l, src) for l in lines[1:5]]
                if len(lines) > 5:
                    out.append("         ... and %d more inlined frames" % (len(lines) - 5))
            else:
                out.append("symbol:  nothing found at %#x in %s (is it the same build as the card's kernel8.img?)" % (addr, elf))
    return out


def main(argv):
    elf = None
    src = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
    pulses = None
    rest = []
    it = iter(argv)
    for a in it:
        if a == "--elf":
            elf = next(it, None)
        elif a == "--src":
            src = next(it, None)
        elif a == "--pulses":
            pulses = next(it, None)
        elif a in ("-h", "--help"):
            print(__doc__)
            return 0
        else:
            rest.append(a)
    try:
        if pulses is not None:
            text = sys.stdin.read() if pulses == "-" else open(pulses).read()
            rec = decode_intervals(parse_pulses(text))
            if rec is None:
                print("no complete crash cycle in the pulse list (needs the fast alarm flashes, then all 23 digits)", file=sys.stderr)
                return 1
        else:
            if not rest:
                print(__doc__, file=sys.stderr)
                return 2
            rec = parse_typed(" ".join(rest))
    except (ValueError, OSError) as e:
        print("led-decode: %s" % e, file=sys.stderr)
        return 2
    print("\n".join(describe(*rec, elf=elf, src=src)))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
