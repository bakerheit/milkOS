#!/usr/bin/env python3
"""Host side of scripts/net-bench.sh (stdlib only). Two TCP servers on 127.0.0.1 only:

  sink:   reads each connection to EOF, then closes (the guest's `tcpbench send` times
          until that close);
  source: sends MIB MiB to each connection, then closes.

  net_bench_host.py PORTFILE MIB     writes "sink=P source=Q" to PORTFILE, serves until killed
"""
import socket
import sys
import threading

portfile, mib = sys.argv[1], int(sys.argv[2])


def serve(sock, handler):
    while True:
        c, _ = sock.accept()
        threading.Thread(target=handler, args=(c,), daemon=True).start()


def sink(c):
    with c:
        while c.recv(1 << 16):
            pass


def source(c):
    chunk = bytes(range(256)) * 256  # 64 KiB
    with c:
        for _ in range(mib * 16):
            c.sendall(chunk)


socks = []
for h in (sink, source):
    s = socket.socket()
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", 0))
    s.listen(4)
    socks.append(s)
    threading.Thread(target=serve, args=(s, h), daemon=True).start()
with open(portfile + ".tmp", "w") as f:
    f.write(f"sink={socks[0].getsockname()[1]} source={socks[1].getsockname()[1]}\n")
import os  # noqa: E402

os.rename(portfile + ".tmp", portfile)
threading.Event().wait()
