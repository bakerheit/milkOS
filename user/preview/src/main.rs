//! Preview: pictures (PNG, JPEG, GIF and WebP, animated ones playing, PPM/PGM/PBM, BMP) and an
//! honest look at PDFs (docs/ui-concepts/
//! SPEC-APPS.md, section 5). The decoding is the `imgdec` crate; this is the window.
//!
//! It reads untrusted files, so it sandboxes itself first: no network, no spawning, no clipboard,
//! and files only from the folder of the picture it was started with (read-only) or what the
//! user hands it through the Open panel, a drop, or Save As.
//!
//! Keys: Left/Up and Right/Down/Space move between the pictures in the folder; Cmd-O open;
//! Cmd-S Save As (PNG); Cmd-0 fit; Cmd-+ / Cmd-- step through the zoom list; Cmd-R / Cmd-Shift-R
//! rotate; Cmd-I info; Esc closes the info panel or the text view; drag to pan.

#![no_std]
#![no_main]

extern crate alloc;

mod draw;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::{Hits, Theme};
use gui::{Event, Key, Mods, Window};
use imgdec::anim::Animation;
use imgdec::pdf::PdfInfo;
use imgdec::{Format, Image};
use rt::sys;

rt::main!(main);

pub const W: i32 = 860;
pub const H: i32 = 660;
pub const BAR: i32 = 40;
pub const STATUS_H: i32 = 24;
pub const STRIP_W: i32 = 150;
pub const THUMB_W: i32 = 118;
pub const THUMB_H: i32 = 66;
pub const THUMB_STEP: i32 = THUMB_H + 36;
/// The biggest file we read.
pub const MAX_FILE: usize = 64 << 20;
/// Thumbnails are only made from files up to this size.
const MAX_THUMB_FILE: u64 = 2 << 20;

pub const ID_PREV: u32 = 1;
pub const ID_NEXT: u32 = 2;
pub const ID_ZOOM: u32 = 10; // 10..=14
pub const ID_ROT: u32 = 20;
pub const ID_INFO: u32 = 21;
pub const ID_SAVE: u32 = 22;
pub const ID_OPEN: u32 = 23;
pub const ID_SHOWTEXT: u32 = 24;
pub const ID_SAVETEXT: u32 = 25;
pub const ID_BACK: u32 = 26;
pub const ID_THUMB: u32 = 100;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Zoom {
    Fit,
    P25,
    P50,
    P100,
    P200,
}

pub const ZOOMS: [Zoom; 5] = [Zoom::Fit, Zoom::P25, Zoom::P50, Zoom::P100, Zoom::P200];

pub enum Content {
    Nothing,
    Picture {
        img: Image,
        fmt: &'static str,
        /// A GIF or WebP with more than one frame: it plays, `img` being the frame on show.
        anim: Option<alloc::boxed::Box<Player>>,
    },
    Pdf {
        info: PdfInfo,
        /// The text view (read-only) is showing.
        text_view: bool,
        scroll: usize,
    },
    Failed(String),
}

/// An animation being played.
pub struct Player {
    pub anim: Animation,
    /// When the next frame is due (monotonic ms).
    pub next_at: u64,
}

pub struct App {
    pub win: Window,
    pub th: Theme,
    pub hits: Hits,
    pub name: String,
    pub path: Option<String>,
    pub size: u64,
    pub mtime: i64,
    /// The pictures next to the open one (full paths), sorted, and which one this is.
    pub files: Vec<String>,
    pub idx: usize,
    pub content: Content,
    pub zoom: Zoom,
    /// Image top-left offset inside the canvas when it is bigger than the canvas.
    pub pan: (i32, i32),
    pub pan_reset: bool,
    pub drag: Option<(i32, i32, (i32, i32))>,
    pub info_open: bool,
    /// The scaled copy for Fit, 25% and 50%: (key, picture).
    pub cache: Option<((Zoom, u32, usize, usize), Image)>,
    /// Bumped when the picture changes (rotation, a new file, a new frame), part of the cache key.
    pub version: u32,
    /// Quarter turns clockwise the user asked for (applied to each new animation frame too).
    pub turns: u8,
    pub thumbs: BTreeMap<usize, Option<Image>>,
    pub strip_scroll: i32,
    /// The picture is bigger than the canvas across / down (set by drawing).
    pub overflow: (bool, bool),
    /// The overview picture for the mini map: (generation, picture).
    pub mini: Option<(u32, Image)>,
    pub msg: Option<(String, u64)>,
    pub quit: bool,
}

