#!/usr/bin/env python3
"""Host-side servers for scripts/web-test.sh (python3 stdlib only, run with python3 -I).

Binds ONLY to 127.0.0.1 (the guest reaches it as 10.0.2.2 through qemu's user-mode
networking) and never forwards anything anywhere:
  * a DNS server, UDP and TCP on the same port, for a made-up zone under milk.test:
      www.milk.test   CNAME web.milk.test        web.milk.test  A 10.0.2.2
      big.milk.test   40 A records (too big for UDP: TC, then the guest asks over TCP)
      v6.milk.test    AAAA fec0::2               (no AAAA elsewhere: NODATA with an SOA)
    everything else NXDOMAIN; TTL 60. Every question is logged ("dns udp NAME TYPE").
  * an HTTP/1.1 server:
      /start -> 301 /step2 -> 302 http://web.milk.test:PORT/step3 -> 307 /page
      /page   200, chunked, gzip (when asked for), an HTML page with entities, a table,
              script/style, an image's alt text and a Latin-1 word
      /loop   302 /loop2 -> 302 /loop (a redirect loop)
    Every request is logged with its Host header and User-Agent.
Writes "dns=PORT http=PORT" to argv[1], then serves until killed.
"""
import gzip
import http.server
import socket
import socketserver
import struct
import sys
import threading

HOST = "127.0.0.1"
TTL = 60
ZONE_A = {"web.milk.test": ["10.0.2.2"], "big.milk.test": ["10.0.2.%d" % (100 + k) for k in range(40)]}
ZONE_CNAME = {"www.milk.test": "web.milk.test"}
ZONE_AAAA = {"v6.milk.test": ["fec0::2"]}
KNOWN = set(ZONE_A) | set(ZONE_CNAME) | set(ZONE_AAAA)

PAGE = (
    "<!DOCTYPE html><html><head><meta charset=\"utf-8\"><title>Milk &amp; the Web</title>"
    "<style>body { color: red }</style><script>var x = 1 < 2; document.write('<p>no</p>');</script>"
    "</head><body><h1>milkOS web test page</h1>"
    "<p>Fetched by <b>host name</b> through a redirect chain, chunked and gzipped. "
    "Café &eacute; &mdash; &ldquo;quotes&rdquo; &copy; 2026.</p>"
    "<div>A <span>div</span> and <a href=\"/page#again\">a link</a>.</div>"
    "<table><tr><th>Drink</th><th>Size</th></tr><tr><td>Milk</td><td>1 L</td></tr></table>"
    "<p><img src=\"cow.png\" alt=\"a cow\"> grazing.</p>"
    "<ul><li>first</li><li>second</li></ul></body></html>"
).encode("utf-8")


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


def enc_name(n):
    return b"".join(bytes([len(l)]) + l.encode() for l in n.split(".")) + b"\0"


def rr(name_ptr, rtype, rdata):
    return name_ptr + struct.pack(">HHIH", rtype, 1, TTL, len(rdata)) + rdata


def answer(q):
    """The reply to query q (bytes)."""
    qid, _flags, qd = struct.unpack(">HHH", q[:6])
    name, off = name_at(q, 12)
    qtype = struct.unpack(">H", q[off : off + 2])[0]
    question = q[12 : off + 4]
    answers = []
    rcode = 0
    cur, ptr = name, b"\xc0\x0c"
    if cur in ZONE_CNAME:
        target = ZONE_CNAME[cur]
        answers.append(rr(ptr, 5, enc_name(target)))
        cur, ptr = target, enc_name(target)
    if qtype == 1 and cur in ZONE_A:
        answers += [rr(ptr, 1, socket.inet_aton(a)) for a in ZONE_A[cur]]
    elif qtype == 28 and cur in ZONE_AAAA:
        answers += [rr(ptr, 28, socket.inet_pton(socket.AF_INET6, a)) for a in ZONE_AAAA[cur]]
    authority = []
    if name not in KNOWN:
        rcode = 3
    if not answers or rcode:
        soa = enc_name("ns.milk.test") + enc_name("admin.milk.test") + struct.pack(">IIIII", 1, 60, 60, 60, 30)
        authority.append(rr(b"\xc0\x0c", 6, soa))
    head = struct.pack(">HHHHHH", qid, 0x8180 | rcode, 1, len(answers), len(authority), 0)
    return head + question + b"".join(answers) + b"".join(authority), name, qtype


