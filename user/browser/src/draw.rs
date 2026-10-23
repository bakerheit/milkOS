//! Drawing the window: the tab strip, the toolbar (back, forward, reload / stop, the address
//! bar with its padlock and bookmark ribbon, the menu), the page (painted by browserkit into
//! an offscreen surface at the tab's zoom, then scaled into the window), the status line or
//! find bar, and the popups (the menu, the padlock's panel, a select's choices).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use appkit::Rect as ARect;
use appkit::widgets;
use browserkit::engine::{Color, ControlId, ControlKind, ImageId, Rect};
use browserkit::paint::{self, Glyphs, Painter, Surface};
use browserkit::{download, font};
use gui::Window;
use imgdec::Image;

use crate::tab::PageKind;
use crate::web::Doc;
use crate::*;

const RADIUS: i32 = 6;

/// Characters of Unifont that fit in `px`.
fn fit_chars(px: i32) -> usize {
    (px / 8).max(0) as usize
}

/// `s` cut to `n` characters with "..." at the end.
fn ellipsize(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return String::from(s);
    }
    if n <= 3 {
        return s.chars().take(n).collect();
    }
    let mut o: String = s.chars().take(n - 3).collect();
    o.push_str("...");
    o
}

/// The end of `s` that fits in `n` characters ("..." in front).
fn tail(s: &str, n: usize) -> String {
    let c = s.chars().count();
    if c <= n {
        return String::from(s);
    }
    let skip = c - n + 3;
    let mut o = String::from("...");
    o.extend(s.chars().skip(skip));
    o
}

fn hline(w: &mut Window, x: i32, y: i32, len: i32, c: Color) {
    w.fill(x, y, len, 1, c);
}

/// The page's form controls, images and focus, for browserkit's painter.
struct PagePainter<'a> {
    doc: &'a Doc,
    images: &'a BTreeMap<String, ImgState>,
    focus: Option<usize>,
    caret: usize,
}

const F_FOCUS: Color = 0xEBB650;
const F_HINT: Color = 0x9C8A76;
const F_CHECK: Color = 0x3A2E28;

