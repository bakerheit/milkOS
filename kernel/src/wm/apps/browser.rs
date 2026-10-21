//! Browser: HTTP/1.0 GET over the kernel socket API (docs/networking.md) and a small HTML
//! subset (h1-h3, p, a, ul/li, pre, b/strong, i/em, br, title) laid out in the milk style,
//! with an address bar, back / forward / reload and clickable links.
//!
//! Network safety: it only ever connects to IP literals or `localhost` (127.0.0.1); any
//! other host name is refused before a socket is made (the kernel resolver refuses them too).
//! Fetches run on their own kernel thread so the window system never blocks on the network.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use crate::gfx::{Color, Rect};
use crate::net::errno::{EALREADY, EINPROGRESS};
use crate::net::socket::{self, POLLIN, POLLOUT, SockType};
use crate::net::wire::{Ipv4, SockAddr};
use crate::sched;
use crate::sync::SpinLock;

use super::super::event::Key;
use super::super::fontx;
use super::super::icons::{self, Icon};
use super::super::paint::{Painter, hit};
use super::super::theme::*;
use super::super::window::{KeyInput, PointerInput, PointerKind, WindowContents};

pub const HOME: &str = "http://127.0.0.1/";

/// An address someone asked to open (`open http://...`); the Browser window picks it up.
static PENDING: SpinLock<Option<String>> = SpinLock::new(None);

pub fn open_url(url: &str) {
    *PENDING.lock() = Some(String::from(url));
}
const BAR_H: i32 = 44;
const PAD: i32 = 24;
const TIMEOUT_NS: u64 = 5_000_000_000;
const MAX_BODY: usize = 512 * 1024;

// ---- URLs ---------------------------------------------------------------------------------

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Url {
    pub host: String,
    pub ip: Ipv4,
    pub port: u16,
    pub path: String,
}

impl Url {
    pub fn to_text(&self) -> String {
        if self.port == 80 {
            format!("http://{}{}", self.host, self.path)
        } else {
            format!("http://{}:{}{}", self.host, self.port, self.path)
        }
    }
}

fn parse_ip(s: &str) -> Option<Ipv4> {
    let mut o = [0u8; 4];
    let mut n = 0;
    for part in s.split('.') {
        if n == 4 || part.is_empty() || part.len() > 3 {
            return None;
        }
        o[n] = part.parse().ok()?;
        n += 1;
    }
    (n == 4).then_some(Ipv4(o))
}

/// Parse `http://host[:port]/path` (the scheme is optional). Only IP literals and
/// `localhost` are accepted: that is the browser's whole network policy.
pub fn parse_url(s: &str) -> Result<Url, String> {
    let s = s.trim();
    let rest = if let Some(r) = s.strip_prefix("http://") {
        r
    } else if s.starts_with("https://") {
        return Err(String::from("https isn't supported (no TLS in milkOS yet)"));
    } else if s.contains("://") {
        return Err(String::from("only http:// addresses work here"));
    } else {
        s
    };
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h, p.parse::<u16>().map_err(|_| format!("bad port '{p}'"))?),
        None => (hostport, 80),
    };
    let ip = if host.eq_ignore_ascii_case("localhost") {
        Ipv4([127, 0, 0, 1])
    } else {
        parse_ip(host).ok_or_else(|| {
            format!("'{host}' is a name. This browser only visits IP addresses and localhost.")
        })?
    };
    Ok(Url {
        host: String::from(host),
        ip,
        port,
        path: String::from(if path.is_empty() { "/" } else { path }),
    })
}

/// Resolve a link against the page it is on.
fn join(base: &Url, href: &str) -> Result<Url, String> {
    let href = href.trim();
    if href.starts_with("http://") || href.contains("://") {
        return parse_url(href);
    }
    let mut u = base.clone();
    let href = href.split('#').next().unwrap_or("");
    if href.is_empty() {
        return Ok(u);
    }
    if href.starts_with('/') {
        u.path = String::from(href);
    } else {
        let dir = match base.path.rfind('/') {
            Some(i) => &base.path[..=i],
            None => "/",
        };
        u.path = format!("{dir}{href}");
    }
    Ok(u)
}

