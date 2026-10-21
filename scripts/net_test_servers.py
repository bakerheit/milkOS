#!/usr/bin/env python3
"""Host-side helpers for scripts/net-test.sh (python3 stdlib only).

Binds ONLY to 127.0.0.1 (the guest reaches the Mac's loopback as 10.0.2.2 through qemu's
user-mode networking):
  * a TCP echo server,
  * a tiny DNS server: A records for the names in NAMES, NXDOMAIN for everything else
    (it never forwards anything anywhere),
and reserves a "refused" port (bound, then closed: nothing listens there).
Writes "echo=PORT dns=PORT refused=PORT" to the file given as argv[1], then serves until
killed. Run it with `python3 -I` from a scratch directory.
"""
import socket
import socketserver
import struct
import sys
import threading

HOST = "127.0.0.1"
NAMES = {"echo.milk.test": "10.0.2.2", "host.milk.test": "10.0.2.2"}


class Echo(socketserver.BaseRequestHandler):
    def handle(self):
        while True:
            data = self.request.recv(65536)
            if not data:
                return
            self.request.sendall(data)


class TCPServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def qname(msg, off):
    labels = []
    while True:
        n = msg[off]
        if n == 0:
            return ".".join(labels), off + 1
        if n & 0xC0:
            raise ValueError("compressed question")
        labels.append(msg[off + 1 : off + 1 + n].decode("ascii", "replace"))
        off += 1 + n


def dns_loop(sock):
    while True:
        msg, peer = sock.recvfrom(512)
        try:
            (qid, flags, qd) = struct.unpack("!HHH", msg[:6])
            name, end = qname(msg, 12)
            qtype, qclass = struct.unpack("!HH", msg[end : end + 4])
        except Exception:
            continue
        question = msg[12 : end + 4]
        ip = NAMES.get(name.lower().rstrip("."))
        if ip and qtype == 1 and qclass == 1:
            ans = b"\xc0\x0c" + struct.pack("!HHIH", 1, 1, 60, 4) + socket.inet_aton(ip)
            hdr = struct.pack("!HHHHHH", qid, 0x8180, 1, 1, 0, 0)
        else:
            ans = b""
            hdr = struct.pack("!HHHHHH", qid, 0x8183, 1, 0, 0, 0)  # NXDOMAIN
        sock.sendto(hdr + question + ans, peer)
        print(f"dns: {name} -> {ip or 'NXDOMAIN'}", flush=True)


def main():
    out = sys.argv[1]
    echo = TCPServer((HOST, 0), Echo)
    dns = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    dns.bind((HOST, 0))
    r = socket.socket()
    r.bind((HOST, 0))
    refused = r.getsockname()[1]
    r.close()
    with open(out + ".tmp", "w") as f:
        f.write(f"echo={echo.server_address[1]} dns={dns.getsockname()[1]} refused={refused}\n")
    import os

    os.rename(out + ".tmp", out)
    threading.Thread(target=dns_loop, args=(dns,), daemon=True).start()
    echo.serve_forever()


if __name__ == "__main__":
    main()
