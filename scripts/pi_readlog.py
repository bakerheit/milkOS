#!/usr/bin/env python3
"""Read the milkOS SD card log (MILKLOG.TXT) without its padding, one boot or all of them.

    scripts/pi-readlog.sh PATH [--all | --last | --boot N | --list] [--tail N]

(that script runs `python3 -I scripts/pi_readlog.py ...`)

    --all       the whole log, every boot in order (the default)
    --last      just the newest boot
    --boot N    just boot N (see --list for the numbers)
    --list      one line per boot: where it starts, how many lines, its first and last uptime stamp, build, and how it
                ended (clean shutdown, restart, crash, or a power cut / reset: no end line)
    --tail N    only the last N lines of what would be printed

The text goes to stdout, the notes (what the file is, warnings) to stderr. Exit status: 0 the log was read and is whole,
1 it was read but has damage, a wrap or a cut-off end that the notes explain, 2 the file is not a milkOS log or cannot
be read.

Formats (rpikit/src/milklog.rs has the reference implementation and the full description):

* format 2 (this kernel): the file is two header sectors and then a ring of sectors, each 500 bytes of text and a
  12-byte trailer (absolute sector number, bytes used, boot number, CRC-32). The header is only a hint; the end of the text
  is found by following the sectors from the header's cursor (or, if the header is damaged, by scanning the whole
  ring), the same way the kernel does at boot.
* format 1 (the first version: one boot, padding, an end marker): `MILKLOG 1 boot=XXXXXXXX`, the text, then
  `<<milklog end boot=XXXXXXXX flush=N bytes=M>>` and spaces. Still read.
"""
import re
import struct
import sys
import zlib

SECTOR = 512
PAYLOAD = 500
HEADER_SECTORS = 2
MIN_RING = 16
MAX_BOOTS = 28
MAGIC = b"MILKLOG2"
STATES = {0: "running or cut off", 1: "shutdown", 2: "restart", 3: "crash"}

# ---- format 1 ---------------------------------------------------------------------------------

V1_MARKER = re.compile(rb"\n<<milklog end boot=([0-9a-f]{8}) flush=([0-9]+) bytes=([0-9]+)( full)?>>\n")
V1_HEAD = re.compile(rb"^MILKLOG 1 boot=([0-9a-f]{8})\n")


def read_v1(data, out):
    head = V1_HEAD.match(data)
    boot = head.group(1)
    m = V1_MARKER.search(data)
    if m:
        out.write(data[: m.start()] + b"\n")
        if m.group(1) != boot:
            note(f"WARNING: the end marker is from another boot ({m.group(1).decode()}, this log is {boot.decode()}): this boot was cut off before its first flush finished")
            return 1
        full = " The file ran out of room: the end of the log is missing, make MILKLOG.TXT bigger." if m.group(4) else ""
        note(f"format 1 log, end marker present: boot {boot.decode()}, flush {int(m.group(2))}, {int(m.group(3))} bytes of log. Everything logged before that flush is above.{full}")
        return 0
    end = data.find(b" " * 64)
    text = data if end < 0 else data[:end]
    out.write(text.rstrip() + b"\n")
    note("format 1 log, NO end marker: the board lost power, or crashed, while a flush was being written. The text above is what reached the card and may stop in the middle of a line.")
    return 1


# ---- format 2 ---------------------------------------------------------------------------------


def open_sector(sec):
    """(abs, len, boot) if sec is a real ring sector, else None."""
    if len(sec) != SECTOR:
        return None
    abs_, ln, boot = struct.unpack_from("<IHH", sec, PAYLOAD)
    if ln > PAYLOAD:
        return None
    if zlib.crc32(sec[: PAYLOAD + 8]) != struct.unpack_from("<I", sec, PAYLOAD + 8)[0]:
        return None
    return abs_, ln, boot


def decode_header(sec):
    if len(sec) != SECTOR or sec[:8] != MAGIC:
        return None
    ver, seq, fsec, ring, payload, boots = struct.unpack_from("<IIIIII", sec, 8)
    cursor, n = struct.unpack_from("<QI", sec, 32)
    if ver != 2 or payload != PAYLOAD or n > MAX_BOOTS or ring < MIN_RING or fsec != ring + HEADER_SECTORS:
        return None
    if zlib.crc32(sec[: SECTOR - 4]) != struct.unpack_from("<I", sec, SECTOR - 4)[0]:
        return None
    table = []
    for i in range(n):
        b, st, start = struct.unpack_from("<IIQ", sec, 48 + 16 * i)
        table.append((b, st, start))
    return {"seq": seq, "file": fsec, "ring": ring, "boot_count": boots, "cursor": cursor, "boots": table}


