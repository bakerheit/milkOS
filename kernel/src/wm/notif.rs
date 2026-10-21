//! The notification drawer (SPEC "Notification drawer (side drawer)"): a glass panel on the
//! right, 316 wide, 12px from the right edge, from y36 to 12px above the bottom, radius 18,
//! glass at 0.78 over Wb (never over windows), floating shadow, drawn above the dock.
//!
//! * Header (78px): "Notifications", "Clear all", filter chips All / Mentions / Today.
//! * Groups by app (8x16 uppercase cocoa label, count right), collapsible.
//! * Cards 64 high (the system's own, from milkOS itself, are compact 56), radius 14, milk at
//!   82% with a white edge; icon 36 at (10, 10), title bold at (56, 8), time right, one line
//!   of body at (56, 28) cut with "~". Selected: straw 13% and a straw edge, all text
//!   primary (SPEC: no secondary text on a selected row). An x on the hovered card dismisses.
//! * A Today card (date, what's running) and the Focus row (44px, a 44x24 switch): the same
//!   single setting as the control centre's Focus tile.
//! * Empty: a check, "All caught up".
//!
//! Opening: the menu bar bell (or the clock), Ctrl+N. Closing: Esc, a click outside. Keys:
//! Up/Down select, Return opens the source app, Backspace dismisses, Left/Right collapse /
//! expand the group, Tab cycles the chips. Badges: the bell shows the unread total (a moon in
//! Do Not Disturb), dock icons their app's unread count.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{Color, Rect};
use crate::timer;

use super::icons::{self, Icon};
use super::paint::{Painter, hit};
use super::shell::PanelKey;
use super::theme::*;
use super::window::App;
use super::{Desktop, rtc};

pub const W: i32 = 316;
const RIGHT: i32 = 12;
const TOP: i32 = 36;
const BOTTOM: i32 = 12;
const PAD: i32 = 12;
const HEADER_H: i32 = 78;
const CHIP_H: i32 = 26;
const CARD_H: i32 = 64;
const CARD_COMPACT_H: i32 = 56;
const CARD_GAP: i32 = 8;
const LABEL_H: i32 = 22;
const TODAY_H: i32 = 110;
const DND_H: i32 = 44;
const MAX_KEPT: usize = 60;
/// Toasts stay this long (paused while the pointer is on them).
pub const TOAST_NS: u64 = 5_000_000_000;

const CHIPS: [(&str, i32, i32); 3] = [("All", 14, 64), ("Mentions", 84, 96), ("Today", 186, 80)];

pub struct Notice {
    pub id: u64,
    pub app: String,
    pub title: String,
    pub sub: String,
    pub at_ns: u64,
    /// Unix day it arrived (the Today chip).
    pub day: u64,
    /// Not yet seen in the open drawer (the badges count these).
    pub unread: bool,
}

/// The drawer's own state (the notices live in `Desktop::history`).
#[derive(Default)]
pub struct Drawer {
    pub filter: usize,
    pub collapsed: Vec<String>,
    pub sel: Option<u64>,
    pub hover: Option<u64>,
    pub scroll: i32,
    pub next_id: u64,
}

pub fn rect() -> Rect {
    Rect::new(screen_w() - RIGHT - W, TOP, W, screen_h() - TOP - BOTTOM)
}

fn clock_hm(at_ns: u64) -> String {
    let base = rtc::now();
    // `at_ns` is uptime; the wall clock now minus how long ago.
    let ago = (timer::now_ns().saturating_sub(at_ns) / 1_000_000_000) as i64;
    let mins = (base.hour as i64 * 60 + base.minute as i64 - (ago + 59) / 60).rem_euclid(24 * 60);
    format!("{}:{:02}", mins / 60, mins % 60)
}

fn today() -> u64 {
    let t = rtc::now();
    (t.year as u64) * 400 + (t.month as u64) * 32 + t.day as u64
}

