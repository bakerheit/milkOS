#!/usr/bin/env python3
"""Host-side servers for scripts/browser-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and never forwards anything anywhere:
  * a DNS server (UDP) for a made-up zone: www.milk.test, shop.milk.test (one site) and
    tracker.test (another site), all A 10.0.2.2; everything else NXDOMAIN. TTL 60.
  * an HTTP/1.1 server with the Browser's test pages:
      /            links, a list, a table, pre, a quote, an image (/cow.png)
      /long        300 paragraphs (id=p1..p300) inside <main>, for scrolling and find
      /form        a GET form (text, checkbox, radio, select, textarea) to /echo and a POST
                   sign-in form to /login
      /echo        the query, as a page            /login (POST)  Set-Cookie, 303 /account
      /account     says whether the session cookie came back
      /cookies     a page that loads an image from tracker.test (third party)
      /pixel.png   tracker.test's image: sets a cookie, logs the Cookie it got
      /track       tracker.test as a page (first party: its cookie may be set and sent)
      /file.zip    200 KB of application/zip, slowly (a download)
      /missing     404
Every request is logged: "http METHOD PATH host=HOST cookie=COOKIE" (and "post BODY").
Writes "dns=PORT http=PORT" to argv[1], then serves until killed.
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
ZONE = {"www.milk.test": "10.0.2.2", "shop.milk.test": "10.0.2.2", "tracker.test": "10.0.2.2"}


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


def png(w, h, pixel):
    """A truecolour PNG (pixel(x, y) -> (r, g, b))."""
    raw = b"".join(b"\0" + bytes(c for x in range(w) for c in pixel(x, y)) for y in range(h))

    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d) & 0xFFFFFFFF)

    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


# A cow-ish picture: a milk-white field with black patches and a green strip of grass.
COW = png(160, 96, lambda x, y: (60, 150, 70) if y > 80 else ((30, 30, 30) if ((x // 24) + (y // 20)) % 3 == 0 else (250, 248, 240)))
PIXEL = png(4, 4, lambda x, y: (229, 140, 151))


def page(title, body):
    return ("<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>%s</title></head><body>%s</body></html>" % (title, body)).encode("utf-8")


HOME = page(
    "Milk Browser test",
    "<h1>The milkOS Browser</h1>"
    "<p>A page from the <b>test server</b>, with <i>styles</i>, <code>code</code> and links: "
    "<a href=\"/long\">a long page</a>, <a href=\"/form\">forms</a>, <a href=\"/cookies\">cookies</a>, "
    "<a href=\"/file.zip\">a download</a>, <a href=\"/missing\">a missing page</a> and "
    "<a href=\"/long#p150\">paragraph 150</a>.</p>"
    "<p><img src=\"/cow.png\" alt=\"a cow\" width=\"160\" height=\"96\"> Café &mdash; &ldquo;quotes&rdquo; &copy; 2026</p>"
    "<ul><li>Whole milk</li><li>Skim<ul><li>Very skim</li></ul></li><li>Chocolate</li></ul>"
    "<ol><li>First</li><li>Second</li></ol>"
    "<table><tr><th>Drink</th><th>Size</th><th>Price</th></tr><tr><td>Milk</td><td>1 L</td><td>1.20</td></tr>"
    "<tr><td>Cream</td><td>250 ml</td><td>2.10</td></tr></table>"
    "<blockquote>Milk is the first food of all mammals.</blockquote>"
    "<pre>fn main() {\n    println!(\"moo\");\n}</pre>"
    "<hr><p><small>Served by scripts/browser_test_servers.py</small></p>",
)

LONG = page(
    "A long page",
    "<p><a href=\"/\">Home</a> | <a href=\"#p300\">to the end</a></p><main><h1>Three hundred paragraphs</h1>"
    + "".join("<p id=\"p%d\">Paragraph %d of the long page: milk, cream, butter and cheese.</p>" % (k, k) for k in range(1, 301))
    + "<p id=\"end\">The end.</p></main>",
)

FORM = page(
    "Forms",
    "<h1>Forms</h1><h2>Search (GET)</h2>"
    "<form action=\"/echo\"><p>Words: <input name=\"q\" size=\"24\" placeholder=\"what to find\"></p>"
    "<p><input type=\"checkbox\" name=\"milk\" checked> milk <input type=\"checkbox\" name=\"cream\" value=\"yes\"> cream</p>"
    "<p>Size: <input type=\"radio\" name=\"size\" value=\"s\"> S <input type=\"radio\" name=\"size\" value=\"m\" checked> M "
    "<input type=\"radio\" name=\"size\" value=\"l\"> L</p>"
    "<p>Drink: <select name=\"drink\"><option>Milk<option value=\"latte\">Latte<option>Cocoa</select></p>"
    "<p><textarea name=\"note\" cols=\"30\" rows=\"2\"></textarea></p>"
    "<p><input type=\"submit\" value=\"Search\"> <input type=\"reset\"></p></form>"
    "<h2>Sign in (POST)</h2>"
    "<form action=\"/login\" method=\"post\"><p>Name: <input name=\"user\"></p><p>Password: <input type=\"password\" name=\"pw\"></p>"
    "<p><button>Sign in</button></p></form>",
)


class Web(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def host(self):
        return (self.headers.get("Host") or "").split(":")[0]

    def note(self, method):
        log("http", method, self.path, "host=" + self.host(), "cookie=" + str(self.headers.get("Cookie")))

    def send(self, code, body, ctype="text/html; charset=utf-8", headers=()):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        for k, v in headers:
            self.send_header(k, v)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        self.note("GET")
        path, _, query = self.path.partition("?")
        port = self.server.server_address[1]
        if self.host() == "tracker.test":
            if path == "/pixel.png":
                return self.send(200, PIXEL, "image/png", [("Set-Cookie", "pixel=1; Max-Age=3600")])
            if path == "/track":
                return self.send(200, page("Tracker", "<h1>The tracker's own page</h1>"), headers=[("Set-Cookie", "track=1; Max-Age=3600")])
            return self.send(404, page("Not here", "<h1>Not here</h1>"))
        if path == "/":
            return self.send(200, HOME)
        if path == "/cow.png":
            return self.send(200, COW, "image/png")
        if path == "/long":
            return self.send(200, LONG)
        if path == "/form":
            return self.send(200, FORM)
        if path == "/echo":
            q = urllib.parse.parse_qsl(query, keep_blank_values=True)
            log("echo", "&".join("%s=%s" % kv for kv in q))
            rows = "".join("<li><b>%s</b> = %s</li>" % (k, v.replace("<", "&lt;")) for k, v in q)
            return self.send(200, page("Echo", "<h1>You sent</h1><ul>%s</ul>" % rows))
        if path == "/account":
            signed = "session=abc123" in (self.headers.get("Cookie") or "")
            return self.send(200, page("Account", "<h1>%s</h1>" % ("Signed in as milk" if signed else "Not signed in")))
        if path == "/cookies":
            return self.send(
                200,
                page("Cookies test", "<h1>A page with a third-party image</h1><p><img src=\"http://tracker.test:%d/pixel.png\" alt=\"pixel\"></p>" % port),
                headers=[("Set-Cookie", "first=1; Max-Age=3600"), ("Set-Cookie", "sess=1")],
            )
        if path == "/file.zip":
            data = bytes((k * 7) & 0xFF for k in range(200 * 1024))
            self.send_response(200)
            self.send_header("Content-Type", "application/zip")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            for k in range(0, len(data), 16 * 1024):
                self.wfile.write(data[k : k + 16 * 1024])
                self.wfile.flush()
                time.sleep(0.15)
            log("http /file.zip sent", len(data))
            return
        if path == "/slow":
            time.sleep(30)
            return self.send(200, page("Slow", "<p>at last</p>"))
        return self.send(404, page("Not found", "<h1>404: no such page</h1><p><a href=\"/\">Home</a></p>"))

    def do_POST(self):
        self.note("POST")
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(min(n, 65536)).decode("utf-8", "replace")
        log("post", self.path, body, "type=" + str(self.headers.get("Content-Type")))
        if self.path == "/login":
            self.send_response(303)
            self.send_header("Location", "/account")
            self.send_header("Set-Cookie", "session=abc123; Path=/; HttpOnly; Max-Age=3600")
            self.send_header("Set-Cookie", "theme=dark")
            self.send_header("Set-Cookie", "evil=1; Domain=tracker.test")
            self.send_header("Content-Length", "0")
            self.end_headers()
            return
        self.send(404, page("Not found", "<h1>404</h1>"))

    def log_message(self, fmt, *args):
        pass


class TCPServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    udp.bind((HOST, 0))
    web = TCPServer((HOST, 0), Web)
    threading.Thread(target=dns_udp, args=(udp,), daemon=True).start()
    threading.Thread(target=web.serve_forever, daemon=True).start()
    with open(sys.argv[1] + ".tmp", "w") as f:
        f.write("dns=%d http=%d\n" % (udp.getsockname()[1], web.server_address[1]))
    os.rename(sys.argv[1] + ".tmp", sys.argv[1])
    log("serving: dns 127.0.0.1:%d (udp), http 127.0.0.1:%d" % (udp.getsockname()[1], web.server_address[1]))
    threading.Event().wait()


if __name__ == "__main__":
    main()
