#!/usr/bin/env python3
"""Host-side helpers for scripts/net-test.sh (python3 stdlib only).

Binds ONLY to the loopback: 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's
user-mode networking) and ::1 (the guest reaches it as fec0::2, slirp's IPv6 host):
  * a TCP echo server on each,
  * a tiny DNS server: A records for the names in NAMES, NXDOMAIN for everything else
    (it never forwards anything anywhere),
  * an HTTP server with one page, /hello.txt (for the guest's `fetch` program), on each,
and reserves a "refused" port (bound, then closed: nothing listens there).
Writes "echo=PORT echo6=PORT dns=PORT http=PORT http6=PORT refused=PORT" to the file given as argv[1], then serves until
killed. Run it with `python3 -I` from a scratch directory.
"""
import http.server
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


HELLO = b"hello from the Mac's loopback, via qemu slirp, to milkOS fetch\n"


class Http(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/hello.txt":
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(HELLO)))
            self.end_headers()
            self.wfile.write(HELLO)
        else:
            self.send_error(404)

    def log_message(self, fmt, *args):
        print("http: " + fmt % args, flush=True)


class TCPServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


class TCPServer6(TCPServer):
    address_family = socket.AF_INET6

    def server_bind(self):
        # ::1 only, IPv6 only: nothing but this host's loopback can reach it.
        self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        super().server_bind()


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
    echo6 = TCPServer6(("::1", 0), Echo)
    threading.Thread(target=echo6.serve_forever, daemon=True).start()
    web = http.server.ThreadingHTTPServer((HOST, 0), Http)
    threading.Thread(target=web.serve_forever, daemon=True).start()

    class Web6(http.server.ThreadingHTTPServer):
        address_family = socket.AF_INET6

        def server_bind(self):
            self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
            super().server_bind()

    web6 = Web6(("::1", 0), Http)
    threading.Thread(target=web6.serve_forever, daemon=True).start()
    dns = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    dns.bind((HOST, 0))
    r = socket.socket()
    r.bind((HOST, 0))
    refused = r.getsockname()[1]
    r.close()
    with open(out + ".tmp", "w") as f:
        f.write(
            f"echo={echo.server_address[1]} echo6={echo6.server_address[1]} dns={dns.getsockname()[1]} "
            f"http={web.server_address[1]} http6={web6.server_address[1]} refused={refused}\n"
        )
    import os

    os.rename(out + ".tmp", out)
    threading.Thread(target=dns_loop, args=(dns,), daemon=True).start()
    echo.serve_forever()


if __name__ == "__main__":
    main()