/// A notice addressed to you (the Mentions chip): it names you or has an @.
fn mentions_you(n: &Notice) -> bool {
    let t = format!("{} {}", n.title, n.sub).to_ascii_lowercase();
    // The name as a whole word, so "milkOS" is not a mention of the user "milk".
    let me = super::session::user_name().to_ascii_lowercase();
    t.contains('@')
        || t.split(|c: char| !c.is_ascii_alphanumeric())
            .any(|w| w == me)
}

/// The app's look for its card icon.
pub(super) fn app_icon(app: &str) -> (Icon, Color, Color) {
    match app {
        "Time Machine" => (Icon::Clock, HONEY, INK),
        "Files" | "Versions" => (Icon::Folder, HONEY, INK),
        "Screenshot" => (Icon::Photo, CREAM, ESPRESSO),
        "Terminal" | "Notify" | "notify" => (Icon::Term, TERM_BG, TERM_FG),
        "Settings" => (Icon::Gear, LATTE, ESPRESSO),
        "Music" => (Icon::Music, STRAW, INK),
        "milkOS" => (Icon::Gear, LATTE, ESPRESSO),
        _ => (Icon::Package, LAV, INK),
    }
}

/// Which dock tile shows an app's badge (by tile name).
pub fn dock_tile_for(app: &str) -> &str {
    match app {
        "Time Machine" | "Versions" | "Screenshot" | "Files" => "Files",
        "Notify" | "notify" | "Terminal" => "Terminal",
        a => a,
    }
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => String::new(),
    }
}

/// One laid-out row of the list (panel coordinates).
enum Row {
    Label { app: String, n: usize, y: i32 },
    Card { id: u64, y: i32, h: i32 },
}

impl Desktop {
    pub(super) fn record_notice(&mut self, app: &str, title: &str, sub: &str) -> u64 {
        let id = self.drawer.next_id;
        self.drawer.next_id += 1;
        let app = if app == "milkOS" {
            String::from(app)
        } else {
            cap(app)
        };
        self.history.push(Notice {
            id,
            app,
            title: String::from(title),
            sub: String::from(sub),
            at_ns: timer::now_ns(),
            day: today(),
            // Open drawer: it arrives in view, read.
            unread: !self.notif,
        });
        if self.history.len() > MAX_KEPT {
            self.history.remove(0);
        }
        self.badges_changed();
        id
    }

    fn badges_changed(&mut self) {
        self.invalidate(PanelKey::MenuBar);
        self.invalidate(PanelKey::Dock);
        if self.notif {
            self.invalidate(PanelKey::Notif);
        }
    }

    /// Unread notices, all apps.
    pub(super) fn unread(&self) -> usize {
        self.history.iter().filter(|n| n.unread).count()
    }

    /// Unread notices badged on dock tile `tile`.
    pub(super) fn unread_for_tile(&self, tile: &str) -> usize {
        self.history
            .iter()
            .filter(|n| n.unread && dock_tile_for(&n.app) == tile)
            .count()
    }

    pub(super) fn toggle_notif(&mut self) {
        if self.notif {
            self.close_notif();
        } else {
            self.open_notif(None);
        }
    }

    /// Open the drawer, optionally on notice `on` (a toast was clicked).
    pub(super) fn open_notif(&mut self, on: Option<u64>) {
        self.notif = true;
        self.control = false;
        self.set_menu(None);
        self.drawer.scroll = 0;
        self.drawer.sel = on.or_else(|| self.history.last().map(|n| n.id));
        // Toasts on screen go: they are in the drawer now.
        let ids: Vec<u32> = self.toasts.iter().map(|t| t.id).collect();
        self.toasts.clear();
        for id in ids {
            self.invalidate(PanelKey::Toast(id));
        }
        for n in &mut self.history {
            n.unread = false;
        }
        self.badges_changed();
    }

    pub(super) fn close_notif(&mut self) {
        if self.notif {
            self.notif = false;
            self.invalidate(PanelKey::Notif);
        }
    }

