//! JavaScript in the Browser: the [`ScriptHost`] a page's scripts reach the app through (the
//! clock, cookies without HttpOnly ones, Web Storage per origin, the dialogs, the "page is
//! slow" bar), and the App's side of it: running each tab's scripts (timers in background
//! tabs once a second), animation frames before drawing, what scripts ask for (navigation,
//! history, scrolling, submissions, new tabs), their console (printed as `[browser] console
//! ...` lines and kept per tab for the panel), and the JavaScript switch.
//!
//! Scripts run on the main thread inside the sandbox; each tab has its own realm with the
//! engine's default budgets (64 MB of heap). A script that runs for more than two seconds
//! without a break gets the slow bar: Stop (or Esc) ends the page's scripts for good, Wait
//! lets it go on for another 10 seconds.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use appkit::{Field, Theme};
use browserkit::cookies::Jar;
use browserkit::engine::{
    ConsoleLevel, KeyInput, KeyMods, PageRequest, ScriptHost, Storage, TextMeasurer, Viewport,
};
use scriptnet::frame::nav::{self, Choice};
use webengine::FrameId;
use browserkit::address::Target;
use browserkit::font::Fonts;
use browserkit::storage::{self, Area};
use gui::{Event, Key, Mods, Window};
use milkweb::url::Url;
use rt::sys;
use rt::thread::Mutex;

use crate::sysfont::SysText;
use crate::tab::{PageKind, Tab};
use crate::{App, STATUS_H, TOP, now_ms};

/// The app's side of a page's scripts, made for one call.
pub struct JsHost<'a> {
    fonts: Fonts<'a>,
    vp: Viewport,
    scroll: i32,
    url: Option<Url>,
    jar: &'a Arc<Mutex<Jar>>,
    local: Option<&'a mut Area>,
    session: Option<&'a mut Area>,
    win: &'a mut Window,
    th: Theme,
    /// The user said Stop (or closed the window) on the slow bar.
    pub stopped: bool,
    pub quit: bool,
    /// "Wait": don't ask again before then (ms).
    wait_until: f64,
    log: bool,
    /// The slow bar has been shown during this call.
    asked: bool,
    /// `document.cookie` works (not for a third-party frame or an opaque origin).
    cookies_ok: bool,
    /// `alert`, `confirm` and `prompt` do nothing (a sandbox without `allow-modals`).
    no_modals: bool,
}

impl ScriptHost for JsHost<'_> {
    fn measurer(&self) -> &dyn TextMeasurer {
        &self.fonts
    }
    fn viewport(&self) -> Viewport {
        self.vp
    }
    fn scroll(&self) -> (i32, i32) {
        (0, self.scroll)
    }
    fn now(&self) -> f64 {
        sys::clock_ns(sys::CLOCK_MONOTONIC) as f64 / 1e6
    }
    fn time(&self) -> f64 {
        sys::clock_ns(sys::CLOCK_REALTIME) as f64 / 1e6
    }
    fn cookies(&mut self) -> String {
        match &self.url {
            Some(u) if self.cookies_ok => self.jar.lock().for_script(u, crate::net::now_secs()),
            _ => String::new(),
        }
    }
    fn set_cookie(&mut self, line: &str) {
        if !self.cookies_ok {
            return;
        }
        if let Some(u) = &self.url {
            let _ = self.jar.lock().set_from_script(u, line, crate::net::now_secs());
        }
    }
    fn storage(&mut self, session: bool) -> Option<&mut dyn Storage> {
        let a = if session {
            self.session.as_deref_mut()
        } else {
            self.local.as_deref_mut()
        };
        a.map(|a| a as &mut dyn Storage)
    }
    fn random(&mut self, out: &mut [u8]) -> bool {
        // The kernel's CSPRNG (getrandom(2)); asked again until all of `out` is filled.
        let mut at = 0;
        while at < out.len() {
            match sys::getrandom(&mut out[at..]) {
                Ok(n) if n > 0 => at += n,
                _ => return false,
            }
        }
        true
    }
    fn set_clipboard(&mut self, text: &str) -> bool {
        crate::clipboard_set(text);
        true
    }
    fn alert(&mut self, msg: &str) {
        if self.no_modals {
            return;
        }
        if self.log {
            rt::println!("[browser] alert: {}", browserkit::clean_line(msg));
        }
        self.dialog(msg, false, None);
    }
    fn confirm(&mut self, msg: &str) -> bool {
        if self.no_modals {
            return false;
        }
        if self.log {
            rt::println!("[browser] confirm: {}", browserkit::clean_line(msg));
        }
        self.dialog(msg, true, None).is_some()
    }
    fn prompt(&mut self, msg: &str, default: &str) -> Option<String> {
        if self.no_modals {
            return None;
        }
        if self.log {
            rt::println!("[browser] prompt: {}", browserkit::clean_line(msg));
        }
        self.dialog(msg, true, Some(default))
    }
    fn slow(&mut self, ran_ms: f64) -> bool {
        if self.stopped {
            return false;
        }
        let now = self.now();
        if now < self.wait_until {
            return true;
        }
        if !self.asked {
            self.asked = true;
            rt::println!("[browser] this page is slow: asking to stop it");
        }
        // The bar, and a look at the window's events (without waiting).
        let (stop, wait) = self.draw_slow_bar(ran_ms);
        while let Some(ev) = self.win.event(0) {
            match ev {
                Event::Down { x, y, .. } if stop.contains(x, y) => self.stopped = true,
                Event::Down { x, y, .. } if wait.contains(x, y) => {
                    self.wait_until = now + 10_000.0;
                }
                Event::Key(Key::Escape, _) => self.stopped = true,
                Event::Key(Key::Enter, _) => self.wait_until = now + 10_000.0,
                Event::Close => {
                    self.stopped = true;
                    self.quit = true;
                }
                _ => {}
            }
        }
        if self.stopped && self.log {
            rt::println!("[browser] stopped the page's scripts (it was too slow)");
        }
        !self.stopped
    }
}

