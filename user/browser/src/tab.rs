//! A tab: the page it shows, the one it is loading, its history, scroll, zoom, find-in-page
//! matches and the form control that has the keyboard.

use alloc::string::String;
use alloc::vec::Vec;

use browserkit::address::Internal;
use browserkit::certinfo::CertInfo;
use browserkit::engine::Rect;
use browserkit::netpolicy::{Limiter, Limits};
use milkweb::url::Url;
use scriptnet::frame::history::SessionHistory;
use webengine::{FrameId, NetRequest};

use crate::net::JobId;
use crate::web::Doc;

/// The zoom steps (percent), Cmd-+ and Cmd-- walk them.
pub const ZOOMS: [i32; 12] = [50, 67, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PageKind {
    Web,
    Internal(Internal),
    Error,
}

pub struct Page {
    /// What the address bar shows (with its #fragment).
    pub address: String,
    pub url: Option<Url>,
    pub title: String,
    pub doc: Doc,
    /// https: the connection's details (the padlock).
    pub cert: Option<CertInfo>,
    pub kind: PageKind,
    pub status: u16,
    /// The width the page is laid out at (0: lay it out again before drawing).
    pub laid_w: i32,
    /// The desktop font family it was laid out in (Settings can change it).
    pub laid_font: usize,
    /// ... and for a dark or light desktop.
    pub laid_dark: bool,
    /// The images it shows (addresses), for stepping animations.
    pub shown: alloc::collections::BTreeSet<String>,
    /// Stylesheets and images it is still waiting for.
    pub waiting: usize,
    /// The kind of address the page came from (None: unknown; the policy guesses from the
    /// name), which limits what its scripts may reach on the local network.
    pub space: Option<browserkit::netpolicy::AddressSpace>,
    /// The `Referrer-Policy` its response set: what its frames' requests send as `Referer`.
    pub ref_policy: browserkit::netpolicy::ReferrerPolicy,
    /// The document's number in the session history.
    pub doc_id: u64,
}

impl Page {
    /// A page; `scripts`: run its JavaScript (web pages, when JavaScript is on).
    pub fn new(
        address: &str,
        url: Option<Url>,
        html: &[u8],
        ct: &str,
        kind: PageKind,
        scripts: bool,
    ) -> Page {
        let base = url
            .as_ref()
            .map_or_else(|| String::from(address), |u| u.to_text());
        let doc = Doc::parse(html, &base, ct, scripts && kind == PageKind::Web);
        Page {
            address: String::from(address),
            url,
            title: doc.title(),
            doc,
            cert: None,
            kind,
            status: 200,
            laid_w: 0,
            laid_font: usize::MAX,
            laid_dark: false,
            shown: alloc::collections::BTreeSet::new(),
            waiting: 0,
            space: None,
            ref_policy: browserkit::netpolicy::ReferrerPolicy::DEFAULT,
            doc_id: 0,
        }
    }

    pub fn is_secure(&self) -> bool {
        self.cert.is_some()
            && self
                .url
                .as_ref()
                .is_some_and(|u| u.scheme == milkweb::url::Scheme::Https)
    }
}

/// A page on its way.
pub struct Load {
    pub job: JobId,
    /// What it is fetching (shown in the address bar meanwhile).
    pub address: String,
    /// A new history entry when it lands (not for back / forward / reload).
    pub push: bool,
    pub fragment: Option<String>,
    /// Back and forward return to where the page was scrolled.
    pub scroll: Option<i32>,
    pub started_ms: u64,
    /// `view-source:`: the answer is shown as text, never run.
    pub source: bool,
    /// Back or forward to an entry: the document it was (to rebind in the history) and where its
    /// frames were.
    pub restore: Option<(u64, alloc::vec::Vec<scriptnet::frame::history::Reload>)>,
}

pub struct Tab {
    pub history: SessionHistory,
    pub page: Page,
    pub load: Option<Load>,
    /// Page pixels from the top, and where smooth scrolling is heading.
    pub scroll: i32,
    pub target: i32,
    pub zoom: i32,
    /// The form control with the keyboard, and the caret in it (characters).
    pub focus: Option<usize>,
    pub caret: usize,
    /// Scroll to this `#name` once laid out.
    pub anchor: Option<String>,
    /// Find in page: the matches (each one's rectangles, page coordinates) and the current one.
    pub found: Vec<Vec<Rect>>,
    /// ... and for each, the frame it is in and the rectangles in that frame's own space.
    pub found_in: Vec<(FrameId, Vec<Rect>)>,
    pub found_at: usize,
    /// sessionStorage, per origin, for as long as the tab lives.
    pub session: alloc::collections::BTreeMap<String, browserkit::storage::Area>,
    /// The page's console (the last lines).
    pub console: alloc::collections::VecDeque<(browserkit::engine::ConsoleLevel, String)>,
    /// When background timers last ran (ms): background tabs run theirs once a second.
    pub last_run: u64,
    /// This tab for good (indexes move when tabs close), and which page it shows now (it
    /// changes with every `show`): what a script request's answer is checked against.
    pub uid: u64,
    pub epoch: u64,
    /// The page's script requests: how many may run, those waiting for their turn, those
    /// running (by the page's request id, with the fetch job), and the log.
    pub limiter: Limiter,
    /// Keyed by (frame, the request's id in that frame's document).
    pub net_wait: alloc::collections::BTreeMap<(u32, u32), NetRequest>,
    pub net_run: alloc::collections::BTreeMap<(u32, u32), crate::net::JobId>,
    /// The page's frames: what each one loaded from, and the loads under way.
    pub frames: crate::frames::FrameTable,
    pub net_log: alloc::collections::VecDeque<NetLogEntry>,
}

/// Console lines a tab keeps.
pub const CONSOLE_LINES: usize = 400;
/// Requests a tab's log keeps.
pub const NET_LOG_LINES: usize = 200;

/// One request a page's script made, as the Network list shows it.
#[derive(Clone, Debug)]
pub struct NetLogEntry {
    pub method: String,
    pub url: String,
    /// 0: none (it failed, or is still going).
    pub status: u16,
    /// How it ended: "ok", "failed", "blocked", "timeout", "cancelled", or "" while running.
    pub state: &'static str,
    pub ms: u64,
    pub bytes: usize,
}

static NEXT_UID: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(1);

impl Tab {
    pub fn new(page: Page) -> Tab {
        Tab {
            history: SessionHistory::new(),
            page,
            load: None,
            scroll: 0,
            target: 0,
            zoom: 100,
            focus: None,
            caret: 0,
            anchor: None,
            found: Vec::new(),
            found_in: Vec::new(),
            found_at: 0,
            session: alloc::collections::BTreeMap::new(),
            console: alloc::collections::VecDeque::new(),
            last_run: 0,
            uid: NEXT_UID.fetch_add(1, core::sync::atomic::Ordering::Relaxed),
            epoch: 0,
            limiter: Limiter::new(&Limits::default()),
            net_wait: alloc::collections::BTreeMap::new(),
            net_run: alloc::collections::BTreeMap::new(),
            frames: crate::frames::FrameTable::default(),
            net_log: alloc::collections::VecDeque::new(),
        }
    }

    pub fn title(&self) -> &str {
        if self.load.is_some() && self.page.title.is_empty() {
            return "Loading...";
        }
        if self.page.title.is_empty() {
            self.page
                .url
                .as_ref()
                .map_or("New Tab", |u| u.host.as_str())
        } else {
            &self.page.title
        }
    }

    /// Show `page` (the scroll back at the top, unless told otherwise).
    pub fn show(&mut self, page: Page, scroll: i32) {
        self.page = page;
        self.scroll = scroll.max(0);
        self.target = self.scroll;
        self.focus = None;
        self.caret = 0;
        self.found.clear();
        self.found_in.clear();
        self.found_at = 0;
        self.frames = crate::frames::FrameTable::default();
        self.console.clear();
        self.last_run = 0;
        // The old page's requests are over (the app cancels their jobs).
        self.epoch += 1;
        self.limiter.clear();
        self.net_wait.clear();
        self.net_run.clear();
        self.net_log.clear();
    }

    /// The page's script requests are outstanding.
    pub fn net_busy(&self) -> bool {
        self.limiter.busy()
    }

    pub fn zoom_step(&mut self, d: i32) {
        let i = ZOOMS.iter().position(|z| *z >= self.zoom).unwrap_or(4) as i32;
        let j = (i + d).clamp(0, ZOOMS.len() as i32 - 1) as usize;
        self.set_zoom(ZOOMS[j]);
    }

    pub fn set_zoom(&mut self, z: i32) {
        if z != self.zoom {
            // Keep the same part of the page at the top.
            self.zoom = z;
            self.page.laid_w = 0;
        }
    }
}
