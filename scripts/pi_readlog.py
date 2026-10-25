#!/usr/bin/env python3
"""Print the boot log out of MILKLOG.TXT without its padding, and say whether the kernel finished saving it.

    scripts/pi-readlog.sh PATH        (this file is what it runs: python3 -I scripts/pi_readlog.py PATH)

The text goes to stdout; the verdict goes to stderr, so `scripts/pi-readlog.sh MILKLOG.TXT > boot.log` keeps the
two apart. Exit status: 0 the end marker is there (everything the kernel logged before its last flush is
in the file), 1 there is text but no end marker (power cut or crash while a flush was running), 2 the
file has not been written by a kernel at all (still the blank file) or cannot be read.

The format is rpikit/src/logimg.rs: a first line `MILKLOG 1 boot=XXXXXXXX`, the log text, then one end marker line
`<<milklog end boot=XXXXXXXX flush=N bytes=M>>`, then spaces. The boot number in the marker must be the one in the first line:
a marker further down the file from an earlier boot is stale.
"""
import re
import sys

MARKER = re.compile(rb"\n<<milklog end boot=([0-9a-f]{8}) flush=([0-9]+) bytes=([0-9]+)( full)?>>\n")
HEAD = re.compile(rb"^MILKLOG 1 boot=([0-9a-f]{8})\n")


def main():
    if len(sys.argv) != 2:
        sys.exit("usage: scripts/pi-readlog.sh PATH-TO-MILKLOG.TXT")
    try:
        with open(sys.argv[1], "rb") as f:
            data = f.read()
    except OSError as e:
        print(f"pi-readlog: cannot read {sys.argv[1]}: {e}", file=sys.stderr)
        return 2
    head = HEAD.match(data)
    if not head:
        print("pi-readlog: this file has not been written by a milkOS boot (it does not start with 'MILKLOG 1 boot=').", file=sys.stderr)
        print("            Was it the file on the card the board booted from, and did the boot get as far as finding it?", file=sys.stderr)
        return 2
    boot = head.group(1)
    m = MARKER.search(data)
    if m:
        text = data[: m.start()] + b"\n"
        sys.stdout.buffer.write(text)
        sys.stdout.buffer.flush()
        note = []
        if m.group(1) != boot:
            note.append(f"WARNING: the end marker is from another boot ({m.group(1).decode()}, this log is {boot.decode()}): this boot was cut off before its first flush finished")
            print("pi-readlog: " + note[0], file=sys.stderr)
            return 1
        full = " The file ran out of room: the end of the log is missing, make MILKLOG.TXT bigger." if m.group(4) else ""
        print(
            f"pi-readlog: end marker present: boot {boot.decode()}, flush {int(m.group(2))}, {int(m.group(3))} bytes of log. "
            f"Everything logged before that flush is above.{full}",
            file=sys.stderr,
        )
        return 0
    # No marker: print up to where the padding starts.
    end = data.find(b" " * 64)
    text = data if end < 0 else data[:end]
    sys.stdout.buffer.write(text.rstrip() + b"\n")
    sys.stdout.buffer.flush()
    print(
        "pi-readlog: NO end marker: the board lost power, or crashed, while a flush was being written. The text above is what "
        "reached the card and may stop in the middle of a line.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
