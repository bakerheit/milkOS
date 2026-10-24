#!/usr/bin/env python3
"""Host-side servers for scripts/workers-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and never forwards anything anywhere:
  * a DNS server (UDP) for a made-up zone: www.milk.test and api.milk.test (one site, two
    origins) and other.test (another site), all A 10.0.2.2; everything else NXDOMAIN. TTL 60.
  * one HTTP/1.1 server (the guest tells the sites apart by the Host header) serving the tree in
    tests/workers (argv[2]) by path, with "@PORT@" replaced by the server's port in text files
    and these paths on top:
      /data.json                    {"answer": 42}, same origin as the pages
      /cors/open.json               JSON with Access-Control-Allow-Origin: *
      /cors/closed.json             JSON with no CORS header at all
      /cors/script.js               a script with Access-Control-Allow-Origin: * (for importScripts)
      /nocors/script.js             a script with no CORS header
      /bad-type.js                  a script served as text/html
      /echo                         the request as JSON: method, cookie, origin, referer, Sec-Fetch-*
      /cookie/set                   sets the cookie wk=1 (Path=/)
      /slow?ms=MS                   answers after MS ms
      /bytes?n=N                    N bytes of a counting pattern (application/octet-stream)
      /count/NAME                   how many times it was asked for
Every request is logged: "http METHOD HOST PATH cookie=C origin=O referer=R dest=D site=S". Writes
"dns=PORT http=PORT" to argv[1], then serves until killed.
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
ZONE = {"www.milk.test": "10.0.2.2", "api.milk.test": "10.0.2.2", "other.test": "10.0.2.2"}
ROOT = None
PORT = 0
COUNTS = {}
LOCK = threading.Lock()

TYPES = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".json": "application/json",
    ".css": "text/css",
    ".txt": "text/plain; charset=utf-8",
}
TEXT = {".html", ".js", ".mjs", ".css", ".txt", ".json"}


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


class Web(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send(self, code, body=b"", ctype="text/html; charset=utf-8", headers=(), reason=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code, reason)
        if ctype is not None:
            self.send_header("Content-Type", ctype)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)
        self.close_connection = True

    def handle_any(self):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else b""
        host = (self.headers.get("Host") or "").split(":")[0]
        log(
            "http",
            self.command,
            host,
            self.path,
            "cookie=" + str(self.headers.get("Cookie")),
            "origin=" + str(self.headers.get("Origin")),
            "referer=" + str(self.headers.get("Referer")),
            "dest=" + str(self.headers.get("Sec-Fetch-Dest")),
            "site=" + str(self.headers.get("Sec-Fetch-Site")),
        )
        u = urllib.parse.urlsplit(self.path)
        path, q = u.path, dict(urllib.parse.parse_qsl(u.query))
        try:
            if path == "/data.json":
                return self.send(200, '{"answer": 42}', "application/json")
            if path == "/cors/open.json":
                return self.send(200, '{"open": true}', "application/json", headers=[("Access-Control-Allow-Origin", "*")])
            if path == "/cors/closed.json":
                return self.send(200, '{"closed": true}', "application/json")
            if path == "/cors/script.js":
                return self.send(200, "var corsLoaded = 'cors-ok';", "text/javascript", headers=[("Access-Control-Allow-Origin", "*")])
            if path == "/nocors/script.js":
                return self.send(200, "var nocorsLoaded = 'oops';", "text/javascript")
            if path == "/bad-type.js":
                return self.send(200, "var bad = 1;", "text/html")
            if path == "/echo":
                return self.send(
                    200,
                    json.dumps(
                        {
                            "method": self.command,
                            "cookie": self.headers.get("Cookie"),
                            "origin": self.headers.get("Origin"),
                            "referer": self.headers.get("Referer"),
                            "dest": self.headers.get("Sec-Fetch-Dest"),
                            "site": self.headers.get("Sec-Fetch-Site"),
                            "mode": self.headers.get("Sec-Fetch-Mode"),
                        }
                    ),
                    "application/json",
                    headers=[("Access-Control-Allow-Origin", "*")],
                )
            if path == "/cookie/set":
                return self.send(200, "set", "text/plain", headers=[("Set-Cookie", "wk=1; Path=/")])
            if path == "/slow":
                time.sleep(int(q.get("ms", "1000")) / 1000.0)
                return self.send(200, "slow done", "text/plain", headers=[("Access-Control-Allow-Origin", "*")])
            if path == "/bytes":
                n = int(q.get("n", "1024"))
                return self.send(200, bytes(i & 255 for i in range(n)), "application/octet-stream", headers=[("Access-Control-Allow-Origin", "*")])
            if path.startswith("/count/"):
                name = path[len("/count/") :]
                with LOCK:
                    COUNTS[name] = COUNTS.get(name, 0) + 1
                    k = COUNTS[name]
                return self.send(200, "count %d" % k, "text/plain")
            return self.static(path)
        except (BrokenPipeError, ConnectionResetError):
            log("aborted", self.path)

    def static(self, path):
        rel = os.path.normpath(urllib.parse.unquote(path)).lstrip("/")
        if rel.startswith("..") or rel == "":
            rel = "index.html" if rel == "" else None
        full = os.path.join(ROOT, rel) if rel else None
        if not full or not os.path.isfile(full) or not os.path.realpath(full).startswith(os.path.realpath(ROOT)):
            return self.send(404, "<!DOCTYPE html><title>Not found</title><h1>404 not here</h1>")
        ext = os.path.splitext(full)[1]
        with open(full, "rb") as f:
            data = f.read()
        if ext in TEXT:
            data = data.replace(b"@PORT@", str(PORT).encode())
        return self.send(200, data, TYPES.get(ext, "application/octet-stream"))

    do_GET = do_POST = do_PUT = do_DELETE = do_OPTIONS = do_HEAD = handle_any

    def log_message(self, fmt, *args):
        pass


class TCPServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    global ROOT, PORT
    ROOT = sys.argv[2]
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind((HOST, 0))
    web = TCPServer((HOST, 0), Web)
    PORT = web.server_address[1]
    threading.Thread(target=dns_udp, args=(udp,), daemon=True).start()
    threading.Thread(target=web.serve_forever, daemon=True).start()
    with open(sys.argv[1] + ".tmp", "w") as f:
        f.write("dns=%d http=%d\n" % (udp.getsockname()[1], PORT))
    os.rename(sys.argv[1] + ".tmp", sys.argv[1])
    log("serving: dns 127.0.0.1:%d (udp), http 127.0.0.1:%d from %s" % (udp.getsockname()[1], PORT, ROOT))
    threading.Event().wait()


if __name__ == "__main__":
    main()
