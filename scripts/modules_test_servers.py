#!/usr/bin/env python3
"""Host-side servers for scripts/modules-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and never forwards anything anywhere:
  * a DNS server (UDP) for a made-up zone: www.milk.test and api.milk.test (one site, two
    origins) and other.test (another site), all A 10.0.2.2; everything else NXDOMAIN. TTL 60.
  * one HTTP/1.1 server (the guest tells the sites apart by the Host header) serving the tree in
    tests/modules (argv[2]) by path, with MIME types by extension (.js and .mjs text/javascript,
    .json application/json, .html, .css), "@PORT@" replaced by the server's port in text files,
    and these paths on top:
      /mime/plain.js        JavaScript served as text/plain      /mime/html.js     as text/html
      /mime/none.js         no Content-Type at all               /mime/json.js     a module as application/json
      /mime/data.json       JSON served as text/javascript
      /status/N.js          a module body with status N          (404 for anything not found)
      /cors/open.js         JavaScript with Access-Control-Allow-Origin: *
      /cors/closed.js       JavaScript with no CORS headers at all
      /cors/origin.js       allows only the asking origin (Vary: Origin) and says which it saw
      /cors/cred.js         allows the asking origin with credentials and says the Cookie it saw
      /cors/setcookie       sets the cookie xs=1 (CORS-open, credentials allowed)
      /cookie/set           sets the cookie sid=m1      /cookie/who.js   a module saying the Cookie it got
      /redirect.js?to=URL   a 302 to URL (CORS-open itself)
      /gen/chain/N.js       a module importing ./N+1.js, for ever
      /gen/wide.js?n=K      a module importing K tiny modules /gen/leaf/I.js
      /gen/leaf/I.js        export const i = I;
      /slow/MS.js           a module that answers after MS ms
      /stream/forever.js    a module whose body trickles for 30 s (to be abandoned by a navigation)
      /count/NAME.js        a module that exports how many times it was asked for
Every request is logged: "http METHOD HOST PATH origin=O cookie=C". A trickle that the guest
abandons logs "aborted PATH". Writes "dns=PORT http=PORT" to argv[1], then serves until killed.
"""
import http.server
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
TEXT = {".html", ".js", ".mjs", ".json", ".css", ".txt"}


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

    def send(self, code, body=b"", ctype="text/javascript; charset=utf-8", headers=(), reason=None):
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
        if n:
            self.rfile.read(n)
        host = (self.headers.get("Host") or "").split(":")[0]
        origin = self.headers.get("Origin")
        log("http", self.command, host, self.path, "origin=" + str(origin), "cookie=" + str(self.headers.get("Cookie")))
        u = urllib.parse.urlsplit(self.path)
        path, q = u.path, dict(urllib.parse.parse_qsl(u.query))
        if self.command == "OPTIONS":
            return self.send(204, headers=[("Access-Control-Allow-Origin", origin or "*")], ctype=None)
        try:
            for prefix, fn in (
                ("/mime/", self.mime),
                ("/status/", self.status),
                ("/cors/", self.cors),
                ("/cookie/", self.cookie),
                ("/gen/", self.gen),
                ("/slow/", self.slow),
                ("/stream/", self.stream),
                ("/count/", self.count),
            ):
                if path.startswith(prefix):
                    r = fn(path, q, origin)
                    if r is not False:
                        return r
            if path == "/redirect.js":
                return self.send(302, "moved", headers=[("Location", q["to"]), ("Access-Control-Allow-Origin", "*")])
            return self.static(path)
        except (BrokenPipeError, ConnectionResetError):
            log("aborted", self.path)

    # ---- files ----
    def static(self, path):
        rel = os.path.normpath(urllib.parse.unquote(path)).lstrip("/")
        if rel.startswith("..") or rel == "":
            rel = "index.html" if rel == "" else None
        full = os.path.join(ROOT, rel) if rel else None
        if not full or not os.path.isfile(full) or not os.path.realpath(full).startswith(os.path.realpath(ROOT)):
            return self.send(404, "<!DOCTYPE html><title>Not found</title><h1>404</h1>", "text/html; charset=utf-8")
        ext = os.path.splitext(full)[1]
        with open(full, "rb") as f:
            body = f.read()
        if ext in TEXT:
            body = body.replace(b"@PORT@", str(PORT).encode())
        return self.send(200, body, TYPES.get(ext, "application/octet-stream"))

    # ---- the special paths ----
    def mime(self, path, q, origin):
        name = path[len("/mime/") :]
        if name == "plain.js":
            return self.send(200, "export const x = 1;", "text/plain; charset=utf-8")
        if name == "html.js":
            return self.send(200, "export const x = 1;", "text/html; charset=utf-8")
        if name == "none.js":
            return self.send(200, "export const x = 1;", None)
        if name == "json.js":
            return self.send(200, "export const x = 1;", "application/json")
        if name == "data.json":
            return self.send(200, '{"a": 1}', "text/javascript; charset=utf-8")
        return False

    def status(self, path, q, origin):
        code = int(path[len("/status/") :].split(".")[0])
        return self.send(code, "export const x = 1;", "text/javascript; charset=utf-8", reason="Status %d" % code)

    def cors(self, path, q, origin):
        o = origin or "*"
        name = path[len("/cors/") :]
        if name == "open.js":
            return self.send(200, "export const where = 'cors-open';", headers=[("Access-Control-Allow-Origin", "*")])
        if name == "closed.js":
            return self.send(200, "export const where = 'cors-closed';")
        if name == "origin.js":
            body = "export const seen = %r;" % str(origin)
            return self.send(200, body, headers=[("Access-Control-Allow-Origin", o), ("Vary", "Origin")])
        if name == "cred.js":
            body = "export const cookie = %r;" % str(self.headers.get("Cookie"))
            return self.send(
                200,
                body,
                headers=[("Access-Control-Allow-Origin", o), ("Access-Control-Allow-Credentials", "true"), ("Vary", "Origin")],
            )
        if name == "setcookie":
            return self.send(
                200,
                "ok",
                "text/plain",
                headers=[
                    ("Access-Control-Allow-Origin", o),
                    ("Access-Control-Allow-Credentials", "true"),
                    ("Set-Cookie", "xs=1; Path=/"),
                ],
            )
        return False

    def cookie(self, path, q, origin):
        name = path[len("/cookie/") :]
        if name == "set":
            return self.send(200, "set", "text/plain", headers=[("Set-Cookie", "sid=m1; Path=/")])
        if name == "who.js":
            body = "export const cookie = %r;" % str(self.headers.get("Cookie"))
            return self.send(200, body)
        return False

    def gen(self, path, q, origin):
        if path.startswith("/gen/chain/"):
            n = int(path[len("/gen/chain/") :].split(".")[0])
            return self.send(200, "import './%d.js'; export const n = %d;" % (n + 1, n))
        if path == "/gen/wide.js":
            k = int(q.get("n", "10"))
            return self.send(200, "".join("import '/gen/leaf/%d.js';\n" % i for i in range(k)) + "export const k = %d;" % k)
        if path.startswith("/gen/leaf/"):
            i = int(path[len("/gen/leaf/") :].split(".")[0])
            return self.send(200, "export const i = %d;" % i)
        return False

    def slow(self, path, q, origin):
        ms = int(path[len("/slow/") :].split(".")[0])
        time.sleep(ms / 1000.0)
        return self.send(200, "export const waited = %d; globalThis.__slow = (globalThis.__slow || 0) + 1;" % ms)

    def stream(self, path, q, origin):
        # Headers at once, then a trickle of comment text; a guest that gives up makes the write fail.
        total = 30 * 1024
        self.send_response(200)
        self.send_header("Content-Type", "text/javascript; charset=utf-8")
        self.send_header("Content-Length", str(total))
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        try:
            for _ in range(total // 1024):
                self.wfile.write(b"/" + b"x" * 1022 + b"\n")
                self.wfile.flush()
                time.sleep(1.0)
        except OSError:
            log("aborted", self.path)
        return None

    def count(self, path, q, origin):
        name = path[len("/count/") :]
        with LOCK:
            COUNTS[name] = COUNTS.get(name, 0) + 1
            n = COUNTS[name]
        return self.send(200, "export const count = %d;" % n)

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
