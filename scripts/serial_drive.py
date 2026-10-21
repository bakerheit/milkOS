#!/usr/bin/env python3
"""Drive the guest's serial console over a qemu unix-socket chardev (stdlib only).

  serial_drive.py SOCK LOG [wait:REGEX | send:TEXT | raw:TEXT | done:FILE | waitfile:FILE] ...

Connects to SOCK (retrying), appends everything the guest prints to LOG, and runs the
steps in order: wait for REGEX to appear in output that arrived after the previous step,
type TEXT followed by Enter, type TEXT with \\xNN escapes and no Enter (raw: e.g. ^D is
raw:\\x04), touch FILE (done:), or wait for the caller to create FILE (waitfile:). Then
keeps logging until killed. Each wait gives up after 60 s (exit status 1).
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
mark = 0
for step in steps:
    kind, _, arg = step.partition(":")
    if kind == "wait":
        pat = re.compile(arg.encode())
        end = time.time() + 60
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