impl Painter for PagePainter<'_> {
    fn image(&self, id: ImageId) -> Option<&Image> {
        match self.images.get(self.doc.image_url(id)) {
            Some(ImgState::Ready(i)) => Some(i),
            _ => None,
        }
    }

    /// Draw what goes inside a control's content box: the engine painted its background and
    /// border already (the page's CSS, or the built-in look).
    fn control(&mut self, s: &mut Surface, r: Rect, id: ControlId) {
        let i = id.0 as usize;
        let Some(c) = self.doc.control(i) else {
            return;
        };
        let focused = self.focus == Some(i);
        let sc = font::scale(&c.font);
        let g = Glyphs {
            scale: sc,
            bold: c.font.weight >= 600,
            italic: c.font.italic,
        };
        let ink = if c.color >> 24 == 0 {
            F_CHECK
        } else {
            c.color & 0xFF_FFFF
        };
        let ink = if c.disabled { F_HINT } else { ink };
        let cell = font::CELL_W * sc;
        let line = font::CELL_H * sc;
        if focused {
            // The focus ring, just outside the box.
            let o = Rect::new(r.x - 3, r.y - 3, r.w + 6, r.h + 6);
            s.border(o, 2, F_FOCUS);
        }
        let old = s.clip;
        s.clip = paint::intersect(Rect::new(r.x + s.dx, r.y + s.dy, r.w, r.h), old);
        match c.kind {
            ControlKind::Text | ControlKind::Password => {
                let shown: String = if c.kind == ControlKind::Password {
                    c.value.chars().map(|_| '*').collect()
                } else {
                    c.value.clone()
                };
                let cols = (r.w / cell).max(1) as usize;
                let n = shown.chars().count();
                let caret = self.caret.min(n);
                // Scroll the text so the caret shows.
                let first = if focused {
                    caret.saturating_sub(cols.saturating_sub(1))
                } else {
                    0
                };
                let vis: String = shown.chars().skip(first).take(cols + 1).collect();
                let ty = r.y + (r.h - line) / 2;
                if shown.is_empty() && !c.placeholder.is_empty() {
                    s.glyphs(r.x, ty, &c.placeholder, g, F_HINT);
                } else {
                    s.glyphs(r.x, ty, &vis, g, ink);
                }
                if focused {
                    let cx = r.x + (caret - first) as i32 * cell;
                    s.fill(Rect::new(cx, ty, 2, line), ink);
                }
            }
            ControlKind::TextArea => {
                let cols = (r.w / cell).max(1) as usize;
                let mut lines: Vec<String> = Vec::new();
                for l in c.value.split('\n') {
                    let ch: Vec<char> = l.chars().collect();
                    if ch.is_empty() {
                        lines.push(String::new());
                    }
                    for part in ch.chunks(cols) {
                        lines.push(part.iter().collect());
                    }
                }
                let step = line + 2;
                let rows = (r.h / step).max(1) as usize;
                let first = lines.len().saturating_sub(rows);
                for (k, l) in lines.iter().skip(first).enumerate() {
                    s.glyphs(r.x, r.y + k as i32 * step, l, g, ink);
                }
                if c.value.is_empty() && !c.placeholder.is_empty() {
                    s.glyphs(r.x, r.y, &c.placeholder, g, F_HINT);
                }
                if focused {
                    let last = lines.last().map_or(0, |l| l.chars().count()) as i32;
                    let row = (lines.len().saturating_sub(first) as i32 - 1).max(0);
                    s.fill(Rect::new(r.x + last * cell, r.y + row * step, 2, line), ink);
                }
            }
            ControlKind::Checkbox => {
                if c.checked {
                    // A tick, sized to the box.
                    let u = (r.w.min(r.h) / 8).max(1);
                    let (x0, y0) = (r.x + r.w / 2 - 4 * u, r.y + r.h / 2 - u);
                    for k in 0..3 {
                        s.fill(Rect::new(x0 + k * u, y0 + k * u, 2 * u, 2 * u), ink);
                    }
                    for k in 0..5 {
                        s.fill(
                            Rect::new(x0 + (3 + k) * u, y0 + (1 - k) * u, 2 * u, 2 * u),
                            ink,
                        );
                    }
                }
            }
            ControlKind::Radio => {
                if c.checked {
                    let rr = (r.w.min(r.h) / 4).max(2);
                    s.disc(r.x + r.w / 2, r.y + r.h / 2, rr, ink);
                }
            }
            ControlKind::Select => {
                let label = c
                    .options
                    .iter()
                    .find(|o| o.selected)
                    .or(c.options.first())
                    .map_or("", |o| o.label.as_str());
                let label = ellipsize(label, ((r.w - 16) / cell).max(1) as usize);
                s.glyphs(r.x, r.y + (r.h - line) / 2, &label, g, ink);
                // A small down arrow at the right.
                let (ax, ay) = (r.x + r.w - 10, r.y + r.h / 2 - 2);
                for k in 0..4 {
                    s.fill(Rect::new(ax + k, ay + k, 8 - 2 * k, 1), ink);
                }
            }
            ControlKind::ImageSubmit => {
                if let Some(img) = c.image.and_then(|id| self.image(id)) {
                    s.image(r, img);
                }
            }
            ControlKind::Submit | ControlKind::Reset | ControlKind::Button | ControlKind::File => {
                if !c.content_drawn {
                    let label = if c.kind == ControlKind::File {
                        "Choose a file"
                    } else {
                        c.label.as_str()
                    };
                    let tw = font::cells(label) * cell;
                    s.glyphs(r.x + (r.w - tw) / 2, r.y + (r.h - line) / 2, label, g, ink);
                }
            }
        }
        s.clip = old;
    }
}

