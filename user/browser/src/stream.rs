//! A page that streams in. The page's job (net.rs) hands the body over as it arrives, up to a
//! cap that follows the machine's memory (`browserkit::memory::Caps::page`); the window's thread
//! keeps it, and while it comes
//!
//! * once 64 KiB are in, and then each time the page has doubled (and a quarter second has
//!   passed), the tab shows the page as far as it has come: parsed afresh from the bytes so far,
//!   cut at the last complete tag, without its scripts and frames, in no history yet. The top of
//!   a page that takes a long time is on screen at once, and where the reader scrolled stays;
//! * when the last byte is in, the whole page is parsed, its scripts run, and it is committed to
//!   the history as any page is (`App::finish`).
//!
//! A page longer than the cap is cut there: the part that came is shown, the console says so, and
//! a bar offers "Load more" (the page is fetched again with a cap four times as big, up to what
//! the machine can take). Nothing is dropped silently, and nothing in this path refuses a page for
//! its size: pages of any length come, and the parser, the layout and the memory plan bound what
//! is made of them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use browserkit::memory::human;
use webengine::ConsoleLevel;

use crate::tab::{Page, PageKind};
use crate::{App, Notice, NoticeKind, now_ms};

/// The page is shown for the first time when this many bytes are in.
pub const FIRST_PREVIEW: usize = 64 << 10;
/// ... and not more often than this (ms).
const PREVIEW_EVERY_MS: u64 = 250;
/// ... and not at all once this much has come (the page is parsed whole when it is in; a
/// parse of a prefix this big would only hold the window up).
const PREVIEW_MAX: usize = 4 << 20;

/// The prefix of `buf` that ends at a complete tag (the last `>` of the final 8 KiB), or all of
/// it when there is none. The rest of a half-written tag would show as text.
pub fn cut_at_tag(buf: &[u8]) -> usize {
    let from = buf.len().saturating_sub(8 << 10);
    buf[from..]
        .iter()
        .rposition(|&b| b == b'>')
        .map_or(buf.len(), |i| from + i + 1)
}

impl App {
    /// The most bytes a page's job takes in, after `more` times "Load more".
    pub fn page_cap(&self, more: u32) -> usize {
        let caps = self.mem.caps();
        caps.page
            .saturating_mul(1usize << (2 * more.min(10)))
            .min(caps.page_max)
            .max(caps.page)
    }

    /// What an image, a stylesheet and a script may be now (the memory plan's).
    pub fn fetch_caps(&self) -> (usize, usize, usize, usize) {
        let c = self.mem.caps();
        (c.image, c.style, c.script, c.page)
    }

    /// Take what the streaming pages have received; show a page that has come far enough.
    pub fn stream_tick(&mut self) -> bool {
        let mut changed = false;
        for ti in 0..self.tabs.len() {
            let Some(l) = self.tabs[ti].load.as_ref() else {
                continue;
            };
            if !l.streaming {
                continue;
            }
            let Some(sh) = self.fetch.shared(l.job) else {
                continue;
            };
            let (chunks, head) = {
                let mut s = sh.lock();
                s.stream.waiting = 0;
                (core::mem::take(&mut s.stream.chunks), s.page_head.clone())
            };
            let l = self.tabs[ti].load.as_mut().unwrap();
            let got = !chunks.is_empty();
            for c in chunks {
                l.buf.extend_from_slice(&c);
            }
            if l.head.is_none() {
                l.head = head;
            }
            let now = now_ms();
            let due = l.head.is_some()
                && l.buf.len() >= l.preview_next
                && l.preview_next <= PREVIEW_MAX
                && now.saturating_sub(l.preview_at) >= PREVIEW_EVERY_MS;
            if due {
                self.stream_preview(ti);
                changed = true;
            }
            changed |=
                got && ti == self.cur && self.tabs[ti].load.as_ref().is_some_and(|l| l.previewed);
        }
        changed
    }

