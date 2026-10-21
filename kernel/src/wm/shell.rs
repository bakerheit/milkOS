//! The shell around the windows: menu bar, dock, toasts, control centre, Skim launcher, Today home
//! cards and the Spaces overview. Each is a panel (a cached buffer, see `Desktop::reconcile`) or a
//! full-screen dim; this file says where each one is, how it paints and what clicks on it do.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{Color, Rect};
use crate::{heap, pmm};

use super::apps;
use super::icons::{self, Icon};
use super::paint::{Painter, hit};
use super::rtc;
use super::theme::*;
use super::window::App;
use super::{Desktop, Drag, Mode};

// ---- layers -------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum PanelKey {
    MenuBar,
    Dock,
    Toast(u32),
    Control,
    LaunchInput,
    LaunchResults,
    Card(u8),
    Thumb(u8),
    Cell(u32),
    /// The dock's hover name.
    Tip,
    /// An open menu bar menu.
    Menu,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Style {
    /// Glass at 0.55 with the 1px white inner border.
    Glass,
    /// Glass at 0.7 (launcher, control centre).
    GlassStrong,
    /// The menu bar: glass, square, white bottom edge.
    Bar,
    /// The painter fills it all itself.
    Opaque,
}

#[derive(Clone, Copy, Debug)]
pub struct PanelSpec {
    pub key: PanelKey,
    pub rect: Rect,
    pub radius: i32,
    pub style: Style,
    pub shadow: bool,
}

pub enum Layer {
    Panel(PanelSpec),
    /// Espresso over everything below, at this alpha.
    Dim(u8),
    /// Bold centred text straight on whatever is below.
    Label {
        x: i32,
        y: i32,
        w: i32,
        text: String,
        color: Color,
    },
}

/// Everything a panel paints: itself plus its float shadow.
pub fn panel_bounds(r: Rect) -> Rect {
    let mut b = r;
    for (g, dy, _) in SHADOW_FLOAT {
        b = b.union(&Rect::new(r.x - g, r.y - g + dy, r.w + 2 * g, r.h + 2 * g));
    }
    b
}

fn spec(key: PanelKey, rect: Rect, radius: i32, style: Style, shadow: bool) -> Layer {
    Layer::Panel(PanelSpec {
        key,
        rect,
        radius,
        style,
        shadow,
    })
}

pub struct Toast {
    pub id: u32,
    pub title: String,
    pub sub: String,
    pub until_ns: u64,
}

// ---- dock ---------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum DockAction {
    App(App),
    /// A userland program, started with `proc::spawn`.
    Program(&'static str),
    Home,
    Spaces,
    /// An installed package (`packages::get(i)`).
    Pkg(usize),
}

struct DockItem {
    name: &'static str,
    icon: Icon,
    bg: Color,
    fg: Color,
    action: DockAction,
}

const DOCK: [DockItem; 9] = [
    DockItem {
        name: "Files",
        icon: Icon::Folder,
        bg: HONEY,
        fg: INK,
        action: DockAction::App(App::Files),
    },
    DockItem {
        name: "Terminal",
        icon: Icon::Term,
        bg: TERM_BG,
        fg: TERM_FG,
        action: DockAction::App(App::Terminal),
    },
    DockItem {
        name: "Browser",
        icon: Icon::Globe,
        bg: BLUE,
        fg: INK,
        action: DockAction::App(App::Browser),
    },
    DockItem {
        name: "Notes",
        icon: Icon::Note,
        bg: MILK,
        fg: ESPRESSO,
        action: DockAction::App(App::Notes),
    },
    DockItem {
        name: "Paint",
        icon: Icon::Paint,
        bg: HONEY,
        fg: INK,
        action: DockAction::Program("/bin/paint"),
    },
    DockItem {
        name: "Music",
        icon: Icon::Music,
        bg: STRAW,
        fg: INK,
        action: DockAction::App(App::Music),
    },
    DockItem {
        name: "Spaces",
        icon: Icon::Spaces,
        bg: MILK,
        fg: ESPRESSO,
        action: DockAction::Spaces,
    },
    DockItem {
        name: "Today",
        icon: Icon::Cal,
        bg: MILK,
        fg: ESPRESSO,
        action: DockAction::Home,
    },
    DockItem {
        name: "Settings",
        icon: Icon::Gear,
        bg: LATTE,
        fg: ESPRESSO,
        action: DockAction::App(App::Settings),
    },
];

/// A dock tile as drawn: the built-in ones, then installed packages (packages.rs).
struct Tile {
    name: String,
    /// None: a package, drawn as its colour with its initial.
    icon: Option<Icon>,
    bg: Color,
    fg: Color,
    action: DockAction,
}

fn dock_len() -> usize {
    DOCK.len() + super::packages::count()
}

fn tiles() -> Vec<Tile> {
    let mut v: Vec<Tile> = DOCK
        .iter()
        .map(|d| Tile {
            name: String::from(d.name),
            icon: Some(d.icon),
            bg: d.bg,
            fg: d.fg,
            action: d.action,
        })
        .collect();
    for (i, p) in super::packages::all().into_iter().enumerate() {
        v.push(Tile {
            name: p.label,
            icon: None,
            bg: p.color,
            fg: INK,
            action: DockAction::Pkg(i),
        });
    }
    v
}

pub fn dock_rect() -> Rect {
    let n = dock_len() as i32;
    let w = 2 * DOCK_PAD + n * dock_icon() + (n - 1) * DOCK_GAP;
    Rect::new(
        (screen_w() - w) / 2,
        screen_h() - DOCK_BOTTOM - dock_h(),
        w,
        dock_h(),
    )
}