impl App {
    /// Lay out the current tab's page if its width changed (zoom, resize, an image came).
    fn layout_current(&mut self) {
        let (lw, lh) = self.page_view(self.tab());
        let t = &self.tabs[self.cur];
        if t.page.laid_w == lw && t.page.doc.laid.is_some() {
            // A link to a place on this page: no new layout, just the scroll.
            if t.anchor.is_some() {
                let max = self.max_scroll(self.tab());
                let t = self.tab_mut();
                if let Some(y) = t.anchor.take().and_then(|a| t.page.doc.anchor(&a)) {
                    t.target = y.clamp(0, max);
                }
            }
            return;
        }
        // Laying out finds CSS background images, and cached ones are supplied at once: a
        // few rounds until the page has what it can have now.
        let cur = self.cur;
        for _ in 0..4 {
            let t = &mut self.tabs[cur];
            t.page.doc.layout(lw, lh);
            t.page.laid_w = lw;
            self.feed(cur);
            if self.tabs[cur].page.laid_w == lw {
                break;
            }
        }
        self.tabs[cur].page.laid_w = lw;
        if let Some(f) = &self.finding {
            let q = f.text.clone();
            self.run_find(&q);
        }
        let max = self.max_scroll(self.tab());
        let t = self.tab_mut();
        if let Some(a) = t.anchor.clone() {
            if let Some(y) = t.page.doc.anchor(&a) {
                t.scroll = y.clamp(0, max);
                t.target = t.scroll;
            }
            // Stylesheets and images still coming move things: aim again after each layout.
            if t.page.waiting == 0 {
                t.anchor = None;
            }
        }
        t.scroll = t.scroll.min(max);
        t.target = t.target.min(max);
    }

    pub fn draw(&mut self) {
        self.layout_current();
        self.hits.clear();
        let th = self.th;
        let (w, h) = (self.win.w, self.win.h);
        self.draw_page();
        self.draw_tabs(w);
        self.draw_toolbar(w);
        // The status line or the find bar.
        let sy = h - STATUS_H;
        self.win.fill(0, sy, w, STATUS_H, th.cream);
        hline(&mut self.win, 0, sy, w, th.sand);
        if self.finding.is_some() {
            self.draw_find(sy, w);
        } else {
            let text = self.status_text();
            let zoom = self.tab().zoom;
            let right = if zoom != 100 {
                format!("{zoom}%")
            } else {
                String::new()
            };
            let room = fit_chars(w - 24 - right.len() as i32 * 8 - 16);
            self.win.text(10, sy + 3, &ellipsize(&text, room), th.cocoa);
            if !right.is_empty() {
                self.win
                    .text(w - 10 - right.len() as i32 * 8, sy + 3, &right, th.cocoa);
            }
        }
        self.draw_popup(w, h);
        let title = String::from(self.tab().title());
        if title != self.shown_title {
            self.win.set_title(&title);
            self.shown_title = title;
        }
        self.win.commit();
    }

    fn status_text(&self) -> String {
        if let Some((m, _)) = &self.flash {
            return m.clone();
        }
        if let Some(d) = self
            .downloads
            .iter()
            .rev()
            .find(|d| matches!(d.state, DlState::Running))
        {
            return format!(
                "Downloading {}",
                download::progress_line(&d.name, d.got, d.total)
            );
        }
        let t = self.tab();
        if let Some(l) = &t.load {
            let host = Url::parse(&l.address).map(|u| u.host).unwrap_or_default();
            let got = self.fetch.shared(l.job).map_or(0, |s| s.lock().got);
            return if got > 0 {
                format!("Loading {host}: {}", download::human_size(got))
            } else {
                format!("Waiting for {host}...")
            };
        }
        if let Some(id) = t.focus
            && let Some(c) = t.page.doc.control(id)
        {
            return String::from(match c.kind {
                ControlKind::TextArea => {
                    "Typing in a text box (Esc to leave it, Tab to the next field)"
                }
                k if crate::web::is_text(k) => {
                    "Typing in a field (Enter sends the form, Tab to the next field)"
                }
                ControlKind::Select => "Up and Down choose, Space opens the list",
                _ => "Space presses it, Tab moves on",
            });
        }
        match t.page.kind {
            PageKind::Web if t.page.status >= 400 => {
                format!("The server said {} for this page", t.page.status)
            }
            PageKind::Web => {
                if t.page.waiting > 0 {
                    format!("Loading {} stylesheet(s) and image(s)...", t.page.waiting)
                } else {
                    String::from(if t.page.is_secure() {
                        "Secure connection"
                    } else {
                        ""
                    })
                }
            }
            _ => String::new(),
        }
    }