pub fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

pub fn wall_secs() -> i64 {
    (sys::clock_ns(sys::CLOCK_REALTIME) / 1_000_000_000) as i64
}

fn read_fd(fd: i32) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match sys::read(fd, &mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > MAX_FILE {
                    return Err(String::from("This file is too big to open (over 64 MB)."));
                }
            }
            Err(e) => return Err(format!("Can't read the file: {}.", e.name())),
        }
    }
}

fn read_path(path: &str) -> Result<Vec<u8>, String> {
    let fd = sys::open(path, sys::O_RDONLY | sys::O_CLOEXEC)
        .map_err(|e| format!("Can't open it: {}.", e.name()))?;
    let r = read_fd(fd);
    let _ = sys::close(fd);
    r
}

fn is_picture_name(n: &str) -> bool {
    let l = n.to_ascii_lowercase();
    [
        "png", "jpg", "jpeg", "jpe", "jfif", "gif", "webp", "ppm", "pgm", "pbm", "pnm", "bmp",
        "pdf",
    ]
    .iter()
    .any(|e| l.ends_with(&format!(".{e}")))
}

fn dir_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => ".",
    }
}

fn base_of(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Decode a file's bytes for display.
fn decode_content(bytes: &[u8]) -> Content {
    match imgdec::sniff(bytes) {
        Format::Pdf => match imgdec::pdf::inspect(bytes) {
            Ok(info) => Content::Pdf {
                info,
                text_view: false,
                scroll: 0,
            },
            Err(e) => Content::Failed(String::from(e.message())),
        },
        f => {
            let fmt = match f {
                Format::Pnm => match bytes.get(1) {
                    Some(b'1' | b'4') => "PBM",
                    Some(b'2' | b'5') => "PGM",
                    _ => "PPM",
                },
                f => f.name(),
            };
            // GIF and WebP may animate: play them.
            if matches!(f, Format::Gif | Format::WebP)
                && let Ok(mut anim) = Animation::new(bytes.to_vec())
                && anim.is_animated()
            {
                return match anim.next_frame() {
                    Some(Ok(fr)) => {
                        let (img, delay) = (fr.image.clone(), fr.delay_ms);
                        Content::Picture {
                            img,
                            fmt,
                            anim: Some(alloc::boxed::Box::new(Player {
                                anim,
                                next_at: now_ms() + delay as u64,
                            })),
                        }
                    }
                    Some(Err(e)) => Content::Failed(String::from(e.message())),
                    None => Content::Failed(String::from("The animation has no frames.")),
                };
            }
            match imgdec::decode(bytes) {
                Ok(img) => Content::Picture {
                    img,
                    fmt,
                    anim: None,
                },
                Err(e) => Content::Failed(String::from(e.message())),
            }
        }
    }
}

impl App {
    pub fn flash(&mut self, s: &str) {
        self.msg = Some((String::from(s), now_ms() + 4000));
    }

    pub fn strip_visible(&self) -> bool {
        self.files.len() > 1
    }

    pub fn canvas(&self) -> (i32, i32, i32, i32) {
        let x = if self.strip_visible() { STRIP_W } else { 0 };
        (x, BAR, self.win.w - x, self.win.h - BAR - STATUS_H)
    }

    fn set_content(&mut self, c: Content) {
        self.content = c;
        self.version += 1;
        self.turns = 0;
        self.cache = None;
        self.pan_reset = true;
        self.drag = None;
        self.info_open = false;
    }

    /// Open the file at `path` (and list its folder).
    fn open_path(&mut self, path: &str) {
        self.name = String::from(base_of(path));
        self.path = Some(String::from(path));
        let st = sys::stat_path(path, true).ok();
        self.size = st.as_ref().map_or(0, |s| s.size);
        self.mtime = st.as_ref().map_or(0, |s| s.mtime);
        let c = match read_path(path) {
            Ok(b) => {
                self.size = b.len() as u64;
                decode_content(&b)
            }
            Err(e) => Content::Failed(e),
        };
        self.set_content(c);
        if self.files.iter().all(|f| f != path) {
            self.list_folder(path);
        }
        self.idx = self.files.iter().position(|f| f == path).unwrap_or(0);
    }

    fn list_folder(&mut self, path: &str) {
        let dir = dir_of(path);
        let mut names: Vec<String> = sys::read_dir(dir)
            .map(|v| {
                v.into_iter()
                    .filter(|(n, kind)| *kind != 4 && is_picture_name(n))
                    .map(|(n, _)| n)
                    .collect()
            })
            .unwrap_or_default();
        names.sort_by_key(|n| n.to_ascii_lowercase());
        let sep = if dir == "/" { "" } else { "/" };
        self.files = names
            .into_iter()
            .map(|n| format!("{dir}{sep}{n}"))
            .collect();
        self.thumbs.clear();
        self.strip_scroll = 0;
    }

    fn open_fd(&mut self, fd: i32) {
        let r = read_fd(fd);
        let _ = sys::close(fd);
        self.path = None;
        self.files.clear();
        self.idx = 0;
        self.name = String::from("Picture");
        self.mtime = 0;
        let c = match r {
            Ok(b) => {
                self.size = b.len() as u64;
                decode_content(&b)
            }
            Err(e) => Content::Failed(e),
        };
        self.set_content(c);
    }

    fn open_panel(&mut self) {
        match sys::powerbox_open() {
            Ok(fd) => self.open_fd(fd),
            Err(sys::Errno(125)) => {}
            Err(e) => self.flash(&format!("Open: {}", e.name())),
        }
    }

    fn step(&mut self, d: isize) {
        if self.files.len() < 2 {
            return;
        }
        let n = self.files.len() as isize;
        let i = (self.idx as isize + d).rem_euclid(n) as usize;
        let p = self.files[i].clone();
        self.open_path(&p);
        self.idx = i;
        self.keep_selected_in_view();
    }

    fn keep_selected_in_view(&mut self) {
        let (_, _, _, ch) = self.canvas();
        let top = self.idx as i32 * THUMB_STEP;
        if top < self.strip_scroll {
            self.strip_scroll = top;
        } else if top + THUMB_STEP > self.strip_scroll + ch {
            self.strip_scroll = top + THUMB_STEP - ch;
        }
    }

    fn save_as(&mut self) {
        match &self.content {
            Content::Picture { img, .. } => {
                let stem = self
                    .name
                    .rsplit_once('.')
                    .map_or(self.name.as_str(), |(a, _)| a);
                let suggested = format!("{stem}.png");
                let bytes = img.to_png();
                match sys::powerbox_save(&suggested) {
                    Ok(fd) => {
                        let r = sys::write_all(fd, &bytes);
                        let _ = sys::close(fd);
                        match r {
                            Ok(()) => self.flash("Saved as PNG"),
                            Err(e) => self.flash(&format!("Save failed: {}", e.name())),
                        }
                    }
                    Err(sys::Errno(125)) => {}
                    Err(e) => self.flash(&format!("Save: {}", e.name())),
                }
            }
            Content::Pdf { info, .. } if !info.text.is_empty() => {
                let stem = self
                    .name
                    .rsplit_once('.')
                    .map_or(self.name.as_str(), |(a, _)| a);
                let text = info.text.clone();
                match sys::powerbox_save(&format!("{stem}.txt")) {
                    Ok(fd) => {
                        let r = sys::write_all(fd, text.as_bytes());
                        let _ = sys::close(fd);
                        match r {
                            Ok(()) => self.flash("Saved the text"),
                            Err(e) => self.flash(&format!("Save failed: {}", e.name())),
                        }
                    }
                    Err(sys::Errno(125)) => {}
                    Err(e) => self.flash(&format!("Save: {}", e.name())),
                }
            }
            _ => self.flash("Nothing to save"),
        }
    }

    fn rotate(&mut self, cw: bool) {
        if let Content::Picture { img, .. } = &mut self.content {
            *img = img.rotated(cw);
            self.turns = (self.turns + if cw { 1 } else { 3 }) % 4;
            self.version += 1;
            self.cache = None;
            self.pan_reset = true;
        }
    }

    fn zoom_step(&mut self, d: isize) {
        let i = ZOOMS.iter().position(|z| *z == self.zoom).unwrap_or(0) as isize;
        // Fit sits at the start of the list; stepping up from it goes to the next real size.
        let j = (i + d).clamp(0, ZOOMS.len() as isize - 1) as usize;
        self.set_zoom(ZOOMS[j]);
    }

    fn set_zoom(&mut self, z: Zoom) {
        if z != self.zoom {
            self.zoom = z;
            self.pan_reset = true;
        }
    }

    fn click(&mut self, x: i32, y: i32) {
        let Some(id) = self.hits.at(x, y) else {
            // Nothing hit: a press on the picture starts a pan.
            let (cx, cy, cw, ch) = self.canvas();
            if x >= cx && x < cx + cw && y >= cy && y < cy + ch {
                self.drag = Some((x, y, self.pan));
            }
            return;
        };
        match id {
            ID_PREV => self.step(-1),
            ID_NEXT => self.step(1),
            ID_ZOOM..=14 => self.set_zoom(ZOOMS[(id - ID_ZOOM) as usize]),
            ID_ROT => self.rotate(true),
            ID_INFO => self.info_open = !self.info_open,
            ID_SAVE | ID_SAVETEXT => self.save_as(),
            ID_OPEN => self.open_panel(),
            ID_SHOWTEXT => {
                if let Content::Pdf {
                    text_view, scroll, ..
                } = &mut self.content
                {
                    *text_view = true;
                    *scroll = 0;
                }
            }
            ID_BACK => {
                if let Content::Pdf { text_view, .. } = &mut self.content {
                    *text_view = false;
                }
            }
            i if i >= ID_THUMB => {
                let k = (i - ID_THUMB) as usize;
                if let Some(p) = self.files.get(k).cloned() {
                    self.open_path(&p);
                }
            }
            _ => {}
        }
    }

    fn key(&mut self, k: Key, m: Mods) {
        let cmd = m.meta || m.ctrl;
        if cmd && let Key::Char(c) = k {
            match c.to_ascii_lowercase() {
                'o' => self.open_panel(),
                's' => self.save_as(),
                '0' => self.set_zoom(Zoom::Fit),
                '+' | '=' => self.zoom_step(1),
                '-' => self.zoom_step(-1),
                'r' => self.rotate(!m.shift),
                'i' => self.info_open = !self.info_open,
                'q' | 'w' => self.quit = true,
                _ => {}
            }
            return;
        }
        match k {
            Key::Left | Key::Up => {
                if let Content::Pdf {
                    text_view: true,
                    scroll,
                    ..
                } = &mut self.content
                {
                    *scroll = scroll.saturating_sub(if k == Key::Up { 1 } else { 10 });
                } else {
                    self.step(-1)
                }
            }
            Key::Right | Key::Down | Key::Char(' ') => {
                if let Content::Pdf {
                    text_view: true,
                    scroll,
                    ..
                } = &mut self.content
                {
                    *scroll += if k == Key::Down { 1 } else { 10 };
                } else {
                    self.step(1)
                }
            }
            Key::Escape => {
                if self.info_open {
                    self.info_open = false;
                } else if let Content::Pdf { text_view, .. } = &mut self.content {
                    *text_view = false;
                }
            }
            _ => {}
        }
    }

    /// Show the animation's next frame if it is due. True if the picture changed.
    fn tick(&mut self, now: u64) -> bool {
        let turns = self.turns;
        let Content::Picture {
            img, anim: Some(p), ..
        } = &mut self.content
        else {
            return false;
        };
        if now < p.next_at {
            return false;
        }
        if p.anim.finished() && p.anim.should_loop() {
            p.anim.rewind();
        }
        match p.anim.next_frame() {
            Some(Ok(fr)) => {
                let mut frame = fr.image.clone();
                p.next_at = now + fr.delay_ms.max(20) as u64;
                for _ in 0..turns {
                    frame = frame.rotated(true);
                }
                *img = frame;
            }
            // The end (it doesn't loop), or a damaged frame: keep showing the last good one.
            _ => {
                p.next_at = u64::MAX;
                return false;
            }
        }
        self.version += 1;
        self.cache = None;
        true
    }

    /// Milliseconds until the next animation frame (at most `max`).
    fn wait_ms(&self, now: u64, max: u64) -> u64 {
        match &self.content {
            Content::Picture { anim: Some(p), .. } => p.next_at.saturating_sub(now).clamp(1, max),
            _ => max,
        }
    }

    /// The wheel works the text view, or pans a picture that is bigger than the canvas, or
    /// scrolls the thumbnail strip.
    fn scroll(&mut self, d: i32) {
        if let Content::Pdf {
            text_view: true,
            scroll,
            ..
        } = &mut self.content
        {
            *scroll = (*scroll as i32 + d * 3).max(0) as usize;
        } else if self.overflow.1 {
            self.pan.1 -= d * 40;
        } else if self.overflow.0 {
            self.pan.0 -= d * 40;
        } else if self.strip_visible() {
            let max = (self.files.len() as i32 * THUMB_STEP - self.canvas().3).max(0);
            self.strip_scroll = (self.strip_scroll + d * 40).clamp(0, max);
        }
    }
}

fn main(args: &rt::Args) -> i32 {
    // Before anything else: lock down what a bad picture could reach.
    let path = args.get(1);
    let spec = match path {
        Some(p) => format!(
            "fs={}:r fs=/dev/wm:r net=none spawn=0 clipboard=0",
            dir_of(p)
        ),
        None => String::from("fs=/dev/wm:r net=none spawn=0 clipboard=0"),
    };
    if let Err(e) = sys::sandbox(&spec) {
        rt::println!("preview: can't sandbox myself: {}", e.name());
        return 1;
    }
    let Ok(win) = Window::new("Preview", W, H) else {
        rt::println!("preview: no desktop to open a window on");
        return 1;
    };
    win.set_resizable(420, 320);
    let mut app = App {
        win,
        th: Theme::current(),
        hits: Hits::default(),
        name: String::from("Preview"),
        path: None,
        size: 0,
        mtime: 0,
        files: Vec::new(),
        idx: 0,
        content: Content::Nothing,
        zoom: Zoom::Fit,
        pan: (0, 0),
        pan_reset: true,
        drag: None,
        info_open: false,
        cache: None,
        version: 0,
        turns: 0,
        thumbs: BTreeMap::new(),
        strip_scroll: 0,
        overflow: (false, false),
        mini: None,
        msg: None,
        quit: false,
    };
    if let Some(p) = path {
        app.open_path(p);
    }
    let mut shown_msg = false;
    app.draw();
    loop {
        let ev = app.win.event(app.wait_ms(now_ms(), 250));
        let mut dirty = app.tick(now_ms());
        match ev {
            Some(Event::Close) => break,
            Some(Event::Resize { w, h }) => {
                let _ = app.win.resize(w, h);
                app.pan_reset = true;
                dirty = true;
            }
            Some(Event::Down { x, y, button: 1 }) => {
                app.click(x, y);
                dirty = true;
            }
            Some(Event::Drag { x, y }) => {
                if let Some((sx, sy, (px, py))) = app.drag {
                    app.pan = (px + x - sx, py + y - sy);
                    dirty = true;
                }
            }
            Some(Event::Up { .. }) => app.drag = None,
            Some(Event::Scroll(d)) => {
                app.scroll(d);
                dirty = true;
            }
            Some(Event::Key(k, m)) => {
                app.key(k, m);
                dirty = true;
            }
            Some(Event::Drop { fd, .. }) => {
                app.open_fd(fd);
                dirty = true;
            }
            _ => {}
        }
        if app.quit {
            break;
        }
        let now = now_ms();
        let msg = app.msg.as_ref().is_some_and(|m| m.1 > now);
        if !msg {
            app.msg = None;
        }
        // Thumbnails still to make: keep drawing (a few per frame).
        if dirty || msg != shown_msg || app.thumbs_pending() {
            shown_msg = msg;
            app.draw();
        }
    }
    0
}

impl App {
    /// Make thumbnails for the strip rows on screen that don't have one, up to a few per call.
    /// True if any are still missing afterwards.
    pub fn thumbs_pending(&mut self) -> bool {
        if !self.strip_visible() {
            return false;
        }
        let (_, _, _, ch) = self.canvas();
        let first = (self.strip_scroll / THUMB_STEP).max(0) as usize;
        let last = (((self.strip_scroll + ch) / THUMB_STEP) as usize + 1).min(self.files.len());
        let mut made = 0;
        let mut missing = false;
        for i in first..last {
            if self.thumbs.contains_key(&i) {
                continue;
            }
            if made >= 2 {
                missing = true;
                continue;
            }
            made += 1;
            let t = self.make_thumb(i);
            self.thumbs.insert(i, t);
        }
        made > 0 || missing
    }

    fn make_thumb(&self, i: usize) -> Option<Image> {
        let p = &self.files[i];
        let st = sys::stat_path(p, true).ok()?;
        if st.size > MAX_THUMB_FILE {
            return None;
        }
        let bytes = read_path(p).ok()?;
        let img = imgdec::decode(&bytes).ok()?;
        let (w, h) = img.fit_size(THUMB_W as usize, THUMB_H as usize);
        let (w, h) = (w.min(img.w).max(1), h.min(img.h).max(1));
        img.scaled(w, h).ok()
    }
}
