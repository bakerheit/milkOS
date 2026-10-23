//! Browser: web pages in tabs, in user space (docs/browser.md).
//!
//! Web pages are hostile input, so before anything else it sandboxes itself: the network
//! yes, files only in its own `~/Library/Browser` and `~/Downloads` (anything else through the
//! Powerbox), no spawning. Pages come through milkweb's HTTP client and the TLS 1.3 client
//! (rt::web / rt::tls) on worker threads (net.rs); the rules (cookies, history, bookmarks,
//! forms, find, the built-in pages, painting) are browserkit's, and the page renderer is
//! behind browserkit::engine (user/webengine's interface).
//!
//! Keys (no app menus for client windows yet, so everything is a key or the toolbar's menu):
//! Cmd-L address, Cmd-T new tab, Ctrl-W close it (Cmd-W is the window system's: it closes
//! the window), Ctrl-Tab next tab, Cmd-1..9 a tab, Cmd-R reload, Esc
//! stop, Cmd-[ / Cmd-] back / forward, Cmd-D bookmark, Cmd-B bookmarks, Cmd-Y history, Cmd-J
//! downloads, Cmd-F find (Cmd-G next), Cmd-+ / Cmd-- / Cmd-0 zoom, Space / Page Up / Page
//! Down / Home / End / arrows scroll, Tab moves between form fields, Cmd-click (or the
//! middle button) opens a link in a new tab, Alt-click downloads it, Cmd-Shift-Delete forgets
//! cookies and history.
//!
//! `browser [--log-input] [ADDRESS]`: `--log-input` prints every key, click and wheel event
//! with its modifier keys (`[browser] input ...`), which scripts/browser-test.sh uses to check
//! the window system's client protocol.

#![no_std]
#![no_main]

extern crate alloc;

mod draw;
mod net;
mod store;
mod sysfont;
mod tab;
mod web;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use appkit::{Field, Hits, Theme};
use browserkit::address::{self, Internal, Target};
use browserkit::bookmarks::Bookmarks;
use browserkit::cookies::{Context, Jar, site_of};
use browserkit::engine::{ControlKind, ResourceKind};
use browserkit::history::Visits;
use browserkit::paint::Surface;
use browserkit::{download, form, pages};
use gui::{Event, Key, Mods, Window};
use imgdec::Image;
use milkweb::http::{Error, Request};
use milkweb::url::{Scheme, Url};
use rt::sys;

use net::{Fetcher, JobId, Kind, Outcome};
use store::Profile;
use tab::{Load, Page, PageKind, Tab};
use web::{Hit, is_button, is_text};

rt::main!(main);

/// The page a new window opens: milkOS's own (the httpd service).
pub const HOME: &str = "http://127.0.0.1/";
pub const W: i32 = 900;
pub const H: i32 = 600;
pub const TAB_H: i32 = 32;
pub const BAR_H: i32 = 40;
pub const TOP: i32 = TAB_H + BAR_H;
pub const STATUS_H: i32 = 22;
pub const MAX_TABS: usize = 24;
/// Decoded images kept, in pixels (about 64 MB).
const MAX_IMAGE_PIXELS: usize = 16 << 20;

// Hit ids.
pub const ID_BACK: u32 = 1;
pub const ID_FORWARD: u32 = 2;
pub const ID_RELOAD: u32 = 3;
pub const ID_ADDRESS: u32 = 4;
pub const ID_STAR: u32 = 5;
pub const ID_MENU: u32 = 6;
pub const ID_PADLOCK: u32 = 7;
pub const ID_NEW_TAB: u32 = 8;
pub const ID_FIND_PREV: u32 = 9;
pub const ID_FIND_NEXT: u32 = 10;
pub const ID_FIND_DONE: u32 = 11;
pub const ID_FIND_FIELD: u32 = 12;
pub const ID_POPUP_BG: u32 = 13;
pub const ID_TAB: u32 = 100; // + index
pub const ID_CLOSE_TAB: u32 = 200; // + index
pub const ID_MENU_ITEM: u32 = 300; // + index
pub const ID_OPTION: u32 = 400; // + index

/// What the toolbar's menu offers (label, shortcut).
pub const MENU: [(&str, &str); 14] = [
    ("New Tab", "Cmd-T"),
    ("Close Tab", "Ctrl-W"),
    ("Bookmark This Page", "Cmd-D"),
    ("Bookmarks", "Cmd-B"),
    ("History", "Cmd-Y"),
    ("Downloads", "Cmd-J"),
    ("Cookies", ""),
    ("Find in Page", "Cmd-F"),
    ("Zoom In", "Cmd-+"),
    ("Zoom Out", "Cmd--"),
    ("Actual Size", "Cmd-0"),
    ("Skip to Content", ""),
    ("Save Link Target", "Alt-click"),
    ("Forget Cookies, History", "Cmd-Shift-Del"),
];

pub enum Popup {
    Menu,
    /// The padlock's panel.
    Site,
    /// A select's choices: the control.
    Select(usize),
}

/// A fetched image or stylesheet, kept for every page that uses it.
pub enum ImgState {
    Loading(JobId),
    Ready(Image),
    /// An animation, its frame on show, and when the next one is due (ms).
    Moving {
        anim: alloc::boxed::Box<imgdec::anim::Animation>,
        frame: Image,
        next_at: u64,
    },
    Sheet(Vec<u8>),
    Failed,
}

impl ImgState {
    /// What a page shows of it now.
    pub fn picture(&self) -> Option<&Image> {
        match self {
            ImgState::Ready(i) | ImgState::Moving { frame: i, .. } => Some(i),
            _ => None,
        }
    }

    fn pixels(&self) -> usize {
        self.picture().map_or(0, |i| i.w * i.h)
    }

    /// A decoded picture, an animation at its first frame.
    fn from_picture(p: net::Picture, now: u64) -> Option<ImgState> {
        match p {
            net::Picture::Still(img) => Some(ImgState::Ready(img)),
            net::Picture::Moving(mut anim) => {
                let (frame, delay) = match anim.next_frame()? {
                    Ok(f) => (f.image.clone(), f.delay_ms),
                    Err(_) => return None,
                };
                Some(ImgState::Moving {
                    anim: alloc::boxed::Box::new(anim),
                    frame,
                    next_at: now + delay.max(20) as u64,
                })
            }
        }
    }

    /// Step an animation whose frame is due. True if the picture changed.
    fn step(&mut self, now: u64) -> bool {
        let ImgState::Moving {
            anim,
            frame,
            next_at,
        } = self
        else {
            return false;
        };
        if now < *next_at {
            return false;
        }
        if anim.finished() && anim.should_loop() {
            anim.rewind();
        }
        match anim.next_frame() {
            Some(Ok(fr)) if fr.image.w == frame.w && fr.image.h == frame.h => {
                frame.px.copy_from_slice(&fr.image.px);
                *next_at = now + fr.delay_ms.max(20) as u64;
                true
            }
            // Done (or a damaged frame): it stays on the last good one.
            _ => {
                *next_at = u64::MAX;
                false
            }
        }
    }
}

pub enum DlState {
    Running,
    Done(String, usize),
    Failed(String),
}

pub struct Download {
    pub job: JobId,
    pub name: String,
    pub url: String,
    pub got: usize,
    pub total: Option<usize>,
    pub state: DlState,
}

