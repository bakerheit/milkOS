//! Browser: web pages in tabs, in user space (docs/browser.md).
//!
//! Web pages are hostile input, so before anything else it sandboxes itself: the network
//! yes, files only in its own `~/Library/Browser` and `~/Downloads` (anything else through the
//! Powerbox; the desktop's settings file is read-only, for the search engine), no spawning.
//! Pages come through milkweb's HTTP client and the TLS 1.3 client
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
//! the window system's client protocol, and says more about script requests.

#![no_std]
#![no_main]

extern crate alloc;

mod draw;
mod frames;
mod js;
mod mem;
mod net;
mod request;
mod store;
mod sysfont;
mod tab;
mod web;
mod workers;

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use appkit::{Field, Hits, Theme};
use browserkit::address::{self, Internal, Target};
use browserkit::bookmarks::Bookmarks;
use browserkit::cookies::{Context, Jar, site_of};
use browserkit::font::Fonts;
use browserkit::engine::{ControlKind, ResourceKind};
use browserkit::history::Visits;
use browserkit::paint::Surface;
use browserkit::{download, pages};
use gui::{Event, Key, Mods, Window};
use imgdec::Image;
use milkweb::http::{Error, Request};
use milkweb::url::{Scheme, Url};
use rt::sys;

use net::{Fetcher, JobId, Kind, Outcome};
use webengine::FrameId;
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
pub const ID_PANEL_CONSOLE: u32 = 14;
pub const ID_PANEL_NET: u32 = 15;
pub const ID_NOTICE_A: u32 = 16;
pub const ID_NOTICE_B: u32 = 17;
pub const ID_TAB: u32 = 100; // + index
pub const ID_CLOSE_TAB: u32 = 200; // + index
pub const ID_MENU_ITEM: u32 = 300; // + index
pub const ID_OPTION: u32 = 400; // + index

/// What the toolbar's menu offers (label, shortcut).
pub const MENU: [(&str, &str); 17] = [
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
    ("JavaScript", ""),
    ("Console", "Cmd-Alt-J"),
    ("View Page Source", "Cmd-Alt-U"),
];
pub const MENU_JAVASCRIPT: usize = 14;
pub const MENU_CONSOLE: usize = 15;
pub const MENU_SOURCE: usize = 16;

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

    pub fn pixels(&self) -> usize {
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
    /// External scripts, by address (ImgState::Sheet holds a body).
    pub scripts: BTreeMap<String, ImgState>,
    /// Bytes of script bodies kept (the cache is dropped past 16 MB).
    pub script_bytes: usize,
    /// localStorage, by origin (js.rs).
    pub local: js::LocalStorage,
    /// JavaScript is on (the menu's switch, kept in ~/Library/Browser/settings).
    pub js_on: bool,
    /// The console panel is open.
    pub console_open: bool,
    /// The panel shows the Network list instead of the console.
    pub console_net: bool,
    /// The network policy's settings and the CORS preflight cache (script requests).
    pub net_cfg: browserkit::netpolicy::Config,
    pub preflight: alloc::sync::Arc<rt::thread::Mutex<browserkit::netpolicy::PreflightCache>>,
    /// Script requests running, by fetch job, and the answers on their way to pages.
    pub net_jobs: BTreeMap<JobId, request::NetJob>,
    pub net_inbox: request::Inbox,
    /// The pages' workers (workers.rs): the hub that keeps their rules, and what the App
    /// remembers of each.
    pub workers: workers::WorkerHub,
    pub worker_meta: BTreeMap<u32, workers::WMeta>,
    /// Frames being fetched: job -> (tab uid, page generation, frame).
    pub frame_jobs: BTreeMap<JobId, (u64, u64, FrameId)>,
    /// The restrictions a frame fetch will run under once it lands.
    pub pending_sandbox: BTreeMap<JobId, scriptnet::frame::sandbox::Sandbox>,
    /// Documents get numbers (for the session history).
    pub next_doc: u64,
    /// Where the pointer last was in the window (clicks and drags): the wheel goes there.
    pub pointer: (i32, i32),
    /// --log-input: say more on the serial line.
    pub log: bool,
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
    /// The system fonts for page text (the desktop's family, read each frame).
    pub sys: sysfont::SysText,
    pub quit: bool,
    pub dirty: bool,
    /// The machine's memory and what each page, frame and worker may have of it (mem.rs).
    pub mem: mem::MemMonitor,
    /// When the tabs were last looked over for memory (ms), the tab that was in front then, and
    /// the realms whose "Stop it?" was answered with Wait (key -> ask again after, ms).
    pub mem_scan_at: u64,
    pub mem_front_uid: u64,
    pub mem_waits: BTreeMap<u64, u64>,
    /// A bar over the top of the page: a memory warning, a question (Stop it?), a page that
    /// was cut short (Load more?).
    pub notice: Option<Notice>,
}