    fn draw_page(&mut self) {
        let (top, vw, vh) = self.view();
        let t = &self.tabs[self.cur];
        let (pw, ph) = self.page_view(t);
        self.surface.resize(pw, ph);
        let mut p = PagePainter {
            doc: &t.page.doc,
            images: &self.images,
            focus: t.focus,
            caret: t.caret,
        };
        let mut marks: Vec<Rect> = Vec::new();
        let mut current = None;
        if self.finding.is_some() {
            for (k, m) in t.found.iter().enumerate() {
                if k == t.found_at {
                    current = Some(marks.len());
                }
                marks.extend_from_slice(m);
            }
        }
        paint::paint_list(
            &mut self.surface,
            t.page.doc.display_list(),
            t.scroll,
            &mut p,
            &marks,
            current,
            self.link,
        );
        // A scroll bar when the page is longer than the view.
        let total = t.page.doc.height().max(1);
        if total > ph {
            let bar_h = ((ph as i64 * ph as i64 / total as i64) as i32)
                .max(24)
                .min(ph);
            let by = (t.scroll as i64 * (ph - bar_h) as i64 / (total - ph).max(1) as i64) as i32;
            self.surface
                .rrect(Rect::new(pw - 7, by + 2, 5, bar_h - 4), 2, 0xC4AE8F);
        }
        let ww = self.win.w;
        let wh = self.win.h;
        let px = self.win.pixels();
        paint::scale_into(&self.surface, px, ww, wh, 0, top, vw, vh);
        // A thin progress line under the toolbar while loading.
        if let Some(l) = &t.load {
            let (got, total) = self.fetch.shared(l.job).map_or((0, None), |s| {
                let s = s.lock();
                (s.got, s.total)
            });
            let frac = match total {
                Some(n) if n > 0 => (got as i64 * 1000 / n as i64) as i32,
                // Unknown length: creep towards the end.
                _ => {
                    let el = (now_ms() - l.started_ms) as i32;
                    900 - 900 * 2000 / (el + 2000)
                }
            };
            let pw = (vw as i64 * frac.clamp(20, 1000) as i64 / 1000) as i32;
            self.win.fill(0, top, pw, 3, gui::HONEY);
        }
    }

    fn draw_tabs(&mut self, w: i32) {
        let th = self.th;
        self.win.fill(0, 0, w, TAB_H, th.oat);
        let n = self.tabs.len() as i32;
        let avail = w - 16 - 36;
        let tw = (avail / n.max(1)).clamp(48, 220);
        for i in 0..self.tabs.len() {
            let x = 8 + i as i32 * tw;
            if x + 24 > w - 36 {
                break;
            }
            let cur = i == self.cur;
            let r = ARect::new(x, 4, tw - 4, TAB_H - 4);
            if cur {
                self.win.rrect(r.x, r.y, r.w, r.h + 6, RADIUS, th.cream);
            } else {
                self.win.rrect(r.x, r.y + 2, r.w, r.h - 4, RADIUS, th.sand);
            }
            self.hits.add(r, ID_TAB + i as u32);
            let t = &self.tabs[i];
            let mut tx = x + 10;
            if t.load.is_some() {
                self.win.disc(tx + 3, TAB_H / 2 + 2, 3, gui::HONEY);
                tx += 12;
            } else if t.page.is_secure() {
                widgets::padlock(&mut self.win, tx, TAB_H / 2 - 4, th.good);
                tx += 14;
            }
            let room = fit_chars(x + tw - 26 - tx);
            let title = ellipsize(t.title(), room);
            let c = if cur { th.espresso } else { th.cocoa };
            if cur {
                self.win.bold(tx, 10, &title, c);
            } else {
                self.win.text(tx, 10, &title, c);
            }
            // The close button.
            let cx = x + tw - 22;
            self.win.text(cx + 2, 10, "x", th.cocoa);
            self.hits.add(
                ARect::new(cx - 2, 6, 18, TAB_H - 8),
                ID_CLOSE_TAB + i as u32,
            );
        }
        // New tab.
        let nx = w - 32;
        self.win.rrect(nx, 6, 24, 22, RADIUS, th.cream);
        self.win.bold(nx + 8, 9, "+", th.espresso);
        self.hits.add(ARect::new(nx, 4, 26, 26), ID_NEW_TAB);
    }