pub struct App {
    pub win: Window,
    pub th: Theme,
    pub hits: Hits,
    pub tabs: Vec<Tab>,
    pub cur: usize,
    pub fetch: Fetcher,
    pub profile: Profile,
    pub bookmarks: Bookmarks,
    pub visits: Visits,
    pub downloads: Vec<Download>,
    pub images: BTreeMap<String, ImgState>,
    pub sheets: BTreeMap<String, ImgState>,
    pub image_pixels: usize,
    pub surface: Surface,
    /// The address bar while it has the keyboard.
    pub editing: Option<Field>,
    /// The first key typed after Cmd-L replaces the whole address.
    pub select_all: bool,
    /// Find in page, while its bar is open.
    pub finding: Option<Field>,
    pub popup: Option<Popup>,
    pub online: bool,
    /// A message for the status line, until (ms).
    pub flash: Option<(String, u64)>,
    pub shown_title: String,
    /// When the desktop's theme was last read (ms).
    pub theme_checked: u64,
    /// The system fonts for page text (the desktop's family, read each frame).
    pub sys: sysfont::SysText,
    pub quit: bool,
    pub dirty: bool,
}

pub fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

/// Is the internet on (/proc/net/internet), so names and searches work?
fn internet_on() -> bool {
    sys::read_file("/proc/net/internet")
        .map(|b| {
            String::from_utf8_lossy(&b)
                .lines()
                .any(|l| l.trim() == "internet=on")
        })
        .unwrap_or(false)
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
        Error::Certificate(_) => {
            "milkOS checks every secure site's certificate against Mozilla's list of trusted authorities. Either the site is set up wrongly, or someone is pretending to be it: the page wasn't opened."
        }
        Error::Tls(_) => "milkOS speaks TLS 1.3 only; the site may need an older version.",
        Error::Timeout => "Try again (reload), or another site.",
        Error::TooBig(_) => {
            "Pages can be up to 4 MB here; a file that big can be saved instead (Alt-click its link)."
        }
        _ => "",
    }
}

impl App {
    pub fn tab(&self) -> &Tab {
        &self.tabs[self.cur]
    }

    pub fn tab_mut(&mut self) -> &mut Tab {
        &mut self.tabs[self.cur]
    }

    pub fn say(&mut self, s: &str) {
        self.flash = Some((String::from(s), now_ms() + 5000));
    }

    /// The page area: (y, width, height) in window pixels.
    pub fn view(&self) -> (i32, i32, i32) {
        (TOP, self.win.w, (self.win.h - TOP - STATUS_H).max(1))
    }

    /// The page area in page pixels at the tab's zoom.
    pub fn page_view(&self, t: &Tab) -> (i32, i32) {
        let (_, w, h) = self.view();
        ((w * 100 / t.zoom).max(1), (h * 100 / t.zoom).max(1))
    }

    pub fn max_scroll(&self, t: &Tab) -> i32 {
        let (_, vh) = self.page_view(t);
        (t.page.doc.height() - vh).max(0)
    }

    // ---- pages ----------------------------------------------------------------------------

    fn internal_html(&self, i: Internal) -> String {
        match i {
            Internal::NewTab => pages::new_tab(&self.bookmarks, self.online),
            Internal::Bookmarks => pages::bookmarks(&self.bookmarks),
            Internal::History => pages::history(&self.visits),
            Internal::Downloads => {
                let rows: Vec<pages::DownloadRow> = self
                    .downloads
                    .iter()
                    .map(|d| pages::DownloadRow {
                        name: d.name.clone(),
                        url: d.url.clone(),
                        state: match &d.state {
                            DlState::Running => {
                                download::progress_line("Downloading", d.got, d.total)
                            }
                            DlState::Done(p, n) => {
                                format!("Saved, {} ({p})", download::human_size(*n))
                            }
                            DlState::Failed(e) => format!("Failed: {e}"),
                        },
                    })
                    .collect();
                pages::downloads(&rows, &self.profile.downloads)
            }
            Internal::Cookies => pages::cookies(&self.fetch.jar.lock()),
        }
    }

    fn internal_page(&self, i: Internal) -> Page {
        let html = self.internal_html(i);
        Page::new(
            i.address(),
            None,
            html.as_bytes(),
            "text/html",
            PageKind::Internal(i),
        )
    }

    fn error_page(
        &self,
        title: &str,
        message: &str,
        hint: &str,
        address: &str,
        url: Option<Url>,
    ) -> Page {
        let html = pages::error(title, message, hint, address);
        let mut p = Page::new(address, url, html.as_bytes(), "text/html", PageKind::Error);
        p.title = String::from(title);
        p
    }

    /// Open `target` in tab `ti`. `push`: a new history entry; `from`: the site of the page
    /// whose link or form led here (cookies' SameSite); `req`: a form's request instead of a
    /// plain GET.
    pub fn go(
        &mut self,
        ti: usize,
        target: Target,
        push: bool,
        from: Option<String>,
        req: Option<Request>,
    ) {
        self.popup = None;
        if let Some(l) = self.tabs[ti].load.take() {
            self.fetch.cancel(l.job);
        }
        let (url, fragment) = match target {
            Target::Internal(i) => {
                let p = self.internal_page(i);
                let t = &mut self.tabs[ti];
                if push {
                    t.history.push(i.address());
                } else {
                    t.history.replace(i.address());
                }
                if let Some(e) = t.history.current_mut() {
                    e.title = String::from(i.title());
                }
                t.show(p, 0);
                return;
            }
            Target::Bad(why) => {
                let p = self.error_page("Can't go there", &why, "", "", None);
                self.tabs[ti].show(p, 0);
                return;
            }
            Target::Search(u, words) => {
                if !self.online {
                    let p = self.error_page(
                        "Searching needs the internet",
                        &format!("milkOS is offline (MILK_OFFLINE=1, or started by a test), so \"{words}\" can't be searched for."),
                        "Type an address instead, like http://10.0.2.2:8000/.",
                        "",
                        None,
                    );
                    self.tabs[ti].show(p, 0);
                    return;
                }
                (u, None)
            }
            Target::Web(u, f) => (u, f),
        };
        // A link to a place on this same page: just scroll there.
        let t = &mut self.tabs[ti];
        if req.is_none()
            && push
            && fragment.is_some()
            && t.page.url.as_ref() == Some(&url)
            && t.page.kind == PageKind::Web
        {
            let a = format!("{}#{}", url.to_text(), fragment.as_deref().unwrap_or(""));
            let scroll = t.scroll;
            if let Some(e) = t.history.current_mut() {
                e.scroll = scroll;
            }
            t.history.push(&a);
            t.page.address = a;
            t.anchor = fragment;
            return;
        }
        let address = match &fragment {
            Some(f) => format!("{}#{f}", url.to_text()),
            None => url.to_text(),
        };
        let req = req.unwrap_or_else(|| Request::get(&url));
        let id = self
            .fetch
            .start(Kind::Page, req, Context::TopLevel { from });
        let scroll = if push {
            None
        } else {
            self.tabs[ti].history.current().map(|e| e.scroll)
        };
        self.tabs[ti].load = Some(Load {
            job: id,
            address,
            push,
            fragment,
            scroll,
            started_ms: now_ms(),
        });
    }

    /// Open what was typed in the address bar.
    pub fn go_typed(&mut self, text: &str) {
        let t = address::interpret(text);
        self.go(self.cur, t, true, None, None);
    }