/// What a notice bar offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NoticeKind {
    /// Just a message: OK dismisses it.
    Info,
    /// "This page uses a lot of memory. Stop it?" about frame `frame` of the tab: Stop ends that
    /// document's scripts, Wait leaves it alone for a while.
    AskStop { frame: FrameId },
}

#[derive(Clone, Debug)]
pub struct Notice {
    /// The tab (its uid) and the page (its epoch) it is about: it goes away with either.
    pub tab: u64,
    pub epoch: u64,
    pub text: String,
    pub kind: NoticeKind,
}

impl Notice {
    pub fn info(tab: u64, epoch: u64, text: &str) -> Notice {
        Notice { tab, epoch, text: String::from(text), kind: NoticeKind::Info }
    }
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
pub(crate) fn hint(e: &Error) -> &'static str {
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
            Internal::NewTab => {
                pages::new_tab(&self.bookmarks, self.online, self.search_engine().name)
            }
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
            None,
            webengine::Limits::default(),
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
        let mut p = Page::new(
            address,
            url,
            html.as_bytes(),
            "text/html",
            PageKind::Error,
            None,
            webengine::Limits::default(),
        );
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
        // A script, stylesheet or picture that failed once is asked for again by the next page
        // (a dropped connection must not keep a site's script away for the whole session).
        if matches!(target, Target::Web(..) | Target::Source(_) | Target::Search(..)) {
            self.scripts.retain(|_, s| !matches!(s, ImgState::Failed));
            self.sheets.retain(|_, s| !matches!(s, ImgState::Failed));
            self.images.retain(|_, s| !matches!(s, ImgState::Failed));
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
                t.history.set_top_doc(0);
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
            Target::Source(u) => {
                // The page as text, fetched like any navigation (its cookies, redirects and
                // limits), shown without running anything.
                let address = address::source_address(&u);
                let id = self
                    .fetch
                    .start(Kind::Page, Request::get(&u), Context::TopLevel { from });
                let scroll = if push {
                    None
                } else {
                    self.tabs[ti].history.current().map(|e| e.scroll)
                };
                self.tabs[ti].load = Some(Load {
                    job: id,
                    address,
                    push,
                    fragment: None,
                    scroll,
                    started_ms: now_ms(),
                    source: true,
                    restore: None,
                });
                return;
            }
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
            t.history.top_same_doc(&a, false);
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
            source: false,
            restore: None,
        });
    }

    /// Back or forward to an entry whose page has to be fetched again: `plan` says where its
    /// frames were, `old_doc` is the document the entry named, `scroll` where the page was.
    pub fn go_restoring(
        &mut self,
        ti: usize,
        target: Target,
        plan: Vec<scriptnet::frame::history::Reload>,
        old_doc: u64,
        scroll: i32,
    ) {
        self.go(ti, target, false, None, None);
        let t = &mut self.tabs[ti];
        if let Some(l) = t.load.as_mut() {
            l.restore = Some((old_doc, plan));
            l.scroll = Some(scroll);
        } else if t.anchor.is_none() {
            // A built-in page shows at once: put the scroll back now.
            t.scroll = scroll;
            t.target = scroll;
        }
    }

    /// The search engine Settings > Browser picked (`search = KEY` in the desktop's settings
    /// file). Read each time it's needed, so a change made in Settings applies to the very
    /// next search without the Browser having to watch the file; the default engine if the
    /// file is missing, too big or doesn't say.
    pub fn search_engine(&self) -> &'static milkweb::search::Engine {
        let path = format!("{}/.config/milk/settings", self.profile.home);
        let text = match sys::stat_path(&path, true) {
            Ok(s) if s.size <= 64 << 10 => sys::read_file(&path).unwrap_or_default(),
            _ => Vec::new(),
        };
        let text = String::from_utf8_lossy(&text);
        milkweb::search::engine(milkweb::search::from_settings(&text))
    }

    /// What was typed (or sent by `open`) means: like `address::interpret`, with the chosen
    /// search engine for bare words.
    pub fn interpret_typed(&self, text: &str) -> Target {
        if milkweb::url::looks_like_search(text) {
            address::interpret_with(text, self.search_engine())
        } else {
            address::interpret(text)
        }
    }

    /// Open what was typed in the address bar.
    pub fn go_typed(&mut self, text: &str) {
        let t = self.interpret_typed(text);
        self.go(self.cur, t, true, None, None);
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
        self.traverse(d);
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
        let found = t.page.doc.find(q);
        t.found = found.iter().map(|m| m.0.clone()).collect();
        t.found_in = found.into_iter().map(|m| (m.1, m.2)).collect();
        t.found_at = 0;
        if self.log {
            rt::println!("[browser] find \"{q}\": {} matches", self.tabs[self.cur].found.len());
        }
        let t = &mut self.tabs[self.cur];
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
        // A match inside a frame: the frame (and the ones above it) scroll to it, and the page
        // to the frame.
        let (frame, frect) = {
            let t = self.tab();
            t.found_in
                .get(t.found_at)
                .map(|(f, r)| (*f, r.first().copied()))
                .unwrap_or((FrameId::TOP, None))
        };
        if !frame.is_top()
            && let Some(fr) = frect
        {
            let r = self.tab_mut().page.doc.reveal(frame, fr);
            self.tab_mut().page.laid_w = 0;
            // Where the match is now, in the page.
            let cur = self.cur;
            let sys = self.sys;
            let dark = self.th.dark;
            let (lw, lh) = self.page_view(&self.tabs[cur]);
            self.tabs[cur].page.doc.layout(lw, lh, dark, Fonts { text: Some(&sys) });
            self.tabs[cur].page.laid_w = lw;
            let q = self.finding.as_ref().map(|f| f.text.clone()).unwrap_or_default();
            let t = &mut self.tabs[cur];
            let found = t.page.doc.find(&q);
            t.found = found.iter().map(|m| m.0.clone()).collect();
            t.found_in = found.into_iter().map(|m| (m.1, m.2)).collect();
            if t.found_at >= t.found.len() {
                t.found_at = t.found.len().saturating_sub(1);
            }
            let rr = t.found.get(t.found_at).and_then(|m| m.first()).copied().unwrap_or(r);
            if rr.y < t.target || rr.y + rr.h > t.target + vh {
                t.target = (rr.y - vh / 3).clamp(0, max);
            }
            return;
        }
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
    /// engine applies the implicit-submission rules), after the constraints are checked and
    /// the page's submit listeners had their say.
    pub fn submit(&mut self, id: usize) {
        if self.tab().page.doc.control(id).is_none() {
            return;
        }
        let ti = self.cur;
        let (f, _) = web::split(id);
        let Some(sub) = self.with_doc_in(ti, f, |d, h| d.submit_form(h, id)) else {
            return;
        };
        self.submit_from(f, sub);
    }

    /// Go where a form submission of the top page says (a script's form.submit()).
    pub fn send_submission(&mut self, sub: browserkit::engine::Submission) {
        self.submit_from(FrameId::TOP, sub);
    }

    pub fn reset_form(&mut self, id: usize) {
        self.tab_mut().page.doc.reset(id);
    }

    /// Give control `id` the keyboard (or take it away with None).
    pub fn focus(&mut self, id: Option<usize>) {
        self.editing = None;
        if self.tab().focus != id || self.tab().page.doc.focused() != id {
            self.page_focus(id);
        }
        let t = self.tab_mut();
        t.focus = id;
        t.caret = id
            .and_then(|i| t.page.doc.control(i))
            .map_or(0, |c| c.value.chars().count());
        // Keep it in view (a frame's control: the frame scrolls to it first).
        if let Some(i) = id
            && !web::split(i).0.is_top()
            && let Some(c) = t.page.doc.control(i).map(|c| c.rect)
        {
            let f = web::split(i).0;
            t.page.doc.reveal(f, c);
            t.page.laid_w = 0;
        }
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
                let changed = t.page.doc.control(id).is_some_and(|c| c.value != v);
                t.page.doc.set_value(id, &v);
                t.caret = caret2;
                if changed {
                    self.control_input(id);
                }
                true
            }
            ControlKind::Radio if matches!(k, Key::Up | Key::Down | Key::Left | Key::Right) => {
                // Arrows move along the radio group (and choose).
                let doc = &self.tab().page.doc;
                let fr = web::split(id).0;
                let group: Vec<usize> = (0..doc.controls_in(fr).len())
                    .map(|i| web::join(fr, i))
                    .filter(|&i| {
                        let o = doc.control(i).unwrap();
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
                    self.focus(Some(next));
                    self.click_control(next);
                }
                true
            }
            ControlKind::Checkbox | ControlKind::Radio => match k {
                Key::Char(' ') => {
                    self.click_control(id);
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
                        if s != sel {
                            self.tab_mut().page.doc.select(id, s);
                            self.control_input(id);
                        }
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
        if n > 0 {
            self.control_input(id);
        }
    }

    /// A click on control `id` from the keyboard (Space on a checkbox, arrows in a radio
    /// group): through the page, so its click listeners run and the engine toggles it.
    fn click_control(&mut self, id: usize) {
        let ti = self.cur;
        let Some(r) = self.tab().page.doc.control_rect(id) else {
            self.tab_mut().page.doc.click_check(id);
            return;
        };
        let scroll = self.tab().scroll;
        let (x, y) = (r.x + r.w / 2, r.y + r.h / 2);
        let (f, point, fscroll) = self.tab().page.doc.pointer(x, y, scroll);
        self.with_doc_in(ti, f, |d, h| {
            d.click_in(f, h, point.0, point.1, fscroll, js::engine_mods(Mods::default()))
        });
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
        let ids: Vec<usize> = t
            .page
            .doc
            .focus_order()
            .into_iter()
            .filter(|&i| t.page.doc.control(i).is_some_and(|c| !c.disabled))
            .filter(|&i| {
                t.page
                    .doc
                    .control(i)
                    .is_some_and(|c| !c.readonly || is_button(c.kind))
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
        // The keys scroll the frame that has the keyboard.
        let ff = self.tab().page.doc.focused_frame();
        if !ff.is_top() {
            let t = self.tab_mut();
            let cur = t.page.doc.frame_scroll(ff).1;
            t.page.doc.set_frame_scroll(ff, cur.saturating_add(d));
            t.page.laid_w = 0;
            return;
        }
        let max = self.max_scroll(self.tab());
        let t = self.tab_mut();
        t.target = (t.target + d).clamp(0, max);
    }

    /// The wheel: the frames under the pointer (where it last was) scroll first, the page gets
    /// what they could not take.
    pub fn wheel(&mut self, dy: i32) {
        let (wx, wy) = self.pointer;
        let (top, _, _) = self.view();
        let t = self.tab();
        if t.page.doc.laid.is_some() && wy >= top {
            let (px, py) = (wx * 100 / t.zoom, (wy - top) * 100 / t.zoom + t.scroll);
            let scroll = t.scroll;
            let left = self.tab_mut().page.doc.wheel(px, py, scroll, dy);
            if left != dy {
                self.tab_mut().page.laid_w = 0;
                self.dirty = true;
            }
            if left != 0 {
                self.scroll_page_by(left);
            }
            return;
        }
        self.scroll_by(dy);
    }

    /// Scroll the page itself (whatever has the keyboard).
    fn scroll_page_by(&mut self, d: i32) {
        let max = self.max_scroll(self.tab());
        let t = self.tab_mut();
        t.target = (t.target + d).clamp(0, max);
    }

    pub fn scroll_to(&mut self, y: i32) {
        let ff = self.tab().page.doc.focused_frame();
        if !ff.is_top() {
            let t = self.tab_mut();
            t.page.doc.set_frame_scroll(ff, y);
            t.page.laid_w = 0;
            return;
        }
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
        // The page's keydown first: a script may cancel what the key would do.
        if !self.page_key(k, m, false) {
            self.page_key(k, m, true);
            return;
        }
        self.page_key_default(k, m);
        self.page_key(k, m, true);
    }

    /// What a key does in the page when no script cancelled it.
    fn page_key_default(&mut self, k: Key, m: Mods) {
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
                } else if self.tab().frames.busy() {
                    let ti = self.cur;
                    self.cancel_all_frames(ti);
                    self.say("Stopped the frames");
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
        self.pointer = (x, y);
        let id = self.hits.at(x, y);
        let popup_was = match self.popup {
            Some(Popup::Select(c)) => Some(Popup::Select(c)),
            _ => None,
        };
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
            Some(ID_NOTICE_A) => self.notice_answer(true),
            Some(ID_NOTICE_B) => self.notice_answer(false),
            Some(ID_PANEL_CONSOLE) => self.console_net = false,
            Some(ID_PANEL_NET) => self.console_net = true,
            Some(i) if (ID_MENU_ITEM..ID_MENU_ITEM + 100).contains(&i) => {
                self.popup = None;
                self.menu_item((i - ID_MENU_ITEM) as usize);
            }
            Some(i) if (ID_OPTION..ID_OPTION + 1000).contains(&i) => {
                self.popup = None;
                if let Some(Popup::Select(c)) = popup_was {
                    let o = (i - ID_OPTION) as usize;
                    if self.tab().page.doc.selected(c) != o {
                        self.tab_mut().page.doc.select(c, o);
                        self.control_input(c);
                    }
                }
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
        let ti = self.cur;
        // Hit testing needs the page as scripts left it.
        if self.tab().page.doc.needs_layout() {
            self.layout_current();
        }
        let t = self.tab();
        let (px, py) = (x * 100 / t.zoom, y * 100 / t.zoom + t.scroll);
        let scroll = t.scroll;
        // The frame under the pointer gets the click (and then the keyboard).
        let (fr, point, fscroll) = t.page.doc.pointer(px, py, scroll);
        // The page's mousedown, mouseup and click (left button): a listener may cancel what
        // the click does (and checkboxes are toggled by the engine).
        if button == 1 {
            let page_before = self.tab().page.address.clone();
            let ok = self.with_doc_in(ti, fr, |d, h| {
                d.click_in(fr, h, point.0, point.1, fscroll, js::engine_mods(m))
            });
            if ti != self.cur
                || self.tabs[ti].load.is_some()
                || self.tabs[ti].page.address != page_before
            {
                return; // the click navigated
            }
            if !ok {
                return;
            }
            if self.tab().page.doc.needs_layout() {
                self.layout_current();
            }
        }
        let t = self.tab();
        match t.page.doc.hit_test(px, py, t.scroll) {
            Some(Hit::Link(link, lf)) => {
                if button == 2 {
                    return;
                }
                if link.url.trim_start().to_ascii_lowercase().starts_with("javascript:") {
                    return; // the engine ran it (or scripts are off)
                }
                self.focus(None);
                self.tab_mut().page.doc.set_focused_frame(fr);
                let new_tab = m.meta || m.ctrl || button == 3;
                self.follow_in(
                    lf,
                    &link.url,
                    link.target.as_deref(),
                    new_tab,
                    m.alt || link.download,
                    true,
                );
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
                    // Toggled by the engine's click above (a right or middle click doesn't).
                    ControlKind::Checkbox | ControlKind::Radio => {}
                    ControlKind::Select => self.popup = Some(Popup::Select(id)),
                    k if is_button(k) && button == 1 => self.press(id),
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
            None => {
                self.focus(None);
                self.tab_mut().page.doc.set_focused_frame(fr);
            }
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
            MENU_JAVASCRIPT => self.toggle_javascript(),
            MENU_CONSOLE => self.console_open = !self.console_open,
            MENU_SOURCE => self.view_source(),
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
        // Script requests: heads and progress first, then the ends (which may carry the
        // head if it came in between), then the answers to the pages.
        changed |= self.net_tick();
        for (id, kind, out, cert, head, peer) in self.fetch.finished() {
            changed = true;
            if kind == Kind::Request {
                self.net_finished(id, out, head);
                continue;
            }
            self.finish(id, kind, out, cert, peer);
        }
        changed |= self.net_tick();
        changed |= self.workers_tick();
        changed |= self.mem_front();
        changed |= self.mem_tick();
        self.frames_sweep();
        changed |= self.frames_tick();
        for a in self.profile.take_inbox() {
            let target = self.interpret_typed(&a);
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
        peer: Option<Vec<u8>>,
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
                Outcome::Image(Ok(_)) | Outcome::Net(_) | Outcome::Refused(_) => {}
            }
            self.refresh_internal(Internal::Downloads);
            return;
        }
        if kind == Kind::Frame {
            self.frame_finished(id, out, cert, peer);
            return;
        }
        if kind == Kind::Script {
            let url = self
                .scripts
                .iter()
                .find(|(_, s)| matches!(s, ImgState::Loading(j) if *j == id))
                .map(|(u, _)| u.clone());
            if let Some(u) = url {
                let st = match out {
                    Outcome::Page(r) if r.status < 400 => {
                        rt::println!("[browser] script {u}: {} bytes", r.body.len());
                        self.script_bytes += r.body.len();
                        ImgState::Sheet(r.body)
                    }
                    Outcome::Page(r) => {
                        rt::println!("[browser] script {u}: {} {}", r.status, r.reason);
                        ImgState::Failed
                    }
                    _ => ImgState::Failed,
                };
                self.scripts.insert(u, st);
            }
            for ti in 0..self.tabs.len() {
                self.feed(ti);
            }
            // Keep the cache bounded (pages fetch again what they need).
            if self.script_bytes > self.mem.caps().script_cache {
                self.scripts.retain(|_, s| matches!(s, ImgState::Loading(_)));
                self.script_bytes = 0;
            }
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
                        Some(st) if self.image_pixels + st.pixels() <= self.mem.caps().image_pixels => {
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
            Outcome::Page(resp) if load.source => {
                self.finish_source(ti, load, resp, cert);
            }
            Outcome::Page(resp) => {
                let ct = String::from(resp.content_type());
                let addr = match &load.fragment {
                    Some(f) => format!("{}#{f}", resp.url.to_text()),
                    None => resp.url.to_text(),
                };
                let scripts = self.js_on.then(|| self.script_limits(mem::Kind::Page, true));
                let mut page = Page::new(
                    &addr,
                    Some(resp.url.clone()),
                    &resp.body,
                    &ct,
                    PageKind::Web,
                    scripts,
                    self.engine_limits(),
                );
                page.status = resp.status;
                page.ref_policy = resp
                    .header("referrer-policy")
                    .and_then(browserkit::netpolicy::ReferrerPolicy::parse_header)
                    .unwrap_or(browserkit::netpolicy::ReferrerPolicy::DEFAULT);
                let doc_id = self.next_doc_id();
                page.doc_id = doc_id;
                if resp.status >= 400 && page.title.is_empty() {
                    page.title = format!("{} {}", resp.status, resp.reason);
                }
                if resp.url.scheme == Scheme::Https {
                    page.cert = cert;
                }
                // Where it came from decides what its scripts may reach (private network rule).
                page.space = peer
                    .as_deref()
                    .and_then(browserkit::netpolicy::address_space_of_ip);
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
                    t.history.push_doc(&addr, doc_id);
                } else if let Some((old, _)) = &load.restore {
                    // Back or forward: the entry keeps its frames; its document is this one now.
                    t.history.rebind(*old, doc_id);
                    t.history.set_top_doc(doc_id);
                } else {
                    t.history.replace_doc(&addr, doc_id);
                }
                if let Some(e) = t.history.current_mut() {
                    e.title = title.clone();
                }
                t.show(page, 0);
                if let Some((_, plan)) = load.restore.clone() {
                    t.frames.restore = plan;
                }
                t.anchor = load.fragment.clone();
                if let Some(s) = load.scroll {
                    t.scroll = s;
                    t.target = s;
                }
                self.visits.add(&addr, &title, net::now_secs());
                // Its frames start loading before its scripts run: they hold its `load` back.
                self.sync_frames(ti);
                self.feed(ti);
                // Its scripts start now (inline ones run, external ones are fetched).
                self.run_page(ti);
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
            Outcome::Saved(..)
            | Outcome::SaveFailed(_)
            | Outcome::Image(_)
            | Outcome::Net(_)
            | Outcome::Refused(_) => {}
        }
    }

    /// A `view-source:` answer: its text, numbered, shown in the tab and run by nothing.
    fn finish_source(
        &mut self,
        ti: usize,
        load: Load,
        resp: milkweb::http::Response,
        cert: Option<browserkit::certinfo::CertInfo>,
    ) {
        let ct = String::from(resp.content_type());
        let (text, _) = webengine::charset::decode(&resp.body, Some(ct.as_str()).filter(|c| !c.is_empty()));
        let status = format!("HTTP {} {}", resp.status, resp.reason);
        let html = pages::view_source(&resp.url.to_text(), &status, resp.body.len(), &text);
        let addr = address::source_address(&resp.url);
        let mut page = Page::new(&addr, Some(resp.url.clone()), html.as_bytes(), "text/html", PageKind::Web, None, webengine::Limits::default());
        page.status = resp.status;
        page.doc_id = self.next_doc_id();
        if resp.url.scheme == Scheme::Https {
            page.cert = cert;
        }
        rt::println!(
            "[browser] view-source:{}: {} {}, {} bytes",
            resp.url,
            resp.status,
            resp.reason,
            resp.body.len()
        );
        let title = page.title.clone();
        let doc_id = page.doc_id;
        let t = &mut self.tabs[ti];
        if load.push {
            let scroll = t.scroll;
            if let Some(e) = t.history.current_mut() {
                e.scroll = scroll;
            }
            t.history.push_doc(&addr, doc_id);
        } else if let Some((old, _)) = &load.restore {
            t.history.rebind(*old, doc_id);
            t.history.set_top_doc(doc_id);
        } else {
            t.history.replace_doc(&addr, doc_id);
        }
        if let Some(e) = t.history.current_mut() {
            e.title = title;
        }
        t.show(page, 0);
        if let Some(s) = load.scroll {
            t.scroll = s;
            t.target = s;
        }
        self.save_cookies();
    }

    /// Show the source of the current page in a new tab (the menu, Cmd-Alt-U): the page is
    /// fetched again as a navigation of its own (same cookies and rules), as text.
    pub fn view_source(&mut self) {
        let t = self.tab();
        let Some(u) = t.page.url.clone().filter(|_| t.page.kind == PageKind::Web) else {
            self.say("Only a web page has a source to view");
            return;
        };
        if self.tabs.len() >= MAX_TABS {
            self.say("That's a lot of tabs: close one first");
            return;
        }
        let p = self.internal_page(Internal::NewTab);
        let mut nt = Tab::new(p);
        nt.history.push(Internal::NewTab.address());
        self.tabs.push(nt);
        let n = self.tabs.len() - 1;
        self.cur = n;
        self.go(n, Target::Source(u), true, None, None);
    }

    /// Give tab `ti`'s page (and its frames) the stylesheets and images they wait for that have
    /// come (or failed), and fetch the rest. Nothing over plain http on a secure page (mixed
    /// content).
    pub fn feed(&mut self, ti: usize) {
        let Some(t) = self.tabs.get(ti) else {
            return;
        };
        if t.page.kind != PageKind::Web {
            return;
        }
        for f in t.page.doc.all_ids() {
            self.feed_frame(ti, f);
        }
    }

    fn feed_frame(&mut self, ti: usize, f: FrameId) {
        let Some(t) = self.tabs.get(ti) else {
            return;
        };
        // The document's own address decides what is mixed content and whose cookies a
        // subresource may carry.
        let (secure, site) = if f.is_top() {
            (
                t.page
                    .url
                    .as_ref()
                    .is_some_and(|u| u.scheme == Scheme::Https),
                t.page
                    .url
                    .as_ref()
                    .map(|u| site_of(&u.host))
                    .unwrap_or_default(),
            )
        } else {
            match t.frames.meta.get(&f) {
                Some(m) => (
                    m.ancestor_url
                        .as_ref()
                        .is_some_and(|u| u.scheme == Scheme::Https)
                        || self.parent_chain(ti, f).iter().flatten().any(|u| u.scheme == Scheme::Https),
                    m.sub_site.clone(),
                ),
                None => return,
            }
        };
        // Supplying a stylesheet can ask for more (@import, background images): a few rounds.
        for _ in 0..8 {
            let pending = self.tabs[ti].page.doc.pending(f);
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
                            self.tabs[ti].page.doc.supply_image(f, id, Some(size));
                            self.tabs[ti].page.shown.insert(r.url.clone());
                            supplied = true;
                        }
                    }
                    Some(ImgState::Sheet(body)) => {
                        let body = body.clone();
                        self.tabs[ti]
                            .page
                            .doc
                            .supply_stylesheet(f, &r.url, Some(&body));
                        supplied = true;
                    }
                    Some(ImgState::Failed) => {
                        match r.kind {
                            ResourceKind::Image(id) => {
                                self.tabs[ti].page.doc.supply_image(f, id, None)
                            }
                            ResourceKind::Stylesheet => {
                                self.tabs[ti].page.doc.supply_stylesheet(f, &r.url, None)
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
            // External scripts.
            let mut scripted = false;
            for u in self.tabs[ti].page.doc.pending_scripts(f).into_iter().take(100) {
                match self.scripts.get(&u) {
                    Some(ImgState::Loading(_)) => waiting += 1,
                    Some(ImgState::Sheet(body)) => {
                        let body = body.clone();
                        self.tabs[ti].page.doc.supply_script(f, &u, Some(&body));
                        scripted = true;
                    }
                    Some(_) => {
                        self.tabs[ti].page.doc.supply_script(f, &u, None);
                        scripted = true;
                    }
                    None => {
                        let url = Url::parse(&u)
                            .ok()
                            .filter(|x| !(secure && x.scheme == Scheme::Http));
                        match url {
                            Some(x) => {
                                let ctx = Context::Subresource {
                                    page_site: site.clone(),
                                };
                                let id = self.fetch.start(Kind::Script, Request::get(&x), ctx);
                                self.scripts.insert(u.clone(), ImgState::Loading(id));
                                waiting += 1;
                            }
                            None => {
                                self.tabs[ti].page.doc.supply_script(f, &u, None);
                                scripted = true;
                            }
                        }
                    }
                }
            }
            if f.is_top() {
                self.tabs[ti].page.waiting = waiting;
            } else if let Some(m) = self.tabs[ti].frames.meta.get_mut(&f) {
                m.waiting = waiting;
            }
            if scripted {
                self.run_frame(ti, f);
            }
            if supplied {
                self.tabs[ti].page.laid_w = 0;
                // The load event may be due now.
                if self.tabs[ti].page.doc.scripting_in(f) {
                    self.tabs[ti].last_run = 0;
                }
            } else if !scripted {
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
    // Flags first, then the address.
    let mut log = false;
    let mut at = 1;
    while let Some(a) = args.get(at) {
        match a {
            "--log-input" => log = true,
            _ => break,
        }
        at += 1;
    }
    let home = args.env("HOME").unwrap_or("/Users/milk");
    let profile = Profile::new(home);
    profile.ensure();
    let online = internet_on();
    // The system fonts now (the rest of /System/Fonts stays readable: other families are
    // read when a page needs their characters, or when Settings picks another).
    gui::text::preload();
    // Before any page is fetched: lock down what a hostile page could reach.
    let spec = format!(
        "fs={}:rw fs={}:rw fs=/System/Fonts:r fs=/dev/wm:rw fs=/dev/clipboard:rw fs=/proc/net/tls:r fs=/proc/net/internet:r fs=/proc/meminfo:r fs=/proc/mempressure:r fs={home}/.config/milk/settings:r net=all spawn=0 clipboard=1",
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
    let profile_settings = profile.read("settings");
    let first = Page::new(
        "",
        None,
        b"",
        "text/html",
        PageKind::Internal(Internal::NewTab),
        None,
        webengine::Limits::default(),
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
        scripts: BTreeMap::new(),
        script_bytes: 0,
        local: BTreeMap::new(),
        js_on: js::javascript_setting(&profile_settings),
        console_open: false,
        console_net: false,
        net_cfg: browserkit::netpolicy::Config::default(),
        preflight: alloc::sync::Arc::new(rt::thread::Mutex::new(
            browserkit::netpolicy::PreflightCache::new(
                browserkit::netpolicy::Limits::default().preflight_cache_entries,
                browserkit::netpolicy::Limits::default().preflight_max_age,
            ),
        )),
        net_jobs: BTreeMap::new(),
        net_inbox: alloc::collections::VecDeque::new(),
        workers: workers::new_hub(webengine::DEFAULT_USER_AGENT, 0),
        worker_meta: BTreeMap::new(),
        frame_jobs: BTreeMap::new(),
        pending_sandbox: BTreeMap::new(),
        next_doc: 0,
        pointer: (0, 0),
        log,
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
        quit: false,
        dirty: true,
        mem: mem::MemMonitor::new(log),
        mem_scan_at: 0,
        mem_front_uid: 0,
        mem_waits: BTreeMap::new(),
        notice: None,
    };
    // Addresses left in the inbox belong to an earlier run; the argument is this one's.
    let _ = app.profile.take_inbox();
    let start = args.get(at).unwrap_or(HOME);
    app.go_typed(start);
    app.draw();
    let mut last_draw = now_ms();
    let mut frame_due: Option<u64> = None;
    loop {
        let busy = app.fetch.busy() || app.tab().scroll != app.tab().target;
        // Wait no longer than the next animation frame (pictures, or the page's scripts).
        let mut wait = if busy {
            16
        } else if app.workers_running() {
            // Workers talking: look at their mailboxes often, without spinning.
            8
        } else if app.net_active() {
            // Script requests going: look at them often, without spinning.
            30
        } else {
            400
        };
        if let Some(due) = frame_due {
            wait = wait.min(due.saturating_sub(now_ms()).max(1));
        }
        if let Some(due) = app.script_wake() {
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
            Some(Event::Drag { x, y }) => {
                app.pointer = (x, y);
                dirty = false;
            }
            Some(Event::Scroll(d)) => {
                if app.popup.is_none() {
                    app.wheel(d * 60);
                }
            }
            Some(Event::Key(Key::Char('j'), mods)) if mods.meta && mods.alt => {
                app.console_open = !app.console_open;
            }
            Some(Event::Key(Key::Char('u'), mods)) if mods.meta && mods.alt => app.view_source(),
            Some(Event::Key(k, mods)) => app.key(k, mods),
            Some(Event::Action { .. }) => {} // the inbox is read every tick
            // Settings changed the font or the theme: the chrome and the pages follow (the
            // next draw re-lays pages out in the new font; nothing is fetched again).
            Some(Event::Appearance { dark, .. }) => {
                app.th = Theme::for_dark(dark);
                if log {
                    rt::println!(
                        "[browser] appearance: font {} dark {dark}",
                        gui::text::current_family()
                    );
                }
            }
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
        dirty |= app.run_scripts();
        // The page's animation frames come with drawing.
        dirty |= app.tab().page.doc.wants_frame();
        dirty |= app.dirty;
        app.dirty = false;
        if app.quit {
            break;
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
