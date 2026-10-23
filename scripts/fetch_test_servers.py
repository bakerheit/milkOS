#!/usr/bin/env python3
"""Host-side servers for scripts/fetch-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and never forwards anything anywhere:
  * a DNS server (UDP) for a made-up zone: www.milk.test, api.milk.test and shop.milk.test (one
    site, three origins) and other.test (another site), all A 10.0.2.2; everything else
    NXDOMAIN. TTL 60.
  * one HTTP/1.1 server (the guest tells the sites apart by the Host header) with the pages in
    tests/fetch (argv[2]) and an API that checks and sets what fetch and XMLHttpRequest need:
      /api/hello            text                  /api/data.json     JSON
      /api/bytes            256 bytes (0..255)    /api/echo          any method: what it got, as JSON
      /api/status/N         status N              /api/slow?ms=N     answers after N ms
      /api/stream?kb=K&chunks=N&ms=M   K KiB sent in N pieces, M ms apart (Content-Length set)
      /api/big?mb=N         N MiB of x            /api/headers       the request headers, as JSON
      /api/redirect?to=URL&code=302    a redirect /api/cookie        sets sid=abc123
      /api/whoami           the Cookie header it saw   /api/beacon   POST, logged
      /api/cheeses.json     the shop's list, after 2.5 s (the render page)
      /cors/open            Access-Control-Allow-Origin: *, exposes X-Visible (X-Hidden stays hidden)
      /cors/origin          allows the asking origin, says which it saw
      /cors/closed          no CORS headers at all
      /cors/credentialed    allows the asking origin with credentials; ?set=1 sets cookie xs=1;
                            says the Cookie it saw
      /cors/star-credentialed   Allow-Origin * with Allow-Credentials true (a mistake: refused)
      /cors/preflight       OPTIONS: 204 with Allow-Methods PUT, DELETE and Allow-Headers x-token,
                            content-type, Max-Age 600; PUT/DELETE: the request, as JSON
      /cors/nopreflight     OPTIONS: 405, no CORS headers
      /cors/redirect?to=URL a 302 to URL (CORS-open itself)
Every request is logged: "http METHOD HOST PATH origin=O cookie=C" (and for a body "body N bytes").
Writes "dns=PORT http=PORT" to argv[1], then serves until killed.
"""
import http.server
import json
import os
import socket
import socketserver
import struct
import sys
import threading
import time
import urllib.parse

HOST = "127.0.0.1"
TTL = 60
ZONE = {"www.milk.test": "10.0.2.2", "api.milk.test": "10.0.2.2", "shop.milk.test": "10.0.2.2", "other.test": "10.0.2.2"}
ROOT = None


def log(*a):
    print(*a, flush=True)


def name_at(msg, off):
    labels = []
    while True:
        n = msg[off]
        if n == 0:
            return ".".join(labels).lower(), off + 1
        if n & 0xC0:
            raise ValueError("compressed question")
        labels.append(msg[off + 1 : off + 1 + n].decode("ascii", "replace"))
        off += 1 + n


def dns_answer(q):
    qid = struct.unpack(">H", q[:2])[0]
    name, off = name_at(q, 12)
    qtype = struct.unpack(">H", q[off : off + 2])[0]
    question = q[12 : off + 4]
    answers = []
    rcode = 0 if name in ZONE else 3
    if qtype == 1 and name in ZONE:
        answers.append(b"\xc0\x0c" + struct.pack(">HHIH", 1, 1, TTL, 4) + socket.inet_aton(ZONE[name]))
    head = struct.pack(">HHHHHH", qid, 0x8180 | rcode, 1, len(answers), 0, 0)
    return head + question + b"".join(answers), name, qtype


def dns_udp(sock):
    while True:
        msg, peer = sock.recvfrom(512)
        try:
            r, name, qtype = dns_answer(msg)
        except Exception as e:  # noqa: BLE001 - a bad packet is logged, not fatal
            log("dns bad packet", e)
            continue
        log("dns", name, qtype)
        sock.sendto(r, peer)