/// Which dock icon is at `lx` pixels from the dock's left edge.
pub fn dock_index(lx: i32) -> Option<usize> {
    let i = (lx - DOCK_PAD) / (dock_icon() + DOCK_GAP);
    let within = (lx - DOCK_PAD) % (dock_icon() + DOCK_GAP) < dock_icon();
    (lx >= DOCK_PAD && within && (i as usize) < dock_len()).then_some(i as usize)
}

impl Desktop {
    /// The dock tile an app lives in (where minimise flies to); bottom centre if none.
    pub(super) fn dock_slot(&self, app: App) -> Rect {
        let prog = match app {
            App::Client(c) => super::client::process_name(c),
            _ => None,
        };
        let i = tiles().iter().position(|d| match d.action {
            DockAction::App(a) => a == app,
            DockAction::Program(p) => prog.as_deref() == p.rsplit('/').next(),
            DockAction::Pkg(k) => super::packages::get(k)
                .is_some_and(|p| prog.as_deref() == p.exec.rsplit('/').next()),
            _ => false,
        });
        match i {
            Some(i) => {
                let (cx, cy) = dock_icon_center(i);
                Rect::new(
                    cx - dock_icon() / 2,
                    cy - dock_icon() / 2,
                    dock_icon(),
                    dock_icon(),
                )
            }
            None => Rect::new(screen_w() / 2 - 24, screen_h() - 60, 48, 48),
        }
    }
}

/// Centre x of dock icon `i` on screen (for scripts).
pub fn dock_icon_center(i: usize) -> (i32, i32) {
    dock_icon_center_of(i, 0)
}

/// The same once `extra` more tiles (packages) are in the dock: it grows from the centre.
pub fn dock_icon_center_of(i: usize, extra: usize) -> (i32, i32) {
    let d = dock_rect();
    let shift = (extra as i32 - super::packages::count() as i32) * (dock_icon() + DOCK_GAP) / 2;
    (
        d.x - shift + DOCK_PAD + i as i32 * (dock_icon() + DOCK_GAP) + dock_icon() / 2,
        d.y + DOCK_PAD + dock_icon() / 2,
    )
}

// ---- launcher -----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    App(App),
    Program(&'static str),
    Home,
    Spaces,
    Control,
    NewSpace,
}

pub struct Entry {
    pub name: &'static str,
    pub sub: &'static str,
    pub icon: Icon,
    pub bg: Color,
    pub fg: Color,
    pub action: Action,
}

pub const ENTRIES: [Entry; 12] = [
    Entry {
        name: "Terminal",
        sub: "Kernel log and a tiny shell",
        icon: Icon::Term,
        bg: TERM_BG,
        fg: TERM_FG,
        action: Action::App(App::Terminal),
    },
    Entry {
        name: "Files",
        sub: "Browse files (placeholder until vfs)",
        icon: Icon::Folder,
        bg: HONEY,
        fg: INK,
        action: Action::App(App::Files),
    },
    Entry {
        name: "Notes",
        sub: "Jot things down",
        icon: Icon::Note,
        bg: MILK,
        fg: ESPRESSO,
        action: Action::App(App::Notes),
    },
    Entry {
        name: "About milkOS",
        sub: "Version, memory, uptime, frame times",
        icon: Icon::Gear,
        bg: LATTE,
        fg: ESPRESSO,
        action: Action::App(App::About),
    },
    Entry {
        name: "Paint",
        sub: "Draw something (a userland app)",
        icon: Icon::Paint,
        bg: HONEY,
        fg: INK,
        action: Action::Program("/bin/paint"),
    },
    Entry {
        name: "Browser",
        sub: "Web pages from IP addresses and localhost",
        icon: Icon::Globe,
        bg: BLUE,
        fg: INK,
        action: Action::App(App::Browser),
    },
    Entry {
        name: "Music",
        sub: "Play the tunes in ~/Music and the system sounds",
        icon: Icon::Music,
        bg: STRAW,
        fg: INK,
        action: Action::App(App::Music),
    },
    Entry {
        name: "Settings",
        sub: "Theme, wallpaper, display, dock, Time Machine",
        icon: Icon::Gear,
        bg: LATTE,
        fg: ESPRESSO,
        action: Action::App(App::Settings),
    },
    Entry {
        name: "Today",
        sub: "Widgets on the wallpaper (F4)",
        icon: Icon::Cal,
        bg: MILK,
        fg: ESPRESSO,
        action: Action::Home,
    },
    Entry {
        name: "Spaces",
        sub: "Overview of your desktops (F3)",
        icon: Icon::Spaces,
        bg: MILK,
        fg: ESPRESSO,
        action: Action::Spaces,
    },
    Entry {
        name: "Control centre",
        sub: "Focus, display, sound",
        icon: Icon::Clock,
        bg: CREAM,
        fg: ESPRESSO,
        action: Action::Control,
    },
    Entry {
        name: "New space",
        sub: "Add a desktop",
        icon: Icon::Spaces,
        bg: CREAM,
        fg: COCOA,
        action: Action::NewSpace,
    },
];

pub struct Launcher {
    pub query: String,
    /// Index into `items` (headers are not selectable).
    pub sel: usize,
    pub items: Vec<super::skim::Item>,
}

/// Fuzzy subsequence match, case-insensitive. Higher is better; None if not all of `q` appears in
/// order. Consecutive letters and word starts score extra, so "ter" ranks Terminal first and
/// "ab" finds About.
pub fn fuzzy(q: &str, name: &str) -> Option<i32> {
    let n: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut score = 0;
    let mut pos = 0usize;
    let mut last: Option<usize> = None;
    for qc in q.chars().map(|c| c.to_ascii_lowercase()) {
        if qc == ' ' {
            continue;
        }
        let found = (pos..n.len()).find(|&i| n[i] == qc)?;
        score += 1;
        if last.is_some_and(|l| l + 1 == found) {
            score += 4;
        }
        if found == 0 || n[found - 1] == ' ' {
            score += 6;
        }
        score -= (found - pos).min(5) as i32 / 2;
        last = Some(found);
        pos = found + 1;
    }
    // A literal substring beats a scattered match by a lot, more so at a word start.
    let name_l: String = n.iter().collect();
    let ql: String = q.trim().to_ascii_lowercase();
    let bonus = match name_l.find(&ql) {
        Some(0) => 120,
        Some(i) if n[i - 1] == ' ' || n[i - 1] == '.' || n[i - 1] == '-' => 100,
        Some(_) => 60,
        None => 0,
    };
    Some(score * 10 + bonus - n.len() as i32 / 4)
}

