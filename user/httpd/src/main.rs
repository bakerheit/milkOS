//! httpd: serve a directory over HTTP/1.0 (GET and HEAD, one request per connection).
//!   httpd [-p PORT] [-q] [DIR]       defaults: port 80, /srv/www
//! Directories serve their index.html, or a listing. Paths with ".." are refused. -q: no
//! request log. init starts `httpd -q /srv/www` at boot.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use rt::net::{self, SockAddr};
use rt::{eprintln, println, sys};

rt::main!(main);

const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;

fn main(args: &rt::Args) -> i32 {
    let (mut port, mut quiet, mut root) = (80u16, false, "/srv/www");
    let mut it = args.argv.iter().skip(1);
    while let Some(a) = it.next() {
        match *a {
            "-p" => port = it.next().and_then(|p| p.parse().ok()).unwrap_or(80),
            "-q" => quiet = true,
            d if !d.starts_with('-') => root = d,
            _ => {
                eprintln!("usage: httpd [-p PORT] [-q] [DIR]");
                return 2;
            }
        }
    }
    let root = root.trim_end_matches('/');
    let s = match net::socket(net::SOCK_STREAM) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("httpd: socket: {e}");
            return 1;
        }
    };
    let _ = net::set_reuseaddr(s);
    if let Err(e) = net::bind(s, &SockAddr { ip: [0; 4], port }) {
        eprintln!("httpd: bind :{port}: {e}");
        return 1;
    }
    if let Err(e) = net::listen(s, 8) {
        eprintln!("httpd: listen: {e}");
        return 1;
    }
    if !quiet {
        println!("httpd: serving {root} on :{port}");
    }
    loop {
        let (c, peer) = match net::accept(s) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("httpd: accept: {e}");
                sys::sleep_ms(100);
                continue;
            }
        };
        let _ = net::set_recv_timeout(c, 5000);
        let (status, line, bytes) = serve(c, root);
        if !quiet {
            println!("httpd: {peer} \"{line}\" {status} {bytes}");
        }
        let _ = sys::close(c);
    }
}

fn read_request(c: i32) -> Option<Vec<u8>> {
    let mut req = Vec::new();
    let mut buf = [0u8; 1024];
    while !req.windows(4).any(|w| w == b"\r\n\r\n") && !req.windows(2).any(|w| w == b"\n\n") {
        match net::recv(c, &mut buf) {
            Ok(0) | Err(_) => return None,
            Ok(n) => req.extend_from_slice(&buf[..n]),
        }
        if req.len() > 8192 {
            return None;
        }
    }
    Some(req)
}

fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "html" | "htm" => "text/html; charset=utf-8",
        "txt" | "md" => "text/plain; charset=utf-8",
        "css" => "text/css",
        "js" => "text/javascript",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// %XX decoding (no '+': that is only for form data).
fn decode(p: &str) -> Option<String> {
    let b = p.as_bytes();
    let mut out = Vec::new();
    let mut k = 0;
    while k < b.len() {
        if b[k] == b'%' && k + 2 < b.len() {
            let h = core::str::from_utf8(&b[k + 1..k + 3]).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            k += 3;
        } else {
            out.push(b[k]);
            k += 1;
        }
    }
    String::from_utf8(out).ok()
}

fn respond(c: i32, status: &str, ctype: &str, body: &[u8], head: bool) -> usize {
    let h = alloc::format!(
        "HTTP/1.0 {status}\r\nServer: milkOS-httpd (user)\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = send_all(c, h.as_bytes());
    if !head {
        let _ = send_all(c, body);
    }
    body.len()
}

fn send_all(c: i32, mut b: &[u8]) -> Result<(), rt::Errno> {
    while !b.is_empty() {
        let n = net::send(c, b)?;
        b = &b[n..];
    }
    Ok(())
}

fn error(c: i32, status: &str, head: bool) -> (String, usize) {
    let body = alloc::format!("{status}\n");
    let n = respond(c, status, "text/plain", body.as_bytes(), head);
    (String::from(status), n)
}

/// Returns (status, request line, body bytes).
fn serve(c: i32, root: &str) -> (String, String, usize) {
    let Some(req) = read_request(c) else {
        return (String::from("-"), String::from("(no request)"), 0);
    };
    let line = String::from_utf8_lossy(req.split(|&b| b == b'\n').next().unwrap_or(&[]))
        .trim_end()
        .into();
    let mut parts = req.split(|&b| b == b' ');
    let method = parts.next().unwrap_or(&[]);
    let target = core::str::from_utf8(parts.next().unwrap_or(&[])).unwrap_or("");
    let head = method == b"HEAD";
    if method != b"GET" && !head {
        let (s, n) = error(c, "405 Method Not Allowed", false);
        return (s, line, n);
    }
    let path = target.split(['?', '#']).next().unwrap_or("/");
    let Some(path) = decode(path) else {
        let (s, n) = error(c, "400 Bad Request", head);
        return (s, line, n);
    };
    if !path.starts_with('/') || path.split('/').any(|p| p == "..") {
        let (s, n) = error(c, "403 Forbidden", head);
        return (s, line, n);
    }
    let full = alloc::format!("{root}{path}");
    let fd = match sys::open(&full, sys::O_RDONLY | sys::O_CLOEXEC) {
        Ok(fd) => fd,
        Err(_) => {
            let (s, n) = error(c, "404 Not Found", head);
            return (s, line, n);
        }
    };
    let (mode, _) = sys::fstat(fd).unwrap_or((0, 0));
    let _ = sys::close(fd);
    if mode & S_IFMT == S_IFDIR {
        if !path.ends_with('/') {
            let loc = alloc::format!(
                "HTTP/1.0 301 Moved Permanently\r\nLocation: {path}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
            let _ = send_all(c, loc.as_bytes());
            return (String::from("301 Moved Permanently"), line, 0);
        }
        let index = alloc::format!("{full}index.html");
        if let Ok(b) = sys::read_file(&index) {
            let n = respond(c, "200 OK", "text/html; charset=utf-8", &b, head);
            return (String::from("200 OK"), line, n);
        }
        let mut entries = sys::read_dir(&full).unwrap_or_default();
        entries.sort();
        let mut html = alloc::format!(
            "<!doctype html>\n<html><head><title>Index of {path}</title></head><body>\n<h1>Index of {path}</h1>\n<ul>\n"
        );
        for (name, kind) in entries {
            if name == "." || name == ".." {
                continue;
            }
            let slash = if kind == 4 { "/" } else { "" };
            html.push_str(&alloc::format!(
                "<li><a href=\"{name}{slash}\">{name}{slash}</a></li>\n"
            ));
        }
        html.push_str("</ul></body></html>\n");
        let n = respond(
            c,
            "200 OK",
            "text/html; charset=utf-8",
            html.as_bytes(),
            head,
        );
        return (String::from("200 OK"), line, n);
    }
    match sys::read_file(&full) {
        Ok(b) => {
            let n = respond(c, "200 OK", content_type(&full), &b, head);
            (String::from("200 OK"), line, n)
        }
        Err(_) => {
            let (s, n) = error(c, "500 Internal Server Error", head);
            (s, line, n)
        }
    }
}