CHEESES = [
    {"name": "Cheddar", "origin": "Somerset", "price": 7.5, "aged": 18},
    {"name": "Gouda", "origin": "Holland", "price": 6.25, "aged": 12},
    {"name": "Brie", "origin": "Meaux", "price": 9.0, "aged": 2},
    {"name": "Manchego", "origin": "La Mancha", "price": 11.4, "aged": 9},
]

TYPES = {".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8", ".css": "text/css", ".json": "application/json"}


class Web(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    # ---- sending ----
    def send(self, code, body=b"", ctype="text/plain; charset=utf-8", headers=(), reason=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code, reason)
        self.send_header("Content-Type", ctype)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.close_connection = True

    def json(self, obj, code=200, headers=()):
        self.send(code, json.dumps(obj), "application/json", headers)

    # ---- the request ----
    def handle_any(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        host = (self.headers.get("Host") or "").split(":")[0]
        origin = self.headers.get("Origin")
        log("http", self.command, host, self.path, "origin=" + str(origin), "cookie=" + str(self.headers.get("Cookie")),
            ("body %d bytes" % len(body)) if body else "")
        u = urllib.parse.urlsplit(self.path)
        path, q = u.path, dict(urllib.parse.parse_qsl(u.query))
        if path.startswith("/cors/"):
            return self.cors(path, q, origin, host, body)
        if path.startswith("/api/"):
            return self.api(path, q, host, body)
        name = os.path.basename(path)
        ext = os.path.splitext(name)[1]
        full = os.path.join(ROOT, name)
        if ext in TYPES and os.path.isfile(full):
            with open(full, "rb") as f:
                return self.send(200, f.read(), TYPES[ext])
        return self.send(404, "<!DOCTYPE html><title>Not found</title><h1>404</h1>", "text/html; charset=utf-8")

    def seen(self, body):
        h = self.headers
        return {
            "method": self.command,
            "content_type": h.get("Content-Type"),
            "x_token": h.get("X-Token"),
            "accept": h.get("Accept"),
            "origin": h.get("Origin"),
            "referer": h.get("Referer"),
            "cookie": h.get("Cookie"),
            "body": body.decode("utf-8", "replace") if len(body) < 4000 else "(%d bytes)" % len(body),
            "length": len(body),
        }

    def api(self, path, q, host, body):
        if path == "/api/hello":
            return self.send(200, "hello milk")
        if path == "/api/data.json":
            return self.json({"items": [1, 2, 3], "name": "milk", "nested": {"ok": True}})
        if path == "/api/cheeses.json":
            time.sleep(2.5)
            return self.json(CHEESES)
        if path == "/api/bytes":
            return self.send(200, bytes(range(256)), "application/octet-stream")
        if path == "/api/echo":
            return self.json(self.seen(body))
        if path == "/api/headers":
            return self.json({k.lower(): v for k, v in self.headers.items()})
        if path.startswith("/api/status/"):
            code = int(path.rsplit("/", 1)[1])
            return self.send(code, "status %d" % code, reason="Status %d" % code)
        if path == "/api/slow":
            time.sleep(int(q.get("ms", "100")) / 1000.0)
            return self.send(200, "slow done")
        if path == "/api/stream":
            total = int(q.get("kb", "100")) * 1024
            chunks = max(1, int(q.get("chunks", "10")))
            gap = int(q.get("ms", "20")) / 1000.0
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(total))
            self.send_header("Connection", "close")
            self.end_headers()
            piece = total // chunks
            sent = 0
            try:
                for i in range(chunks):
                    n = piece if i < chunks - 1 else total - sent
                    self.wfile.write(b"s" * n)
                    self.wfile.flush()
                    sent += n
                    time.sleep(gap)
            except OSError:
                pass
            self.close_connection = True
            return None
        if path == "/api/big":
            mb = int(q.get("mb", "1"))
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(mb << 20))
            self.send_header("Connection", "close")
            self.end_headers()
            try:
                for _ in range(mb):
                    self.wfile.write(b"x" * (1 << 20))
            except OSError:
                pass
            self.close_connection = True
            return None
        if path == "/api/redirect":
            return self.send(int(q.get("code", "302")), "moved", headers=[("Location", q["to"])])
        if path == "/api/cookie":
            return self.send(200, "set", headers=[("Set-Cookie", "sid=abc123; Path=/")])
        if path == "/api/whoami":
            return self.send(200, self.headers.get("Cookie") or "none")
        if path == "/api/beacon":
            log("beacon", body.decode("utf-8", "replace"))
            return self.send(204)
        return self.send(404, "no such api")

    def cors(self, path, q, origin, host, body):
        o = origin or "*"
        if path == "/cors/open":
            return self.send(
                200,
                "open",
                headers=[("Access-Control-Allow-Origin", "*"), ("Access-Control-Expose-Headers", "X-Visible"), ("X-Visible", "yes"), ("X-Hidden", "no")],
            )
        if path == "/cors/origin":
            return self.json({"origin": origin, "cookie": self.headers.get("Cookie")}, headers=[("Access-Control-Allow-Origin", o), ("Vary", "Origin")])
        if path == "/cors/closed":
            return self.send(200, "closed")
        if path == "/cors/credentialed":
            h = [("Access-Control-Allow-Origin", o), ("Access-Control-Allow-Credentials", "true"), ("Vary", "Origin")]
            if q.get("set"):
                h.append(("Set-Cookie", "xs=1; Path=/"))
            return self.json({"origin": origin, "cookie": self.headers.get("Cookie")}, headers=h)
        if path == "/cors/star-credentialed":
            return self.send(200, "star", headers=[("Access-Control-Allow-Origin", "*"), ("Access-Control-Allow-Credentials", "true")])
        if path == "/cors/preflight":
            if self.command == "OPTIONS":
                log("preflight", self.headers.get("Access-Control-Request-Method"), self.headers.get("Access-Control-Request-Headers"))
                return self.send(
                    204,
                    headers=[
                        ("Access-Control-Allow-Origin", o),
                        ("Access-Control-Allow-Methods", "PUT, DELETE"),
                        ("Access-Control-Allow-Headers", "x-token, content-type"),
                        ("Access-Control-Max-Age", "600"),
                    ],
                )
            return self.json(self.seen(body), headers=[("Access-Control-Allow-Origin", o)])
        if path == "/cors/nopreflight":
            if self.command == "OPTIONS":
                return self.send(405, "no")
            return self.json(self.seen(body), headers=[("Access-Control-Allow-Origin", o)])
        if path == "/cors/redirect":
            return self.send(302, "moved", headers=[("Location", q["to"])])
        return self.send(404, "no such cors endpoint")

    do_GET = do_POST = do_PUT = do_DELETE = do_PATCH = do_OPTIONS = do_HEAD = handle_any

    def log_message(self, fmt, *args):
        pass


class TCPServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    global ROOT
    ROOT = sys.argv[2]
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind((HOST, 0))
    web = TCPServer((HOST, 0), Web)
    threading.Thread(target=dns_udp, args=(udp,), daemon=True).start()
    threading.Thread(target=web.serve_forever, daemon=True).start()
    with open(sys.argv[1] + ".tmp", "w") as f:
        f.write("dns=%d http=%d\n" % (udp.getsockname()[1], web.server_address[1]))
    os.rename(sys.argv[1] + ".tmp", sys.argv[1])
    log("serving: dns 127.0.0.1:%d (udp), http 127.0.0.1:%d from %s" % (udp.getsockname()[1], web.server_address[1], ROOT))
    threading.Event().wait()


if __name__ == "__main__":
    main()