impl JsHost<'_> {
    fn page_area(&self) -> (i32, i32, i32) {
        (TOP, self.win.w, (self.win.h - TOP - STATUS_H).max(1))
    }

    /// "This page is slow — Stop it?" over the top of the page: the Stop and Wait buttons.
    fn draw_slow_bar(&mut self, ran_ms: f64) -> (appkit::Rect, appkit::Rect) {
        let th = self.th;
        let (top, w, _) = self.page_area();
        let h = 40;
        self.win.fill(0, top, w, h, th.straw);
        self.win.fill(0, top + h - 1, w, 1, th.honey);
        let secs = (ran_ms / 1000.0) as i64;
        let msg = format!("This page is slow (a script has run for {secs} s). Stop it?");
        self.win.text(14, top + 12, &msg, th.ink);
        let sw = appkit::widgets::button_w("Stop", false);
        let ww = appkit::widgets::button_w("Wait", false);
        let stop = appkit::Rect::new(w - sw - ww - 24, top + 6, sw, 28);
        let wait = appkit::Rect::new(w - ww - 12, top + 6, ww, 28);
        let mut hits = appkit::Hits::default();
        appkit::widgets::button(&mut self.win, &th, &mut hits, stop, "Stop", appkit::widgets::Kind::Danger, 1, true, false);
        appkit::widgets::button(&mut self.win, &th, &mut hits, wait, "Wait", appkit::widgets::Kind::Secondary, 2, true, false);
        self.win.commit();
        (stop, wait)
    }

    /// alert / confirm / prompt over the page; waits for the user. Some(text) for OK (the
    /// prompt's text, else ""), None for Cancel.
    fn dialog(&mut self, msg: &str, cancel: bool, input: Option<&str>) -> Option<String> {
        let th = self.th;
        let (top, w, h) = self.page_area();
        let mut field = input.map(|d| Field::new(d, 2000));
        let bw = 420.min(w - 40).max(200);
        let lines = wrap(msg, bw - 32);
        let bh = 24 + lines.len() as i32 * 20 + if field.is_some() { 40 } else { 0 } + 56;
        let bx = (w - bw) / 2;
        let by = top + ((h - bh) / 3).max(10);
        loop {
            self.win.rrect(bx - 1, by - 1, bw + 2, bh + 2, 12, th.sand);
            self.win.rrect(bx, by, bw, bh, 12, th.milk);
            let mut y = by + 16;
            for l in &lines {
                self.win.text(bx + 16, y, l, th.espresso);
                y += 20;
            }
            let mut hits = appkit::Hits::default();
            if let Some(f) = &field {
                let r = appkit::Rect::new(bx + 16, y + 4, bw - 32, 28);
                appkit::widgets::field(&mut self.win, &th, &mut hits, r, f, "", true, true, 9);
                y += 40;
            }
            let okw = appkit::widgets::button_w("OK", false);
            let ok = appkit::Rect::new(bx + bw - okw - 16, y + 8, okw, 28);
            appkit::widgets::button(&mut self.win, &th, &mut hits, ok, "OK", appkit::widgets::Kind::Primary, 1, true, false);
            let cw = appkit::widgets::button_w("Cancel", false);
            let can = appkit::Rect::new(ok.x - cw - 10, y + 8, cw, 28);
            if cancel {
                appkit::widgets::button(&mut self.win, &th, &mut hits, can, "Cancel", appkit::widgets::Kind::Secondary, 2, true, false);
            }
            self.win.commit();
            let done_ok = || Some(field.as_ref().map(|f| f.text.clone()).unwrap_or_default());
            match self.win.event(1000) {
                Some(Event::Close) => {
                    self.quit = true;
                    return None;
                }
                Some(Event::Down { x, y, .. }) => match hits.at(x, y) {
                    Some(1) => return done_ok(),
                    Some(2) => return None,
                    _ => {}
                },
                Some(Event::Key(Key::Enter, _)) => return done_ok(),
                Some(Event::Key(Key::Escape, _)) => {
                    return if cancel { None } else { Some(String::new()) };
                }
                Some(Event::Key(k, m)) => {
                    if let Some(f) = field.as_mut() {
                        f.key(k, m);
                    }
                }
                _ => {}
            }
        }
    }
}