impl Launcher {
    pub fn new() -> Launcher {
        super::skim::refresh_index();
        let mut l = Launcher {
            query: String::new(),
            sel: 0,
            items: Vec::new(),
        };
        l.update();
        l
    }

    pub fn update(&mut self) {
        self.items = super::skim::search(&self.query);
        self.sel = 0;
    }

    pub fn result_count(&self) -> usize {
        self.items.len()
    }

    /// Row y offsets in the results panel: (y, height, header kind or item index).
    fn layout(&self) -> Vec<(i32, i32, Option<super::skim::Kind>, Option<usize>)> {
        let mut y = 8;
        let mut v = Vec::new();
        for (h, it) in super::skim::rows(&self.items) {
            let rh = if h.is_some() {
                super::skim::HEADER_H
            } else {
                LAUNCH_ROW_H
            };
            v.push((y, rh, h, it));
            y += rh;
        }
        v
    }

    /// Item under panel y.
    pub fn item_at(&self, y: i32) -> Option<usize> {
        self.layout()
            .into_iter()
            .find(|&(ry, rh, _, it)| it.is_some() && y >= ry && y < ry + rh)
            .and_then(|r| r.3)
    }
}

fn launcher_x() -> i32 {
    (screen_w() - LAUNCH_W) / 2
}

fn results_height(l: &Launcher) -> i32 {
    let rows: i32 = l.layout().iter().map(|r| r.1).sum();
    8 + rows.max(LAUNCH_ROW_H) + 36
}

// ---- control centre -----------------------------------------------------------------------

pub fn control_rect() -> Rect {
    Rect::new(screen_w() - CC_RIGHT - CC_W, CC_TOP, CC_W, CC_H)
}

const SLIDER_Y: [i32; 2] = [88, 160];
const TRACK_X: i32 = 30;
const TRACK_W: i32 = CC_W - 60;

// ---- home cards ---------------------------------------------------------------------------

fn card_rect(i: u8) -> Rect {
    let y1 = 56 + 96 + CARD_GAP;
    let y2 = y1 + 228 + CARD_GAP;
    match i {
        0 => Rect::new(CARD_MARGIN, 56, screen_w() - 2 * CARD_MARGIN, 96),
        1..=3 => Rect::new(
            CARD_MARGIN + (i as i32 - 1) * (296 + CARD_GAP),
            y1,
            296,
            228,
        ),
        _ => Rect::new(
            CARD_MARGIN + (i as i32 - 4) * (456 + CARD_GAP),
            y2,
            456,
            188,
        ),
    }
}

const HOME_APPS: [(App, Icon, Color, u8, Color); 4] = [
    (App::Files, Icon::Folder, HONEY, FOLDER_A, COCOA),
    (App::Terminal, Icon::Term, TERM_BG, 255, TERM_FG),
    (App::Notes, Icon::Note, CREAM, 255, ESPRESSO),
    (App::Settings, Icon::Gear, LATTE, 255, ESPRESSO),
];

// ---- spaces -------------------------------------------------------------------------------

/// Size of a window's overview cell: its surface scaled to fit 300x210 (never enlarged).
pub fn cell_size(w: i32, h: i32) -> (i32, i32) {
    let k = (300 * 1024 / w.max(1)).min(210 * 1024 / h.max(1)).min(1024);
    ((w * k / 1024).max(8), (h * k / 1024).max(8))
}

fn thumb_rect(i: usize, n: usize) -> Rect {
    let total = n as i32 * THUMB_W + (n as i32 - 1) * THUMB_GAP;
    let sx = (screen_w() - total) / 2;
    Rect::new(
        sx + i as i32 * (THUMB_W + THUMB_GAP),
        SPACES_STRIP_Y,
        THUMB_W,
        THUMB_H,
    )
}

const SPACE_NAMES: [&str; 6] = ["Main", "Notes", "Play", "Work", "Extra", "More"];
const MAX_SPACES: usize = 6;

fn space_label(i: usize) -> String {
    format!("{} {}", i + 1, SPACE_NAMES[i.min(SPACE_NAMES.len() - 1)])
}

pub fn fmt_uptime(ns: u64) -> String {
    let s = ns / 1_000_000_000;
    format!("{}h {:02}m", s / 3600, s / 60 % 60)
}

impl Desktop {
    /// Overview cells: (window id, slot rect, cell rect).
    fn cells(&self) -> Vec<(u32, Rect)> {
        let ids: Vec<&super::window::Window> = self
            .windows
            .iter()
            .filter(|w| w.space == self.space)
            .take(6)
            .collect();
        ids.iter()
            .enumerate()
            .map(|(i, w)| {
                let (col, row) = (i as i32 % 3, i as i32 / 3);
                let (sx, sy) = (60 + col * 330, 170 + row * 270);
                let (cw, ch) = cell_size(w.rect.w, w.rect.h);
                (
                    w.id,
                    Rect::new(sx + (300 - cw) / 2, sy + (210 - ch) / 2, cw, ch),
                )
            })
            .collect()
    }