    /// Follow a link on the current page: here, in a new tab, or save what it points at.
    pub fn follow(&mut self, href: &str, new_tab: bool, save: bool) {
        let t = self.tab();
        let base = t.page.doc.base_url().or_else(|| t.page.url.clone());
        let from = t.page.url.as_ref().map(|u| site_of(&u.host));
        let target = address::resolve(base.as_ref(), href);
        if save {
            if let Target::Web(u, _) = target {
                self.start_download(&u, from);
            }
            return;
        }
        if new_tab {
            if self.tabs.len() >= MAX_TABS {
                self.say("That's a lot of tabs: close one first");
                return;
            }
            let p = self.internal_page(Internal::NewTab);
            self.tabs.push(Tab::new(p));
            let ti = self.tabs.len() - 1;
            self.go(ti, target, true, from, None);
            self.say("Opened in a new tab (Ctrl-Tab to go to it)");
            return;
        }
        self.go(self.cur, target, true, from, None);
    }

    fn start_download(&mut self, u: &Url, from: Option<String>) {
        let id = self
            .fetch
            .start(Kind::Download, Request::get(u), Context::TopLevel { from });
        let name = download::file_name(u, None);
        self.flash = None;
        self.downloads.push(Download {
            job: id,
            name,
            url: u.to_text(),
            got: 0,
            total: None,
            state: DlState::Running,
        });
    }

    pub fn back_forward(&mut self, d: isize) {
        let t = self.tab_mut();
        let scroll = t.scroll;
        let Some(e) = t.history.go(d, scroll) else {
            return;
        };
        let (url, scroll) = (e.url.clone(), e.scroll);
        let target = address::interpret(&url);
        self.go(self.cur, target, false, None, None);
        // Built-in pages show at once: put the scroll back now.
        let t = self.tab_mut();
        if t.load.is_none() && t.anchor.is_none() {
            t.scroll = scroll;
            t.target = scroll;
        }
    }

    pub fn reload(&mut self) {
        let t = self.tab();
        let a = t
            .history
            .current()
            .map_or_else(|| t.page.address.clone(), |e| e.url.clone());
        if a.is_empty() {
            return;
        }
        let scroll = self.tab().scroll;
        if let Some(e) = self.tab_mut().history.current_mut() {
            e.scroll = scroll;
        }
        let target = address::interpret(&a);
        self.go(self.cur, target, false, None, None);
    }

    pub fn stop(&mut self) {
        if let Some(l) = self.tab_mut().load.take() {
            self.fetch.cancel(l.job);
            self.say("Stopped");
        }
    }

    pub fn new_tab(&mut self) {
        if self.tabs.len() >= MAX_TABS {
            self.say("That's a lot of tabs: close one first");
            return;
        }
        let p = self.internal_page(Internal::NewTab);
        let mut t = Tab::new(p);
        t.history.push(Internal::NewTab.address());
        self.tabs.push(t);
        self.cur = self.tabs.len() - 1;
        self.edit_address();
    }

    pub fn close_tab(&mut self, i: usize) {
        if i >= self.tabs.len() {
            return;
        }
        if let Some(l) = self.tabs[i].load.take() {
            self.fetch.cancel(l.job);
        }
        if self.tabs.len() == 1 {
            // The last tab: the window stays, with a new tab.
            let p = self.internal_page(Internal::NewTab);
            self.tabs[0] = Tab::new(p);
            self.tabs[0].history.push(Internal::NewTab.address());
            return;
        }
        self.tabs.remove(i);
        if self.cur >= self.tabs.len() || (self.cur > i) {
            self.cur = self.cur.saturating_sub(1);
        }
        self.cur = self.cur.min(self.tabs.len() - 1);
        self.editing = None;
        self.popup = None;
    }