/// `s` in lines of at most `max` pixels (the UI font), at most 12 lines.
fn wrap(s: &str, max: i32) -> Vec<String> {
    let f = gui::text::Font::body(false);
    let mut out: Vec<String> = Vec::new();
    for para in s.split('\n') {
        let mut line = String::new();
        for w in para.split(' ') {
            let try_ = if line.is_empty() {
                String::from(w)
            } else {
                format!("{line} {w}")
            };
            if f.width(&try_) > max && !line.is_empty() {
                out.push(core::mem::take(&mut line));
                line = String::from(w);
            } else {
                line = try_;
            }
        }
        out.push(line);
        if out.len() >= 12 {
            break;
        }
    }
    out.truncate(12);
    out
}

/// A gui key as scripts see it (KeyboardEvent.key and .code).
pub fn key_input(k: Key, m: Mods) -> Option<KeyInput> {
    let (key, code) = match k {
        Key::Char(c) => {
            let code = if c.is_ascii_alphabetic() {
                format!("Key{}", c.to_ascii_uppercase())
            } else if c.is_ascii_digit() {
                format!("Digit{c}")
            } else if c == ' ' {
                String::from("Space")
            } else {
                String::new()
            };
            (String::from(c), code)
        }
        Key::Enter => (String::from("Enter"), String::from("Enter")),
        Key::Backspace => (String::from("Backspace"), String::from("Backspace")),
        Key::Tab => (String::from("Tab"), String::from("Tab")),
        Key::Escape => (String::from("Escape"), String::from("Escape")),
        Key::Up => (String::from("ArrowUp"), String::from("ArrowUp")),
        Key::Down => (String::from("ArrowDown"), String::from("ArrowDown")),
        Key::Left => (String::from("ArrowLeft"), String::from("ArrowLeft")),
        Key::Right => (String::from("ArrowRight"), String::from("ArrowRight")),
        Key::Delete => (String::from("Delete"), String::from("Delete")),
        Key::Home => (String::from("Home"), String::from("Home")),
        Key::End => (String::from("End"), String::from("End")),
        Key::PageUp => (String::from("PageUp"), String::from("PageUp")),
        Key::PageDown => (String::from("PageDown"), String::from("PageDown")),
        Key::Other(_) => return None,
    };
    Some(KeyInput {
        key,
        code,
        mods: engine_mods(m),
    })
}