    fn draw_toolbar(&mut self, w: i32) {
        let th = self.th;
        let y0 = TAB_H;
        self.win.fill(0, y0, w, BAR_H, th.cream);
        hline(&mut self.win, 0, y0 + BAR_H - 1, w, th.sand);
        let t = &self.tabs[self.cur];
        let (can_back, can_fwd, loading) = (
            t.history.can_back(),
            t.history.can_forward(),
            t.load.is_some(),
        );
        let by = y0 + 6;
        for (k, (id, on)) in [
            (ID_BACK, can_back),
            (ID_FORWARD, can_fwd),
            (ID_RELOAD, true),
        ]
        .into_iter()
        .enumerate()
        {
            let bx = 8 + k as i32 * 32;
            self.win.rrect(bx, by, 28, 28, 14, th.oat);
            let c = if on { th.espresso } else { th.latte };
            let (cx, cy) = (bx + 14, by + 14);
            match id {
                ID_BACK | ID_FORWARD => {
                    // A chevron.
                    let dir = if id == ID_BACK { -1 } else { 1 };
                    for i in 0..9 {
                        // The tip points the way it goes.
                        let x = cx + dir * (2 - (i - 4i32).abs());
                        self.win.fill(x - 1, cy - 4 + i, 3, 1, c);
                    }
                }
                _ if loading => {
                    // Stop: an x.
                    for i in 0..8 {
                        self.win.fill(cx - 4 + i, cy - 4 + i, 2, 2, c);
                        self.win.fill(cx + 3 - i, cy - 4 + i, 2, 2, c);
                    }
                }
                _ => {
                    // Reload: a ring with a gap and an arrowhead.
                    appkit::shapes::ring(&mut self.win, cx, cy, 5, 7, Some(0.8), c);
                    self.win.fill(cx + 3, cy - 9, 5, 2, c);
                    self.win.fill(cx + 6, cy - 9, 2, 5, c);
                }
            }
            self.hits.add(ARect::new(bx, by, 28, 28), id);
        }
        // The address bar.
        let ax = 108;
        let aw = w - ax - 48;
        let editing = self.editing.is_some();
        let fill = th.milk;
        let border = if editing { gui::HONEY } else { th.sand };
        self.win.rrect(ax, by, aw, 28, 14, border);
        let inset = if editing { 2 } else { 1 };
        self.win.rrect(
            ax + inset,
            by + inset,
            aw - 2 * inset,
            28 - 2 * inset,
            14 - inset,
            fill,
        );
        let t = &self.tabs[self.cur];
        let mut tx = ax + 12;
        if !editing {
            if t.page.is_secure() && t.load.is_none() {
                widgets::padlock(&mut self.win, tx, by + 8, th.good);
                self.hits.add(ARect::new(ax, by, 30, 28), ID_PADLOCK);
                tx += 18;
            } else if t.page.kind == PageKind::Web && t.load.is_none() {
                // Plain http: an open circle and the words.
                self.win.text(tx, by + 6, "http", th.latte);
                self.hits.add(ARect::new(ax, by, 46, 28), ID_PADLOCK);
                tx += 40;
            }
        }
        self.hits
            .add(ARect::new(tx, by, aw - (tx - ax) - 30, 28), ID_ADDRESS);
        let room = fit_chars(ax + aw - 36 - tx);
        match &self.editing {
            Some(f) => {
                let n = f.text.chars().count();
                let first = f.cur.saturating_sub(room);
                let vis: String = f.text.chars().skip(first).take(room).collect();
                if self.select_all && n > 0 {
                    let wsel = (vis.chars().count() as i32 * 8).min(room as i32 * 8);
                    self.win.fill(tx - 1, by + 5, wsel + 2, 18, th.sand);
                }
                self.win.text(tx, by + 6, &vis, th.espresso);
                let cx = tx + (f.cur - first) as i32 * 8;
                self.win.fill(cx, by + 6, 2, 16, th.espresso);
            }
            None => {
                let shown = match (&t.load, t.page.kind) {
                    (Some(l), _) => l.address.clone(),
                    (None, PageKind::Internal(browserkit::address::Internal::NewTab)) => {
                        String::new()
                    }
                    (None, _) => t.page.address.clone(),
                };
                if shown.is_empty() {
                    self.win.text(
                        tx,
                        by + 6,
                        if self.online {
                            "Search or type an address"
                        } else {
                            "Type an address"
                        },
                        th.latte,
                    );
                } else {
                    self.win.text(tx, by + 6, &tail(&shown, room), th.espresso);
                }
            }
        }
        // The bookmark ribbon at the right end of the bar.
        let marked = self.bookmarks.contains(&t.page.address);
        let (sx, sy) = (ax + aw - 26, by + 6);
        let c = if marked { gui::HONEY } else { th.latte };
        self.win.fill(sx + 2, sy, 10, 12, c);
        for i in 0..5 {
            self.win.fill(sx + 2, sy + 12 + i, 5 - i, 1, c);
            self.win.fill(sx + 7 + i, sy + 12 + i, 5 - i, 1, c);
        }
        if !marked {
            self.win.fill(sx + 4, sy + 2, 6, 9, fill);
        }
        self.hits.add(ARect::new(sx - 4, by, 24, 28), ID_STAR);
        // The menu.
        let mx = w - 38;
        self.win.rrect(mx, by, 30, 28, 14, th.oat);
        for i in 0..3 {
            self.win.fill(mx + 8, by + 8 + i * 5, 14, 2, th.espresso);
        }
        self.hits.add(ARect::new(mx, by, 30, 28), ID_MENU);
    }

