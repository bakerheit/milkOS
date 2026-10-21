#!/usr/bin/env python3
"""runlimit.py SECS LOG -- CMD...: run CMD in its own process group with output going to LOG,
stop the whole group (and so every qemu / helper it started, and nothing else) if it runs
longer than SECS. Exit status: CMD's, 124 on timeout (like coreutils timeout; macOS has none)."""
import os, signal, subprocess, sys, time

secs, log = float(sys.argv[1]), sys.argv[2]
cmd = sys.argv[sys.argv.index("--") + 1:]
with open(log, "wb") as out:
    p = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                         start_new_session=True)
    try:
        rc = p.wait(timeout=secs)
    except subprocess.TimeoutExpired:
        for sig in (signal.SIGTERM, signal.SIGKILL):
            try:
                os.killpg(p.pid, sig)
            except ProcessLookupError:
                break
            try:
                p.wait(timeout=5)
                break
            except subprocess.TimeoutExpired:
                pass
        out.write(f"\nrunlimit: killed after {secs:.0f}s\n".encode())
        sys.exit(124)
sys.exit(rc if rc >= 0 else 128 - rc)
