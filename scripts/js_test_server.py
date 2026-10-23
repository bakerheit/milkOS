#!/usr/bin/env python3
"""The HTTP server for scripts/js-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and serves the pages in tests/js (argv[2]) and nothing else:
  /NAME.html, /NAME.js   the fixture pages and their scripts
  /thanks                where the validated form goes (logs "thanks QUERY")
  /storage.html          also sets an HttpOnly cookie (scripts must not see it)
Every request is logged: "http GET PATH cookie=COOKIE". Writes "http=PORT" to argv[1], then
serves until killed.
"""
import http.server
import os
import socketserver
import sys
import threading
import urllib.parse

HOST = "127.0.0.1"
ROOT = None
TYPES = {".html": "text/html; charset=utf-8", ".js": "text/javascript; charset=utf-8"}


def log(*a):
    print(*a, flush=True)


class Web(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def send(self, code, body, ctype="text/html; charset=utf-8", headers=()):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        log("http GET", self.path, "cookie=" + str(self.headers.get("Cookie")))
        path, _, query = self.path.partition("?")
        if path == "/thanks":
            q = urllib.parse.parse_qsl(query, keep_blank_values=True)
            log("thanks", "&".join("%s=%s" % kv for kv in q))
            return self.send(200, b"<!DOCTYPE html><title>Thanks</title><h1>Thanks!</h1>")
        name = os.path.basename(path)
        ext = os.path.splitext(name)[1]
        full = os.path.join(ROOT, name)
        if ext in TYPES and os.path.isfile(full):
            headers = []
            if name == "storage.html":
                headers.append(("Set-Cookie", "secret=1; Path=/; HttpOnly"))
            with open(full, "rb") as f:
                return self.send(200, f.read(), TYPES[ext], headers)
        return self.send(404, b"<!DOCTYPE html><title>Not found</title><h1>404</h1>")

    def log_message(self, fmt, *args):
        pass


class TCPServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    global ROOT
    ROOT = sys.argv[2]
    web = TCPServer((HOST, 0), Web)
    threading.Thread(target=web.serve_forever, daemon=True).start()
    with open(sys.argv[1] + ".tmp", "w") as f:
        f.write("http=%d\n" % web.server_address[1])
    os.rename(sys.argv[1] + ".tmp", sys.argv[1])
    log("serving: http 127.0.0.1:%d from %s" % (web.server_address[1], ROOT))
    threading.Event().wait()


if __name__ == "__main__":
    main()
