#!/usr/bin/env python3
"""Driver for scripts/flicker-test.sh: launch Activity Monitor, drag inside it, sample the screen.
  flicker_probe.py QMP_SOCKET WORKDIR SAMPLES
Exit 0 = no torn sample, 1 = at least one, 2 = could not set up."""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from qmp import Qmp, char_keys, ABS_MAX  # noqa: E402

SCREEN = (1024, 768)
MILK = b"\xff\xfd\xf8"
CREAM = b"\xfb\xf6\xec"


def read_ppm(path):
    d = open(path, "rb").read()
    parts = d.split(b"\n", 3)
    assert parts[0] == b"P6", parts[0]
    w, h = (int(v) for v in parts[1].split())
    return w, h, parts[3]


def dump(q, path):
    # The QMP reply comes once the file is written.
    if os.path.exists(path):
        os.unlink(path)
    q.cmd("screendump", filename=path, format="ppm")
    return read_ppm(path)


def count(px, w, x0, y0, x1, y1, colour):
    n = 0
    for y in range(y0, y1):
        row = px[(y * w + x0) * 3:(y * w + x1) * 3]
        n += row.count(colour)
    return n


def to_abs(v, size):
    return min(ABS_MAX, (2 * v + 1) * (ABS_MAX + 1) // (2 * size))


def move(q, x, y):
    q.events([{"type": "abs", "data": {"axis": "x", "value": to_abs(x, SCREEN[0])}},
              {"type": "abs", "data": {"axis": "y", "value": to_abs(y, SCREEN[1])}}])


def main():
    sock, work, samples = sys.argv[1], sys.argv[2], int(sys.argv[3])
    q = Qmp(sock)
    base = dump(q, f"{work}/base.ppm")
    # Skim: Cmd+Space, type, Return.
    q.keys(["meta_l", "spc"])
    time.sleep(0.3)
    for c in "activity":
        q.keys(char_keys(c))
        time.sleep(0.03)
    time.sleep(0.4)
    q.keys(["ret"])
    time.sleep(3.0)
    w, h, px = dump(q, f"{work}/idle.ppm")
    # Probe bands inside the Activity Monitor window (it opens at the same place on a fresh
    # image): the toolbar strip to the right of the search field, and the summary bar at the
    # bottom. Both are cream when the frame is complete, milk-white when the body was cleared
    # and not redrawn yet.
    bands = {"toolbar": (700, 100, 855, 124), "summary": (140, 670, 930, 690)}
    def frac(px, name):
        x0, y0, x1, y1 = bands[name]
        return count(px, w, x0, y0, x1, y1, CREAM) / ((x1 - x0) * (y1 - y0))
    for name in bands:
        f = frac(px, name)
        print(f"flicker-probe: idle {name} band is {100 * f:.0f}% cream")
        if f < 0.6:
            print("flicker-probe: the window is not where the probe expects it (see idle.ppm)")
            return 2
    # Press inside the table and drag round and round, sampling the screen as we go.
    move(q, 400, 300)
    q.events([{"type": "btn", "data": {"button": "left", "down": True}}])
    torn = 0
    t0 = time.time()
    for i in range(samples):
        a = i * 0.45
        move(q, 400 + int(180 * __import__("math").cos(a)), 300 + int(120 * __import__("math").sin(a)))
        _, _, px = dump(q, f"{work}/s.ppm")
        bad = [n for n in bands if frac(px, n) < 0.5]
        if bad:
            torn += 1
            os.replace(f"{work}/s.ppm", f"{work}/torn-{i}.ppm")
            print(f"flicker-probe: sample {i}: torn ({', '.join(bad)} band missing)")
    q.events([{"type": "btn", "data": {"button": "left", "down": False}}])
    print(f"flicker-probe: {samples} samples in {time.time() - t0:.1f}s, {torn} torn")
    if os.environ.get("FLICKER_RESIZE"):
        # Resize the window by its bottom-right corner (Activity Monitor is resizable) and keep
        # the picture for a look: the committed frame must follow the new size.
        move(q, 935, 691)
        q.events([{"type": "btn", "data": {"button": "left", "down": True}}])
        for k in range(1, 11):
            move(q, 935 + 4 * k, 691 - 8 * k)
            time.sleep(0.05)
        q.events([{"type": "btn", "data": {"button": "left", "down": False}}])
        time.sleep(1.0)
        dump(q, f"{work}/resized.ppm")
    return 1 if torn else 0


sys.exit(main())
