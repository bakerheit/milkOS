//! Welcome to milkOS: a short tour of what is different here, shown on first boot (until
//! "Don't show again") and from the logo menu. Every button really does the thing:
//! a Time Machine snapshot, Versions of a sample note, `apps try` in a Terminal tab,
//! sandboxed apps and the audit log, Chocolate milk, Spaces. Cards that can't work on
//! this machine (no Milk disk, no package repo, moo2048 not installed) say why instead.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs;
use crate::gfx::{Color, Rect, rgb};

use super::super::button::{self, Kind, State};
use super::super::icons::{self, Icon};
use super::super::paint::{Painter, hit};
use super::super::theme::*;
use super::super::window::{
    PointerInput, PointerKind, WindowContents, hover_index, hover_move, paint_hover,
};
use super::super::{Mode, Request, packages, prefs, request};
use super::{files, fsx, versions};

const SAMPLE: &str = "/Users/milk/Documents/Welcome note.txt";
const SAMPLE_VERSIONS: [&str; 3] = [
    "Things to try in milkOS:\n- Ctrl+Space\n",
    "Things to try in milkOS:\n- Ctrl+Space\n- F3 for Spaces\n",
    "Things to try in milkOS:\n- Ctrl+Space\n- F3 for Spaces\n- drag a window to the edge\n",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Do {
    Snapshot,
    Versions,
    TryFortune,
    Moo,
    Peek,
    Audit,
    Theme,
    Spaces,
    DontShow,
    Close,
    /// The next page of cards (a short window shows four at a time).
    NextPage,
}

struct Card {
    title: &'static str,
    lines: [&'static str; 3],
    icon: Icon,
    bg: Color,
    fg: Color,
    buttons: Vec<(String, Option<Do>)>,
}

pub struct Welcome {
    hits: Vec<(Rect, Do)>,
    closing: bool,
    /// Last thing a button did (shown in the footer).
    said: String,
    hover: Option<(i32, i32)>,
    /// Which page of cards a short window (an 800x480 panel) shows.
    page: usize,
}

fn has_milkfs() -> bool {
    fs::milkfs::mount_path().is_some()
}

/// A repo URL in /System/Settings/milk/repos (reachable or not: milk will say).
fn has_repo() -> bool {
    fs::read_to_string("/System/Settings/milk/repos").is_ok_and(|t| {
        t.lines().any(|l| {
            l.trim_start().starts_with("http://") || l.trim_start().starts_with("https://")
        })
    })
}

fn has_moo() -> bool {
    packages::all().iter().any(|p| p.name == "moo2048")
}

/// The sample note, with three versions in snapshots the first time.
fn sample_versions() -> Result<(), String> {
    if !has_milkfs() {
        return Err(String::from(
            "Versions live on the Milk disk, and there isn't one",
        ));
    }
    let n = fs::milkfs::versions(SAMPLE).map_or(0, |v| v.len());
    if n < 3 {
        let _ = fs::mkdir_all(fsx::DOCS, 0o755);
        for (i, text) in SAMPLE_VERSIONS.iter().enumerate() {
            fs::write_file(SAMPLE, text.as_bytes()).map_err(|e| format!("{e}"))?;
            if i + 1 < SAMPLE_VERSIONS.len() {
                let _ = fs::sync_all();
                let name = format!("welcome-{}", i + 1);
                if fs::milkfs::snapshots().is_ok_and(|s| s.iter().all(|x| x.name != name)) {
                    fs::milkfs::snapshot(&name).map_err(|e| format!("snapshot: {e}"))?;
                }
            }
        }
        let _ = fs::sync_all();
    }
    Ok(())
}

impl Welcome {
    pub fn new() -> Welcome {
        Welcome {
            hits: Vec::new(),
            closing: false,
            said: String::new(),
            hover: None,
            page: 0,
        }
    }

    fn cards(&self) -> [Card; 6] {
        let dark = prefs::get().dark;
        let tm = has_milkfs();
        let repo = has_repo();
        let moo = has_moo();
        [
            Card {
                title: "Time Machine",
                lines: if tm {
                    [
                        "The whole Milk disk, snapshotted",
                        "copy-on-write in an instant. Take one",
                        "now, then walk around inside it.",
                    ]
                } else {
                    [
                        "Snapshots of the Milk disk, in an",
                        "instant. This boot has no Milk disk",
                        "(scripts/run.sh attaches one).",
                    ]
                },
                icon: Icon::Clock,
                bg: HONEY,
                fg: INK,
                buttons: alloc::vec![(String::from("Take a snapshot"), tm.then_some(Do::Snapshot))],
            },
            Card {
                title: "Every file has a past",
                lines: [
                    "Versions shows a file across all",
                    "its snapshots, side by side. Here's",
                    "a little note with three of them.",
                ],
                icon: Icon::Note,
                bg: CREAM,
                fg: ESPRESSO,
                buttons: alloc::vec![(
                    String::from("Show its versions"),
                    tm.then_some(Do::Versions)
                )],
            },
            Card {
                title: "Try before you install",
                lines: if repo {
                    [
                        "apps try installs into a throwaway",
                        "clone of the disk: keep it or toss it.",
                        "Needs the repo: milk-repo.sh serve.",
                    ]
                } else {
                    [
                        "apps try installs into a throwaway",
                        "clone of the disk. No package repo",
                        "is set up in /System/Settings/milk/repos.",
                    ]
                },
                icon: Icon::Package,
                bg: LAV,
                fg: INK,
                buttons: alloc::vec![(
                    String::from("apps try fortune"),
                    (repo && tm).then_some(Do::TryFortune)
                )],
            },
            Card {
                title: "Apps get a sandbox",
                lines: if moo {
                    [
                        "Installed apps see only their own",
                        "folder. Peek sees nothing, tries to",
                        "read your todo list, and is refused.",
                    ]
                } else {
                    [
                        "Installed apps see only their own",
                        "folder (apps install moo2048 for one).",
                        "Peek sees nothing and is refused.",
                    ]
                },
                icon: Icon::Folder,
                bg: STRAW,
                fg: INK,
                buttons: alloc::vec![
                    (String::from("moo2048"), moo.then_some(Do::Moo)),
                    (String::from("Peek"), Some(Do::Peek)),
                    (String::from("Audit log"), Some(Do::Audit)),
                ],
            },
            Card {
                title: if dark {
                    "Chocolate milk"
                } else {
                    "Whole or chocolate"
                },
                lines: [
                    "One click turns the whole desktop",
                    "dark roast, night pasture and all.",
                    "Settings keeps whichever you pick.",
                ],
                icon: Icon::Gear,
                bg: rgb(0x6B, 0x55, 0x4A),
                fg: TERM_FG, // fixed light: cream turns dark in Chocolate milk
                buttons: alloc::vec![(
                    String::from(if dark {
                        "Back to Whole milk"
                    } else {
                        "Chocolate milk"
                    }),
                    Some(Do::Theme)
                )],
            },
            Card {
                title: "Spaces",
                lines: [
                    "Desktops side by side: F3 or",
                    "Ctrl+Up shows them all, and you",
                    "can drag windows between them.",
                ],
                icon: Icon::Spaces,
                bg: MILK,
                fg: ESPRESSO,
                buttons: alloc::vec![(String::from("Open Spaces"), Some(Do::Spaces))],
            },
        ]
    }

    fn run(&mut self, d: Do) {
        self.said = match d {
            Do::Snapshot => match fsx::ctl_write("/System/Kernel/fs/milkfs/auto", "now") {
                Ok(()) => {
                    files::show_path("timemachine:");
                    request(Request::OpenHere(super::super::window::App::Files));
                    String::from("Snapshot taken: it's in Time Machine now")
                }
                Err(e) => format!("Couldn't take a snapshot: {e}"),
            },
            Do::Versions => match sample_versions() {
                Ok(()) => {
                    versions::show(SAMPLE);
                    String::from("Pick an older one, then Restore")
                }
                Err(e) => e,
            },
            Do::TryFortune => {
                request(Request::NewTab(Some(String::from("apps try fortune"))));
                String::from("Then: apps keep fortune, or discard")
            }
            Do::Moo => {
                request(Request::OpenPackage(String::from("moo2048")));
                String::from("moo2048: its own folder, no network")
            }
            Do::Peek => {
                request(Request::OpenAny(String::from("peek")));
                String::from("Refused! Press o in Peek to pick a file")
            }
            Do::Audit => {
                request(Request::NewTab(Some(String::from("audit --event sandbox"))));
                String::from("enter = launched, deny = refused")
            }
            Do::Theme => {
                let mut p = prefs::get();
                p.dark = !p.dark;
                request(Request::Prefs(p));
                String::new()
            }
            Do::Spaces => {
                request(Request::Mode(Mode::Spaces));
                String::new()
            }
            Do::DontShow => {
                let mut p = prefs::get();
                p.welcome = !p.welcome;
                request(Request::Prefs(p));
                String::from(if p.welcome {
                    "Welcome will show at startup"
                } else {
                    "OK. It stays in the logo menu"
                })
            }
            Do::Close => {
                self.closing = true;
                String::new()
            }
            Do::NextPage => {
                self.page += 1; // wraps in draw
                String::new()
            }
        };
    }
}

const CARD_W: i32 = 332;
const CARD_H: i32 = 124;
const GAP: i32 = 14;
const TOP: i32 = 104;

impl WindowContents for Welcome {
    fn title(&self) -> &str {
        "Welcome to milkOS"
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Window", "Help"]
    }

    fn wants_close(&self) -> bool {
        self.closing
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.hits.clear();
        // A short window (an 800x480 panel): a one-line header, tighter cards, four to a page.
        let compact = h < 540;
        let (top, card_h, pitch, btn_y) = if compact {
            (50, 108, 15, 80)
        } else {
            (TOP, CARD_H, 17, CARD_H - 32)
        };
        // Header: the drop, hello, and the promise.
        icons::drop_shape(p, 44, if compact { 34 } else { 52 }, 18, 40, BLUE);
        icons::drop_shape(p, 44, if compact { 34 } else { 52 }, 14, 32, MILK);
        let x2 = p.text_x(78, if compact { 10 } else { 20 }, "Welcome to ", ESPRESSO, 2, true);
        let mark = crate::sysfont::Font::of(crate::sysfont::BRAND, 2, crate::sysfont::Weight::Bold);
        let x3 = p.text_f(x2, if compact { 10 } else { 20 }, "milk", ESPRESSO, mark);
        p.text_f(x3, if compact { 10 } else { 20 }, "OS", BLUE, mark);
        if compact {
            p.text_p(x3 + 56, 24, "Every button below really does it.", COCOA, false);
        } else {
            p.text_p(
                78,
                58,
                "A small system made of milk. Six things it does differently;",
                COCOA,
                false,
            );
            p.text_p(78, 76, "every button below really does it.", COCOA, false);
        }
        // Cards, two columns; as many rows as the window has room for, the rest on later pages.
        // (Cards are 332 wide; a window narrower than 700 gets narrower ones and shorter lines.)
        let card_w = ((w - 2 * 12 - GAP) / 2).min(CARD_W);
        let x0 = (w - 2 * card_w - GAP) / 2;
        let gap = if compact { 8 } else { GAP };
        let rows = (((h - top - 54 + gap) / (card_h + gap)).clamp(1, 3)) as usize;
        let per_page = rows * 2;
        let pages = 6usize.div_ceil(per_page);
        self.page %= pages;
        // The pills paint their own hover (SPEC Buttons).
        let mut pills: Vec<Rect> = Vec::new();
        for (i, c) in self
            .cards()
            .iter()
            .enumerate()
            .skip(self.page * per_page)
            .take(per_page)
        {
            let i = i - self.page * per_page;
            let cx = x0 + (i as i32 % 2) * (card_w + GAP);
            let cy = top + (i as i32 / 2) * (card_h + gap);
            let r = Rect::new(cx, cy, card_w, card_h);
            p.rrect_r(r, 16, CREAM, 255);
            p.rrect_stroke(r.x, r.y, r.w, r.h, 16, 1, SAND, 255);
            icons::draw(
                p,
                cx + 14,
                cy + 14,
                36,
                c.icon,
                c.bg,
                255,
                c.fg,
                c.bg == MILK,
            );
            p.text_p(cx + 62, cy + 14, c.title, ESPRESSO, true);
            let body = crate::sysfont::Font::body(false);
            for (k, l) in c.lines.iter().enumerate() {
                let l = body.truncate(l, card_w - 62 - 10);
                p.text_p(cx + 62, cy + 34 + k as i32 * pitch, &l, COCOA, false);
            }
            let mut bx = cx + 62;
            for (label, act) in &c.buttons {
                let bw = button::pill_w(label);
                let br = Rect::new(bx, cy + btn_y, bw, button::PILL_H);
                let kind = if act.is_some() {
                    Kind::Primary
                } else {
                    Kind::Disabled
                };
                let hov = self.hover.is_some_and(|(x, y)| hit(&br, x, y));
                button::paint(p, br, label, kind, State::hover(hov && act.is_some()));
                if act.is_some() {
                    pills.push(br);
                }
                if let Some(a) = act {
                    self.hits.push((br, *a));
                }
                bx += bw + 8;
            }
        }
        // Footer: what just happened, Don't show again, Get started.
        let fy = h - 44;
        p.fill(0, fy - 10, w, 1, SAND);
        let off = !prefs::get().welcome;
        let cb = Rect::new(x0, fy + 4, 18, 18);
        p.rrect_r(cb, 5, if off { STRAW } else { MILK }, 255);
        p.rrect_stroke(
            cb.x,
            cb.y,
            cb.w,
            cb.h,
            5,
            1,
            if off { STRAW } else { LATTE },
            255,
        );
        if off {
            // The tick is ink: a glyph on straw (milk on straw is 2.4:1).
            p.fill(cb.x + 4, cb.y + 9, 3, 3, INK);
            p.fill(cb.x + 7, cb.y + 11, 3, 3, INK);
            p.fill(cb.x + 10, cb.y + 5, 3, 7, INK);
        }
        let end = p.text_p(cb.x + 26, fy + 5, "Don't show again", ESPRESSO, false);
        self.hits
            .push((Rect::new(cb.x, cb.y - 4, end - cb.x + 6, 26), Do::DontShow));
        // More cards than fit: "More" turns the page.
        let mut right = w - x0 - 132;
        if pages > 1 {
            let label = format!("More ({}/{pages})", self.page + 1);
            let bw = button::pill_w(&label);
            let br = Rect::new(right - 12 - bw, fy + 1, bw, button::PILL_H);
            let hov = self.hover.is_some_and(|(x, y)| hit(&br, x, y));
            button::paint(p, br, &label, Kind::Secondary, State::hover(hov));
            pills.push(br);
            self.hits.push((br, Do::NextPage));
            right = br.x - 12;
        }
        if !self.said.is_empty() {
            // Clear of "Get started" (and "More") on the right.
            let room = right - 16 - (end + 24);
            let s = crate::sysfont::Font::body(false).truncate(&self.said, room);
            p.text_p(end + 24, fy + 5, &s, COCOA, false);
        }
        let go = Rect::new(w - x0 - 132, fy, 132, 28);
        p.rrect_r(go, R_BUTTON, ESPRESSO, 255);
        p.text_p_center(go.x, go.w, go.y + 6, "Get started", MILK, true);
        self.hits.push((go, Do::Close));
        if let Some(i) = hover_index(&self.hits, self.hover)
            && !pills.contains(&self.hits[i].0)
        {
            paint_hover(p, self.hits[i].0, false);
        }
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        hover_move(&self.hits, &mut self.hover, x, y)
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let PointerKind::Down(_) = e.kind else {
            return false;
        };
        let Some(d) = self
            .hits
            .iter()
            .find(|(r, _)| hit(r, e.x, e.y))
            .map(|x| x.1)
        else {
            return false;
        };
        self.run(d);
        true
    }

    fn tick(&mut self, _now: u64) -> bool {
        false
    }
}
