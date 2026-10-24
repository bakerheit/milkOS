#!/usr/bin/env python3
"""Host-side servers for scripts/memory-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode networking)
and never forwards anything anywhere:
  * a DNS server (UDP) for a made-up zone: www.milk.test and api.milk.test (A 10.0.2.2);
    everything else NXDOMAIN. TTL 60.
  * one HTTP/1.1 server serving the tree in tests/memory (argv[2]) by path, with "@PORT@"
    replaced by the server's port in text files, and these paths on top:
      /big.html?mb=N[&gap=MS][&gz=1]  a page of about N MB sent in 64 KB chunks (Transfer-Encoding:
                                 chunked), a pause of MS ms (default 2) after every 256 KB; the top
                                 of it says "TOP OF THE BIG PAGE" and every row is a numbered
                                 paragraph; with gz=1 it is gzip-encoded as it goes. The log says
                                 "big start" when the first chunk goes out and "big done MB" when the last
                                 one has been taken by the client ("big aborted MB" if the client hung up).
      /bundle.js?mb=N            a script of about N MB made of plain functions, ending in a line
                                 that reports (MEMTEST bundle ...)
      /stream?mb=N[&chunk=K][&gap=MS]  a body of N MB of a counting pattern, sent in K KB chunks
                                 (default 64) with MS ms between them (default 0), "stream start", then
                                 "stream done MB" or "stream aborted MB" in the log
      /dl?mb=N                   N MB as application/octet-stream with Content-Disposition: attachment
      /count/NAME                how many times it was asked for
Every request is logged: "http METHOD HOST PATH". Writes "dns=PORT http=PORT" to argv[1], then
serves until killed.
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
ZONE = {"www.milk.test": "10.0.2.2", "api.milk.test": "10.0.2.2"}
ROOT = None
PORT = 0
COUNTS = {}
LOCK = threading.Lock()
T0 = time.time()

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
    print("%7.2f" % (time.time() - T0), *a, flush=True)


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
        sock.sendto(r, peer)


def big_rows(mb):
    """The text of a big page, as an iterator of byte chunks of about 64 KB."""
    total = mb << 20
    head = (
        '<!DOCTYPE html><html><head><meta charset="utf-8"><title>The big page</title>'
        "<style>body{font-family:sans-serif;margin:14px;background:#fffdf8;color:#3a2e28}p{margin:2px 0}</style></head>"
        '<body><h1 id="top">TOP OF THE BIG PAGE</h1><p>Rows follow. Each one is a numbered paragraph.</p>'
    ).encode()
    yield head
    sent, n = len(head), 0
    row = "<p>row %07d: milk comes from cows, and so does the cheese made from it, one numbered row at a time.</p>\n"
    buf = []
    size = 0
    while sent + size < total:
        s = (row % n).encode()
        buf.append(s)
        size += len(s)
        n += 1
        if size >= 65536:
            sent += size
            yield b"".join(buf)
            buf, size = [], 0
    if buf:
        yield b"".join(buf)
    yield ('<p id="bottom">END OF THE BIG PAGE after %d rows</p></body></html>' % n).encode()


def bundle(mb):
    """A script of plain functions, about mb MB; the last line reports."""
    total = mb << 20
    out = ["var __sum = 0, __n = 0;\n"]
    size, i = len(out[0]), 0
    while size < total:
        s = "function fn%d(a, b) { var t = a * %d + b; if (t > 1000000) { t = t %% 1000003; } return t + %d; }\n" % (i, i % 97 + 1, i)
        out.append(s)
        size += len(s)
        i += 1
    out.append("for (var __i = 0; __i < %d; __i += %d) { __sum += eval('fn' + __i)(__i, 3); __n++; }\n" % (i, max(1, i // 50)))
    out.append("console.log('MEMTEST bundle loaded ' + %d + ' functions, ran ' + __n + ' sum ' + __sum);\n" % i)
    return "".join(out).encode()


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

    def chunked(self, ctype, chunks, extra=(), gap=0.0, every=1, what="stream", gz=False):
        """Send `chunks` (an iterator of bytes) with chunked transfer encoding."""
        self.send_response(200)
        self.send_header("Content-Type", ctype)
        self.send_header("Transfer-Encoding", "chunked")
        self.send_header("Connection", "close")
        for k, v in extra:
            self.send_header(k, v)
        if gz:
            self.send_header("Content-Encoding", "gzip")
        self.end_headers()
        sent = 0
        n = 0
        z = zlib.compressobj(6, zlib.DEFLATED, 31) if gz else None
        log(what, "start")
        try:
            for c in chunks:
                sent += len(c)
                out = z.compress(c) + z.flush(zlib.Z_SYNC_FLUSH) if z else c
                if out:
                    self.wfile.write(b"%x\r\n" % len(out) + out + b"\r\n")
                    self.wfile.flush()
                n += 1
                if gap and n % every == 0:
                    time.sleep(gap)
            if z:
                tail = z.flush()
                if tail:
                    self.wfile.write(b"%x\r\n" % len(tail) + tail + b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
            self.wfile.flush()
            log(what, "done %d MB (%d bytes)" % (sent >> 20, sent))
        except (BrokenPipeError, ConnectionResetError, OSError):
            log(what, "aborted at %d MB (%d bytes)" % (sent >> 20, sent))
        self.close_connection = True

    def handle_any(self):
        n = int(self.headers.get("Content-Length") or 0)
        if n:
            self.rfile.read(n)
        host = (self.headers.get("Host") or "").split(":")[0]
        log("http", self.command, host, self.path)
        u = urllib.parse.urlsplit(self.path)
        path, q = u.path, dict(urllib.parse.parse_qsl(u.query))
        try:
            if path == "/big.html":
                mb = int(q.get("mb", "40"))
                gap = float(q.get("gap", "2")) / 1000.0
                return self.chunked(
                    "text/html; charset=utf-8", big_rows(mb), gap=gap, every=4, what="big", gz=q.get("gz") == "1"
                )
            if path == "/bundle.js":
                return self.send(200, bundle(int(q.get("mb", "3"))), "text/javascript; charset=utf-8")
            if path == "/stream":
                mb = int(q.get("mb", "10"))
                chunk = int(q.get("chunk", "64")) * 1024
                gap = float(q.get("gap", "0")) / 1000.0
                total = mb << 20
                block = bytes(i & 255 for i in range(chunk))

                def gen():
                    left = total
                    while left > 0:
                        yield block[: min(chunk, left)]
                        left -= chunk

                return self.chunked(
                    "application/octet-stream", gen(), extra=[("Access-Control-Allow-Origin", "*")], gap=gap, what="stream"
                )
            if path == "/dl":
                mb = int(q.get("mb", "10"))
                return self.chunked(
                    "application/octet-stream",
                    (bytes([i & 255]) * 65536 for i in range(mb * 16)),
                    extra=[("Content-Disposition", 'attachment; filename="big.bin"')],
                    what="dl",
                )
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