    fn passes(&self, n: &Notice) -> bool {
        match self.drawer.filter {
            1 => mentions_you(n),
            2 => n.day == today(),
            _ => true,
        }
    }

    /// Groups (app, notices newest first), the group with the newest notice first.
    fn groups(&self) -> Vec<(String, Vec<&Notice>)> {
        let mut g: Vec<(String, Vec<&Notice>)> = Vec::new();
        for n in self.history.iter().rev().filter(|n| self.passes(n)) {
            match g.iter_mut().find(|x| x.0 == n.app) {
                Some(x) => x.1.push(n),
                None => g.push((n.app.clone(), alloc::vec![n])),
            }
        }
        g
    }

    fn list_area() -> (i32, i32) {
        let h = rect().h;
        (HEADER_H + 10, h - PAD - DND_H - 10 - TODAY_H - 10)
    }

    /// The list as drawn (scrolled), plus the full height it needs.
    fn layout(&self) -> (Vec<Row>, i32) {
        let (y0, _) = Self::list_area();
        let mut y = y0 - self.drawer.scroll;
        let mut rows = Vec::new();
        for (app, items) in self.groups() {
            rows.push(Row::Label {
                app: app.clone(),
                n: items.len(),
                y,
            });
            y += LABEL_H;
            if !self.drawer.collapsed.contains(&app) {
                for n in items {
                    let h = if n.app == "milkOS" {
                        CARD_COMPACT_H
                    } else {
                        CARD_H
                    };
                    rows.push(Row::Card { id: n.id, y, h });
                    y += h + CARD_GAP;
                }
            }
            y += 6;
        }
        (rows, y + self.drawer.scroll - y0)
    }

    /// The cards in list order (what Up / Down walk).
    fn visible_cards(&self) -> Vec<u64> {
        self.layout()
            .0
            .iter()
            .filter_map(|r| match r {
                Row::Card { id, .. } => Some(*id),
                _ => None,
            })
            .collect()
    }

    /// Keep the selected card in view.
    fn scroll_to_sel(&mut self) {
        let (y0, y1) = Self::list_area();
        let Some(sel) = self.drawer.sel else {
            return;
        };
        let (rows, total) = self.layout();
        for r in rows {
            if let Row::Card { id, y, h } = r
                && id == sel
            {
                if y < y0 {
                    self.drawer.scroll -= y0 - y + LABEL_H;
                } else if y + h > y1 {
                    self.drawer.scroll += y + h - y1;
                }
            }
        }
        let max = (total - (y1 - y0)).max(0);
        self.drawer.scroll = self.drawer.scroll.clamp(0, max);
    }