def log(*a):
    print(*a, flush=True)


def dns_udp(sock):
    while True:
        msg, peer = sock.recvfrom(512)
        try:
            r, name, qtype = answer(msg)
        except Exception as e:  # noqa: BLE001 - a bad packet is logged, not fatal
            log("dns udp bad packet", e)
            continue
        if len(r) > 512:
            # Truncated: the header with TC and no records, then the question.
            qend = 12 + len(msg) - 12
            r = struct.pack(">HHHHHH", struct.unpack(">H", r[:2])[0], 0x8380, 1, 0, 0, 0) + msg[12:qend]
            log("dns udp", name, qtype, "truncated")
        else:
            log("dns udp", name, qtype)
        sock.sendto(r, peer)


class DnsTcp(socketserver.BaseRequestHandler):
    def handle(self):
        data = b""
        while len(data) < 2 or len(data) < 2 + struct.unpack(">H", data[:2])[0]:
            chunk = self.request.recv(4096)
            if not chunk:
                return
            data += chunk
        n = struct.unpack(">H", data[:2])[0]
        r, name, qtype = answer(data[2 : 2 + n])
        log("dns tcp", name, qtype, len(r))
        self.request.sendall(struct.pack(">H", len(r)) + r)


class Web(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def redirect(self, code, where):
        self.send_response(code)
        self.send_header("Location", where)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):
        port = self.server.server_address[1]
        log("http", self.path, "host=" + str(self.headers.get("Host")), "ua=" + str(self.headers.get("User-Agent")))
        if self.path == "/start":
            return self.redirect(301, "/step2")
        if self.path == "/step2":
            return self.redirect(302, "http://web.milk.test:%d/step3" % port)
        if self.path == "/step3":
            return self.redirect(307, "/page")
        if self.path == "/loop":
            return self.redirect(302, "/loop2")
        if self.path == "/loop2":
            return self.redirect(302, "/loop")
        if self.path == "/page":
            body = PAGE
            zipped = "gzip" in (self.headers.get("Accept-Encoding") or "")
            if zipped:
                body = gzip.compress(PAGE, mtime=0)
            self.send_response(200)
            self.send_header("Content-Type", "text/html; charset=utf-8")
            if zipped:
                self.send_header("Content-Encoding", "gzip")
            self.send_header("Transfer-Encoding", "chunked")
            self.end_headers()
            for k in range(0, len(body), 100):
                part = body[k : k + 100]
                self.wfile.write(b"%x\r\n" % len(part) + part + b"\r\n")
            self.wfile.write(b"0\r\n\r\n")
            log("http /page sent", len(body), "bytes", "gzip" if zipped else "plain", "chunked")
            return
        self.send_error(404)

    def log_message(self, fmt, *args):
        pass


class TCPServer(socketserver.ThreadingTCPServer):
    daemon_threads = True
    allow_reuse_address = True


def main():
    # DNS: one port for both UDP and TCP.
    for _ in range(50):
        udp = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        udp.bind((HOST, 0))
        port = udp.getsockname()[1]
        try:
            tcp = TCPServer((HOST, port), DnsTcp)
            break
        except OSError:
            udp.close()
    else:
        sys.exit("no free port for DNS")
    web = TCPServer((HOST, 0), Web)
    threading.Thread(target=dns_udp, args=(udp,), daemon=True).start()
    threading.Thread(target=tcp.serve_forever, daemon=True).start()
    threading.Thread(target=web.serve_forever, daemon=True).start()
    with open(sys.argv[1] + ".tmp", "w") as f:
        f.write("dns=%d http=%d\n" % (port, web.server_address[1]))
    import os
    os.rename(sys.argv[1] + ".tmp", sys.argv[1])
    log("serving: dns 127.0.0.1:%d (udp+tcp), http 127.0.0.1:%d" % (port, web.server_address[1]))
    threading.Event().wait()


if __name__ == "__main__":
    main()