    pub fn switch_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.cur {
            self.cur = i;
            self.editing = None;
            self.popup = None;
            if let Some(f) = &self.finding {
                let q = f.text.clone();
                self.run_find(&q);
            }
        }
    }

    pub fn edit_address(&mut self) {
        let t = self.tab();
        let shown = match (&t.load, t.page.kind) {
            (Some(l), _) => l.address.clone(),
            (None, PageKind::Internal(Internal::NewTab)) => String::new(),
            (None, _) => t.page.address.clone(),
        };
        self.editing = Some(Field::new(&shown, 2000));
        self.select_all = !shown.is_empty();
        self.popup = None;
    }

    pub fn toggle_bookmark(&mut self) {
        let t = self.tab();
        if t.page.kind == PageKind::Error || t.page.address.is_empty() {
            return;
        }
        let (a, title) = (t.page.address.clone(), t.page.title.clone());
        let on = self.bookmarks.toggle(&a, &title);
        self.profile.write("bookmarks", &self.bookmarks.to_file());
        self.say(if on {
            "Bookmarked (Cmd-B shows your bookmarks)"
        } else {
            "Bookmark removed"
        });
        self.refresh_internal(Internal::Bookmarks);
        self.refresh_internal(Internal::NewTab);
    }

    /// Redraw the built-in pages of kind `i` that are open (their contents changed).
    fn refresh_internal(&mut self, i: Internal) {
        for k in 0..self.tabs.len() {
            if self.tabs[k].page.kind == PageKind::Internal(i) && self.tabs[k].load.is_none() {
                let scroll = self.tabs[k].scroll;
                let p = self.internal_page(i);
                self.tabs[k].show(p, scroll);
            }
        }
    }

    pub fn forget_everything(&mut self) {
        self.fetch.jar.lock().clear();
        self.visits.0.clear();
        self.save_cookies();
        self.profile.write("history", "");
        self.say("Cookies and history forgotten");
        self.refresh_internal(Internal::Cookies);
        self.refresh_internal(Internal::History);
    }

    pub fn save_cookies(&self) {
        let text = self.fetch.jar.lock().to_file(net::now_secs());
        self.profile.write("cookies", &text);
    }

    // ---- find ---------------------------------------------------------------------------

    pub fn run_find(&mut self, q: &str) {
        let t = &mut self.tabs[self.cur];
        t.found = t.page.doc.find(q);
        t.found_at = 0;
        // The first match at or below the top of the view.
        if let Some(i) = t
            .found
            .iter()
            .position(|r| r.first().is_some_and(|r| r.y >= t.target))
        {
            t.found_at = i;
        }
        self.show_match();
    }

    pub fn find_step(&mut self, d: isize) {
        let t = self.tab_mut();
        if t.found.is_empty() {
            return;
        }
        let n = t.found.len() as isize;
        t.found_at = ((t.found_at as isize + d).rem_euclid(n)) as usize;
        self.show_match();
    }

    fn show_match(&mut self) {
        let (_, vh) = {
            let t = self.tab();
            self.page_view(t)
        };
        let max = self.max_scroll(self.tab());
        let t = self.tab_mut();
        if let Some(r) = t.found.get(t.found_at).and_then(|m| m.first()).copied()
            && (r.y < t.target || r.y + r.h > t.target + vh)
        {
            // Measured against where scrolling is heading, not where it is on the way.
            t.target = (r.y - vh / 3).clamp(0, max);
        }
    }

    // ---- forms --------------------------------------------------------------------------

    /// Send the form that control `id` belongs to (a button, or Enter in a field: the
    /// engine applies the implicit-submission rules).
    pub fn submit(&mut self, id: usize) {
        let t = self.tab();
        let doc = &t.page.doc;
        let Some(c) = doc.control(id) else {
            return;
        };
        let Some(sub) = doc.submit(id) else {
            return;
        };
        let from = t.page.url.as_ref().map(|u| site_of(&u.host));
        // A password typed on a plain-http page goes in the clear: say so.
        let password = doc
            .controls()
            .iter()
            .any(|o| o.kind == ControlKind::Password && o.form == c.form && !o.value.is_empty());
        let new_tab = sub.target.as_deref() == Some("_blank");
        match form::request(&sub) {
            Ok(req) => {
                if password && req.url.scheme == Scheme::Http {
                    self.say("Note: this password went over plain http, not encrypted");
                }
                let target = Target::Web(req.url.clone(), None);
                let ti = if new_tab && self.tabs.len() < MAX_TABS {
                    let p = self.internal_page(Internal::NewTab);
                    self.tabs.push(Tab::new(p));
                    self.cur = self.tabs.len() - 1;
                    self.cur
                } else {
                    self.cur
                };
                self.go(ti, target, true, from, Some(req));
            }
            Err(e) => self.say(&format!("The form can't be sent: {e}")),
        }
    }

    pub fn reset_form(&mut self, id: usize) {
        self.tab_mut().page.doc.reset(id);
    }

    /// Give control `id` the keyboard (or take it away with None).
    pub fn focus(&mut self, id: Option<usize>) {
        self.editing = None;
        let t = self.tab_mut();
        t.focus = id;
        t.caret = id
            .and_then(|i| t.page.doc.control(i))
            .map_or(0, |c| c.value.chars().count());
        // Keep it in view.
        if let Some(r) = id.and_then(|i| t.page.doc.control_rect(i)) {
            let zoom = t.zoom;
            let (_, h) = (0, (self.win.h - TOP - STATUS_H).max(1) * 100 / zoom);
            let t = self.tab_mut();
            if r.y < t.scroll || r.y + r.h > t.scroll + h {
                t.target = (r.y - h / 3).max(0);
            }
        }
    }

    /// Typing into the focused control. True if it used the key.
    fn control_key(&mut self, k: Key, m: Mods) -> bool {
        let t = self.tab();
        let Some(id) = t.focus else {
            return false;
        };
        let Some(c) = t.page.doc.control(id).cloned() else {
            return false;
        };
        if m.meta || m.ctrl {
            if is_text(c.kind) && matches!(k, Key::Char('v')) {
                let clip = clipboard_get();
                let clip = if c.kind == ControlKind::TextArea {
                    clip
                } else {
                    clip.replace(['\r', '\n'], " ")
                };
                self.insert_text(id, &clip);
                return true;
            }
            return false;
        }
        match c.kind {
            k2 if is_text(k2) && c.readonly => match k {
                Key::Enter if k2 != ControlKind::TextArea => {
                    self.submit(id);
                    true
                }
                Key::Escape => {
                    self.focus(None);
                    true
                }
                _ => false,
            },
            k2 if is_text(k2) => {
                let mut chars: Vec<char> = c.value.chars().collect();
                let caret = self.tab().caret.min(chars.len());
                let mut caret2 = caret;
                match k {
                    Key::Char(ch) if !ch.is_control() => {
                        self.insert_text(id, &String::from(ch));
                        return true;
                    }
                    Key::Enter if c.kind == ControlKind::TextArea => {
                        self.insert_text(id, "\n");
                        return true;
                    }
                    Key::Enter => {
                        self.submit(id);
                        return true;
                    }
                    Key::Backspace if caret > 0 => {
                        chars.remove(caret - 1);
                        caret2 = caret - 1;
                    }
                    Key::Delete if caret < chars.len() => {
                        chars.remove(caret);
                    }
                    Key::Left => caret2 = caret.saturating_sub(1),
                    Key::Right => caret2 = (caret + 1).min(chars.len()),
                    Key::Home => caret2 = 0,
                    Key::End => caret2 = chars.len(),
                    Key::Up | Key::Down if c.kind == ControlKind::TextArea => {
                        caret2 = if k == Key::Up { 0 } else { chars.len() };
                    }
                    Key::Escape => {
                        self.focus(None);
                        return true;
                    }
                    Key::Backspace | Key::Delete => return true,
                    _ => return false,
                }
                let v: String = chars.into_iter().collect();
                let t = self.tab_mut();
                t.page.doc.set_value(id, &v);
                t.caret = caret2;
                true
            }
            ControlKind::Radio if matches!(k, Key::Up | Key::Down | Key::Left | Key::Right) => {
                // Arrows move along the radio group (and choose).
                let doc = &self.tab().page.doc;
                let group: Vec<usize> = (0..doc.controls().len())
                    .filter(|&i| {
                        let o = &doc.controls()[i];
                        o.kind == ControlKind::Radio
                            && o.name == c.name
                            && o.form == c.form
                            && !o.disabled
                    })
                    .collect();
                if let Some(p) = group.iter().position(|&i| i == id) {
                    let n = group.len();
                    let next = group[if matches!(k, Key::Up | Key::Left) {
                        (p + n - 1) % n
                    } else {
                        (p + 1) % n
                    }];
                    self.tab_mut().page.doc.click_check(next);
                    self.focus(Some(next));
                }
                true
            }
            ControlKind::Checkbox | ControlKind::Radio => match k {
                Key::Char(' ') => {
                    self.tab_mut().page.doc.click_check(id);
                    true
                }
                Key::Enter => {
                    self.submit(id);
                    true
                }
                Key::Escape => {
                    self.focus(None);
                    true
                }
                _ => false,
            },
            ControlKind::Select => match k {
                Key::Up | Key::Down => {
                    let n = c.options.len();
                    let sel = self.tab().page.doc.selected(id);
                    if n > 0 {
                        let s = if k == Key::Up {
                            sel.saturating_sub(1)
                        } else {
                            (sel + 1).min(n - 1)
                        };
                        self.tab_mut().page.doc.select(id, s);
                    }
                    true
                }
                Key::Char(' ') | Key::Enter => {
                    self.popup = Some(Popup::Select(id));
                    true
                }
                Key::Escape => {
                    self.focus(None);
                    true
                }
                _ => false,
            },
            k2 if is_button(k2) => match k {
                Key::Char(' ') | Key::Enter => {
                    self.press(id);
                    true
                }
                Key::Escape => {
                    self.focus(None);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }

    fn insert_text(&mut self, id: usize, s: &str) {
        let t = self.tab_mut();
        let Some(c) = t.page.doc.control(id) else {
            return;
        };
        if c.readonly || c.disabled {
            return;
        }
        let mut chars: Vec<char> = c.value.chars().collect();
        let caret = t.caret.min(chars.len());
        let room = match c.max_length {
            Some(n) => (n as usize).saturating_sub(chars.len()),
            None => 100_000usize.saturating_sub(chars.len()),
        };
        let add: Vec<char> = s
            .chars()
            .filter(|ch| !ch.is_control() || *ch == '\n')
            .take(room)
            .collect();
        let n = add.len();
        chars.splice(caret..caret, add);
        let v: String = chars.into_iter().collect();
        t.page.doc.set_value(id, &v);
        t.caret = caret + n;
    }

    /// A click (or Space) on a button control.
    fn press(&mut self, id: usize) {
        let Some(kind) = self.tab().page.doc.control(id).map(|c| c.kind) else {
            return;
        };
        match kind {
            ControlKind::Submit | ControlKind::ImageSubmit => self.submit(id),
            ControlKind::File => self.say("Sending files with a form isn't supported yet"),
            ControlKind::Reset => self.reset_form(id),
            _ => {}
        }
    }

    /// Move the keyboard to the next (or previous) form control.
    fn tab_focus(&mut self, back: bool) {
        let t = self.tab();
        let ids: Vec<usize> = (0..t.page.doc.controls().len())
            .filter(|&i| t.page.doc.control_rect(i).is_some() && !t.page.doc.controls()[i].disabled)
            .filter(|&i| {
                !t.page.doc.controls()[i].readonly || is_button(t.page.doc.controls()[i].kind)
            })
            .collect();
        if ids.is_empty() {
            return;
        }
        let next = match t.focus.and_then(|f| ids.iter().position(|&i| i == f)) {
            Some(p) if back => ids[(p + ids.len() - 1) % ids.len()],
            Some(p) => ids[(p + 1) % ids.len()],
            None if back => *ids.last().unwrap(),
            None => ids[0],
        };
        self.focus(Some(next));
    }

    // ---- scrolling ----------------------------------------------------------------------

    pub fn scroll_by(&mut self, d: i32) {
        let max = self.max_scroll(self.tab());
        let t = self.tab_mut();
        t.target = (t.target + d).clamp(0, max);
    }

    pub fn scroll_to(&mut self, y: i32) {
        let max = self.max_scroll(self.tab());
        let t = self.tab_mut();
        t.target = y.clamp(0, max);
    }

    /// One step of smooth scrolling. True while it is still moving.
    fn animate(&mut self) -> bool {
        let t = self.tab_mut();
        if t.scroll == t.target {
            return false;
        }
        let d = t.target - t.scroll;
        let step = if d.abs() <= 2 { d } else { d / 3 + d.signum() };
        t.scroll += step;
        true
    }

    pub fn skip_to_content(&mut self) {
        match self.tab().page.doc.main_content() {
            Some(y) => self.scroll_to(y - 8),
            None => self.say("This page doesn't mark its main content"),
        }
    }

    // ---- input --------------------------------------------------------------------------

    fn key(&mut self, k: Key, m: Mods) {
        let cmd = m.meta || m.ctrl;
        // Ctrl-Tab / Ctrl-Shift-Tab: the next / previous tab.
        if m.ctrl && k == Key::Tab {
            let n = self.tabs.len();
            let i = if m.shift {
                (self.cur + n - 1) % n
            } else {
                (self.cur + 1) % n
            };
            self.switch_tab(i);
            return;
        }
        if cmd && (k == Key::Backspace || k == Key::Delete) && m.shift {
            self.forget_everything();
            return;
        }
        if cmd && let Key::Char(c) = k {
            match c.to_ascii_lowercase() {
                'l' => self.edit_address(),
                't' => self.new_tab(),
                'w' => {
                    let i = self.cur;
                    self.close_tab(i)
                }
                'q' => self.quit = true,
                'r' => self.reload(),
                '[' => self.back_forward(-1),
                ']' => self.back_forward(1),
                'd' => self.toggle_bookmark(),
                'b' => self.go(
                    self.cur,
                    Target::Internal(Internal::Bookmarks),
                    true,
                    None,
                    None,
                ),
                'y' => self.go(
                    self.cur,
                    Target::Internal(Internal::History),
                    true,
                    None,
                    None,
                ),
                'j' => self.go(
                    self.cur,
                    Target::Internal(Internal::Downloads),
                    true,
                    None,
                    None,
                ),
                'f' => {
                    self.editing = None;
                    self.finding = Some(Field::new("", 200));
                }
                'g' => self.find_step(if m.shift { -1 } else { 1 }),
                '=' | '+' => self.tab_mut().zoom_step(1),
                '-' => self.tab_mut().zoom_step(-1),
                '0' => self.tab_mut().set_zoom(100),
                '.' => self.stop(),
                'c' if self.editing.is_some() => {
                    if let Some(f) = &self.editing {
                        clipboard_set(&f.text);
                    }
                }
                'v' if self.editing.is_some() || self.finding.is_some() => {
                    let clip = clipboard_get().replace(['\r', '\n'], " ");
                    let f = if self.editing.is_some() {
                        self.editing.as_mut()
                    } else {
                        self.finding.as_mut()
                    };
                    if let Some(f) = f {
                        for ch in clip.chars().take(2000) {
                            f.key(Key::Char(ch), Mods::default());
                        }
                    }
                    if let Some(q) = self.finding.as_ref().map(|f| f.text.clone()) {
                        self.run_find(&q);
                    }
                }
                d @ '1'..='9' => {
                    let i = (d as u8 - b'1') as usize;
                    let i = if d == '9' { self.tabs.len() - 1 } else { i };
                    self.switch_tab(i);
                }
                _ => {
                    let _ = self.control_key(k, m);
                }
            }
            return;
        }
        if cmd
            && matches!(k, Key::Left | Key::Right)
            && self.editing.is_none()
            && self.tab().focus.is_none()
        {
            self.back_forward(if k == Key::Left { -1 } else { 1 });
            return;
        }
        // The address bar.
        if let Some(f) = self.editing.as_mut() {
            match k {
                Key::Enter => {
                    let text = f.text.clone();
                    self.editing = None;
                    self.go_typed(&text);
                }
                Key::Escape => self.editing = None,
                _ => {
                    if self.select_all && matches!(k, Key::Char(_) | Key::Backspace | Key::Delete) {
                        f.set("");
                    }
                    self.select_all = false;
                    match k {
                        Key::Home | Key::Up => f.cur = 0,
                        Key::End | Key::Down => f.cur = f.text.chars().count(),
                        _ => {
                            f.key(k, m);
                        }
                    }
                }
            }
            return;
        }
        // The find bar.
        if let Some(f) = self.finding.as_mut() {
            match k {
                Key::Enter => self.find_step(if m.shift { -1 } else { 1 }),
                Key::Escape => {
                    self.finding = None;
                    self.tab_mut().found.clear();
                }
                _ => {
                    if f.key(k, m) {
                        let q = f.text.clone();
                        self.run_find(&q);
                    }
                }
            }
            return;
        }
        if let Some(Popup::Select(id)) = self.popup {
            match k {
                Key::Escape | Key::Enter | Key::Char(' ') => self.popup = None,
                Key::Up | Key::Down => {
                    let _ = self.control_key(k, m);
                }
                _ => {}
            }
            let _ = id;
            return;
        }
        if self.popup.is_some() && k == Key::Escape {
            self.popup = None;
            return;
        }
        if k == Key::Tab {
            self.tab_focus(m.shift);
            return;
        }
        if self.control_key(k, m) {
            return;
        }
        let (_, vh) = self.page_view(self.tab());
        let page = (vh - 40).max(20);
        match k {
            Key::Escape => {
                if self.tab().load.is_some() {
                    self.stop();
                }
            }
            Key::Down => self.scroll_by(40),
            Key::Up => self.scroll_by(-40),
            Key::PageDown => self.scroll_by(page),
            Key::PageUp => self.scroll_by(-page),
            Key::Char(' ') => self.scroll_by(if m.shift { -page } else { page }),
            Key::Home => self.scroll_to(0),
            Key::End => self.scroll_to(i32::MAX / 2),
            Key::Backspace => self.back_forward(-1),
            _ => {}
        }
    }

    /// A click at (x, y) in the window.
    fn click(&mut self, x: i32, y: i32, button: u8, m: Mods) {
        let id = self.hits.at(x, y);
        // An open popup: a click outside closes it.
        if self.popup.is_some()
            && !matches!(id, Some(i) if i == ID_MENU || (ID_MENU_ITEM..ID_MENU_ITEM + 100).contains(&i) || (ID_OPTION..ID_OPTION + 1000).contains(&i))
        {
            self.popup = None;
            if id == Some(ID_POPUP_BG) {
                return;
            }
        }
        match id {
            Some(ID_BACK) => self.back_forward(-1),
            Some(ID_FORWARD) => self.back_forward(1),
            Some(ID_RELOAD) => {
                if self.tab().load.is_some() {
                    self.stop()
                } else {
                    self.reload()
                }
            }
            Some(ID_ADDRESS) => {
                if self.editing.is_none() {
                    self.edit_address();
                }
            }
            Some(ID_STAR) => self.toggle_bookmark(),
            Some(ID_PADLOCK) => self.popup = Some(Popup::Site),
            Some(ID_MENU) => {
                self.popup = match self.popup {
                    Some(Popup::Menu) => None,
                    _ => Some(Popup::Menu),
                }
            }
            Some(ID_NEW_TAB) => self.new_tab(),
            Some(ID_FIND_PREV) => self.find_step(-1),
            Some(ID_FIND_NEXT) => self.find_step(1),
            Some(ID_FIND_DONE) => {
                self.finding = None;
                self.tab_mut().found.clear();
            }
            Some(ID_FIND_FIELD) => {}
            Some(ID_POPUP_BG) => {}
            Some(i) if (ID_MENU_ITEM..ID_MENU_ITEM + 100).contains(&i) => {
                self.popup = None;
                self.menu_item((i - ID_MENU_ITEM) as usize);
            }
            Some(i) if (ID_OPTION..ID_OPTION + 1000).contains(&i) => {
                if let Some(Popup::Select(c)) = self.popup {
                    self.tab_mut().page.doc.select(c, (i - ID_OPTION) as usize);
                }
                self.popup = None;
            }
            Some(i) if (ID_CLOSE_TAB..ID_CLOSE_TAB + 100).contains(&i) => {
                self.close_tab((i - ID_CLOSE_TAB) as usize)
            }
            Some(i) if (ID_TAB..ID_TAB + 100).contains(&i) => {
                let k = (i - ID_TAB) as usize;
                if button == 3 {
                    self.close_tab(k);
                } else {
                    self.switch_tab(k);
                }
            }
            _ => {
                let (top, _, vh) = self.view();
                if y >= top && y < top + vh {
                    self.editing = None;
                    self.page_click(x, y - top, button, m);
                }
            }
        }
    }

    fn page_click(&mut self, x: i32, y: i32, button: u8, m: Mods) {
        let t = self.tab();
        let (px, py) = (x * 100 / t.zoom, y * 100 / t.zoom + t.scroll);
        match t.page.doc.hit_test(px, py, t.scroll) {
            Some(Hit::Link(link)) => {
                if button == 2 {
                    return;
                }
                self.focus(None);
                let new_tab =
                    m.meta || m.ctrl || button == 3 || link.target.as_deref() == Some("_blank");
                self.follow(&link.url, new_tab, m.alt || link.download);
            }
            Some(Hit::Control(id)) => {
                let Some(c) = self.tab().page.doc.control(id).cloned() else {
                    return;
                };
                if c.disabled {
                    return;
                }
                self.focus(Some(id));
                match c.kind {
                    ControlKind::Checkbox | ControlKind::Radio => {
                        self.tab_mut().page.doc.click_check(id)
                    }
                    ControlKind::Select => self.popup = Some(Popup::Select(id)),
                    k if is_button(k) => self.press(id),
                    k if is_text(k) => {
                        // The caret goes where the click was (single-line fields).
                        if let Some(r) = self.tab().page.doc.control_rect(id)
                            && k != ControlKind::TextArea
                        {
                            let shown: String = if k == ControlKind::Password {
                                c.value.chars().map(|_| '\u{2022}').collect()
                            } else {
                                c.value.clone()
                            };
                            let f = sysfont::font_for(&c.font, self.sys.ui);
                            self.tab_mut().caret = f.index_at(&shown, (px - r.x).max(0));
                        }
                    }
                    _ => {}
                }
            }
            None => self.focus(None),
        }
    }

    fn menu_item(&mut self, i: usize) {
        let c = self.cur;
        match i {
            0 => self.new_tab(),
            1 => self.close_tab(c),
            2 => self.toggle_bookmark(),
            3 => self.go(c, Target::Internal(Internal::Bookmarks), true, None, None),
            4 => self.go(c, Target::Internal(Internal::History), true, None, None),
            5 => self.go(c, Target::Internal(Internal::Downloads), true, None, None),
            6 => self.go(c, Target::Internal(Internal::Cookies), true, None, None),
            7 => self.finding = Some(Field::new("", 200)),
            8 => self.tab_mut().zoom_step(1),
            9 => self.tab_mut().zoom_step(-1),
            10 => self.tab_mut().set_zoom(100),
            11 => self.skip_to_content(),
            12 => self.say("Alt-click a link to save what it points at in ~/Downloads"),
            13 => self.forget_everything(),
            _ => {}
        }
    }

    // ---- animations -----------------------------------------------------------------------

    /// Step the animated pictures the current tab shows (background tabs' stand still).
    /// Returns whether one changed, and when the next frame is due (ms).
    fn animate_images(&mut self, now: u64) -> (bool, Option<u64>) {
        let mut changed = false;
        let mut next: Option<u64> = None;
        let t = &self.tabs[self.cur];
        for u in &t.page.shown {
            if let Some(st) = self.images.get_mut(u) {
                changed |= st.step(now);
                if let ImgState::Moving { next_at, .. } = st
                    && *next_at != u64::MAX
                {
                    next = Some(next.map_or(*next_at, |n: u64| n.min(*next_at)));
                }
            }
        }
        (changed, next)
    }

    // ---- jobs ---------------------------------------------------------------------------

    /// Take in finished jobs, download progress and the open inbox. True if anything changed.
    fn tick(&mut self) -> bool {
        let mut changed = false;
        // Pages that turned out to be files: the tab lets go, the download goes on.
        for ti in 0..self.tabs.len() {
            let Some(l) = &self.tabs[ti].load else {
                continue;
            };
            let Some(sh) = self.fetch.shared(l.job) else {
                continue;
            };
            let name = sh.lock().download.clone();
            if let Some(name) = name {
                let l = self.tabs[ti].load.take().unwrap();
                self.flash = None;
                self.downloads.push(Download {
                    job: l.job,
                    name,
                    url: l.address,
                    got: 0,
                    total: None,
                    state: DlState::Running,
                });
                changed = true;
            }
        }
        for d in &mut self.downloads {
            if let DlState::Running = d.state
                && let Some(sh) = self.fetch.shared(d.job)
            {
                let s = sh.lock();
                if (s.got, s.total) != (d.got, d.total) {
                    d.got = s.got;
                    d.total = s.total;
                    changed = true;
                }
                if let Some(n) = &s.download
                    && *n != d.name
                {
                    d.name = n.clone();
                }
            }
        }
        for (id, kind, out, cert) in self.fetch.finished() {
            changed = true;
            self.finish(id, kind, out, cert);
        }
        for a in self.profile.take_inbox() {
            let target = address::interpret(&a);
            if self.tabs.len() < MAX_TABS {
                let p = self.internal_page(Internal::NewTab);
                self.tabs.push(Tab::new(p));
                self.cur = self.tabs.len() - 1;
            }
            self.go(self.cur, target, true, None, None);
            changed = true;
        }
        changed
    }

    fn finish(
        &mut self,
        id: JobId,
        kind: Kind,
        out: Outcome,
        cert: Option<browserkit::certinfo::CertInfo>,
    ) {
        if let Some(di) = self.downloads.iter().position(|d| d.job == id) {
            let d = &mut self.downloads[di];
            match out {
                Outcome::Saved(path, n) => {
                    rt::println!("[browser] saved {} ({} bytes) from {}", path, n, d.url);
                    let short = String::from(path.rsplit('/').next().unwrap_or(&path));
                    d.state = DlState::Done(path, n);
                    self.say(&format!("Saved {short} in ~/Downloads"));
                }
                Outcome::SaveFailed(e) | Outcome::Image(Err(e)) => {
                    d.state = DlState::Failed(e.clone());
                    self.say(&format!("Download failed: {e}"));
                }
                Outcome::Failed(f) => {
                    let e = format!("{}", f.error);
                    rt::println!("[browser] download {}: {e}", d.url);
                    d.state = DlState::Failed(e.clone());
                    self.say(&format!("Download failed: {e}"));
                }
                Outcome::Page(r) => {
                    d.state = DlState::Failed(format!("{} {}", r.status, r.reason));
                    self.say(&format!("Download failed: {} {}", r.status, r.reason));
                }
                Outcome::Image(Ok(_)) => {}
            }
            self.refresh_internal(Internal::Downloads);
            return;
        }
        if kind == Kind::Image || kind == Kind::Style {
            let cache_hit = |m: &BTreeMap<String, ImgState>| {
                m.iter()
                    .find(|(_, s)| matches!(s, ImgState::Loading(j) if *j == id))
                    .map(|(u, _)| u.clone())
            };
            let url = cache_hit(if kind == Kind::Image {
                &self.images
            } else {
                &self.sheets
            });
            if let Some(u) = url {
                let st = match out {
                    Outcome::Image(Err(e)) => {
                        rt::println!("[browser] image {u}: {e}");
                        ImgState::Failed
                    }
                    Outcome::Image(Ok(pic)) => match ImgState::from_picture(pic, now_ms()) {
                        Some(st) if self.image_pixels + st.pixels() <= MAX_IMAGE_PIXELS => {
                            self.image_pixels += st.pixels();
                            st
                        }
                        _ => ImgState::Failed,
                    },
                    Outcome::Page(r) if r.status < 400 && kind == Kind::Style => {
                        ImgState::Sheet(r.body)
                    }
                    _ => ImgState::Failed,
                };
                if kind == Kind::Image {
                    if let Some(i) = st.picture() {
                        let moving = if let ImgState::Moving { anim, .. } = &st {
                            alloc::format!(", animated, {} frames", anim.frame_count())
                        } else {
                            String::new()
                        };
                        rt::println!("[browser] image {u}: {}x{}{moving}", i.w, i.h);
                    }
                    self.images.insert(u, st);
                } else {
                    self.sheets.insert(u, st);
                }
            }
            for ti in 0..self.tabs.len() {
                self.feed(ti);
            }
            return;
        }
        let Some(ti) = self
            .tabs
            .iter()
            .position(|t| t.load.as_ref().is_some_and(|l| l.job == id))
        else {
            return; // stopped, or the tab is gone
        };
        let load = self.tabs[ti].load.take().unwrap();
        match out {
            Outcome::Page(resp) => {
                let ct = String::from(resp.content_type());
                let addr = match &load.fragment {
                    Some(f) => format!("{}#{f}", resp.url.to_text()),
                    None => resp.url.to_text(),
                };
                let mut page = Page::new(
                    &addr,
                    Some(resp.url.clone()),
                    &resp.body,
                    &ct,
                    PageKind::Web,
                );
                page.status = resp.status;
                if resp.status >= 400 && page.title.is_empty() {
                    page.title = format!("{} {}", resp.status, resp.reason);
                }
                if resp.url.scheme == Scheme::Https {
                    page.cert = cert;
                }
                rt::println!(
                    "[browser] {}: {} {}, {} bytes, '{}', {} redirects",
                    resp.url,
                    resp.status,
                    resp.reason,
                    resp.body.len(),
                    page.title,
                    resp.redirects.len()
                );
                let title = page.title.clone();
                let t = &mut self.tabs[ti];
                if load.push {
                    let scroll = t.scroll;
                    if let Some(e) = t.history.current_mut() {
                        e.scroll = scroll;
                    }
                    t.history.push(&addr);
                } else {
                    t.history.replace(&addr);
                }
                if let Some(e) = t.history.current_mut() {
                    e.title = title.clone();
                }
                t.show(page, 0);
                t.anchor = load.fragment.clone();
                if let Some(s) = load.scroll {
                    t.scroll = s;
                    t.target = s;
                }
                self.visits.add(&addr, &title, net::now_secs());
                self.feed(ti);
                self.save_cookies();
                self.profile.write("history", &self.visits.to_file());
            }
            Outcome::Failed(f) => {
                if f.error == Error::Cancelled {
                    return;
                }
                let msg = format!("{}", f.error);
                rt::println!("[browser] {}: {msg}", f.url);
                let addr = f.url.to_text();
                let page = self.error_page(
                    f.error.title(),
                    &msg,
                    hint(&f.error),
                    &addr,
                    Some(f.url.clone()),
                );
                let t = &mut self.tabs[ti];
                if load.push {
                    t.history.push(&addr);
                } else {
                    t.history.replace(&addr);
                }
                t.show(page, 0);
            }
            Outcome::Saved(..) | Outcome::SaveFailed(_) | Outcome::Image(_) => {}
        }
    }

    /// Give tab `ti`'s page the stylesheets and images it waits for that have come (or
    /// failed), and fetch the rest. Nothing over plain http on a secure page (mixed content).
    pub fn feed(&mut self, ti: usize) {
        let Some(t) = self.tabs.get(ti) else {
            return;
        };
        if t.page.kind != PageKind::Web {
            return;
        }
        let secure = t
            .page
            .url
            .as_ref()
            .is_some_and(|u| u.scheme == Scheme::Https);
        let site = t
            .page
            .url
            .as_ref()
            .map(|u| site_of(&u.host))
            .unwrap_or_default();
        // Supplying a stylesheet can ask for more (@import, background images): a few rounds.
        for _ in 0..8 {
            let pending = self.tabs[ti].page.doc.pending();
            let mut waiting = 0;
            let mut supplied = false;
            for r in pending.into_iter().take(300) {
                let style = r.kind == ResourceKind::Stylesheet;
                let cache = if style { &self.sheets } else { &self.images };
                match cache.get(&r.url) {
                    Some(ImgState::Loading(_)) => waiting += 1,
                    Some(st @ (ImgState::Ready(_) | ImgState::Moving { .. })) => {
                        let img = st.picture().unwrap();
                        if let ResourceKind::Image(id) = r.kind {
                            let size = (img.w as u32, img.h as u32);
                            self.tabs[ti].page.doc.supply_image(id, Some(size));
                            self.tabs[ti].page.shown.insert(r.url.clone());
                            supplied = true;
                        }
                    }
                    Some(ImgState::Sheet(body)) => {
                        let body = body.clone();
                        self.tabs[ti]
                            .page
                            .doc
                            .supply_stylesheet(&r.url, Some(&body));
                        supplied = true;
                    }
                    Some(ImgState::Failed) => {
                        match r.kind {
                            ResourceKind::Image(id) => {
                                self.tabs[ti].page.doc.supply_image(id, None)
                            }
                            ResourceKind::Stylesheet => {
                                self.tabs[ti].page.doc.supply_stylesheet(&r.url, None)
                            }
                        }
                        supplied = true;
                    }
                    None => {
                        let url = Url::parse(&r.url)
                            .ok()
                            .filter(|u| !(secure && u.scheme == Scheme::Http));
                        let cache = if style {
                            &mut self.sheets
                        } else {
                            &mut self.images
                        };
                        match url {
                            Some(u) => {
                                let ctx = Context::Subresource {
                                    page_site: site.clone(),
                                };
                                let kind = if style { Kind::Style } else { Kind::Image };
                                let id = self.fetch.start(kind, Request::get(&u), ctx);
                                cache.insert(r.url.clone(), ImgState::Loading(id));
                                waiting += 1;
                            }
                            // Not an address we fetch, or mixed content: it failed.
                            None => {
                                cache.insert(r.url.clone(), ImgState::Failed);
                                supplied = true;
                            }
                        }
                    }
                }
            }
            self.tabs[ti].page.waiting = waiting;
            if supplied {
                self.tabs[ti].page.laid_w = 0;
            } else {
                break;
            }
        }
    }
}

fn clipboard_get() -> String {
    let Ok(fd) = sys::open("/dev/clipboard", sys::O_RDONLY) else {
        return String::new();
    };
    let mut out = Vec::new();
    let mut buf = [0u8; 4096];
    while let Ok(n) = sys::read(fd, &mut buf) {
        if n == 0 || out.len() > 64 * 1024 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    let _ = sys::close(fd);
    String::from_utf8_lossy(&out).into_owned()
}

fn clipboard_set(s: &str) {
    if let Ok(fd) = sys::open("/dev/clipboard", sys::O_WRONLY) {
        let _ = sys::write_all(fd, s.as_bytes());
        let _ = sys::close(fd);
    }
}

/// "shift+meta", or "-" for none.
fn mods_text(m: Mods) -> String {
    let v: Vec<&str> = [
        (m.shift, "shift"),
        (m.ctrl, "ctrl"),
        (m.alt, "alt"),
        (m.meta, "meta"),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, n)| n)
    .collect();
    if v.is_empty() {
        String::from("-")
    } else {
        v.join("+")
    }
}

/// One input event as a log line (for `--log-input`).
fn log_input(ev: &Event, m: Mods) {
    let mods = mods_text(m);
    match ev {
        Event::Key(k, km) => rt::println!("[browser] input key {:?} mods={}", k, mods_text(*km)),
        Event::Down { x, y, button } => {
            rt::println!("[browser] input down {x} {y} button={button} mods={mods}")
        }
        Event::Up { x, y, button } => {
            rt::println!("[browser] input up {x} {y} button={button} mods={mods}")
        }
        Event::Drag { x, y } => rt::println!("[browser] input drag {x} {y} mods={mods}"),
        Event::Scroll(d) => rt::println!("[browser] input scroll {d} mods={mods}"),
        _ => {}
    }
}

fn main(args: &rt::Args) -> i32 {
    let log = args.get(1) == Some("--log-input");
    let home = args.env("HOME").unwrap_or("/Users/milk");
    let profile = Profile::new(home);
    profile.ensure();
    let online = internet_on();
    // The system fonts now (the rest of /System/Fonts stays readable: other families are
    // read when a page needs their characters, or when Settings picks another).
    gui::text::preload();
    // Before any page is fetched: lock down what a hostile page could reach.
    let spec = format!(
        "fs={}:rw fs={}:rw fs=/System/Fonts:r fs=/dev/wm:rw fs=/dev/clipboard:rw fs=/proc/net/tls:r fs=/proc/net/internet:r fs={home}/.config/milk/settings:r net=all spawn=0 clipboard=1",
        profile.lib, profile.downloads
    );
    if let Err(e) = sys::sandbox(&spec) {
        rt::println!("browser: can't sandbox myself: {}", e.name());
        return 1;
    }
    let Ok(win) = Window::new("Browser", W, H) else {
        rt::println!("browser: no desktop to open a window on");
        return 1;
    };
    win.set_resizable(520, 360);
    let jar = Jar::from_file(&profile.read("cookies"), net::now_secs());
    let bookmarks = Bookmarks::from_file(&profile.read("bookmarks"));
    let visits = Visits::from_file(&profile.read("history"));
    let downloads = profile.downloads.clone();
    let first = Page::new(
        "",
        None,
        b"",
        "text/html",
        PageKind::Internal(Internal::NewTab),
    );
    let mut app = App {
        win,
        th: Theme::current(),
        hits: Hits::default(),
        tabs: alloc::vec![Tab::new(first)],
        cur: 0,
        fetch: Fetcher::new(jar, downloads),
        profile,
        bookmarks,
        visits,
        downloads: Vec::new(),
        images: BTreeMap::new(),
        sheets: BTreeMap::new(),
        image_pixels: 0,
        surface: Surface::new(W, H, 0xFFFFFF),
        editing: None,
        select_all: false,
        finding: None,
        popup: None,
        online,
        flash: None,
        shown_title: String::new(),
        sys: sysfont::SysText::now(),
        theme_checked: 0,
        quit: false,
        dirty: true,
    };
    // Addresses left in the inbox belong to an earlier run; the argument is this one's.
    let _ = app.profile.take_inbox();
    let start = args.get(if log { 2 } else { 1 }).unwrap_or(HOME);
    app.go_typed(start);
    app.draw();
    let mut last_draw = now_ms();
    let mut frame_due: Option<u64> = None;
    loop {
        let busy = app.fetch.busy() || app.tab().scroll != app.tab().target;
        // Wait no longer than the next animation frame.
        let mut wait = if busy { 16 } else { 400 };
        if let Some(due) = frame_due {
            wait = wait.min(due.saturating_sub(now_ms()).max(1));
        }
        let ev = app.win.event(wait);
        let m = app.win.mods();
        if log && let Some(e) = &ev {
            log_input(e, m);
        }
        let mut dirty = ev.is_some();
        match ev {
            Some(Event::Close) => break,
            Some(Event::Resize { w, h }) => {
                let _ = app.win.resize(w, h);
                for t in &mut app.tabs {
                    t.page.laid_w = 0;
                }
            }
            Some(Event::Down { x, y, button }) => app.click(x, y, button, m),
            Some(Event::Scroll(d)) => {
                if app.popup.is_none() {
                    app.scroll_by(d * 60);
                }
            }
            Some(Event::Key(k, mods)) => app.key(k, mods),
            Some(Event::Action { .. }) => {} // the inbox is read every tick
            Some(Event::Drop { fd, .. }) => {
                let _ = sys::close(fd);
                app.say(
                    "Drop a web address in the address bar instead (files open in their own apps)",
                );
            }
            _ => dirty = false,
        }
        if app.quit {
            break;
        }
        dirty |= app.tick();
        // The desktop's theme, once a second: the chrome and dark-capable pages follow it.
        let now = now_ms();
        if now.saturating_sub(app.theme_checked) >= 1000 {
            app.theme_checked = now;
            let th = Theme::current();
            if th.dark != app.th.dark {
                app.th = th;
                dirty = true;
            }
        }
        dirty |= app.animate();
        let (moved, due) = app.animate_images(now_ms());
        dirty |= moved;
        frame_due = due;
        let now = now_ms();
        if app.flash.as_ref().is_some_and(|f| f.1 <= now) {
            app.flash = None;
            dirty = true;
        }
        // While loading, the progress bar moves.
        if app.fetch.busy() && now - last_draw > 120 {
            dirty = true;
        }
        if dirty {
            app.draw();
            last_draw = now;
        }
    }
    app.save_cookies();
    0
}