    /// The shell's layers this frame, bottom to top.
    pub(super) fn layers(&self) -> Vec<Layer> {
        let mut v = Vec::new();
        let bar = spec(
            PanelKey::MenuBar,
            Rect::new(0, 0, screen_w(), MENU_H),
            0,
            Style::Bar,
            false,
        );
        let dock = spec(PanelKey::Dock, dock_rect(), R_DOCK, Style::Glass, true);
        match self.mode {
            Mode::Desktop => {
                v.push(bar);
                v.push(dock);
            }
            Mode::Home => {
                for i in 0..6 {
                    v.push(spec(
                        PanelKey::Card(i),
                        card_rect(i),
                        R_CARD,
                        Style::Glass,
                        true,
                    ));
                }
                v.push(bar);
                v.push(dock);
            }
            Mode::Spaces => {
                v.push(Layer::Dim(SPACES_DIM_A));
                v.push(bar);
                let n = (self.nspaces + 1).min(MAX_SPACES + 1);
                for i in 0..n {
                    if i == MAX_SPACES {
                        break;
                    }
                    let st = if i == self.space {
                        Style::GlassStrong
                    } else {
                        Style::Glass
                    };
                    v.push(spec(
                        PanelKey::Thumb(i as u8),
                        thumb_rect(i, n.min(MAX_SPACES)),
                        THUMB_R,
                        st,
                        true,
                    ));
                }
                for (id, r) in self.cells() {
                    v.push(spec(PanelKey::Cell(id), r, R_WINDOW, Style::Opaque, true));
                    if let Some(w) = self.win(id) {
                        v.push(Layer::Label {
                            x: r.x - 20,
                            y: r.y + r.h + 10,
                            w: r.w + 40,
                            text: String::from(w.contents.title()),
                            color: MILK,
                        });
                    }
                }
                v.push(Layer::Label {
                    x: 0,
                    y: 722,
                    w: screen_w(),
                    text: String::from(
                        "Click a window to focus   Drag it to another space   Esc to leave",
                    ),
                    color: MILK,
                });
            }
        }
        if let Some(i) = self.tip
            && self.launcher.is_none()
        {
            let (cx, _) = dock_icon_center(i);
            let name = tiles().get(i).map_or(String::new(), |t| t.name.clone());
            let w = super::fontx::prop_width(&name, false) + 24;
            let r = Rect::new(cx - w / 2, dock_rect().y - 8 - 28, w, 28);
            v.push(spec(PanelKey::Tip, r, R_BUTTON, Style::Glass, true));
        }
        for (i, t) in self.toasts.iter().enumerate() {
            let r = Rect::new(
                screen_w() - TOAST_RIGHT - TOAST_W,
                TOAST_TOP + i as i32 * (TOAST_H + 8),
                TOAST_W,
                TOAST_H,
            );
            v.push(spec(PanelKey::Toast(t.id), r, R_TOAST, Style::Glass, true));
        }
        if let Some(m) = self.menu_layer() {
            v.push(m);
        }
        if self.control {
            v.push(spec(
                PanelKey::Control,
                control_rect(),
                R_CARD,
                Style::GlassStrong,
                true,
            ));
        }
        if let Some(l) = &self.launcher {
            v.push(Layer::Dim(LAUNCH_DIM_A));
            let x = launcher_x();
            v.push(spec(
                PanelKey::LaunchInput,
                Rect::new(x, LAUNCH_Y, LAUNCH_W, LAUNCH_INPUT_H),
                R_LAUNCHER,
                Style::GlassStrong,
                true,
            ));
            v.push(spec(
                PanelKey::LaunchResults,
                Rect::new(
                    x,
                    LAUNCH_Y + LAUNCH_INPUT_H + LAUNCH_GAP,
                    LAUNCH_W,
                    results_height(l),
                ),
                R_LAUNCHER,
                Style::GlassStrong,
                true,
            ));
        }
        v
    }

    pub(super) fn status_text(&self) -> String {
        let t = rtc::now();
        format!(
            "{} {:02}:{:02}",
            &rtc::DAYS[t.weekday as usize][..3],
            t.hour,
            t.minute
        )
    }

    /// Width of the menu bar's right side: "cpu" + one bar per CPU + gap + the clock.
    pub(super) fn status_width(&self) -> i32 {
        let n = self.cpu_loads.len().max(1) as i32;
        super::fontx::prop_width("cpu", false)
            + 4
            + n * 6
            + 14
            + super::fontx::prop_width(&self.status, false)
    }

    pub(super) fn app_title(&self) -> String {
        String::from(match self.mode {
            Mode::Home => "Today",
            Mode::Spaces => "Overview",
            Mode::Desktop => match self.focused_window() {
                None => "milkOS",
                Some(w) => match w.app {
                    App::About => "milkOS",
                    // A userland app: its window title names it.
                    App::Client(_) => w.contents.title(),
                    a => a.name(),
                },
            },
        })
    }

    // ---- painting -------------------------------------------------------------------------

    pub(super) fn paint_panel(&self, key: PanelKey, p: &mut Painter, w: i32, h: i32, solid: bool) {
        match key {
            PanelKey::MenuBar => self.paint_menubar(p, w),
            PanelKey::Dock => self.paint_dock(p),
            PanelKey::Toast(id) => {
                if let Some(t) = self.toasts.iter().find(|t| t.id == id) {
                    icons::draw(p, 12, 12, 40, Icon::Cal, HONEY, 0x88, INK, false);
                    p.text_p(64, 12, &t.title, ESPRESSO, true);
                    p.text_p(64, 32, &t.sub, COCOA, false);
                }
            }
            PanelKey::Control => self.paint_control(p, solid),
            PanelKey::Menu => self.paint_menu(p, w),
            PanelKey::Tip => {
                if let Some(t) = self.tip.and_then(|i| tiles().into_iter().nth(i)) {
                    p.text_p_center(0, w, 6, &t.name, ESPRESSO, false);
                }
            }
            PanelKey::LaunchInput => self.paint_launch_input(p, w),
            PanelKey::LaunchResults => self.paint_launch_results(p, w),
            PanelKey::Card(i) => self.paint_card(i, p, w, h),
            PanelKey::Thumb(i) => self.paint_thumb(i as usize, p, w, h),
            PanelKey::Cell(id) => {
                if let Some(t) = self.thumbs.get(&id) {
                    p.blit(0, 0, t, 0, 0, w, h);
                } else {
                    p.fill(0, 0, w, h, MILK);
                }
                if self.focus == Some(id) {
                    p.rrect_stroke(0, 0, w, h, R_WINDOW, SEL_BORDER, HONEY, 255);
                } else {
                    p.rrect_stroke(0, 0, w, h, R_WINDOW, 1, SAND, 255);
                }
            }
        }
    }