// ---- HTTP -----------------------------------------------------------------------------------

pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub url: Url,
}

fn get_once(url: &Url) -> Result<(u16, Vec<(String, String)>, Vec<u8>), String> {
    let s = socket::socket(SockType::Stream).map_err(|e| format!("socket: {e}"))?;
    let close = |s| {
        let _ = socket::close(s);
    };
    let addr = SockAddr::new(url.ip, url.port);
    loop {
        match socket::connect(s, addr) {
            Ok(()) => break,
            Err(e) if e == EINPROGRESS || e == EALREADY => {
                if socket::wait(s, POLLOUT, TIMEOUT_NS) == 0 {
                    close(s);
                    return Err(format!("{}: timed out connecting", addr));
                }
            }
            Err(e) => {
                close(s);
                return Err(format!(
                    "can't connect to {addr}: {}",
                    crate::net::errno::name(e)
                ));
            }
        }
    }
    let req = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: milkOS-Browser/1.0\r\nAccept: text/html, text/plain\r\nConnection: close\r\n\r\n",
        url.path, url.host
    );
    let mut sent = 0;
    let bytes = req.as_bytes();
    while sent < bytes.len() {
        match socket::send(s, &bytes[sent..]) {
            Ok(n) => sent += n,
            Err(e) if e == crate::net::errno::EAGAIN => {
                socket::wait(s, POLLOUT, TIMEOUT_NS);
            }
            Err(e) => {
                close(s);
                return Err(format!("send: {}", crate::net::errno::name(e)));
            }
        }
    }
    let mut data = Vec::new();
    let mut buf = alloc::vec![0u8; 4096];
    loop {
        match socket::recv(s, &mut buf) {
            Ok(0) => break,
            Ok(n) => {
                data.extend_from_slice(&buf[..n]);
                if data.len() > MAX_BODY || complete(&data) {
                    break;
                }
            }
            Err(e) if e == crate::net::errno::EAGAIN => {
                if socket::wait(s, POLLIN, TIMEOUT_NS) == 0 {
                    break;
                }
            }
            Err(e) => {
                close(s);
                return Err(format!("recv: {}", crate::net::errno::name(e)));
            }
        }
    }
    close(s);
    let split = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| String::from("not an HTTP response"))?;
    let head = String::from_utf8_lossy(&data[..split]).into_owned();
    let body = data[split + 4..].to_vec();
    let mut lines = head.lines();
    let status_line = lines.next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .ok_or_else(|| format!("bad status line '{status_line}'"))?;
    let headers = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), String::from(v.trim())))
        .collect();
    Ok((status, headers, body))
}

/// Have we got the whole response? (Headers plus Content-Length bytes, when the server says
/// how long it is; otherwise we read until the server closes.)
fn complete(data: &[u8]) -> bool {
    let Some(split) = data.windows(4).position(|w| w == b"\r\n\r\n") else {
        return false;
    };
    let head = String::from_utf8_lossy(&data[..split]).to_ascii_lowercase();
    head.lines()
        .find_map(|l| l.strip_prefix("content-length:"))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .is_some_and(|len| data.len() >= split + 4 + len)
}

/// GET `url`, following up to 3 redirects. Thread context (sleeps on the socket).
pub fn fetch(url: &Url) -> Result<Response, String> {
    let mut url = url.clone();
    for _ in 0..4 {
        let (status, headers, body) = get_once(&url)?;
        let header = |k: &str| headers.iter().find(|(h, _)| h == k).map(|(_, v)| v.clone());
        if (300..400).contains(&status)
            && let Some(loc) = header("location")
        {
            url = join(&url, &loc)?;
            continue;
        }
        return Ok(Response {
            status,
            content_type: header("content-type").unwrap_or_default(),
            body,
            url,
        });
    }
    Err(String::from("too many redirects"))
}

