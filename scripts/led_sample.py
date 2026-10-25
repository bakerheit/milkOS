#!/usr/bin/env python3
"""Watch the ACT LED pin (GPIO 42) of a running qemu raspi4b and decode the crash cycle from it.

  led_sample.py QMP_SOCKET SECONDS OUT

Reads GPLEV1 (0xfe200038, bit 10) over QMP again and again, keeps the levels' on/off times in milliseconds
(OUT gets them, one per line, alternating on/off) and stops as soon as scripts/led-decode.py's decoder finds
one complete crash cycle in them. Prints the record as `stage=.. ec=0o.. pc=0o.. far=0o..` and exits 0, or
exits 1 after SECONDS without one. Used by scripts/pi-qemu-test.sh.
"""
import importlib.util
import os
import sys
import time

here = os.path.dirname(os.path.abspath(__file__))
for name, mod in (("qmp", "qmp.py"), ("led_decode", "led-decode.py")):
    spec = importlib.util.spec_from_file_location(name, os.path.join(here, mod))
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    globals()[name] = m


def main():
    sock, seconds, out = sys.argv[1], float(sys.argv[2]), sys.argv[3]
    end = time.monotonic() + seconds
    q = None
    while q is None and time.monotonic() < end:
        try:
            q = qmp.Qmp(sock)
        except (OSError, SystemExit):
            time.sleep(0.2)
    if q is None:
        return 1
    ivs = []
    level, since = None, time.monotonic()
    open(out, "w").close()
    while time.monotonic() < end:
        try:
            r = q.cmd("human-monitor-command", **{"command-line": "xp /1wx 0xfe200038"})
        except (OSError, SystemExit):
            return 1
        now = time.monotonic()
        try:
            v = (int(r.split(":")[1].split()[0], 16) >> 10) & 1
        except (IndexError, ValueError):
            continue
        if level is None:
            level, since = v, now
        elif v != level:
            ivs.append((bool(level), int(round((now - since) * 1000))))
            level, since = v, now
            if not ivs[-1][0]:
                rec = led_decode.decode_intervals(ivs)
                if rec:
                    with open(out, "w") as f:
                        f.write("\n".join(str(ms) for _, ms in ivs[ivs.index(next(i for i in ivs if i[0])):]) + "\n")
                    stage, ec, pc, far = rec
                    print("stage=%d ec=0o%02o pc=0o%012o far=0o%08o" % (stage, ec, pc, far))
                    return 0
    return 1


if __name__ == "__main__":
    sys.exit(main())