    pub(super) fn paint_notif(&self, p: &mut Painter, _solid: bool) {
        let r = rect();
        let (w, h) = (r.w, r.h);
        // GlassStrong (milk 0.7 over Wb) plus milk to SPEC's 0.78.
        p.fill_a(0, 0, w, h, MILK, a(267));
        // Header.
        p.text_p(18, 14, "Notifications", ESPRESSO, true);
        if !self.history.is_empty() {
            p.text_right(w - 18, 14, "Clear all", COCOA, 1, false);
        }
        for (i, (label, x, cw)) in CHIPS.iter().enumerate() {
            let on = i == self.drawer.filter;
            let (bg, ba, fg) = if on {
                (ESPRESSO, 255, CREAM)
            } else {
                (MILK, a(850), ESPRESSO)
            };
            p.rrect(*x, 46, *cw, CHIP_H, CHIP_H / 2, bg, ba);
            if !on {
                p.rrect_stroke(*x, 46, *cw, CHIP_H, CHIP_H / 2, 1, SAND, 255);
            }
            p.text_p_center(*x, *cw, 46 + 5, label, fg, false);
        }
        let (y0, y1) = Self::list_area();
        let (rows, _) = self.layout();
        let cw = w - 2 * PAD;
        if rows.is_empty() {
            // All caught up.
            let cx = w / 2;
            p.circle(cx - 36, 150, 72, MATCHA, 0x66);
            p.ring(cx - 36, 150, 72, 3, MATCHA, 255);
            check(p, cx, 186);
            p.text_p_center(0, w, 244, "All caught up", ESPRESSO, true);
            p.text_p_center(0, w, 266, "Nothing needs you right now.", COCOA, false);
        } else {
            let mut lp = p.clipped(Rect::new(0, y0, w, y1 - y0));
            for row in &rows {
                match row {
                    Row::Label { app, n, y } => {
                        let open = !self.drawer.collapsed.contains(app);
                        tri(&mut lp, 18, *y + 4, open, COCOA);
                        let label = app.to_ascii_uppercase();
                        lp.text(30, *y, &label, COCOA);
                        lp.text_right(w - 18, *y, &format!("{n}"), COCOA, 1, false);
                    }
                    Row::Card { id, y, h } => {
                        let Some(n) = self.history.iter().find(|n| n.id == *id) else {
                            continue;
                        };
                        let sel = self.drawer.sel == Some(*id);
                        self.paint_notice(&mut lp, PAD, *y, cw, *h, n, sel);
                    }
                }
            }
        }
        // Today card.
        let ty = h - PAD - DND_H - 10 - TODAY_H;
        let ty = if rows.is_empty() { 320.min(ty) } else { ty };
        p.rrect(PAD, ty, cw, TODAY_H, 14, MILK, a(820));
        p.rrect_stroke(PAD, ty, cw, TODAY_H, 14, 1, WHITE, a(900));
        let t = rtc::now();
        p.text(PAD + 14, ty + 10, "TODAY", COCOA);
        p.text_p(
            PAD + 14,
            ty + 32,
            &format!(
                "{}, {} {}",
                rtc::DAYS[t.weekday as usize],
                t.day,
                rtc::MONTHS[t.month as usize - 1]
            ),
            ESPRESSO,
            true,
        );
        let up = timer::now_ns() / 1_000_000_000;
        let n = self.windows.len();
        p.text_p(
            PAD + 14,
            ty + 52,
            &if up < 60 {
                // Nothing to say about uptime in the first minute.
                format!("{n} window{} open", if n == 1 { "" } else { "s" })
            } else {
                format!(
                    "Up {}:{:02}, {n} window{} open",
                    up / 3600,
                    up / 60 % 60,
                    if n == 1 { "" } else { "s" }
                )
            },
            COCOA,
            false,
        );
        p.text_p(
            PAD + 14,
            ty + 72,
            "No calendar events here yet.",
            COCOA,
            false,
        );
        // Focus (holds toasts and sounds).
        let dy = h - PAD - DND_H;
        let on = self.settings.focus_mode;
        if on {
            p.rrect(PAD, dy, cw, DND_H, 14, LAV, 0x44);
            p.rrect_stroke(PAD, dy, cw, DND_H, 14, 1, LAV, 255);
        } else {
            p.rrect(PAD, dy, cw, DND_H, 14, MILK, a(820));
            p.rrect_stroke(PAD, dy, cw, DND_H, 14, 1, WHITE, a(900));
        }
        // "Focus", the same single setting as the Control centre tile (SPEC). Focus has no
        // end time yet, so the sub-line says what is true instead of "On until 12:00".
        if on {
            p.text_p(PAD + 14, dy + 4, "Focus", ESPRESSO, true);
            p.text_p(PAD + 14, dy + 22, "On until you turn it off", COCOA, false);
        } else {
            p.text_p(PAD + 14, dy + 14, "Focus", ESPRESSO, false);
        }
        let sx = PAD + cw - 58;
        p.rrect(sx, dy + 10, 44, 24, 12, if on { STRAW } else { SAND }, 255);
        p.circle(sx + if on { 22 } else { 2 }, dy + 12, 20, MILK, 255);
    }