    fn draw_find(&mut self, sy: i32, w: i32) {
        let th = self.th;
        self.win.text(10, sy + 3, "Find:", th.cocoa);
        let fx = 58;
        let fw = 220;
        self.win.rrect(fx, sy + 2, fw, STATUS_H - 4, 8, gui::HONEY);
        self.win
            .rrect(fx + 1, sy + 3, fw - 2, STATUS_H - 6, 7, th.milk);
        self.hits
            .add(ARect::new(fx, sy, fw, STATUS_H), ID_FIND_FIELD);
        if let Some(f) = &self.finding {
            let room = fit_chars(fw - 16);
            let first = f.cur.saturating_sub(room);
            let vis: String = f.text.chars().skip(first).take(room).collect();
            self.win.text(fx + 8, sy + 3, &vis, th.espresso);
            self.win.fill(
                fx + 8 + (f.cur - first) as i32 * 8,
                sy + 4,
                2,
                15,
                th.espresso,
            );
        }
        let t = self.tab();
        let q = self
            .finding
            .as_ref()
            .map_or(String::new(), |f| f.text.clone());
        let msg = if q.is_empty() {
            String::from("Type to find on this page")
        } else if t.found.is_empty() {
            String::from("Not found")
        } else {
            format!("{} of {}", t.found_at + 1, t.found.len())
        };
        let mut x = fx + fw + 12;
        self.win.text(x, sy + 3, &msg, th.cocoa);
        x += msg.len() as i32 * 8 + 16;
        for (label, id) in [
            ("<", ID_FIND_PREV),
            (">", ID_FIND_NEXT),
            ("Done", ID_FIND_DONE),
        ] {
            let bw = label.len() as i32 * 8 + 16;
            if x + bw > w {
                break;
            }
            self.win.rrect(x, sy + 2, bw, STATUS_H - 4, 8, th.oat);
            self.win.text(x + 8, sy + 3, label, th.espresso);
            self.hits.add(ARect::new(x, sy, bw, STATUS_H), id);
            x += bw + 6;
        }
    }