def seq_newer(a, b):
    return a != b and ((a - b) & 0xFFFFFFFF) < 0x80000000


class Log:
    """The ring of a format 2 file."""

    def __init__(self, data):
        self.data = data
        self.sectors = len(data) // SECTOR
        self.ring = self.sectors - HEADER_SECTORS

    def sector(self, idx):
        return self.data[idx * SECTOR : (idx + 1) * SECTOR]

    def slot(self, a):
        return self.sector(HEADER_SECTORS + a % self.ring)

    def best_header(self):
        h0, h1 = decode_header(self.sector(0)), decode_header(self.sector(1))
        if h0 and h1:
            return h1 if seq_newer(h1["seq"], h0["seq"]) else h0
        return h0 or h1

    def forward(self, a0):
        """Follow valid sectors from absolute sector a0: (abs, len, boot, started)."""
        abs_, ln, boot, started = a0, 0, 0, False
        for done in range(self.ring):
            expect = (a0 + done) & 0xFFFFFFFF
            info = open_sector(self.slot(expect))
            if not info or info[0] != expect or (started and ((info[2] - boot) & 0xFFFF) >= 0x8000):
                return abs_, ln, boot, started
            started = True
            abs_, ln, boot = expect, info[1], info[2]
            if ln < PAYLOAD:
                return abs_, ln, boot, started
            abs_, ln = (expect + 1) & 0xFFFFFFFF, 0
        return abs_, ln, boot, started

    def previous_is_full(self, a):
        prev = (a - 1) & 0xFFFFFFFF
        info = open_sector(self.slot(prev))
        return bool(info and info[0] == prev and info[1] == PAYLOAD)

    def scan_newest(self):
        best = None
        for slot in range(self.ring):
            info = open_sector(self.sector(HEADER_SECTORS + slot))
            if info and info[0] % self.ring == slot and (best is None or info[0] > best[0]):
                best = info
        return best

    def recover(self):
        """(end position in text bytes, how, header or None), or (None, reason, header)."""
        if self.ring < MIN_RING:
            return None, "the file is too small for a ring", None
        best = self.best_header()
        valid = best if best and best["file"] == self.sectors and best["ring"] == self.ring else None
        if valid:
            a = valid["cursor"] // PAYLOAD
            if a < 0x7FFFFFFF:
                abs_, ln, boot, started = self.forward(a)
                want = valid["cursor"] % PAYLOAD
                good = (abs_ > a or ln >= want) if started else (want == 0 and (a == 0 or self.previous_is_full(a)))
                if good:
                    return abs_ * PAYLOAD + ln, "from the header", valid
        if best is None:
            s0, s1 = self.sector(0), self.sector(1)
            if s0.startswith(b"MILKLOG 1 boot="):
                return None, "v1", None
            body = s0
            if s0.startswith(b"MILKLOG.TXT:"):
                nl = s0.find(b"\n")
                body = s0[nl:] if nl >= 0 else b""
            plain = b" \0\n\r"
            if all(c in plain for c in body) and all(c in plain for c in s1) and all(c in plain for c in self.sector(HEADER_SECTORS)):
                return 0, "a blank file: nothing logged yet", None
        newest = self.scan_newest()
        if newest is None:
            return None, "nothing in the file is a milkOS log", best
        abs_, ln, boot, started = self.forward(newest[0])
        return abs_ * PAYLOAD + ln, "by scanning the whole ring (the header is damaged or wrong)", None

    def text(self, end):
        """The log text: (list of (stream position, bytes), number of damaged sectors, first absolute sector)."""
        end_abs = -(-end // PAYLOAD)
        first = max(0, end_abs - self.ring)
        chunks, damaged, started, missing = [], 0, False, 0
        for a in range(first, end_abs):
            info = open_sector(self.slot(a))
            if info and info[0] == a:
                if missing:
                    chunks.append((None, missing))
                    damaged += missing
                    missing = 0
                started = True
                chunks.append((a * PAYLOAD, self.slot(a)[: info[1]]))
            elif started or first == 0:
                missing += 1
        if missing:
            chunks.append((None, missing))
            damaged += missing
        return chunks, damaged, first


BANNER = re.compile(rb"^=== milkOS boot ([0-9]+) === (.*)$")
ENDLINE = re.compile(rb"^=== milkOS boot ([0-9]+) ended: (.*?) ===$")
STAMP = re.compile(rb"^\[ *([0-9]+)\.([0-9]{3})\] ")
BUILD = re.compile(rb"build (\S+)")


def note(msg):
    print("pi-readlog: " + msg, file=sys.stderr)


def sections(text):
    """Split the text at banner lines: a list of dicts (boot, start offset, end offset, ...)."""
    out = []
    pos = 0
    lines = text.split(b"\n")
    offsets = []
    for ln in lines:
        offsets.append(pos)
        pos += len(ln) + 1
    cur = None
    for ln, off in zip(lines, offsets):
        m = BANNER.match(ln)
        if m:
            if cur:
                cur["end"] = off
                out.append(cur)
            b = BUILD.search(m.group(2))
            cur = {"boot": int(m.group(1)), "start": off, "end": len(text), "banner": ln, "build": b.group(1).decode() if b else "?"}
    if cur:
        out.append(cur)
    first = out[0]["start"] if out else len(text)
    return out, first


def describe(sec, text, hdr_states, last):
    body = text[sec["start"] : sec["end"]]
    lines = body.split(b"\n")
    stamps = [int(m.group(1)) * 1000 + int(m.group(2)) for m in (STAMP.match(l) for l in lines) if m]
    sec["lines"] = sum(1 for l in lines if l)
    sec["first"] = stamps[0] if stamps else None
    sec["last"] = stamps[-1] if stamps else None
    ended = None
    for l in lines:
        m = ENDLINE.match(l)
        if m and int(m.group(1)) == sec["boot"]:
            ended = m.group(2).decode()
    if ended:
        sec["ended"] = ended
    elif b"*** KERNEL PANIC" in body or b"led: crash code" in body:
        sec["ended"] = "crash (the end line is missing)"
    elif sec["boot"] in hdr_states and hdr_states[sec["boot"]] in (1, 2, 3):
        sec["ended"] = STATES[hdr_states[sec["boot"]]] + " (from the header)"
    elif last:
        sec["ended"] = "no end line: still running when the card was read, or power was cut"
    else:
        sec["ended"] = "no end line: power cut or reset (nothing says it ended)"


def fmt_ms(ms):
    if ms is None:
        return "-"
    s = ms / 1000.0
    return f"{int(s // 3600)}h{int(s % 3600 // 60):02d}m{s % 60:04.1f}s" if s >= 3600 else f"{int(s // 60)}m{s % 60:04.1f}s"


def read_v2(data, args, out):
    log = Log(data)
    end, how, hdr = log.recover()
    if end is None:
        if how == "v1":
            return None
        note(f"{how}.")
        return 2
    if end == 0:
        note(how)
        return 0
    chunks, damaged, first = log.text(end)
    buf = bytearray()
    spans = []  # (text offset, stream position) of each chunk, to turn an offset back into a position
    for pos, payload in chunks:
        if pos is None:
            buf += f"\n[pi-readlog: {payload} sector(s) of the log are missing or damaged here: about {payload * PAYLOAD} bytes of text lost]\n".encode()
        else:
            spans.append((len(buf), pos))
            buf += payload
    text = bytes(buf)
    wrapped = first > 0
    secs, first_banner = sections(text)
    states = {b[0]: b[1] for b in hdr["boots"]} if hdr else {}
    for i, s in enumerate(secs):
        describe(s, text, states, i == len(secs) - 1)

    def position(off):
        p = 0
        for t, sp in spans:
            if t <= off:
                p = sp + (off - t)
        return p

    rc = 0
    notes = []
    if wrapped:
        rc = 1
        notes.append(f"WARNING: the ring has wrapped ({first * PAYLOAD} bytes of the oldest text have been overwritten): the log starts partway through the oldest boot shown. A bigger MILKLOG.TXT keeps more.")
    if damaged:
        rc = 1
        notes.append(f"WARNING: {damaged} sector(s) of the ring are damaged or missing (marked in the text).")
    if first_banner > 0 and secs:
        pre = text[:first_banner].strip()
        if pre and not wrapped:
            notes.append("note: some text comes before the first boot banner.")

    # What to print.
    mode = args["mode"]
    selected = text
    title = "all boots"
    if mode in ("last", "boot"):
        if not secs:
            note("no boot banner was found in the log; showing everything.")
        else:
            want = secs[-1] if mode == "last" else next((s for s in secs if s["boot"] == args["boot"]), None)
            if want is None:
                note(f"no boot {args['boot']} in the log (boots found: {', '.join(str(s['boot']) for s in secs)})")
                return 2
            selected = text[want["start"] : want["end"]].lstrip(b"\n")
            title = f"boot {want['boot']}"
    if mode == "list":
        table = [f"{'boot':>5} {'starts at byte':>14} {'lines':>6} {'first stamp':>12} {'last stamp':>12} {'uptime':>10}  {'build':<14} ended"]
        if first_banner > 0 and (not secs or text[:first_banner].strip()):
            table.append(f"{'(cut)':>5} {'-':>14} {text[:first_banner].count(bytes([10])):>6} {'-':>12} {'-':>12} {'-':>10}  {'-':<14} text from before the oldest banner (the ring wrapped)")
        for s in secs:
            up = (s["last"] - s["first"]) if s["first"] is not None and s["last"] is not None else None
            table.append(
                f"{s['boot']:>5} {position(s['start']):>14} {s['lines']:>6} {fmt_ms(s['first']):>12} {fmt_ms(s['last']):>12} {fmt_ms(up):>10}  {s['build']:<14} {s['ended']}"
            )
        out.write(("\n".join(table) + "\n").encode())
        if hdr:
            note(f"header: boot counter {hdr['boot_count']}, cursor {hdr['cursor']}, {len(hdr['boots'])} boots indexed")
    else:
        if args["tail"] is not None:
            lines = selected.split(b"\n")
            if lines and lines[-1] == b"":
                lines.pop()
            selected = b"\n".join(lines[-args["tail"] :]) + b"\n" if args["tail"] > 0 else b""
        out.write(selected if selected.endswith(b"\n") or not selected else selected + b"\n")
    last = secs[-1] if secs else None
    note(
        f"format 2 log, {title}: {len(secs)} boot(s) in the file, {end} bytes of text ever written ({how}); "
        + (f"the newest boot is {last['boot']}: {last['ended']}" if last else "no boot banner found")
    )
    for n in notes:
        note(n)
    return rc


class Usage(Exception):
    pass


USAGE = "usage: scripts/pi-readlog.sh PATH [--all | --last | --boot N | --list] [--tail N]"


def parse_args(argv):
    args = {"mode": "all", "boot": None, "tail": None, "path": None}
    it = iter(argv)
    for a in it:
        if a in ("--all", "--last", "--list"):
            args["mode"] = a[2:]
        elif a == "--boot":
            v = next(it, None)
            if v is None or not v.isdigit():
                raise Usage("--boot needs a boot number (see --list)")
            args["mode"], args["boot"] = "boot", int(v)
        elif a == "--tail":
            v = next(it, None)
            if v is None or not v.isdigit():
                raise Usage("--tail needs a number of lines")
            args["tail"] = int(v)
        elif a.startswith("-"):
            raise Usage(f"unknown option {a}")
        elif args["path"] is None:
            args["path"] = a
        else:
            raise Usage("one file at a time")
    if args["path"] is None:
        raise Usage("which file?")
    return args


def main():
    try:
        args = parse_args(sys.argv[1:])
    except Usage as e:
        print(f"pi-readlog: {e}\n{USAGE}", file=sys.stderr)
        return 2
    try:
        with open(args["path"], "rb") as f:
            data = f.read()
    except OSError as e:
        note(f"cannot read {args['path']}: {e}")
        return 2
    out = sys.stdout.buffer
    if V1_HEAD.match(data):
        if args["mode"] not in ("all",) or args["tail"] is not None:
            note("this is a format 1 log (one boot): showing all of it")
        rc = read_v1(data, out)
        out.flush()
        return rc
    if len(data) < (HEADER_SECTORS + MIN_RING) * SECTOR:
        note("this file is too small to be a milkOS log.")
        return 2
    rc = read_v2(data, args, out)
    out.flush()
    if rc is None:
        note("this file has not been written by a milkOS boot (it is not a log file in format 1 or 2). Was it the file on the card the board booted from, and did the boot get as far as finding it?")
        return 2
    return rc


if __name__ == "__main__":
    sys.exit(main())