pub fn engine_mods(m: Mods) -> KeyMods {
    KeyMods {
        shift: m.shift,
        ctrl: m.ctrl,
        alt: m.alt,
        meta: m.meta,
    }
}

/// The origin of a page address ("scheme://host:port"), for storage.
pub fn origin(u: &Url) -> String {
    let scheme = match u.scheme {
        milkweb::url::Scheme::Https => "https",
        _ => "http",
    };
    format!("{scheme}://{}:{}", u.host.to_ascii_lowercase(), u.port)
}

impl App {
    /// Run `f` with tab `ti`'s document and a host for the scripts of its top page.
    pub fn with_doc<R>(
        &mut self,
        ti: usize,
        f: impl FnOnce(&mut crate::web::Doc, &mut JsHost) -> R,
    ) -> R {
        self.with_doc_in(ti, FrameId::TOP, f)
    }

    /// Run `f` with tab `ti`'s document and a host for the scripts of frame `frame` (its
    /// viewport, its origin's storage, the cookies it may have, the dialogs it may show). Saves
    /// localStorage afterwards if a script changed it, and takes in what the scripts asked for.
    pub fn with_doc_in<R>(
        &mut self,
        ti: usize,
        frame: FrameId,
        f: impl FnOnce(&mut crate::web::Doc, &mut JsHost) -> R,
    ) -> R {
        let dark = self.th.dark;
        let sys: SysText = self.sys;
        // What this frame is: its viewport, scroll, address and rights.
        let (vp, scroll, url, origin, cookies_ok, no_modals) = {
            let t = &self.tabs[ti];
            if frame.is_top() {
                let (lw, lh) = self.page_view(t);
                (
                    Viewport {
                        dark,
                        ..Viewport::new(lw.max(1), lh.max(1))
                    },
                    t.scroll,
                    t.page.url.clone(),
                    t.page.url.as_ref().map(origin),
                    true,
                    false,
                )
            } else {
                let vp = t
                    .page
                    .doc
                    .viewport_of(frame)
                    .unwrap_or(Viewport::new(300, 150));
                let m = t.frames.meta.get(&frame);
                (
                    Viewport { dark, ..vp },
                    t.page.doc.frame_scroll(frame).1,
                    m.and_then(|m| m.ancestor_url.clone()),
                    m.and_then(|m| m.storage_key.clone()),
                    m.is_some_and(|m| m.cookies_ok),
                    m.is_some_and(|m| m.sandbox.no_modals()),
                )
            }
        };
        // localStorage of this origin, read from the profile the first time.
        if let Some(o) = &origin
            && !self.local.contains_key(o)
        {
            let text = self.profile.read(&format!("storage/{}", storage::file_name(o)));
            if self.local.len() > 64 {
                // Keep the memory bounded: forget saved, unchanged areas.
                self.local.retain(|_, a| a.dirty);
            }
            self.local.insert(o.clone(), Area::from_file(&text));
        }
        let App {
            tabs,
            win,
            fetch,
            local,
            th,
            log,
            ..
        } = self;
        let t: &mut Tab = &mut tabs[ti];
        let session = match &origin {
            Some(o) => {
                if !t.session.contains_key(o) && t.session.len() < 32 {
                    t.session.insert(o.clone(), Area::new());
                }
                t.session.get_mut(o)
            }
            None => None,
        };
        let mut host = JsHost {
            fonts: Fonts { text: Some(&sys) },
            vp,
            scroll,
            url,
            jar: &fetch.jar,
            local: origin.as_ref().and_then(|o| local.get_mut(o)),
            session,
            win,
            th: *th,
            stopped: false,
            quit: false,
            wait_until: 0.0,
            log: *log,
            asked: false,
            cookies_ok,
            no_modals,
        };
        let out = f(&mut t.page.doc, &mut host);
        let (stopped, quit) = (host.stopped, host.quit);
        drop(host);
        if stopped {
            // "Stop it" ends the scripts of the frame that was running; the rest of the page
            // carries on. (The page's own: all of them were its.)
            self.tabs[ti].page.doc.stop_scripts_in(frame);
            self.say("Stopped the page's scripts (it was too slow)");
        }
        if quit {
            self.quit = true;
        }
        if let Some(o) = &origin
            && let Some(a) = self.local.get_mut(o)
            && a.dirty
        {
            let _ = sys::mkdir(&format!("{}/storage", self.profile.lib), 0o700);
            let text = a.to_file();
            if self
                .profile
                .write(&format!("storage/{}", storage::file_name(o)), &text)
            {
                a.dirty = false;
            }
        }
        self.after_scripts(ti);
        out
    }

