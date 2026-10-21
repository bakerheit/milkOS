//! Welcome to milkOS: a short tour of what is different here, shown on first boot (until
//! "Don't show again") and from the logo menu. Every button really does the thing:
//! a Time Machine snapshot, Versions of a sample note, `milk try` in a Terminal tab,
//! sandboxed apps and the audit log, Chocolate milk, Spaces. Cards that can't work on
//! this machine (no Milk disk, no package repo, moo2048 not installed) say why instead.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs;
use crate::gfx::{Color, Rect, rgb};

use super::super::icons::{self, Icon};
use super::super::paint::{Painter, hit};
use super::super::theme::*;
use super::super::window::{PointerInput, PointerKind, WindowContents};
use super::super::{Mode, Request, packages, prefs, request};
use super::{files, fsx, versions};

const SAMPLE: &str = "/home/milk/Documents/Welcome note.txt";
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
}

fn has_milkfs() -> bool {
    fs::milkfs::mount_path().is_some()
}

/// A repo URL in /etc/milk/repos (reachable or not: milk will say).
fn has_repo() -> bool {
    fs::read_to_string("/etc/milk/repos").is_ok_and(|t| {
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
                        "milk try installs into a throwaway",
                        "clone of the disk: keep it or bin it.",
                        "Needs the repo: milk-repo.sh serve.",
                    ]
                } else {
                    [
                        "milk try installs into a throwaway",
                        "clone of the disk. No package repo",
                        "is set up in /etc/milk/repos.",
                    ]
                },
                icon: Icon::Package,
                bg: LAV,
                fg: INK,
                buttons: alloc::vec![(
                    String::from("milk try fortune"),
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
                        "folder (milk install moo2048 for one).",
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
                fg: CREAM,
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
            Do::Snapshot => match fsx::ctl_write("/sys/fs/milkfs/auto", "now") {
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
                request(Request::NewTab(Some(String::from("milk try fortune"))));
                String::from("Then: milk keep fortune, or discard")
            }
            Do::Moo => {
                request(Request::OpenPackage(String::from("moo2048")));
                String::from("moo2048: its own folder, no network")
            }
            Do::Peek => {
                request(Request::OpenAny(String::from("/bin/peek")));
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
        // Header: the drop, hello, and the promise.
        icons::drop_shape(p, 44, 52, 18, 40, BLUE);
        icons::drop_shape(p, 44, 52, 14, 32, MILK);
        let x2 = p.text_x(78, 20, "Welcome to ", ESPRESSO, 2, true);
        let x3 = p.text_x(x2, 20, "milk", ESPRESSO, 2, true);
        p.text_x(x3, 20, "OS", BLUE, 2, true);
        p.text_p(
            78,
            58,
            "A small system made of milk. Six things it does differently;",
            COCOA,
            false,
        );
        p.text_p(78, 76, "every button below really does it.", COCOA, false);
        // Cards, two columns.
        let x0 = (w - 2 * CARD_W - GAP) / 2;
        for (i, c) in self.cards().iter().enumerate() {
            let cx = x0 + (i as i32 % 2) * (CARD_W + GAP);
            let cy = TOP + (i as i32 / 2) * (CARD_H + GAP);
            let r = Rect::new(cx, cy, CARD_W, CARD_H);
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
            for (k, l) in c.lines.iter().enumerate() {
                p.text_p(cx + 62, cy + 34 + k as i32 * 17, l, COCOA, false);
            }
            let mut bx = cx + 62;
            for (label, act) in &c.buttons {
                let bw = super::super::fontx::prop_width(label, true) + 24;
                let br = Rect::new(bx, cy + CARD_H - 32, bw, 24);
                let (bg, fg) = match act {
                    Some(_) => (STRAW, MILK),
                    None => (OAT, LATTE),
                };
                p.rrect_r(br, R_BUTTON, bg, 255);
                p.text_p_center(br.x, br.w, br.y + 4, label, fg, act.is_some());
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
            p.fill(cb.x + 4, cb.y + 9, 3, 3, MILK);
            p.fill(cb.x + 7, cb.y + 11, 3, 3, MILK);
            p.fill(cb.x + 10, cb.y + 5, 3, 7, MILK);
        }
        let end = p.text_p(cb.x + 26, fy + 5, "Don't show again", ESPRESSO, false);
        self.hits
            .push((Rect::new(cb.x, cb.y - 4, end - cb.x + 6, 26), Do::DontShow));
        if !self.said.is_empty() {
            let max = ((w - 2 * x0 - 380) / 7).max(8) as usize;
            let s: String = self.said.chars().take(max).collect();
            p.text_p(end + 24, fy + 5, &s, COCOA, false);
        }
        let go = Rect::new(w - x0 - 132, fy, 132, 28);
        p.rrect_r(go, R_BUTTON, ESPRESSO, 255);
        p.text_p_center(go.x, go.w, go.y + 6, "Get started", MILK, true);
        self.hits.push((go, Do::Close));
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
