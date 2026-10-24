#!/usr/bin/env python3
"""The HTTP server for scripts/js-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and serves the pages in tests/js (argv[2]) and nothing else:
  /NAME.html, /NAME.js   the fixture pages and their scripts
  /thanks                where the validated form goes (logs "thanks QUERY")
  /storage.html          also sets an HttpOnly cookie (scripts must not see it)
  /big.js?mb=N           a generated minified bundle of about N MB (functions, closures,
                         object and array literals; nothing but generated code), which sets
                         window.__big = {sum, want, count}
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


def big_bundle(mb):
    """About `mb` MB of minified-looking code. Every function is `function fN(a,b){...;return N}`;
    the page calls every 50th and compares the sum with the one computed here."""
    target = int(mb * 1024 * 1024)
    out = ["(function(){var G=typeof window!=='undefined'?window:this;var fns=[],mods=[],sum=0;"]
    size = len(out[0])
    n = 0
    while size < target:
        k = n % 4
        if k == 0:
            piece = "function f%d(a,b){var c=a*%d+b;if(c>%d){c-=%d}else{c+=%d}for(var i=0;i<2;i++){c^=i}return %d}fns.push(f%d);" % (n, n % 97 + 3, n * 13 + 1000, n % 31 + 1, n % 17, n, n)
        elif k == 1:
            piece = "function g%d(x){return x%%%d===0?%d:x+%d}fns.push(function(){return g%d(%d)*0+%d});" % (n, n % 11 + 2, n % 5, n % 7, n, n % 9 + 1, n)
        elif k == 2:
            piece = "mods.push((function(){var s=%d;return{get:function(){return s},add:function(x){s+=x;return s}}})());fns.push(function(){return mods[mods.length-1].get()*0+%d});" % (n, n)
        else:
            nums = ",".join(str((n * 7 + j * 13) % 1000) for j in range(24))
            keys = ",".join("k%d_%d:%d" % (n, j, j) for j in range(12))
            piece = "var d%d=[%s],o%d={%s};fns.push(function(){return d%d.length*0+o%d.k%d_0*0+%d});" % (n, nums, n, keys, n, n, n, n)
        out.append(piece)
        size += len(piece)
        n += 1
    want = sum(i for i in range(n) if i % 50 == 0)
    out.append("for(var i=0;i<fns.length;i+=50){sum+=fns[i](i,1)}G.__big={sum:sum,want:%d,count:fns.length};" % want)
    out.append("})();")
    return "".join(out)


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
        if path == "/big.js":
            q = dict(urllib.parse.parse_qsl(query))
            try:
                mb = max(0.1, min(float(q.get("mb", "1")), 16.0))
            except ValueError:
                mb = 1.0
            return self.send(200, big_bundle(mb).encode(), TYPES[".js"])
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