    fn paint_menubar(&self, p: &mut Painter, w: i32) {
        icons::logo(p, 12, 8, 13, ESPRESSO);
        for (k, (x, tw, name)) in self.menu_titles().into_iter().enumerate() {
            if self.menu == Some(k) {
                p.rrect(x - 8, 3, tw + 16, MENU_H - 6, 6, STRAW, SEL_ROW_A);
            }
            p.text_p(x, MENU_TEXT_Y, &name, ESPRESSO, k == 0);
        }
        let sw = super::fontx::prop_width(&self.status, false);
        let tx = w - MENU_PAD_X - sw;
        p.text_p(tx, MENU_TEXT_Y, &self.status, ESPRESSO, false);
        // A tiny per-CPU load meter: one bar per online CPU.
        let n = self.cpu_loads.len().max(1) as i32;
        let mx = tx - 14 - n * 6;
        for (i, &l) in self.cpu_loads.iter().enumerate() {
            let x = mx + i as i32 * 6;
            p.rrect(x, 7, 4, 14, 2, SAND, 255);
            let h = (14 * l.min(100) as i32 / 100).max(2);
            let c = if l > 75 { HONEY } else { MATCHA };
            p.rrect(x, 21 - h, 4, h, 2, c, 255);
        }
        p.text_p(
            mx - 4 - super::fontx::prop_width("cpu", false),
            MENU_TEXT_Y,
            "cpu",
            COCOA,
            false,
        );
    }

    fn paint_dock(&self, p: &mut Painter) {
        for (i, it) in tiles().iter().enumerate() {
            let ix = DOCK_PAD + i as i32 * (dock_icon() + DOCK_GAP);
            match it.icon {
                Some(icon) => {
                    icons::draw(p, ix, DOCK_PAD, dock_icon(), icon, it.bg, 255, it.fg, false)
                }
                None => {
                    // A package: its colour and initial.
                    let r = (dock_icon() * 24 + 50) / 100;
                    p.rrect(ix, DOCK_PAD, dock_icon(), dock_icon(), r, it.bg, 255);
                    let c: String = it
                        .name
                        .chars()
                        .take(1)
                        .flat_map(char::to_uppercase)
                        .collect();
                    let cw = crate::font::str_width(&c, 2);
                    p.text_x(
                        ix + (dock_icon() - cw) / 2,
                        DOCK_PAD + (dock_icon() - 32) / 2,
                        &c,
                        it.fg,
                        2,
                        true,
                    );
                }
            }
            // SPEC hover / pressed: lighten 4% / darken 4%.
            let r = (dock_icon() * 24 + 50) / 100;
            if self.dock_pressed == Some(i) {
                p.rrect(ix, DOCK_PAD, dock_icon(), dock_icon(), r, 0, a(40));
            } else if self.tip == Some(i) {
                p.rrect(ix, DOCK_PAD, dock_icon(), dock_icon(), r, WHITE, a(40));
            }
            let running = match it.action {
                DockAction::App(a) => self.windows.iter().any(|w| w.app == a),
                DockAction::Home => self.mode == Mode::Home,
                DockAction::Spaces => self.mode == Mode::Spaces,
                DockAction::Program(p) => self.running_program(p),
                DockAction::Pkg(k) => super::packages::get(k)
                    .is_some_and(|p| p.window && self.running_program(&p.exec)),
            };
            if running {
                p.rrect(
                    ix + dock_icon() / 2 - DOCK_DOT / 2,
                    dock_h() - DOCK_DOT_GAP - DOCK_DOT + 1,
                    DOCK_DOT,
                    DOCK_DOT,
                    2,
                    COCOA,
                    255,
                );
            }
        }
    }

    fn paint_control(&self, p: &mut Painter, solid: bool) {
        let s = &self.settings;
        // Tiles are milk at 0.7 over glass; over plain milk (solid mode) use oat so they show.
        let (tbg, ta) = if solid { (OAT, 255) } else { (MILK, a(700)) };
        let tile = |p: &mut Painter, tx: i32, label: &str, sub: &str, on: bool| {
            if on {
                p.rrect(tx, 16, 124, CC_TILE_H, 16, STRAW, 255);
            } else {
                p.rrect(tx, 16, 124, CC_TILE_H, 16, tbg, ta);
            }
            let (c1, c2) = if on { (MILK, MILK) } else { (ESPRESSO, COCOA) };
            p.text_p(tx + 14, 26, label, c1, true);
            p.text_p(tx + 14, 46, sub, c2, false);
        };
        tile(p, 16, "Network", "not yet", false);
        tile(
            p,
            148,
            "Focus",
            if s.focus_mode { "On" } else { "Off" },
            s.focus_mode,
        );
        for (i, (label, v)) in [("Display", s.brightness), ("Sound", s.volume)]
            .into_iter()
            .enumerate()
        {
            let sy = SLIDER_Y[i];
            p.rrect(16, sy, CC_W - 32, CC_TILE_H, 16, tbg, ta);
            p.text_p(30, sy + 8, label, ESPRESSO, true);
            p.rrect(TRACK_X, sy + 36, TRACK_W, CC_SLIDER_H, 4, COCOA, 0x40);
            p.rrect(
                TRACK_X,
                sy + 36,
                (TRACK_W * v / 100).max(8),
                CC_SLIDER_H,
                4,
                ESPRESSO,
                255,
            );
        }
        let up = fmt_uptime(crate::timer::now_ns());
        p.text(
            18,
            236,
            &format!("Up {}   cpu {}%", up, self.stats.cpu_pct),
            COCOA,
        );
        let t = rtc::now();
        p.text(
            18,
            258,
            &format!(
                "{} {} {}",
                &rtc::DAYS[t.weekday as usize][..3],
                t.day,
                rtc::MONTHS[t.month as usize - 1]
            ),
            COCOA,
        );
    }

