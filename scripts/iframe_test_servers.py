#!/usr/bin/env python3
"""Host-side servers for scripts/iframe-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and never forwards anything anywhere:
  * a DNS server (UDP) for a made-up zone: www.milk.test and api.milk.test (one site, two
    origins) and other.test (another site), all A 10.0.2.2; everything else NXDOMAIN. TTL 60.
  * one HTTP/1.1 server (the guest tells the sites apart by the Host header) serving the tree in
    tests/iframe (argv[2]) by path, with "@PORT@" replaced by the server's port in text files
    and these paths on top:
      /xfo/deny.html  /xfo/same.html  /xfo/allowall.html  /xfo/conflict.html  /xfo/obsolete.html
                                    pages with X-Frame-Options DENY, SAMEORIGIN, ALLOWALL, two
                                    conflicting headers, and ALLOW-FROM
      /csp/none.html /csp/self.html /csp/www.html /csp/other.html /csp/wild.html /csp/both.html
                                    pages with a frame-ancestors of 'none', 'self', the origin
                                    www.milk.test, the origin other.test, *.milk.test, and (both)
                                    X-Frame-Options: DENY plus frame-ancestors *
      /csp/sandbox.html             a page whose CSP says `sandbox allow-scripts`
      Each of these shows "FRAMED name" and posts "framed:name" to its parent when it is shown.
      /echo                         the request as a page: method, query, form fields, Cookie,
                                    Referer; it posts what it got to its parent
      /cookie/set                   a page that sets the cookie sid=m1 (Domain=milk.test)
      /cookie/who                   a page that says the Cookie it was sent (and posts it up)
      /deep?n=K                     a page "level K" holding a frame of /deep?n=K+1
      /slow?ms=MS                   a page that answers after MS ms
      /status/N                     an HTML page with status N
      /redirect?to=URL              a 302 to URL
      /download/file.bin            Content-Disposition: attachment
      /plain.txt                    text/plain, /pic.png a 64x48 PNG, /pic.bin an image of no type
      /tall                         a page of 120 numbered lines
      /count/NAME                   a page saying how many times it was asked for
Every request is logged: "http METHOD HOST PATH cookie=C referer=R dest=D site=S". A page
that the guest abandons mid-answer logs "aborted PATH". Writes "dns=PORT http=PORT" to argv[1],
then serves until killed.
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
import zlib

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
    ".css": "text/css",
    ".txt": "text/plain; charset=utf-8",
    ".png": "image/png",
}
TEXT = {".html", ".js", ".css", ".txt"}


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


def png(w, h, rgb):
    """A solid-colour PNG."""

    def chunk(kind, data):
        c = struct.pack(">I", len(data)) + kind + data
        return c + struct.pack(">I", zlib.crc32(kind + data) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + bytes(rgb) * w for _ in range(h))
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw))
        + chunk(b"IEND", b"")
    )


def framed_page(name, extra=""):
    """A page that says it was shown, and tells its parent so."""
    return (
        "<!DOCTYPE html><html><head><meta charset=utf-8><title>FRAMED %s</title>"
        "<style>body{font-family:sans-serif;margin:8px;background:#e8f2e0;color:#3a2e28}</style></head>"
        "<body><h3>FRAMED %s</h3><script>try{parent.postMessage('framed:%s','*')}catch(e){}"
        "console.log('IFRTEST framed %s shown')</script>%s</body></html>" % (name, name, name, name, extra)
    )


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
            "referer=" + str(self.headers.get("Referer")),
            "dest=" + str(self.headers.get("Sec-Fetch-Dest")),
            "site=" + str(self.headers.get("Sec-Fetch-Site")),
        )
        u = urllib.parse.urlsplit(self.path)
        path, q = u.path, dict(urllib.parse.parse_qsl(u.query))
        try:
            for prefix, fn in (
                ("/xfo/", self.xfo),
                ("/csp/", self.csp),
                ("/cookie/", self.cookie),
                ("/status/", self.status),
                ("/download/", self.download),
                ("/count/", self.count),
            ):
                if path.startswith(prefix):
                    r = fn(path, q, body)
                    if r is not False:
                        return r
            special = {
                "/echo": self.echo,
                "/deep": self.deep,
                "/slow": self.slow,
                "/redirect": self.redirect,
                "/tall": self.tall,
                "/plain.txt": lambda p, q, b: self.send(200, "just some plain text\nin a frame\n", "text/plain; charset=utf-8"),
                "/pic.png": lambda p, q, b: self.send(200, png(64, 48, (200, 120, 60)), "image/png"),
                "/pic.bin": lambda p, q, b: self.send(200, png(40, 40, (60, 120, 200)), None),
            }
            if path in special:
                return special[path](path, q, body)
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
            return self.send(404, "<!DOCTYPE html><title>Not found</title><h1>404 not here</h1>")
        ext = os.path.splitext(full)[1]
        with open(full, "rb") as f:
            data = f.read()
        if ext in TEXT:
            data = data.replace(b"@PORT@", str(PORT).encode())
        return self.send(200, data, TYPES.get(ext, "application/octet-stream"))

    # ---- who may frame what ----
    def xfo(self, path, q, body):
        name = path[len("/xfo/") :].split(".")[0]
        page = framed_page("xfo-" + name)
        if name == "deny":
            return self.send(200, page, headers=[("X-Frame-Options", "DENY")])
        if name == "same":
            return self.send(200, page, headers=[("X-Frame-Options", "SAMEORIGIN")])
        if name == "allowall":
            return self.send(200, page, headers=[("X-Frame-Options", "ALLOWALL")])
        if name == "conflict":
            return self.send(200, page, headers=[("X-Frame-Options", "SAMEORIGIN"), ("X-Frame-Options", "DENY")])
        if name == "obsolete":
            return self.send(200, page, headers=[("X-Frame-Options", "ALLOW-FROM http://nobody.test/")])
        return False

    def csp(self, path, q, body):
        name = path[len("/csp/") :].split(".")[0]
        page = framed_page("csp-" + name)
        fa = {
            "none": "frame-ancestors 'none'",
            "self": "frame-ancestors 'self'",
            "www": "frame-ancestors http://www.milk.test:%d" % PORT,
            "other": "frame-ancestors http://other.test:%d" % PORT,
            "wild": "frame-ancestors http://*.milk.test:%d" % PORT,
        }
        if name in fa:
            return self.send(200, page, headers=[("Content-Security-Policy", fa[name])])
        if name == "both":
            return self.send(200, page, headers=[("X-Frame-Options", "DENY"), ("Content-Security-Policy", "frame-ancestors *")])
        if name == "sandbox":
            return self.send(
                200,
                framed_page("csp-sandbox", "<script>console.log('IFRTEST csp-sandbox script-ran ' + String(window.origin))</script>"),
                headers=[("Content-Security-Policy", "sandbox allow-scripts")],
            )
        return False

    # ---- pages for frames ----
    def echo(self, path, q, body):
        form = dict(urllib.parse.parse_qsl(body.decode("utf-8", "replace")))
        ctype = self.headers.get("Content-Type")
        text = "method=%s query=%s form=%s type=%s cookie=%s referer=%s" % (
            self.command,
            sorted(q.items()),
            sorted(form.items()),
            ctype,
            self.headers.get("Cookie"),
            self.headers.get("Referer"),
        )
        esc = text.replace("&", "&amp;").replace("<", "&lt;")
        js = text.replace("\\", "\\\\").replace("'", "\\'")
        page = (
            "<!DOCTYPE html><html><head><meta charset=utf-8><title>Echo</title>"
            "<style>body{font-family:sans-serif;margin:8px;background:#f0e8f8;color:#3a2e28}</style></head>"
            "<body><h3>ECHO</h3><p id=what>%s</p><script>try{parent.postMessage('echo:%s','*')}catch(e){}"
            "console.log('IFRTEST echo got %s')</script></body></html>" % (esc, js, js)
        )
        return self.send(200, page)

    def cookie(self, path, q, body):
        name = path[len("/cookie/") :]
        if name == "set":
            return self.send(
                200,
                framed_page("cookie-set"),
                headers=[("Set-Cookie", "sid=m1; Domain=milk.test; Path=/"), ("Set-Cookie", "hostonly=h1; Path=/")],
            )
        if name == "who":
            c = str(self.headers.get("Cookie")).replace("'", "")
            return self.send(
                200,
                "<!DOCTYPE html><title>who</title><body><p>COOKIE %s</p><script>try{parent.postMessage('cookie:%s','*')}catch(e){}"
                "console.log('IFRTEST cookie-who %s')</script></body>" % (c, c, c),
            )
        return False

    def deep(self, path, q, body):
        k = int(q.get("n", "1"))
        return self.send(
            200,
            "<!DOCTYPE html><title>level %d</title><body style='margin:4px;background:#fffdf8'>level %d"
            "<iframe src='/deep?n=%d' style='display:block;width:%dpx;height:%dpx'></iframe>"
            "<script>console.log('IFRTEST deep level-%d shown')</script></body>" % (k, k, k + 1, max(40, 330 - 40 * k), max(40, 200 - 24 * k), k),
        )

    def slow(self, path, q, body):
        time.sleep(int(q.get("ms", "1000")) / 1000.0)
        return self.send(200, framed_page("slow"))

    def status(self, path, q, body):
        code = int(path[len("/status/") :])
        return self.send(code, "<!DOCTYPE html><title>Status %d</title><h2>The server says %d</h2>" % (code, code), reason="Status %d" % code)

    def redirect(self, path, q, body):
        return self.send(302, "moved", headers=[("Location", q["to"])])

    def download(self, path, q, body):
        return self.send(
            200, b"\x00\x01binary file\x02", "application/octet-stream", headers=[("Content-Disposition", 'attachment; filename="file.bin"')]
        )

    def tall(self, path, q, body):
        lines = "".join("<p id=l%d>line %d of the tall page</p>" % (i, i) for i in range(1, 121))
        return self.send(
            200,
            "<!DOCTYPE html><title>Tall</title><body style='margin:6px;background:#fffbe8'>%s"
            "<script>window.addEventListener('scroll',function(){try{parent.postMessage('scrolled:'+Math.round(window.scrollY),'*')}catch(e){}})</script>"
            "</body>" % lines,
        )

    def count(self, path, q, body):
        name = path[len("/count/") :]
        with LOCK:
            COUNTS[name] = COUNTS.get(name, 0) + 1
            n = COUNTS[name]
        return self.send(200, framed_page("count-%s-%d" % (name, n)))

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