    #[allow(clippy::too_many_arguments)]
    fn paint_notice(&self, p: &mut Painter, x: i32, y: i32, w: i32, h: i32, n: &Notice, sel: bool) {
        if sel {
            p.rrect(x, y, w, h, 14, STRAW, 0x22);
            p.rrect_stroke(x, y, w, h, 14, 1, STRAW, 255);
        } else {
            p.rrect(x, y, w, h, 14, MILK, a(820));
            p.rrect_stroke(x, y, w, h, 14, 1, WHITE, a(900));
        }
        let (icon, bg, fg) = app_icon(&n.app);
        icons::app_tile(p, x + 10, y + 10, 36, &n.app, icon, bg, fg);
        // SPEC: everything primary on the selected card.
        let second = if sel { ESPRESSO } else { COCOA };
        let time = clock_hm(n.at_ns);
        let tw = super::fontx::prop_width(&time, false);
        let tmax = ((w - 56 - 12 - tw - 8) / 8).max(4) as usize;
        p.text_p(x + 56, y + 8, &cut(&n.title, tmax), ESPRESSO, true);
        let hovered = self.drawer.hover == Some(n.id);
        if hovered {
            // The x that dismisses it (where the time is).
            let bx = x + w - 12 - 24;
            p.circle(bx, y + 4, 24, MILK, 255);
            p.ring(bx, y + 4, 24, 1, SAND, 255);
            p.text_p_center(bx, 24, y + 8, "x", COCOA, false);
        } else {
            p.text_right(x + w - 12, y + 8, &time, second, 1, false);
        }
        let bmax = ((w - 70) / 8).max(4) as usize;
        p.text_p(x + 56, y + 28, &cut(&n.sub, bmax), second, false);
    }

    /// The card / label / control at panel point (x, y).
    fn notif_hit(&self, x: i32, y: i32) -> Option<NHit> {
        let w = rect().w;
        if (14..32).contains(&y) && x >= w - 18 - 70 && !self.history.is_empty() {
            return Some(NHit::ClearAll);
        }
        for (i, (_, cx, cw)) in CHIPS.iter().enumerate() {
            if hit(&Rect::new(*cx, 46, *cw, CHIP_H), x, y) {
                return Some(NHit::Chip(i));
            }
        }
        let h = rect().h;
        let dy = h - PAD - DND_H;
        if hit(&Rect::new(PAD, dy, w - 2 * PAD, DND_H), x, y) {
            return Some(NHit::Dnd);
        }
        let (y0, y1) = Self::list_area();
        if y < y0 || y >= y1 {
            return None;
        }
        let cw = w - 2 * PAD;
        for row in self.layout().0 {
            match row {
                Row::Label { app, y: ly, .. } if y >= ly && y < ly + LABEL_H => {
                    return Some(NHit::Group(app));
                }
                Row::Card { id, y: cy, h } if y >= cy && y < cy + h && x >= PAD && x < PAD + cw => {
                    let bx = PAD + cw - 12 - 24;
                    if self.drawer.hover == Some(id) && hit(&Rect::new(bx, cy + 4, 24, 24), x, y) {
                        return Some(NHit::Dismiss(id));
                    }
                    return Some(NHit::Card(id));
                }
                _ => {}
            }
        }
        None
    }

    pub(super) fn click_notif(&mut self, x: i32, y: i32) {
        match self.notif_hit(x, y) {
            Some(NHit::ClearAll) => {
                self.history.clear();
                self.drawer.sel = None;
            }
            Some(NHit::Chip(i)) => {
                self.drawer.filter = i;
                self.drawer.scroll = 0;
            }
            Some(NHit::Dnd) => self.set_dnd(!self.settings.focus_mode),
            Some(NHit::Group(app)) => self.toggle_group(&app),
            Some(NHit::Dismiss(id)) => self.dismiss(id),
            Some(NHit::Card(id)) => {
                if self.drawer.sel == Some(id) {
                    self.open_source(id);
                } else {
                    self.drawer.sel = Some(id);
                }
            }
            None => return,
        }
        self.invalidate(PanelKey::Notif);
    }