    fn paint_launch_input(&self, p: &mut Painter, w: i32) {
        let Some(l) = &self.launcher else {
            return;
        };
        icons::draw(p, 14, 8, 40, Icon::Globe, CREAM, 255, COCOA, false);
        let end = if l.query.is_empty() {
            p.text_x(66, 12, "Search", LATTE, 2, false);
            66
        } else {
            p.text_x(66, 12, &l.query, ESPRESSO, 2, false)
        };
        p.fill(end, 12, 3, 32, ESPRESSO);
        p.text_right(w - 20, 20, "esc", COCOA, 1, false);
    }

    fn paint_launch_results(&self, p: &mut Painter, w: i32) {
        let Some(l) = &self.launcher else {
            return;
        };
        if l.items.is_empty() {
            p.text_p(64, 8 + 16, "No matches", COCOA, false);
        }
        let sel = l.sel.min(l.items.len().saturating_sub(1));
        for (y, rh, header, item) in l.layout() {
            if let Some(k) = header {
                p.text(16, y + 6, k.label(), COCOA);
                continue;
            }
            let Some(i) = item else {
                continue;
            };
            let e = &l.items[i];
            if i == sel {
                p.rrect(8, y, w - 16, rh, R_ROW, STRAW, SEL_ROW_A);
            }
            let border = e.bg == MILK || e.bg == CREAM;
            icons::draw(p, 16, y + 6, 36, e.icon, e.bg, 255, e.fg, border);
            p.text_p(64, y + 6, &e.title, ESPRESSO, true);
            p.text_p(64, y + 24, &e.sub, COCOA, false);
            if i == sel {
                p.text_right(w - 20, y + 16, "Enter", COCOA, 1, false);
            }
        }
        let fy = results_height(l) - 30;
        p.text_p(
            16,
            fy,
            "Tab next kind    = calculator    > run a command    Esc closes",
            COCOA,
            false,
        );
    }

    fn paint_card(&self, i: u8, p: &mut Painter, w: i32, _h: i32) {
        let t = rtc::now();
        let now = crate::timer::now_ns();
        let pad = CARD_PAD;
        let label = |p: &mut Painter, s: &str| {
            p.text(pad, pad, s, COCOA);
        };
        match i {
            0 => {
                let greet = match t.hour {
                    0..=4 => "Still up, Andrew?",
                    5..=11 => "Good morning, Andrew",
                    12..=17 => "Good afternoon, Andrew",
                    _ => "Good evening, Andrew",
                };
                p.text_x(pad + 8, 16, greet, ESPRESSO, 2, true);
                let n = self.windows.len();
                let line = format!(
                    "{}, {} {}   {} window{} open",
                    rtc::DAYS[t.weekday as usize],
                    t.day,
                    rtc::MONTHS[t.month as usize - 1],
                    n,
                    if n == 1 { "" } else { "s" }
                );
                p.text(pad + 8, 58, &line, COCOA);
                let clock = format!("{:02}:{:02}", t.hour, t.minute);
                p.text_right(w - 24, 16, &clock, ESPRESSO, 4, true);
            }
            1 => {
                label(p, "CALENDAR");
                for (k, d) in ["M", "T", "W", "T", "F", "S", "S"].iter().enumerate() {
                    p.text_center(12 + k as i32 * 38, 38, 40, d, COCOA, false);
                }
                // Last week, this week, next week; today in the middle row.
                let first = rtc::first_weekday(&t);
                let _ = first;
                let start = t.day as i32 - t.weekday as i32 - 7; // Monday of last week
                let prev_len = rtc::days_in_month(
                    if t.month == 1 { t.year - 1 } else { t.year },
                    if t.month == 1 { 12 } else { t.month - 1 },
                ) as i32;
                let len = rtc::days_in_month(t.year, t.month) as i32;
                for c in 0..21 {
                    let mut n = start + c;
                    if n < 1 {
                        n += prev_len;
                    } else if n > len {
                        n -= len;
                    }
                    let cx = 12 + (c % 7) * 38;
                    let cy = 66 + (c / 7) * 34;
                    let today = c == 7 + t.weekday as i32;
                    if today {
                        p.rrect(cx + 3, cy - 4, 32, 24, 12, STRAW, 255);
                    }
                    let s = format!("{n}");
                    p.text_center(cx, 38, cy, &s, if today { MILK } else { ESPRESSO }, today);
                }
                let m = format!("{} {}", rtc::MONTHS[t.month as usize - 1], t.year);
                p.text(pad, 180, &m, COCOA);
            }
            2 => {
                label(p, "SESSION");
                p.text_x(pad, 44, &fmt_uptime(now), ESPRESSO, 2, true);
                let n = self.windows.len();
                p.text_x(
                    pad,
                    84,
                    &format!("{} window{}", n, if n == 1 { "" } else { "s" }),
                    ESPRESSO,
                    2,
                    false,
                );
                p.text(
                    pad,
                    124,
                    &format!("space {} of {}", self.space + 1, self.nspaces),
                    COCOA,
                );
                p.text(pad, 176, "F3 Spaces   Ctrl+Space Skim", COCOA);
            }
            3 => {
                label(p, "SYSTEM");
                let ps = pmm::stats();
                let hs = heap::stats();
                let mem_used = 100 - ps.free_frames * 100 / ps.total_frames.max(1);
                let heap_used = hs.used * 100 / hs.total.max(1);
                let rows = [
                    ("cpu", self.stats.cpu_pct as i32, MATCHA),
                    ("mem", mem_used as i32, HONEY),
                    ("heap", heap_used as i32, BLUE),
                ];
                for (k, (name, v, c)) in rows.into_iter().enumerate() {
                    let y = 44 + k as i32 * 34;
                    p.text(pad, y, name, ESPRESSO);
                    p.rrect(64, y + 2, 200, 12, 6, SAND, 255);
                    p.rrect(
                        64,
                        y + 2,
                        (200 * v.clamp(0, 100) / 100).max(12),
                        12,
                        6,
                        c,
                        255,
                    );
                }
                p.text(pad, 156, &format!("Up {}", fmt_uptime(now)), COCOA);
                p.bold(pad, 180, "All systems fine", MATCHA);
            }
            4 => {
                label(p, "NOTES");
                let lines = apps::notes::snapshot();
                let mut y = 44;
                for (k, l) in lines.iter().take(5).enumerate() {
                    if k == 0 {
                        p.bold(pad, y, l, ESPRESSO);
                        y += 24;
                    } else {
                        p.text(pad, y, l, ESPRESSO);
                        y += 18;
                    }
                }
                let ago = apps::notes::edited_ago(now);
                p.text(pad, 150, &ago, LATTE);
            }
            _ => {
                label(p, "APPS");
                for (k, (app, icon, bg, ba, fg)) in HOME_APPS.iter().enumerate() {
                    let x = 16 + k as i32 * 106;
                    let border = *bg == CREAM;
                    icons::draw(p, x + 10, 44, 56, *icon, *bg, *ba, *fg, border);
                    let name = match app {
                        App::Settings => "Settings",
                        a => a.name(),
                    };
                    p.text_center(x - 6, 92, 108, name, ESPRESSO, false);
                }
            }
        }
    }

