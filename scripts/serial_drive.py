#!/usr/bin/env python3
"""Drive the guest's serial console over a qemu unix-socket chardev (stdlib only).

  serial_drive.py SOCK LOG [wait:REGEX | send:TEXT | raw:TEXT | done:FILE | waitfile:FILE] ...

Connects to SOCK (retrying), appends everything the guest prints to LOG, and runs the
steps in order: wait for REGEX to appear in output that arrived after the previous step,
type TEXT followed by Enter (after the output has paused for 0.1 s), type TEXT with \\xNN escapes and no Enter (raw: e.g. ^D is
raw:\\x04), touch FILE (done:), or wait for the caller to create FILE (waitfile:). Then
keeps logging until killed. Each wait gives up after 60 s (exit status 1); wait@N:REGEX
waits up to N seconds instead; seen@N:REGEX also accepts output that came before the step
(two events in either order).
"""
import os
import re
import socket
import sys
import threading
import time

sock_path, log_path, steps = sys.argv[1], sys.argv[2], sys.argv[3:]
s = socket.socket(socket.AF_UNIX)
for _ in range(100):
    try:
        s.connect(sock_path)
        break
    except OSError:
        time.sleep(0.1)
else:
    sys.exit("serial_drive: cannot connect")

buf = bytearray()
lock = threading.Lock()


def reader():
    with open(log_path, "ab", buffering=0) as log:
        while True:
            data = s.recv(4096)
            if not data:
                return
            log.write(data)
            with lock:
                buf.extend(data)


threading.Thread(target=reader, daemon=True).start()


def settle(quiet=0.1, limit=1.0):
    """Wait (up to `limit` s) until the guest has printed nothing for `quiet` s, so a
    command isn't typed while a burst of asynchronous kernel log lines is still coming
    (they would land inside its echo; scripts/console_clean.py repairs what slips by)."""
    end = time.time() + limit
    with lock:
        n = len(buf)
    last = time.time()
    while time.time() < end:
        time.sleep(0.02)
        with lock:
            m = len(buf)
        if m != n:
            n, last = m, time.time()
        elif time.time() - last >= quiet:
            return


mark = 0
for step in steps:
    kind, _, arg = step.partition(":")
    limit = 60
    if kind.startswith("wait@"):  # wait@SECONDS:REGEX, a longer (or shorter) wait
        kind, limit = "wait", int(kind[5:])
    if kind.startswith("seen@"):  # seen@SECONDS:REGEX, like wait@ but also matches output already past
        kind, limit = "seen", int(kind[5:])
    if kind == "seen":
        # For two events whose order is not fixed (a kernel line and the shell's prompt): wait for
        # the first, then `seen` the second, and it counts whether it came before or after.
        pat = re.compile(arg.encode())
        end = time.time() + limit
        while True:
            with lock:
                m = pat.search(buf)
            if m:
                mark = max(mark, m.end())
                break
            if time.time() > end:
                sys.exit(f"serial_drive: timed out waiting for {arg!r}")
            time.sleep(0.05)
    elif kind == "wait":
        pat = re.compile(arg.encode())
        end = time.time() + limit
        while True:
            with lock:
                m = pat.search(buf, mark)
            if m:
                mark = m.end()
                break
            if time.time() > end:
                sys.exit(f"serial_drive: timed out waiting for {arg!r}")
            time.sleep(0.05)
    elif kind == "send":
        settle()
        for ch in arg.encode() + b"\r":
            s.sendall(bytes([ch]))
            time.sleep(0.002)
    elif kind == "raw":
        for ch in arg.encode().decode("unicode_escape").encode("latin-1"):
            s.sendall(bytes([ch]))
            time.sleep(0.002)
    elif kind == "waitfile":
        end = time.time() + 60
        while not os.path.exists(arg):
            if time.time() > end:
                sys.exit(f"serial_drive: timed out waiting for file {arg}")
            time.sleep(0.05)
    elif kind == "done":
        open(arg, "w").close()
while True:
    time.sleep(1)
