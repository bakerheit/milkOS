#!/usr/bin/env python3
"""Screenshots for the desktop tests that wait for the screen instead of sleeping.

  shotwait.py QMP OUT.png [--differ-from REF.png] [--timeout S] [--stable N]
      Take screendumps over QMP until the picture has settled (N frames in a row alike, default 3,
      0.3 s apart) and, with --differ-from, differs from REF. Writes OUT.png and OUT.ppm (the
      sidecar the comparisons read). Exit 0 if it settled (and differed) within the timeout, else 1
      (the last frame is still written, so the test's own check then fails with a picture to look at).
  shotwait.py --same A.png B.png        exit 0 if the two pictures are alike
  shotwait.py --differ A.png B.png      exit 0 if they are not

"Alike" means at most TOLERANCE pixels differ outside the menu bar: the clock changes every minute,
the caret blinks and the pointer moves, none of which is a change of the window under test. A
real change of a window (a new picture, a panel opening) changes thousands of pixels.
"""
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

MENU_BAR = 32      # rows ignored at the top (the clock lives there)
TOLERANCE = 600    # pixels
W, H = 1024, 768


def read_ppm(path):
    d = open(path, "rb").read()
    parts = d.split(b"\n", 3)
    assert parts[0] == b"P6", parts[0]
    w, h = (int(v) for v in parts[1].split())
    return w, h, parts[3]


def diff_pixels(a, b, limit=None):
    """Pixels that differ between two (w, h, data) pictures below the menu bar."""
    (w, h, da), (w2, h2, db) = a, b
    if (w, h) != (w2, h2):
        return 1 << 30
    n = 0
    for y in range(MENU_BAR, h):
        ra, rb = da[y * w * 3:(y + 1) * w * 3], db[y * w * 3:(y + 1) * w * 3]
        if ra == rb:
            continue
        for x in range(w):
            if ra[x * 3:x * 3 + 3] != rb[x * 3:x * 3 + 3]:
                n += 1
        if limit is not None and n > limit:
            return n
    return n


def alike(a, b):
    return diff_pixels(a, b, TOLERANCE) <= TOLERANCE


def sidecar(png):
    return os.path.splitext(png)[0] + ".ppm"


def main(argv):
    if len(argv) == 3 and argv[0] in ("--same", "--differ"):
        a, b = read_ppm(sidecar(argv[1])), read_ppm(sidecar(argv[2]))
        same = alike(a, b)
        return 0 if same == (argv[0] == "--same") else 1
    from qmp import Qmp
    qmp, out = argv[0], argv[1]
    ref = timeout = stable = None
    i = 2
    while i < len(argv):
        if argv[i] == "--differ-from":
            ref = read_ppm(sidecar(argv[i + 1]))
            i += 2
        elif argv[i] == "--timeout":
            timeout = float(argv[i + 1])
            i += 2
        elif argv[i] == "--stable":
            stable = int(argv[i + 1])
            i += 2
        else:
            raise SystemExit(f"shotwait: unknown argument {argv[i]}")
    timeout = 20.0 if timeout is None else timeout
    stable = 3 if stable is None else stable
    q = Qmp(qmp)
    ppm = sidecar(out)
    deadline = time.time() + timeout
    last = None
    run = 0
    ok = False
    while True:
        if os.path.exists(ppm):
            os.unlink(ppm)
        q.cmd("screendump", filename=ppm, format="ppm")
        cur = read_ppm(ppm)
        run = run + 1 if last is not None and alike(last, cur) else 1
        last = cur
        if run >= stable and (ref is None or not alike(ref, cur)):
            ok = True
            break
        if time.time() > deadline:
            break
        time.sleep(0.3)
    q.cmd("screendump", filename=out, format="png")
    if not ok:
        why = "never settled" if run < stable else "never changed from the reference"
        print(f"shotwait: {os.path.basename(out)}: {why} within {timeout:.0f}s", file=sys.stderr)
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