    fn paint_thumb(&self, i: usize, p: &mut Painter, w: i32, h: i32) {
        if i >= self.nspaces {
            p.text_center(0, w, 36, "+", COCOA, true);
            return;
        }
        // A tiny screen: each window of that space at its place, from the overview thumbnails.
        let (aw, ah) = (w - 8, 58);
        for win in self.windows.iter().filter(|x| x.space == i && !x.minimized) {
            let r = win.rect;
            let rr = Rect::new(
                4 + r.x * aw / screen_w(),
                4 + (r.y - MENU_H).max(0) * ah / (screen_h() - MENU_H),
                (r.w * aw / screen_w()).max(4),
                (r.h * ah / (screen_h() - MENU_H)).max(4),
            );
            match self.thumbs.get(&win.id) {
                Some(t) => p.blit_scaled(rr, t),
                None => p.rrect_r(rr, 3, CREAM, 255),
            }
        }
        p.text_p_center(0, w, 66, &space_label(i), COCOA, i == self.space);
        if i == self.space {
            p.rrect_stroke(0, 0, w, h, THUMB_R, SEL_BORDER, HONEY, 255);
        }
    }

    // ---- clicks ---------------------------------------------------------------------------

    pub(super) fn click_panel(&mut self, key: PanelKey, x: i32, y: i32) {
        match key {
            PanelKey::MenuBar => {
                let sw = self.status_width();
                if x < 30 {
                    // The logo: the system menu (About, Skim, Lock Screen, Restart ...).
                    let m = if self.menu == Some(0) { None } else { Some(0) };
                    self.control = false;
                    self.set_menu(m);
                } else if x >= screen_w() - MENU_PAD_X - sw - 8 {
                    self.set_menu(None);
                    self.control = !self.control;
                } else if let Some(t) = self.menu_title_at(x) {
                    let m = if self.menu == Some(t) { None } else { Some(t) };
                    self.control = false;
                    self.set_menu(m);
                } else {
                    self.set_menu(None);
                }
            }
            PanelKey::Dock => {
                let Some(i) = dock_index(x) else {
                    return;
                };
                self.dock_pressed = Some(i);
                self.control = false;
                let Some(t) = tiles().into_iter().nth(i) else {
                    return;
                };
                match t.action {
                    DockAction::App(a) => self.open(a),
                    DockAction::Home => self.toggle_mode(Mode::Home),
                    DockAction::Spaces => self.toggle_mode(Mode::Spaces),
                    DockAction::Program(path) => self.launch(path),
                    DockAction::Pkg(k) => {
                        if let Some(p) = super::packages::get(k) {
                            self.launch_package(&p);
                        }
                    }
                }
                self.invalidate(PanelKey::Dock);
            }
            PanelKey::Toast(id) => {
                self.toasts.retain(|t| t.id != id);
                let ids: Vec<u32> = self.toasts.iter().map(|t| t.id).collect();
                for i in ids {
                    self.invalidate(PanelKey::Toast(i));
                }
            }
            PanelKey::Control => {
                if hit(&Rect::new(148, 16, 124, CC_TILE_H), x, y) {
                    self.settings.focus_mode = !self.settings.focus_mode;
                    self.invalidate(PanelKey::Control);
                }
                for (i, sy) in SLIDER_Y.iter().enumerate() {
                    if hit(&Rect::new(16, *sy, CC_W - 32, CC_TILE_H), x, y) {
                        self.drag = Some(Drag::Slider(i));
                        self.slider_to(i, x + control_rect().x);
                    }
                }
            }
            PanelKey::LaunchInput => {}
            PanelKey::LaunchResults => {
                if let Some(l) = &mut self.launcher
                    && let Some(i) = l.item_at(y)
                {
                    l.sel = i;
                    self.launcher_activate();
                }
            }
            PanelKey::Card(5) => {
                for (k, (app, ..)) in HOME_APPS.iter().enumerate() {
                    let ix = 16 + k as i32 * 106 + 10;
                    if hit(&Rect::new(ix, 44, 56, 56), x, y) {
                        self.set_mode(Mode::Desktop);
                        self.open(*app);
                    }
                }
            }
            PanelKey::Card(_) => {}
            PanelKey::Thumb(i) => {
                let i = i as usize;
                if i >= self.nspaces {
                    self.nspaces = (self.nspaces + 1).min(MAX_SPACES);
                } else {
                    self.switch_space(i);
                }
                self.damage_all();
                self.invalidate_spaces();
            }
            PanelKey::Cell(id) => {
                self.drag = Some(Drag::Cell { id });
            }
            PanelKey::Tip => {}
            PanelKey::Menu => {
                if let Some(row) = self.menu_row_at(y) {
                    self.menu_activate(row);
                }
            }
        }
    }