    /// Run the scripts of every tab that has work due (the current one now; others at most
    /// once a second). True if the current page may have changed.
    pub fn run_scripts(&mut self) -> bool {
        let now = now_ms();
        let mut changed = false;
        for ti in 0..self.tabs.len() {
            let t = &self.tabs[ti];
            if t.page.kind != PageKind::Web || !t.page.doc.any_scripting() {
                continue;
            }
            let due = t
                .page
                .doc
                .next_wake()
                .is_some_and(|w| w <= sys::clock_ns(sys::CLOCK_MONOTONIC) as f64 / 1e6);
            let fresh = t.last_run == 0;
            if !due && !fresh {
                continue;
            }
            // Background tabs: their timers at most once a second.
            if ti != self.cur && !fresh && now.saturating_sub(t.last_run) < 1000 {
                continue;
            }
            self.tabs[ti].last_run = now.max(1);
            let mut did = false;
            for f in self.tabs[ti].page.doc.all_ids() {
                if !self.tabs[ti].page.doc.scripting_in(f) {
                    continue;
                }
                did |= self.with_doc_in(ti, f, |d, h| d.run(f, h));
            }
            if did && ti == self.cur {
                changed = true;
            }
        }
        changed
    }

    /// Run the scripts of tab `ti`'s page (the top document) now.
    pub fn run_page(&mut self, ti: usize) -> bool {
        if ti >= self.tabs.len() || !self.tabs[ti].page.doc.scripting() {
            return false;
        }
        self.tabs[ti].last_run = now_ms().max(1);
        self.with_doc(ti, |d, h| d.run(FrameId::TOP, h))
    }

    /// Run the scripts of frame `f` of tab `ti` now.
    pub fn run_frame(&mut self, ti: usize, f: FrameId) -> bool {
        if ti >= self.tabs.len() || !self.tabs[ti].page.doc.scripting_in(f) {
            return false;
        }
        self.tabs[ti].last_run = now_ms().max(1);
        self.with_doc_in(ti, f, |d, h| d.run(f, h))
    }

    /// When the scripts of the current tab next want to run (ms, monotonic), or a frame.
    pub fn script_wake(&self) -> Option<u64> {
        let t = self.tab();
        if !t.page.doc.any_scripting() {
            return None;
        }
        if t.page.doc.wants_frame() {
            return Some(now_ms() + 16);
        }
        t.page.doc.next_wake().map(|w| w.max(0.0) as u64)
    }

    /// requestAnimationFrame callbacks of the current tab, before it is drawn.
    pub fn animation_frame(&mut self) {
        let ti = self.cur;
        for f in self.tabs[ti].page.doc.all_ids() {
            if self.tabs[ti].page.doc.wants_frame_in(f) {
                self.with_doc_in(ti, f, |d, h| d.frame_anim(f, h));
            }
        }
    }

    /// What tab `ti`'s scripts asked for and printed, and the focus they moved.
    pub fn after_scripts(&mut self, ti: usize) {
        if ti >= self.tabs.len() {
            return;
        }
        let ids = self.tabs[ti].page.doc.all_ids();
        for &f in &ids {
            let lines = self.tabs[ti].page.doc.take_console(f);
            if lines.is_empty() {
                continue;
            }
            let label = self.tabs[ti]
                .frames
                .meta
                .get(&f)
                .map(|m| m.label.clone())
                .unwrap_or_default();
            for l in lines {
                self.console_line(ti, &label, l.level, &l.text);
            }
        }
        // The focus as the page has it now (controls may have come and gone).
        let t = &mut self.tabs[ti];
        let f = t.page.doc.focused();
        if f != t.focus {
            t.focus = f;
            t.caret = f
                .and_then(|i| t.page.doc.control(i))
                .map_or(0, |c| c.value.chars().count());
        }
        if let Some(c) = t.focus.and_then(|i| t.page.doc.control(i)) {
            t.caret = t.caret.min(c.value.chars().count());
        }
        if t.page.doc.needs_layout() {
            t.page.laid_w = 0;
            // The page changed under an open select list: close it.
            if ti == self.cur {
                self.popup = self.popup.take().filter(|p| !matches!(p, crate::Popup::Select(_)));
            }
            self.dirty = true;
        }
        for &f in &ids {
            let reqs = self.tabs[ti].page.doc.take_requests(f);
            for r in reqs {
                if ti >= self.tabs.len() {
                    return;
                }
                self.page_request(ti, f, r);
            }
            if ti < self.tabs.len() {
                self.net_requests(ti, f);
            }
        }
        if ti < self.tabs.len() {
            self.sync_frames(ti);
        }
    }

