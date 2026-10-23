//! A tab: the page it shows, the one it is loading, its history, scroll, zoom, find-in-page
//! matches and the form control that has the keyboard.

use alloc::string::String;
use alloc::vec::Vec;

use browserkit::address::Internal;
use browserkit::certinfo::CertInfo;
use browserkit::engine::Rect;
use browserkit::history::History;
use milkweb::url::Url;

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
    /// Stylesheets and images it is still waiting for.
    pub waiting: usize,
}

impl Page {
    pub fn new(address: &str, url: Option<Url>, html: &[u8], ct: &str, kind: PageKind) -> Page {
        let base = url
            .as_ref()
            .map_or_else(|| String::from(address), |u| u.to_text());
        let doc = Doc::parse(html, &base, ct);
        Page {
            address: String::from(address),
            url,
            title: doc.title(),
            doc,
            cert: None,
            kind,
            status: 200,
            laid_w: 0,
            waiting: 0,
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
}

pub struct Tab {
    pub history: History,
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
    pub found_at: usize,
}

impl Tab {
    pub fn new(page: Page) -> Tab {
        Tab {
            history: History::new(),
            page,
            load: None,
            scroll: 0,
            target: 0,
            zoom: 100,
            focus: None,
            caret: 0,
            anchor: None,
            found: Vec::new(),
            found_at: 0,
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
        self.found_at = 0;
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