    /// Show tab `ti`'s page as far as it has come.
    fn stream_preview(&mut self, ti: usize) {
        let mut limits = self.engine_limits();
        limits.max_frames = 0;
        let t = &mut self.tabs[ti];
        let Some(l) = t.load.as_mut() else { return };
        let Some(head) = l.head.clone() else { return };
        let ct = head
            .headers
            .iter()
            .find(|(k, _)| k == "content-type")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        let cut = cut_at_tag(&l.buf);
        let addr = match &l.fragment {
            Some(f) => format!("{}#{f}", head.url.to_text()),
            None => head.url.to_text(),
        };
        let mut page = Page::new(
            &addr,
            Some(head.url.clone()),
            &l.buf[..cut],
            &ct,
            PageKind::Web,
            None,
            limits,
        );
        page.status = head.status;
        page.preview = true;
        let had = l.buf.len();
        let first = !l.previewed;
        l.previewed = true;
        l.preview_at = now_ms();
        l.preview_next = (had * 4).max(FIRST_PREVIEW);
        // The first time, the page it replaces is left: its scroll is kept for the history and
        // the new one starts where a back or reload says (the top, else); after that, where
        // the reader has scrolled to stays.
        let (push, start) = (l.push, l.scroll.unwrap_or(0));
        let scroll = if first {
            if push {
                let s = t.scroll;
                if let Some(e) = t.history.current_mut() {
                    e.scroll = s;
                }
            }
            start
        } else {
            t.scroll
        };
        t.show(page, scroll);
        if self.log {
            rt::println!(
                "[browser] streaming {}: showing the top after {} KB (cut at {} KB)",
                head.url,
                had >> 10,
                cut >> 10
            );
        }
        self.feed(ti);
        self.dirty = true;
    }

    /// The page was cut at its cap: say so on the console and on the bar.
    pub fn announce_cut(&mut self, ti: usize, got: usize) {
        let msg = if got > 0 {
            format!(
                "This page is too big to show completely: the first {} are shown. Load more takes in four times as much.",
                human(got)
            )
        } else {
            String::from(
                "This page is too big to show completely: it has more elements than the Browser can keep for one page right now. Load more raises that to four times as many.",
            )
        };
        self.console_line(ti, "", ConsoleLevel::Warn, &msg);
        if ti == self.cur {
            let (uid, epoch) = (self.tabs[ti].uid, self.tabs[ti].epoch);
            self.notice = Some(Notice {
                tab: uid,
                epoch,
                text: String::from("This page is too big to show completely. Load more?"),
                kind: NoticeKind::LoadMore,
            });
            self.dirty = true;
        }
    }

    /// "Load more": the page again, with a window four times as big.
    pub fn load_more(&mut self, ti: usize) {
        let more = self.tabs[ti].page_more + 1;
        self.tabs[ti].more_next = Some(more);
        self.tabs[ti].page_more = more;
        self.reload_tab(ti);
    }

    /// Reload tab `ti` (not just the current one).
    pub fn reload_tab(&mut self, ti: usize) {
        let t = &self.tabs[ti];
        let a = t
            .history
            .current()
            .map_or_else(|| t.page.address.clone(), |e| e.url.clone());
        if a.is_empty() {
            return;
        }
        let scroll = t.scroll;
        if let Some(e) = self.tabs[ti].history.current_mut() {
            e.scroll = scroll;
        }
        let target = browserkit::address::interpret(&a);
        self.go(ti, target, false, None, None);
    }

    /// The bytes that came of a page whose job ended (or failed) without a finished answer,
    /// as a response to show.
    pub fn partial_response(load: &mut crate::tab::Load) -> Option<milkweb::http::Response> {
        let head = load.head.clone()?;
        if load.buf.is_empty() {
            return None;
        }
        let body: Vec<u8> = core::mem::take(&mut load.buf);
        Some(milkweb::http::Response {
            status: head.status,
            reason: head.reason,
            headers: head.headers,
            wire_bytes: body.len(),
            body,
            url: head.url,
            redirects: Vec::new(),
            encoding: None,
            streamed: true,
            cut: true,
        })
    }
}