    /// The pointer over the drawer (panel coordinates; None = elsewhere): the dismiss x.
    pub(super) fn hover_notif(&mut self, at: Option<(i32, i32)>) {
        let over = at.and_then(|(x, y)| match self.notif_hit(x, y) {
            Some(NHit::Card(id) | NHit::Dismiss(id)) => Some(id),
            _ => None,
        });
        if over != self.drawer.hover {
            self.drawer.hover = over;
            self.invalidate(PanelKey::Notif);
        }
    }

    pub(super) fn scroll_notif(&mut self, dy: i32) {
        let (y0, y1) = Self::list_area();
        let (_, total) = self.layout();
        let max = (total - (y1 - y0)).max(0);
        self.drawer.scroll = (self.drawer.scroll + dy * 24).clamp(0, max);
        self.invalidate(PanelKey::Notif);
    }

    /// Do Not Disturb is Focus mode (the control centre's tile is the same setting).
    pub(super) fn set_dnd(&mut self, on: bool) {
        self.settings.focus_mode = on;
        self.invalidate(PanelKey::Control);
        self.invalidate(PanelKey::MenuBar);
        self.invalidate(PanelKey::Notif);
    }

    fn toggle_group(&mut self, app: &str) {
        if let Some(i) = self.drawer.collapsed.iter().position(|a| a == app) {
            self.drawer.collapsed.remove(i);
        } else {
            self.drawer.collapsed.push(String::from(app));
        }
    }

    fn dismiss(&mut self, id: u64) {
        let order = self.visible_cards();
        let pos = order.iter().position(|&c| c == id);
        self.history.retain(|n| n.id != id);
        if self.drawer.sel == Some(id) {
            self.drawer.sel = pos.and_then(|p| {
                order
                    .get(p + 1)
                    .or_else(|| p.checked_sub(1).and_then(|q| order.get(q)))
                    .copied()
            });
        }
        if self.drawer.hover == Some(id) {
            self.drawer.hover = None;
        }
        self.badges_changed();
    }

    /// Return on a card: the app it came from.
    fn open_source(&mut self, id: u64) {
        let Some(app) = self
            .history
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.app.clone())
        else {
            return;
        };
        self.close_notif();
        match app.as_str() {
            "Files" | "Versions" => self.open(App::Files),
            "Time Machine" => {
                super::apps::files::show_path("timemachine:");
                self.open(App::Files);
            }
            "Screenshot" => {
                super::apps::files::show_path(super::shot::DIR);
                self.open(App::Files);
            }
            "Terminal" | "Notify" => self.open(App::Terminal),
            "Settings" => self.open(App::Settings),
            "Music" => self.open(App::Music),
            other => {
                let lower = other.to_ascii_lowercase();
                if let Some(p) = super::packages::all().into_iter().find(|p| p.name == lower) {
                    self.launch_package(&p);
                }
            }
        }
    }

    /// Keys while the drawer is open. True if it used the key.
    pub(super) fn notif_key(&mut self, k: &super::window::KeyInput) -> bool {
        use super::event::Key;
        let cards = self.visible_cards();
        let pos = self
            .drawer
            .sel
            .and_then(|s| cards.iter().position(|&c| c == s));
        match k.key {
            Key::Escape => {
                self.close_notif();
                return true;
            }
            Key::Down => {
                let next = pos.map_or(0, |p| (p + 1).min(cards.len().saturating_sub(1)));
                self.drawer.sel = cards.get(next).copied();
            }
            Key::Up => {
                let prev = pos.map_or(0, |p| p.saturating_sub(1));
                self.drawer.sel = cards.get(prev).copied();
            }
            Key::Enter => {
                if let Some(id) = self.drawer.sel {
                    self.open_source(id);
                }
                return true;
            }
            Key::Backspace | Key::Delete => {
                if let Some(id) = self.drawer.sel {
                    self.dismiss(id);
                }
            }
            Key::Left | Key::Right => {
                // The selected card's group (or the first group).
                let app = self
                    .drawer
                    .sel
                    .and_then(|s| self.history.iter().find(|n| n.id == s))
                    .map(|n| n.app.clone())
                    .or_else(|| self.groups().first().map(|g| g.0.clone()));
                if let Some(app) = app {
                    let collapsed = self.drawer.collapsed.contains(&app);
                    if (k.key == Key::Left) != collapsed {
                        self.toggle_group(&app);
                    }
                }
                // The selection went into a collapsed group: the first card still showing.
                let cards = self.visible_cards();
                if self.drawer.sel.is_none_or(|s| !cards.contains(&s)) {
                    self.drawer.sel = cards.first().copied();
                }
            }
            Key::Tab => {
                self.drawer.filter = (self.drawer.filter + 1) % CHIPS.len();
                self.drawer.scroll = 0;
            }
            _ => return false,
        }
        self.scroll_to_sel();
        self.invalidate(PanelKey::Notif);
        true
    }

    /// Once a minute (times on the cards, the Today card).
    pub(super) fn notif_tick(&mut self) {
        if self.notif {
            self.invalidate(PanelKey::Notif);
        }
    }
}

