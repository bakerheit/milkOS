//! Browser: HTTP/1.1 GET through milkweb's client (redirects, chunked, gzip) over kernel
//! sockets (`net::web`), and milkweb's HTML subset (headings, paragraphs, lists, quotes,
//! tables as rows, pre, rules, links, bold / italic, `<img alt>`; entities; UTF-8 or Latin-1)
//! laid out in the milk style, with an address bar, back / forward / reload and links.
//!
//! Network policy (the boot's, `net::internet`): with the internet on it visits host names
//! (looked up by `net::dns`), and a bare word in the address bar becomes a search on
//! DuckDuckGo's lite page; offline (every test boot) it only visits IP literals and
//! localhost, refused before a socket is made. `https://` says secure pages aren't supported
//! yet (no TLS), before any lookup. Fetches run on their own kernel thread so the window
//! system never blocks on the network.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use milkweb::html::{self, Block, BlockKind, Doc, Run};
use milkweb::http::{self, Error, Failure, Options, Response};
use milkweb::url::{self, Url};

use crate::gfx::{Color, Rect};
use crate::net::{internet, web::KernelNet};
use crate::sched;
use crate::sync::SpinLock;
use crate::sysfont::{Font, Weight};

use super::super::event::Key;
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
/// The most a page may be, as received and once uncompressed.
const MAX_BODY: usize = 2 * 1024 * 1024;

fn options() -> Options {
    Options {
        max_body: MAX_BODY,
        timeout_ms: 30_000,
        connect_timeout_ms: 8_000,
        ..Options::default()
    }
}

/// GET `url`, following redirects. Thread context (sleeps on the socket).
pub fn fetch(url: &Url) -> Result<Response, Failure> {
    http::get(&mut KernelNet::new(), url, &options())
}

/// What to say under an error's heading.
fn hint(e: &Error) -> &'static str {
    match e {
        Error::Offline(_) => {
            "The internet is off: milkOS was started offline (MILK_OFFLINE=1, or by a test). Numeric addresses like http://10.0.2.2/ still work."
        }
        Error::NoDns(_) => {
            "Is the network up? Settings > Network shows the internet and DNS status."
        }
        Error::NotFound(_) => "Check the spelling of the address.",
        Error::Connect(_) => "The server may be down, or not taking connections on that port.",
        Error::SecureUnsupported => "If the site also has an http:// address, that one will open.",
        Error::Timeout => "Try again (reload), or another site.",
        _ => "",
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
    for l in lines.iter().filter(|l| !l.is_empty()) {
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
    Loading(Arc<SpinLock<Option<Result<Response, Failure>>>>),
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
        let text = text.trim();
        if text.is_empty() {
            return;
        }
        let online = internet::online();
        let target = if url::looks_like_search(text) {
            if !online {
                self.address = String::from(text);
                self.show(message_doc(
                    "Searching needs the internet",
                    &[
                        "milkOS is offline (MILK_OFFLINE=1, or started by a test), so a word can't be searched for.",
                        "Type an address instead, like http://10.0.2.2:8000/.",
                    ],
                ));
                return;
            }
            url::search_url(text)
        } else {
            String::from(text)
        };
        let url = match Url::parse(&target) {
            Ok(u) => u,
            Err(e) => {
                self.address = String::from(text);
                self.show(message_doc("Can't go there", &[&format!("{e}")]));
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
        // Offline, a name is refused here, before any thread or socket.
        if !online && !url.is_ip_literal() && url.host != "localhost" {
            let e = Error::Offline(url.host.clone());
            self.url = Some(url);
            let msg = format!("{e}");
            self.show(message_doc(e.title(), &[&msg, hint(&e)]));
            return;
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

    /// Show where a fetch ended up (after redirects) in the address bar and the history.
    fn moved_to(&mut self, u: &Url) {
        let shown = u.to_text();
        if shown != self.address {
            self.address = shown.clone();
            if let Some(h) = self.history.get_mut(self.pos) {
                *h = shown;
            }
        }
        self.url = Some(u.clone());
    }

    fn finish(&mut self, r: Result<Response, Failure>) {
        self.load = Load::Idle;
        match r {
            Err(f) => {
                self.moved_to(&f.url);
                let msg = format!("{}", f.error);
                crate::kprintln!("[browser] {}: {msg}", f.url);
                self.show(message_doc(f.error.title(), &[&msg, hint(&f.error)]));
            }
            Ok(resp) => {
                self.moved_to(&resp.url);
                let ct = resp.content_type();
                let text = html::decode_bytes(&resp.body, ct);
                let mut doc = if html::is_html(ct, &text) {
                    html::parse(&text)
                } else {
                    html::plain(&text)
                };
                if resp.status >= 400 && doc.title.is_empty() {
                    doc.title = format!("{} {}", resp.status, resp.reason);
                }
                crate::kprintln!(
                    "[browser] {}: {} {}, {} bytes, '{}', {} redirects",
                    resp.url,
                    resp.status,
                    resp.reason,
                    resp.body.len(),
                    doc.title,
                    resp.redirects.len()
                );
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
        let vis = Font::body(false).tail(&shown, ar.w - 44);
        let end = p.text(ar.x + 32, ar.y + 6, vis, ESPRESSO);
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
                BlockKind::Quote => (1, false, 12, COCOA, 18),
                BlockKind::Rule => (1, false, 12, SAND, 0),
            };
            let line_h = 16 * scale + 4;
            if b.kind == BlockKind::Rule {
                if let Some(p) = p.as_deref_mut() {
                    p.fill(x0, y + 4, maxw, 1, SAND);
                }
                y += 9 + gap;
                continue;
            }
            let y_start = y;
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
                        q.mono(x0 + indent, y + 8 + k as i32 * 18, l, ESPRESSO);
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
            let font = |bold: bool| Font::ui(scale, Weight::bold(bold));
            let width = |s: &str, bold: bool| font(bold).width(s);
            for (wd, r) in &words {
                if wd == "\n" {
                    x = x0 + indent;
                    y += line_h;
                    continue;
                }
                let bold = bold_all || r.bold;
                if wd == " " {
                    if x > x0 + indent {
                        x += font(bold).advance(' ');
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
                    p.text_f(x, y, wd, c, font(bold));
                    if r.link.is_some() {
                        p.fill(x, y + font(bold).baseline() + scale + 1, ww, 1, BLUE);
                    }
                }
                if let Some(href) = &r.link {
                    self.links
                        .push((Rect::new(x, y, ww + 5, line_h), href.clone()));
                }
                x += ww;
            }
            if b.kind == BlockKind::Quote
                && let Some(p) = p.as_deref_mut()
            {
                p.fill(x0 + 4, y_start, 3, y - y_start + line_h, SAND);
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
                        Some(base) => base
                            .join(&href)
                            .map(|u| u.to_text())
                            .map_err(|e| format!("{e}")),
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
