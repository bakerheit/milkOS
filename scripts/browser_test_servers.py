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
      /styled      a page with a stylesheet (/site.css): colours, a flex row, a table
      /unicode     Greek, Cyrillic, Latin Extended, dashes, quotes and symbols
      /images      a JPEG, an animated GIF, WebP (lossy and animated), from user/imgdec's fixtures
      /source      a page with a script (it logs SRCTEST page script ran), markup and entities
                   in its text, for view-source:
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
    "<p><a href=\"/form\" style=\"display: block; height: 60px; font-size: 40px\">Forms</a></p>"
    "<h1>The milkOS Browser</h1>"
    "<p>A page from the <b>test server</b>, with <i>styles</i>, <code>code</code> and links: "
    "<a href=\"/long\">a long page</a>, <a href=\"/form\">forms</a>, <a href=\"/cookies\">cookies</a>, "
    "<a href=\"/file.zip\">a download</a>, <a href=\"/missing\">a missing page</a> and "
    "<a href=\"/long#p150\">paragraph 150</a>, <a href=\"/styled\">a styled page</a>, <a href=\"/unicode\">Unicode</a> and <a href=\"/images\">pictures</a>.</p>"
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

SOURCE = (
    "<!DOCTYPE html>\n<html>\n<head><title>Source test</title></head>\n<body>\n<h1>A page with a script</h1>\n"
    "<p>Fish &amp; chips &lt;hot&gt;</p>\n<script>console.log('SRCTEST page script ran');</script>\n</body>\n</html>\n"
)

STYLED = page(
    "Styled page",
    "<link rel=\"stylesheet\" href=\"/site.css\"><header><h1>Milk &amp; Co.</h1><nav><a href=\"/\">Home</a> <a href=\"/long\">Long</a> <a href=\"/form\">Forms</a></nav></header>"
    "<main><div class=\"cards\"><div class=\"card\"><h2>Whole</h2><p>Creamy and full.</p></div><div class=\"card\"><h2>Skim</h2><p>Light and clear.</p></div>"
    "<div class=\"card hot\"><h2>Cocoa</h2><p>Warm, sweet, brown.</p></div></div>"
    "<p class=\"note\">A <a href=\"/\">link</a> in a styled paragraph, with <strong>bold</strong> and <em>italic</em> text.</p></main>"
    "<footer>Served with a stylesheet</footer>",
)
CSS = (
    "body { margin: 0; background: #fffdf8; color: #3a2e28; font-family: sans-serif; }"
    "header { background: #3a2e28; color: #fbf6ec; padding: 12px 20px; display: flex; justify-content: space-between; align-items: center; }"
    "header h1 { margin: 0; font-size: 28px; } nav a { color: #ebb650; margin-left: 16px; }"
    "main { padding: 16px 20px; } .cards { display: flex; gap: 16px; }"
    ".card { flex: 1; background: #efe6d4; border: 2px solid #c4ae8f; border-radius: 8px; padding: 8px 12px; }"
    ".card.hot { background: #e58c97; } .note { border-left: 4px solid #9cbf9b; padding-left: 12px; }"
    "footer { margin: 0 20px; padding-top: 8px; border-top: 1px dashed #c4ae8f; color: #6b5846; }"
).encode()

UNICODE = page(
    "\u03a4\u03bf \u03b3\u03ac\u03bb\u03b1 \u2014 \u041c\u043e\u043b\u043e\u043a\u043e",
    "<h1>\u0393\u03ac\u03bb\u03b1 \u2014 \u041c\u043e\u043b\u043e\u043a\u043e \u2014 Mleko</h1>"
    "<p lang=\"el\">\u03a4\u03bf \u03b3\u03ac\u03bb\u03b1 \u03b5\u03af\u03bd\u03b1\u03b9 \u03bb\u03b5\u03c5\u03ba\u03cc \u03ba\u03b1\u03b9 \u03b6\u03b5\u03c3\u03c4\u03cc. \u0391\u0392\u0393\u0394\u0395 \u03b1\u03b2\u03b3\u03b4\u03b5 \u03c9\u03c8\u03c7</p>"
    "<p lang=\"ru\">\u041c\u043e\u043b\u043e\u043a\u043e \u0431\u0435\u043b\u043e\u0435 \u0438 \u0442\u0451\u043f\u043b\u043e\u0435. \u0410\u0411\u0412\u0413\u0414 \u0436\u0437\u0438\u0439 \u044d\u044e\u044f</p>"
    "<p>Latin Extended: \u0141\u00f3d\u017a, \u010cesk\u00fd, \u0153uvre, \u0219i \u021b, \u00df, \u0131</p>"
    "<p>Dashes \u2013 and \u2014, \u201cdouble\u201d and \u2018single\u2019 quotes, \u2026 ellipsis, \u2022 bullet, \u2192 arrows \u2190, \u00a9 \u20ac 5 \u00bd \u2122 \u2605 \u2713</p>"
    "<p><code>code stays monospace: \u03bb x \u2192 x</code> and <tt>tt</tt></p>"
    "<pre>pre   keeps   its   spaces</pre>"
    "<p style=\"font-family: serif\">A serif paragraph. <span style=\"font-family: cursive\">Cursive.</span> <b>Bold</b> <i>italic</i> <small>small</small></p>"
    "<p style=\"font-size: 32px\">Big text \u2014 \u039c\u03b5\u03b3\u03ac\u03bb\u03bf</p>",
)
FIXTURES = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "user", "imgdec", "tests", "fixtures")
IMAGES = page(
    "Pictures",
    "<h1>Pictures</h1><p>A JPEG <img src=\"/fx/jpeg-big.jpg\" alt=\"jpeg\"> and an animated GIF "
    "<img src=\"/fx/gif-anim-slow.gif\" alt=\"gif\"></p>"
    "<p>WebP, lossy <img src=\"/fx/webp-lossy-big.webp\" alt=\"webp\"> and animated <img src=\"/fx/webp-anim.webp\" alt=\"webp anim\"></p>"
    "<p>A progressive JPEG <img src=\"/fx/jpeg-progressive.jpg\" alt=\"progressive\"> and a broken one <img src=\"/broken.jpg\" alt=\"(broken)\"></p>",
)
TYPES = {".jpg": "image/jpeg", ".gif": "image/gif", ".webp": "image/webp", ".png": "image/png"}

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
        if path == "/unicode":
            return self.send(200, UNICODE)
        if path == "/images":
            return self.send(200, IMAGES)
        if path == "/broken.jpg":
            return self.send(200, b"\xff\xd8\xff\xe0 not really a jpeg", "image/jpeg")
        if path.startswith("/fx/"):
            name = os.path.basename(path[4:])
            ext = os.path.splitext(name)[1]
            full = os.path.join(FIXTURES, name)
            if ext in TYPES and os.path.isfile(full):
                return self.send(200, open(full, "rb").read(), TYPES[ext])
            return self.send(404, page("Not found", "<h1>404</h1>"))
        if path == "/source":
            return self.send(200, SOURCE)
        if path == "/styled":
            return self.send(200, STYLED)
        if path == "/site.css":
            return self.send(200, CSS, "text/css")
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
