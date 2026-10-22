#!/usr/bin/env python3
"""runlimit.py SECS LOG [--status FILE] -- CMD...: run CMD in its own process group with output
going to LOG, stop the whole group (and so every qemu / helper it started, and nothing else) if
it runs longer than SECS. Exit status: CMD's, 124 on timeout (like coreutils timeout; macOS has
none), 128+N if runlimit itself gets signal N (SIGINT/SIGTERM/SIGHUP: the group is stopped first).
When CMD exits, anything it left running in its group (a stray qemu) is stopped too.
--status FILE: also write "RC SECONDS" to FILE when done (scripts/check-all.sh polls for it)."""
import os, signal, subprocess, sys, time

args = sys.argv[1:sys.argv.index("--")]
cmd = sys.argv[sys.argv.index("--") + 1:]
status = None
if "--status" in args:
    i = args.index("--status")
    status = args[i + 1]
    del args[i:i + 2]
secs, log = float(args[0]), args[1]
start = time.monotonic()


def stop_group(pgid):
    """TERM, then KILL, the process group pgid. True if anything was still in it."""
    try:
        os.killpg(pgid, 0)
    except (ProcessLookupError, PermissionError):
        return False
    for sig in (signal.SIGTERM, signal.SIGKILL):
        try:
            os.killpg(pgid, sig)
        except (ProcessLookupError, PermissionError):
            break
        for _ in range(50):
            try:
                os.killpg(pgid, 0)
            except (ProcessLookupError, PermissionError):
                return True
            time.sleep(0.1)
    return True


def finish(rc):
    if status:
        tmp = status + ".tmp"
        with open(tmp, "w") as f:
            f.write(f"{rc} {time.monotonic() - start:.0f}\n")
        os.replace(tmp, status)
    sys.exit(rc)


with open(log, "wb") as out:
    p = subprocess.Popen(cmd, stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
                         start_new_session=True)

    def on_signal(signum, _frame):
        stop_group(p.pid)
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        out.write(f"\nrunlimit: stopped by signal {signum}\n".encode())
        out.flush()
        finish(128 + signum)

    for s in (signal.SIGINT, signal.SIGTERM, signal.SIGHUP):
        signal.signal(s, on_signal)
    try:
        rc = p.wait(timeout=secs)
    except subprocess.TimeoutExpired:
        stop_group(p.pid)
        try:
            p.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass
        out.write(f"\nrunlimit: killed after {secs:.0f}s\n".encode())
        out.flush()
        finish(124)
    if stop_group(p.pid):
        out.write(b"\nrunlimit: the command left processes running in its group; stopped them\n")
        out.flush()
finish(rc if rc >= 0 else 128 - rc)