    fn page_request(&mut self, ti: usize, frame: FrameId, r: PageRequest) {
        if self.log {
            rt::println!("[browser] script asks: {r:?}");
        }
        match r {
            PageRequest::Navigate { url, replace } => {
                if frame.is_top() {
                    let target = browserkit::address::interpret(&url);
                    let from = self.tabs[ti]
                        .page
                        .url
                        .as_ref()
                        .map(|u| browserkit::cookies::site_of(&u.host));
                    self.go(ti, target, !replace, from, None);
                } else {
                    self.script_navigates(ti, frame, frame, &url, replace);
                }
            }
            PageRequest::NavigateOther {
                target,
                url,
                replace,
            } => self.script_navigates(ti, frame, target, &url, replace),
            PageRequest::NavigateTarget { url, target } => {
                if ti == self.cur {
                    self.follow_in(frame, &url, Some(&target), false, false, false);
                }
            }
            PageRequest::Reload => {
                if ti == self.cur {
                    if frame.is_top() {
                        self.reload();
                    } else {
                        self.reload_frame(ti, frame);
                    }
                }
            }
            PageRequest::History(d) => {
                if ti == self.cur {
                    self.back_forward(d as isize);
                }
            }
            PageRequest::SetAddress { url, replace } => {
                if frame.is_top() {
                    let t = &mut self.tabs[ti];
                    if replace {
                        t.history.top_same_doc(&url, true);
                    } else {
                        let scroll = t.scroll;
                        if let Some(e) = t.history.current_mut() {
                            e.scroll = scroll;
                        }
                        t.history.top_same_doc(&url, false);
                    }
                    if let Ok(u) = Url::parse(&url) {
                        t.page.url = Some(u);
                    }
                    t.page.address = url;
                } else {
                    let t = &mut self.tabs[ti];
                    if let Some(m) = t.frames.meta.get(&frame) {
                        let path = m.path.clone();
                        t.history.frame_same_doc(&path, &url, replace);
                    }
                }
                self.dirty = true;
            }
            PageRequest::ScrollTo { y, .. } => {
                if frame.is_top() {
                    let max = self.max_scroll(&self.tabs[ti]);
                    let t = &mut self.tabs[ti];
                    t.target = y.clamp(0, max);
                } else {
                    self.tabs[ti].page.doc.set_frame_scroll(frame, y);
                    self.tabs[ti].page.laid_w = 0;
                }
                self.dirty = true;
            }
            PageRequest::Submit(sub) => {
                if ti == self.cur {
                    self.submit_from(frame, sub);
                }
            }
            PageRequest::Open(url) => {
                let sb = self.sandbox_of_frame(ti, frame);
                if sb.no_popups() {
                    self.frame_console(
                        ti,
                        frame,
                        ConsoleLevel::Error,
                        "Blocked opening a new tab from a sandboxed frame: the sandbox does not have 'allow-popups'.",
                    );
                } else if self.tabs.len() < crate::MAX_TABS {
                    let target = browserkit::address::interpret(&url);
                    let p = self.internal_page(browserkit::address::Internal::NewTab);
                    self.tabs.push(Tab::new(p));
                    let n = self.tabs.len() - 1;
                    self.go(n, target, true, None, None);
                    self.say("The page opened a new tab (Ctrl-Tab to go to it)");
                }
            }
            PageRequest::Title => {
                if frame.is_top() {
                    let t = &mut self.tabs[ti];
                    t.page.title = t.page.doc.title();
                    let title = t.page.title.clone();
                    if let Some(e) = t.history.current_mut() {
                        e.title = title;
                    }
                }
                self.dirty = true;
            }
            PageRequest::Invalid(msg) => {
                if ti == self.cur {
                    self.say(&msg);
                }
            }
        }
    }