enum NHit {
    ClearAll,
    Chip(usize),
    Dnd,
    Group(String),
    Card(u64),
    Dismiss(u64),
}

/// `s` cut to `n` characters, the last one "~".
fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return String::from(s);
    }
    let mut c: String = s.chars().take(n.saturating_sub(1)).collect();
    c.push('~');
    c
}

/// A group's arrow: pointing down (open) or right (collapsed), 8 px.
fn tri(p: &mut Painter, x: i32, y: i32, open: bool, c: Color) {
    for k in 0..5 {
        if open {
            p.fill(x + k, y + k, 9 - 2 * k, 1, c);
        } else {
            p.fill(x + k, y + k, 1, 9 - 2 * k, c);
        }
    }
}

/// The "all caught up" check mark (the font has no check glyph), centred on (cx, cy).
fn check(p: &mut Painter, cx: i32, cy: i32) {
    for k in 0..7 {
        p.fill(cx - 10 + k, cy - 2 + k, 3, 3, ESPRESSO);
    }
    for k in 0..12 {
        p.fill(cx - 4 + k, cy + 4 - k, 3, 3, ESPRESSO);
    }
}

/// The bell in the menu bar at (x, y): a cup and a clapper, the unread count badge, a moon
/// in Do Not Disturb.
pub fn paint_bell(p: &mut Painter, x: i32, y: i32, unread: usize, dnd: bool) {
    p.rrect(x, y + 2, 14, 12, 6, ESPRESSO, 255);
    p.fill(x, y + 10, 14, 4, ESPRESSO);
    p.rrect(x + 5, y + 14, 4, 3, 2, ESPRESSO, 255);
    if dnd {
        p.circle(x + 9, y - 5, 12, LAV, 255);
        p.ring(x + 9, y - 5, 12, 2, MILK, 255);
    } else if unread > 0 {
        badge(p, x + 7, y - 6, unread);
    }
}

/// SPEC badge: straw circle 18, milk 2px ring, the count in milk.
pub fn badge(p: &mut Painter, x: i32, y: i32, n: usize) {
    p.circle(x, y, 18, MILK, 255);
    p.circle(x + 2, y + 2, 14, STRAW, 255);
    let s = if n > 9 {
        String::from("9+")
    } else {
        format!("{n}")
    };
    p.text_p_center(x, 18, y + 1, &s, INK, true); // ink on straw: 5.33:1
}