    fn draw_popup(&mut self, w: i32, h: i32) {
        let th = self.th;
        match self.popup {
            None => {}
            Some(Popup::Menu) => {
                let pw = 330;
                let rows = MENU.len() as i32;
                let ph = rows * 24 + 12;
                let (px, py) = (w - pw - 8, TOP - 4);
                self.hits.add(ARect::new(0, 0, w, h), ID_POPUP_BG);
                self.win.rrect(px - 1, py - 1, pw + 2, ph + 2, 10, th.sand);
                self.win.rrect(px, py, pw, ph, 9, th.milk);
                for (i, (label, key)) in MENU.iter().enumerate() {
                    let y = py + 6 + i as i32 * 24;
                    self.win.text(px + 14, y + 4, label, th.espresso);
                    if !key.is_empty() {
                        self.win
                            .text(px + pw - 14 - key.len() as i32 * 8, y + 4, key, th.latte);
                    }
                    self.hits
                        .add(ARect::new(px, y, pw, 24), ID_MENU_ITEM + i as u32);
                }
            }
            Some(Popup::Site) => {
                let t = self.tab();
                let secure = t.page.is_secure();
                let host = t
                    .page
                    .url
                    .as_ref()
                    .map_or(String::new(), |u| u.host.clone());
                let mut lines: Vec<(String, bool)> = Vec::new();
                match (&t.page.cert, t.page.is_secure()) {
                    (Some(c), true) => {
                        lines.push((format!("{host}: the connection is secure"), true));
                        lines.extend(c.lines().into_iter().map(|l| (l, false)));
                    }
                    _ => {
                        lines.push((format!("{host}: not secure"), true));
                        lines.push((
                            String::from("This page came over plain http: anyone on the way can"),
                            false,
                        ));
                        lines.push((
                            String::from("read or change it. Don't type passwords here."),
                            false,
                        ));
                    }
                }
                let pw = (lines
                    .iter()
                    .map(|l| l.0.chars().count())
                    .max()
                    .unwrap_or(20) as i32
                    * 8
                    + 32)
                    .min(w - 24);
                let ph = lines.len() as i32 * 22 + 16;
                let (px, py) = (100, TOP - 4);
                self.hits.add(ARect::new(0, 0, w, h), ID_POPUP_BG);
                self.win.rrect(px - 1, py - 1, pw + 2, ph + 2, 10, th.sand);
                self.win.rrect(px, py, pw, ph, 9, th.milk);
                if secure {
                    widgets::padlock(&mut self.win, px + 14, py + 12, th.good);
                }
                for (i, (l, bold)) in lines.iter().enumerate() {
                    let y = py + 10 + i as i32 * 22;
                    let x = if i == 0 { px + 32 } else { px + 16 };
                    let l = ellipsize(l, fit_chars(pw - (x - px) - 10));
                    if *bold {
                        self.win.bold(x, y, &l, th.espresso);
                    } else {
                        self.win.text(x, y, &l, th.cocoa);
                    }
                }
            }
            Some(Popup::Select(id)) => {
                let t = self.tab();
                let Some(c) = t.page.doc.controls().get(id) else {
                    return;
                };
                let Some(r) = t.page.doc.control_rect(id) else {
                    return;
                };
                let z = t.zoom;
                let x = r.x * z / 100;
                let y = TOP + (r.y + r.h - t.scroll) * z / 100;
                let n = c.options.len().min(16) as i32;
                let pw = (r.w * z / 100).max(120);
                let ph = n * 22 + 8;
                let y = if y + ph > h - STATUS_H {
                    (TOP + (r.y - t.scroll) * z / 100 - ph).max(TOP)
                } else {
                    y
                };
                let opts: Vec<String> =
                    c.options.iter().take(16).map(|o| o.label.clone()).collect();
                let sel = t.page.doc.selected(id);
                self.hits.add(ARect::new(0, 0, w, h), ID_POPUP_BG);
                self.win.rrect(x - 1, y - 1, pw + 2, ph + 2, 8, th.sand);
                self.win.rrect(x, y, pw, ph, 7, th.milk);
                for (i, o) in opts.iter().enumerate() {
                    let oy = y + 4 + i as i32 * 22;
                    if i == sel {
                        self.win.rrect(x + 4, oy, pw - 8, 22, 6, th.oat);
                    }
                    self.win.text(
                        x + 12,
                        oy + 3,
                        &ellipsize(o, fit_chars(pw - 24)),
                        th.espresso,
                    );
                    self.hits
                        .add(ARect::new(x, oy, pw, 22), ID_OPTION + i as u32);
                }
            }
        }
    }
}