    /// A script of `source` navigates frame `target` (itself, or another window) to `url`.
    fn script_navigates(&mut self, ti: usize, source: FrameId, target: FrameId, url: &str, replace: bool) {
        let base = self.tabs[ti]
            .page
            .doc
            .base_url_of(source)
            .or_else(|| self.tabs[ti].page.url.clone());
        let t = browserkit::address::resolve(base.as_ref(), url);
        let sb = self.sandbox_of_frame(ti, source);
        let choice = Choice::Frame(target.0);
        let verdict = {
            let tree = crate::frames::tree_of(&self.tabs[ti].page.doc);
            nav::allowed(&tree, source.0, &choice, sb, false)
        };
        if let Err(d) = verdict {
            self.refuse_navigation(ti, source, d);
            return;
        }
        let Target::Web(u, fragment) = t else {
            if let Target::Internal(_) | Target::Search(..) = t {
                let from = None;
                if target.is_top() {
                    self.go(ti, t, !replace, from, None);
                }
            }
            return;
        };
        if target.is_top() {
            let from = self.referrer_site(ti, source);
            self.go(ti, Target::Web(u, fragment), !replace, from, None);
        } else {
            self.navigate_frame(
                ti,
                target,
                crate::frames::Nav {
                    url: u,
                    fragment,
                    post: None,
                    replace,
                    source,
                    how: None,
                },
            );
        }
    }

    /// A key for the page (focused element or body), before the app acts on it: false if a
    /// script cancelled it.
    pub fn page_key(&mut self, k: Key, m: Mods, up: bool) -> bool {
        let ti = self.cur;
        let ff = self.tabs[ti].page.doc.focused_frame();
        if !self.tabs[ti].page.doc.scripting_in(ff) {
            return true;
        }
        let Some(ki) = key_input(k, m) else {
            return true;
        };
        self.with_doc_in(ti, ff, |d, h| d.key(ff, h, &ki, up))
    }

    /// The user changed control `id` (a combined index) of the current page.
    pub fn control_input(&mut self, id: usize) {
        let ti = self.cur;
        let (f, _) = crate::web::split(id);
        if self.tabs[ti].page.doc.scripting_in(f) {
            self.with_doc_in(ti, f, |d, h| d.input(h, id));
        }
    }

    /// The focus moved to `id` (or away): the page's focus events, in the frame the control
    /// is in (and in the frame it leaves).
    pub fn page_focus(&mut self, id: Option<usize>) {
        let ti = self.cur;
        let old = self.tabs[ti].page.doc.focused_frame();
        match id {
            Some(i) => {
                let (f, _) = crate::web::split(i);
                if f != old {
                    self.with_doc_in(ti, old, |d, h| d.set_focus(h, None));
                }
                self.with_doc_in(ti, f, |d, h| d.set_focus(h, Some(i)));
            }
            None => {
                self.with_doc_in(ti, old, |d, h| d.set_focus(h, None));
            }
        }
    }

    /// Turn JavaScript on or off (kept in ~/Library/Browser/settings); the page reloads.
    pub fn toggle_javascript(&mut self) {
        self.js_on = !self.js_on;
        self.profile.write(
            "settings",
            if self.js_on {
                "javascript=on\n"
            } else {
                "javascript=off\n"
            },
        );
        rt::println!("[browser] javascript {}", if self.js_on { "on" } else { "off" });
        self.say(if self.js_on {
            "JavaScript is on (the page reloads)"
        } else {
            "JavaScript is off (the page reloads)"
        });
        if self.tab().page.kind == PageKind::Web {
            self.reload();
        }
    }
}

/// Is JavaScript on in the profile's settings (on unless it says off)?
pub fn javascript_setting(text: &str) -> bool {
    !text
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case("javascript=off"))
}

/// The local storage areas, by origin.
pub type LocalStorage = BTreeMap<String, Area>;