// ---- HTML -----------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum BlockKind {
    H1,
    H2,
    H3,
    P,
    Li,
    Pre,
}

#[derive(Clone, Debug)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    link: Option<String>,
}

#[derive(Clone, Debug)]
struct Block {
    kind: BlockKind,
    runs: Vec<Run>,
}

struct Doc {
    title: String,
    blocks: Vec<Block>,
}

fn entity(name: &str) -> Option<&'static str> {
    Some(match name {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" | "#39" => "'",
        "nbsp" => " ",
        "middot" => "\u{b7}",
        "copy" => "\u{a9}",
        "mdash" | "ndash" => "-",
        "hellip" => "...",
        _ => return None,
    })
}

fn decode(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find(';').filter(|&j| j <= 8) {
            Some(j) => {
                let name = &after[..j];
                if let Some(r) = entity(name) {
                    out.push_str(r);
                } else if let Some(num) = name.strip_prefix('#') {
                    let v = match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                        Some(h) => u32::from_str_radix(h, 16).ok(),
                        None => num.parse().ok(),
                    };
                    let c = v.and_then(char::from_u32).unwrap_or('?');
                    out.push(match c as u32 {
                        0x2013 | 0x2014 => '-',
                        0x2018 | 0x2019 => '\'',
                        0x201c | 0x201d => '"',
                        _ => c,
                    });
                } else {
                    out.push('&');
                    out.push_str(name);
                    out.push(';');
                }
                rest = &after[j + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let i = lower.find(&format!("{name}="))?;
    let v = &tag[i + name.len() + 1..];
    let v = v.trim_start();
    let val = if let Some(r) = v.strip_prefix('"') {
        r.split('"').next()?
    } else if let Some(r) = v.strip_prefix('\'') {
        r.split('\'').next()?
    } else {
        v.split(|c: char| c.is_whitespace() || c == '>').next()?
    };
    Some(decode(val))
}

/// Parse the subset. Unknown tags are ignored (their text is kept); script and style are
/// dropped; whitespace collapses except inside pre.
fn parse_html(src: &str) -> Doc {
    let mut doc = Doc {
        title: String::new(),
        blocks: Vec::new(),
    };
    let mut cur = Block {
        kind: BlockKind::P,
        runs: Vec::new(),
    };
    let (mut bold, mut italic, mut pre, mut in_title, mut skip) = (0, 0, false, false, false);
    let mut link: Option<String> = None;
    let flush = |doc: &mut Doc, cur: &mut Block, next: BlockKind| {
        let has_text = cur.runs.iter().any(|r| !r.text.trim().is_empty());
        if has_text {
            doc.blocks.push(core::mem::replace(
                cur,
                Block {
                    kind: next,
                    runs: Vec::new(),
                },
            ));
        } else {
            cur.runs.clear();
            cur.kind = next;
        }
    };
    let mut rest = src;
    while !rest.is_empty() {
        if let Some(stripped) = rest.strip_prefix("<!--") {
            rest = stripped.find("-->").map_or("", |i| &stripped[i + 3..]);
            continue;
        }
        if rest.starts_with('<') {
            let end = rest.find('>').unwrap_or(rest.len() - 1);
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            let closing = tag.starts_with('/');
            let name: String = tag
                .trim_start_matches('/')
                .split(|c: char| c.is_whitespace() || c == '/')
                .next()
                .unwrap_or("")
                .to_ascii_lowercase();
            match (name.as_str(), closing) {
                ("script" | "style", false) => skip = true,
                ("script" | "style", true) => skip = false,
                ("title", false) => in_title = true,
                ("title", true) => in_title = false,
                ("h1", false) => flush(&mut doc, &mut cur, BlockKind::H1),
                ("h2", false) => flush(&mut doc, &mut cur, BlockKind::H2),
                ("h3" | "h4" | "h5" | "h6", false) => flush(&mut doc, &mut cur, BlockKind::H3),
                ("li", false) => flush(&mut doc, &mut cur, BlockKind::Li),
                ("pre", false) => {
                    flush(&mut doc, &mut cur, BlockKind::Pre);
                    pre = true;
                }
                ("pre", true) => {
                    pre = false;
                    flush(&mut doc, &mut cur, BlockKind::P);
                }
                (
                    "p" | "div" | "ul" | "ol" | "body" | "section" | "article" | "table" | "tr",
                    _,
                )
                | ("h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "li", true) => {
                    flush(&mut doc, &mut cur, BlockKind::P)
                }
                ("br", _) => cur.runs.push(Run {
                    text: String::from("\n"),
                    bold: false,
                    italic: false,
                    link: None,
                }),
                ("b" | "strong", false) => bold += 1,
                ("b" | "strong", true) => bold = (bold - 1).max(0),
                ("i" | "em", false) => italic += 1,
                ("i" | "em", true) => italic = (italic - 1).max(0),
                ("a", false) => link = attr(tag, "href"),
                ("a", true) => link = None,
                _ => {}
            }
            continue;
        }
        let end = rest.find('<').unwrap_or(rest.len());
        let raw = &rest[..end];
        rest = &rest[end..];
        if skip {
            continue;
        }
        let text = decode(raw);
        if in_title {
            doc.title.push_str(text.trim());
            continue;
        }
        let text = if pre {
            text
        } else {
            let mut t = String::new();
            let mut space = false;
            for c in text.chars() {
                if c.is_whitespace() {
                    space = true;
                } else {
                    if space {
                        t.push(' ');
                        space = false;
                    }
                    t.push(c);
                }
            }
            if space {
                t.push(' ');
            }
            t
        };
        if text.is_empty() {
            continue;
        }
        cur.runs.push(Run {
            text,
            bold: bold > 0,
            italic: italic > 0,
            link: link.clone(),
        });
    }
    flush(&mut doc, &mut cur, BlockKind::P);
    doc
}

fn plain_doc(text: &str) -> Doc {
    Doc {
        title: String::new(),
        blocks: alloc::vec![Block {
            kind: BlockKind::Pre,
            runs: alloc::vec![Run {
                text: String::from(text),
                bold: false,
                italic: false,
                link: None,
            }],
        }],
    }
}

fn message_doc(title: &str, lines: &[&str]) -> Doc {
    let mut blocks = alloc::vec![Block {
        kind: BlockKind::H2,
        runs: alloc::vec![Run {
            text: String::from(title),
            bold: true,
            italic: false,
            link: None,
        }],
    }];
    for l in lines {
        blocks.push(Block {
            kind: BlockKind::P,
            runs: alloc::vec![Run {
                text: String::from(*l),
                bold: false,
                italic: false,
                link: None,
            }],
        });
    }
    Doc {
        title: String::from(title),
        blocks,
    }
}

// ---- the window -----------------------------------------------------------------------------

enum Load {
    Idle,
    Loading(Arc<SpinLock<Option<Result<Response, String>>>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Ctl {
    Back,
    Forward,
    Reload,
    Address,
}

pub struct Browser {
    history: Vec<String>,
    pos: usize,
    url: Option<Url>,
    address: String,
    editing: bool,
    doc: Doc,
    title: String,
    load: Load,
    scroll: i32,
    content_h: i32,
    view_h: i32,
    links: Vec<(Rect, String)>,
    ctls: Vec<(Rect, Ctl)>,
}

impl Browser {
    pub fn new() -> Browser {
        let mut b = Browser {
            history: Vec::new(),
            pos: 0,
            url: None,
            address: String::new(),
            editing: false,
            doc: message_doc("Browser", &[]),
            title: String::from("Browser"),
            load: Load::Idle,
            scroll: 0,
            content_h: 0,
            view_h: 0,
            links: Vec::new(),
            ctls: Vec::new(),
        };
        let start = PENDING.lock().take();
        b.navigate(start.as_deref().unwrap_or(HOME), true);
        b
    }

    /// Go to `text`; `push`: a new history entry (not back / forward / reload).
    fn navigate(&mut self, text: &str, push: bool) {
        self.editing = false;
        let url = match parse_url(text) {
            Ok(u) => u,
            Err(e) => {
                self.address = String::from(text);
                self.show(message_doc("Can't go there", &[&e]));
                return;
            }
        };
        self.address = url.to_text();
        if push {
            self.history
                .truncate(self.pos + usize::from(!self.history.is_empty()));
            self.history.push(self.address.clone());
            self.pos = self.history.len() - 1;
        }
        let slot = Arc::new(SpinLock::new(None));
        let out = slot.clone();
        let u = url.clone();
        drop(sched::spawn("browser-get", move || {
            let r = fetch(&u);
            *out.lock() = Some(r);
            super::super::request(super::super::Request::Wake);
        }));
        self.url = Some(url);
        self.load = Load::Loading(slot);
        self.title = String::from("Loading...");
    }

    fn show(&mut self, doc: Doc) {
        self.title = if doc.title.is_empty() {
            self.url
                .as_ref()
                .map_or(String::from("Browser"), |u| u.host.clone())
        } else {
            doc.title.clone()
        };
        self.doc = doc;
        self.scroll = 0;
    }

    fn finish(&mut self, r: Result<Response, String>) {
        self.load = Load::Idle;
        match r {
            Err(e) => self.show(message_doc("Couldn't load the page", &[&e])),
            Ok(resp) => {
                // A redirect may have moved us.
                let shown = resp.url.to_text();
                if shown != self.address {
                    self.address = shown.clone();
                    if let Some(h) = self.history.get_mut(self.pos) {
                        *h = shown;
                    }
                }
                self.url = Some(resp.url.clone());
                let text = String::from_utf8_lossy(&resp.body).into_owned();
                let html = resp.content_type.contains("html")
                    || (resp.content_type.is_empty() && text.trim_start().starts_with('<'));
                let mut doc = if html {
                    parse_html(&text)
                } else {
                    plain_doc(&text)
                };
                if resp.status >= 400 && doc.title.is_empty() {
                    doc.title = format!("{} error", resp.status);
                }
                self.show(doc);
            }
        }
    }

    fn go_back(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
            let u = self.history[self.pos].clone();
            self.navigate(&u, false);
        }
    }

    fn go_forward(&mut self) {
        if self.pos + 1 < self.history.len() {
            self.pos += 1;
            let u = self.history[self.pos].clone();
            self.navigate(&u, false);
        }
    }

    fn reload(&mut self) {
        let u = self.address.clone();
        self.navigate(&u, false);
    }

    fn draw_bar(&mut self, p: &mut Painter, w: i32) {
        p.fill(0, 0, w, BAR_H, CREAM);
        p.fill(0, BAR_H - 1, w, 1, SAND);
        self.ctls.clear();
        let can_back = self.pos > 0;
        let can_fwd = self.pos + 1 < self.history.len();
        for (i, (label, ctl, on)) in [
            ("<", Ctl::Back, can_back),
            (">", Ctl::Forward, can_fwd),
            ("o", Ctl::Reload, true),
        ]
        .into_iter()
        .enumerate()
        {
            let r = Rect::new(10 + i as i32 * 34, 8, 28, 28);
            p.rrect_r(r, R_BUTTON, OAT, 255);
            let c = if on { ESPRESSO } else { LATTE };
            if ctl == Ctl::Reload {
                p.ring(r.x + 8, r.y + 8, 12, 2, c, 255);
                p.fill(r.x + 17, r.y + 6, 4, 4, OAT);
            } else {
                p.text_center(r.x, r.w, r.y + 6, label, c, true);
            }
            self.ctls.push((r, ctl));
        }
        let ar = Rect::new(116, 8, w - 116 - 12, 28);
        p.rrect_r(ar, 14, MILK, 255);
        p.rrect_stroke(
            ar.x,
            ar.y,
            ar.w,
            ar.h,
            14,
            if self.editing { 2 } else { 1 },
            if self.editing { HONEY } else { SAND },
            255,
        );
        icons::draw(
            p,
            ar.x + 6,
            ar.y + 4,
            20,
            Icon::Globe,
            CREAM,
            255,
            COCOA,
            false,
        );
        let shown = if self.editing {
            self.address.clone()
        } else {
            match &self.load {
                Load::Loading(_) => format!("{}   loading...", self.address),
                Load::Idle => self.address.clone(),
            }
        };
        let max = ((ar.w - 44) / 8).max(4) as usize;
        let n = shown.chars().count();
        let vis: String = if n > max {
            shown.chars().skip(n - max).collect()
        } else {
            shown
        };
        let end = p.text(ar.x + 32, ar.y + 6, &vis, ESPRESSO);
        if self.editing {
            p.fill(end + 1, ar.y + 6, 2, 16, ESPRESSO);
        }
        self.ctls.push((ar, Ctl::Address));
    }

    /// Lay out and (if `p` is given) draw the document; returns its height.
    fn layout(&mut self, mut p: Option<&mut Painter>, w: i32) -> i32 {
        self.links.clear();
        let x0 = PAD;
        let maxw = w - 2 * PAD;
        let mut y = 20 - self.scroll;
        for b in &self.doc.blocks {
            let (scale, bold_all, gap, color, indent) = match b.kind {
                BlockKind::H1 => (2, true, 14, ESPRESSO, 0),
                BlockKind::H2 => (2, false, 12, ESPRESSO, 0),
                BlockKind::H3 => (1, true, 10, ESPRESSO, 0),
                BlockKind::P => (1, false, 12, ESPRESSO, 0),
                BlockKind::Li => (1, false, 6, ESPRESSO, 18),
                BlockKind::Pre => (1, false, 12, ESPRESSO, 12),
            };
            let line_h = 16 * scale + 4;
            if b.kind == BlockKind::Li
                && let Some(p) = p.as_deref_mut()
            {
                p.circle(x0 + 4, y + 6, 6, HONEY, 255);
            }
            if b.kind == BlockKind::Pre {
                // Monospace, as is, on an oat card.
                let text: String = b.runs.iter().map(|r| r.text.as_str()).collect();
                let text = text.trim_matches('\n');
                let lines: Vec<&str> = text.lines().collect();
                let h = lines.len() as i32 * 18 + 16;
                if let Some(p) = p.as_deref_mut() {
                    p.rrect(x0, y, maxw, h, R_ROW, OAT, 255);
                    let mut q = p.clipped(Rect::new(x0, y, maxw, h));
                    for (k, l) in lines.iter().enumerate() {
                        q.text(x0 + indent, y + 8 + k as i32 * 18, l, ESPRESSO);
                    }
                }
                y += h + gap;
                continue;
            }
            // Words with their style, wrapped greedily.
            let mut x = x0 + indent;
            let mut words: Vec<(String, &Run)> = Vec::new();
            for r in &b.runs {
                if r.text == "\n" {
                    words.push((String::from("\n"), r));
                    continue;
                }
                let mut first = true;
                for wd in r.text.split(' ') {
                    if !first {
                        words.push((String::from(" "), r));
                    }
                    first = false;
                    if !wd.is_empty() {
                        words.push((String::from(wd), r));
                    }
                }
            }
            let width = |s: &str, bold: bool| {
                if scale == 1 {
                    fontx::prop_width(s, bold)
                } else {
                    crate::font::str_width(s, scale)
                }
            };
            for (wd, r) in &words {
                if wd == "\n" {
                    x = x0 + indent;
                    y += line_h;
                    continue;
                }
                let bold = bold_all || r.bold;
                if wd == " " {
                    if x > x0 + indent {
                        x += if scale == 1 { 5 } else { 16 };
                    }
                    continue;
                }
                let ww = width(wd, bold);
                if x + ww > x0 + maxw && x > x0 + indent {
                    x = x0 + indent;
                    y += line_h;
                }
                let c: Color = if r.link.is_some() {
                    BLUE
                } else if r.italic {
                    COCOA
                } else {
                    color
                };
                if let Some(p) = p.as_deref_mut()
                    && y + line_h > 0
                    && y < self.view_h
                {
                    if scale == 1 {
                        p.text_p(x, y, wd, c, bold);
                    } else {
                        p.text_x(x, y, wd, c, scale, bold);
                    }
                    if r.link.is_some() {
                        p.fill(x, y + 16 * scale, ww, 1, BLUE);
                    }
                }
                if let Some(href) = &r.link {
                    self.links
                        .push((Rect::new(x, y, ww + 5, line_h), href.clone()));
                }
                x += ww;
            }
            y += line_h + gap;
        }
        y + self.scroll
    }
}

impl WindowContents for Browser {
    fn title(&self) -> &str {
        &self.title
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "View", "History", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.view_h = h - BAR_H;
        {
            let mut page = p.sub(Rect::new(0, BAR_H, w, h - BAR_H));
            page.fill(0, 0, w, h - BAR_H, MILK);
            self.content_h = self.layout(Some(&mut page), w);
        }
        // Link hit boxes were laid out in page coordinates.
        for (r, _) in &mut self.links {
            r.y += BAR_H;
        }
        self.draw_bar(p, w);
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        match e.kind {
            PointerKind::Down(_) => {
                if let Some(&(_, c)) = self.ctls.iter().find(|(r, _)| hit(r, e.x, e.y)) {
                    match c {
                        Ctl::Back => self.go_back(),
                        Ctl::Forward => self.go_forward(),
                        Ctl::Reload => self.reload(),
                        Ctl::Address => self.editing = true,
                    }
                    return true;
                }
                self.editing = false;
                if let Some((_, href)) = self.links.iter().find(|(r, _)| hit(r, e.x, e.y)) {
                    let href = href.clone();
                    let target = match &self.url {
                        Some(base) => join(base, &href).map(|u| u.to_text()),
                        None => Ok(href.clone()),
                    };
                    match target {
                        Ok(t) => self.navigate(&t, true),
                        Err(err) => self.show(message_doc("Can't go there", &[&err])),
                    }
                }
                true
            }
            _ => false,
        }
    }

    fn scroll(&mut self, dy: i32) -> bool {
        let max = (self.content_h - self.view_h + 20).max(0);
        let s = (self.scroll + dy * 40).clamp(0, max);
        if s != self.scroll {
            self.scroll = s;
            return true;
        }
        false
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        if (k.mods.meta || k.mods.ctrl) && k.key == Key::Char('l') {
            self.editing = true;
            return true;
        }
        if self.editing {
            match k.key {
                Key::Enter => {
                    let a = self.address.clone();
                    self.navigate(&a, true);
                }
                Key::Escape => {
                    self.editing = false;
                    self.address = self.url.as_ref().map_or(String::new(), |u| u.to_text());
                }
                Key::Backspace => {
                    self.address.pop();
                }
                _ => match k.ch {
                    Some(c) if !c.is_control() && self.address.len() < 200 => self.address.push(c),
                    _ => return false,
                },
            }
            return true;
        }
        match k.key {
            Key::Down => self.scroll(1),
            Key::Up => self.scroll(-1),
            Key::PageDown | Key::Char(' ') => self.scroll(6),
            Key::PageUp => self.scroll(-6),
            Key::Left if k.mods.meta => {
                self.go_back();
                true
            }
            Key::Right if k.mods.meta => {
                self.go_forward();
                true
            }
            Key::Backspace => {
                self.go_back();
                true
            }
            _ => false,
        }
    }

    fn tick(&mut self, _now: u64) -> bool {
        let asked = PENDING.lock().take();
        if let Some(u) = asked {
            self.navigate(&u, true);
            return true;
        }
        let done = match &self.load {
            Load::Loading(slot) => slot.lock().take(),
            Load::Idle => None,
        };
        match done {
            Some(r) => {
                self.finish(r);
                true
            }
            None => false,
        }
    }
}