    fn invalidate_spaces(&mut self) {
        let keys: Vec<PanelKey> = self
            .cache
            .keys()
            .filter(|k| matches!(k, PanelKey::Thumb(_) | PanelKey::Cell(_)))
            .copied()
            .collect();
        for k in keys {
            self.invalidate(k);
        }
    }

    /// Release after dragging an overview cell: onto a space thumbnail moves the window there;
    /// released where it was pressed focuses it and leaves the overview.
    pub(super) fn drop_cell(&mut self, id: u32, x: i32, y: i32) {
        let n = (self.nspaces + 1).min(MAX_SPACES);
        for i in 0..self.nspaces {
            if hit(&thumb_rect(i, n), x, y) {
                if let Some(w) = self.win_mut(id) {
                    w.space = i;
                }
                if self.focus == Some(id) {
                    self.focus_top();
                }
                self.invalidate_spaces();
                self.damage_all();
                return;
            }
        }
        if self
            .cells()
            .iter()
            .any(|(cid, r)| *cid == id && hit(r, x, y))
        {
            self.show_window(id);
        }
    }

    pub(super) fn slider_to(&mut self, i: usize, sx: i32) {
        let x = sx - control_rect().x - TRACK_X;
        let v = (x * 100 / TRACK_W).clamp(0, 100);
        if i == 0 {
            let v = v.max(20);
            if v != self.settings.brightness {
                self.settings.brightness = v;
                self.damage_all();
            }
        } else {
            self.settings.volume = v;
        }
        self.invalidate(PanelKey::Control);
    }

    // ---- launcher ---------------------------------------------------------------------------

    pub(super) fn toggle_launcher(&mut self) {
        if self.launcher.is_some() {
            self.close_launcher();
        } else {
            self.control = false;
            self.launcher = Some(Launcher::new());
        }
    }

    pub(super) fn close_launcher(&mut self) {
        self.launcher = None;
    }

    fn launcher_changed(&mut self) {
        self.invalidate(PanelKey::LaunchInput);
        self.invalidate(PanelKey::LaunchResults);
    }

    pub(super) fn launcher_key(&mut self, k: &super::window::KeyInput) {
        let Some(l) = &mut self.launcher else {
            return;
        };
        let n = l.items.len();
        match k.key {
            super::Key::Enter => return self.launcher_activate(),
            super::Key::Backspace => {
                l.query.pop();
                l.update();
            }
            super::Key::Up => l.sel = l.sel.saturating_sub(1),
            super::Key::Down => l.sel = (l.sel + 1).min(n.saturating_sub(1)),
            super::Key::Tab => {
                // First item of the next kind (wrapping).
                if n > 0 {
                    let cur = l.items[l.sel.min(n - 1)].kind;
                    l.sel = (l.sel..n).find(|&i| l.items[i].kind != cur).unwrap_or(0);
                }
            }
            _ => match k.ch {
                Some(c) if !c.is_control() && l.query.len() < 60 => {
                    l.query.push(c);
                    l.update();
                }
                _ => return,
            },
        }
        self.launcher_changed();
    }

    fn launcher_activate(&mut self) {
        let Some(l) = &self.launcher else {
            return;
        };
        let Some(e) = l.items.get(l.sel.min(l.items.len().saturating_sub(1))) else {
            return;
        };
        let act = e.act.clone();
        self.close_launcher();
        use super::skim::Act;
        match act {
            Act::Entry(a) => self.run_entry(a),
            Act::Action(f) => f(),
            Act::Open(path) => self.open_any(&path),
            Act::Setting(i) => {
                super::apps::settings::show_section(i);
                self.open(App::Settings);
            }
            Act::Package(k) => {
                if let Some(p) = super::packages::get(k) {
                    self.launch_package(&p);
                }
            }
            Act::Calc(r) => {
                if !r.is_empty() {
                    super::clipboard::set(&r);
                    self.post_toast(&format!("= {r}"), "copied to the clipboard");
                }
            }
            Act::Command(cmd) => {
                if !cmd.is_empty() {
                    super::request(super::Request::NewTab(Some(cmd)));
                }
            }
        }
    }

    fn run_entry(&mut self, action: Action) {
        match action {
            Action::App(a) => self.open(a),
            Action::Program(p) => self.launch(p),
            Action::Home => self.toggle_mode(Mode::Home),
            Action::Spaces => self.toggle_mode(Mode::Spaces),
            Action::Control => self.control = true,
            Action::NewSpace => {
                self.nspaces = (self.nspaces + 1).min(MAX_SPACES);
                self.set_mode(Mode::Spaces);
            }
        }
    }
}
