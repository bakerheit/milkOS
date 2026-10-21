//! Files: a placeholder browser with the SPEC sidebar (160 wide, oat, sand edge), an icon grid
//! and a status bar. The items are sample data until `vfs` lands: swap [`Files::items`] for a
//! directory listing then.

use alloc::format;

use crate::gfx::Rect;

use super::super::icons::{self, Icon};
use super::super::paint::{Painter, hit};
use super::super::theme::*;
use super::super::window::{PointerInput, PointerKind, WindowContents};

const FAVS: [&str; 5] = ["Documents", "Downloads", "Desktop", "Pictures", "Music"];
const DEVICES: [&str; 2] = ["milk-ssd", "Network"];

pub struct Files {
    place: usize,
    selected: Option<usize>,
}

impl Files {
    pub fn new() -> Files {
        Files {
            place: 0,
            selected: None,
        }
    }

    fn items(&self) -> &'static [(Icon, &'static str)] {
        &[
            (Icon::Folder, "projects"),
            (Icon::Folder, "school"),
            (Icon::Doc, "todo.txt"),
            (Icon::Photo, "bike.png"),
            (Icon::Note, "ideas.md"),
            (Icon::Folder, "kernel"),
            (Icon::Doc, "notes.txt"),
            (Icon::Term, "run.sh"),
        ]
    }

    fn cell(i: usize) -> (i32, i32) {
        (
            SIDEBAR_W + 24 + (i as i32 % 4) * 100,
            20 + (i as i32 / 4) * 108,
        )
    }
}

impl WindowContents for Files {
    fn title(&self) -> &str {
        FAVS.get(self.place).copied().unwrap_or("milk-ssd")
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "View", "Go", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        // Sidebar.
        p.fill(0, 0, SIDEBAR_W, h, OAT);
        p.fill(SIDEBAR_W - 1, 0, 1, h, SAND);
        p.text(16, 20, "FAVOURITES", COCOA);
        for (i, f) in FAVS.iter().enumerate() {
            let y = 44 + i as i32 * 28;
            if i == self.place {
                p.rrect(8, y, SIDEBAR_W - 16, 24, R_BUTTON, STRAW, SEL_SIDEBAR_A);
            }
            p.text(18, y + 4, f, ESPRESSO);
        }
        p.text(16, 200, "DEVICES", COCOA);
        for (i, d) in DEVICES.iter().enumerate() {
            let y = 222 + i as i32 * 28;
            if self.place == FAVS.len() + i {
                p.rrect(8, y, SIDEBAR_W - 16, 24, R_BUTTON, STRAW, SEL_SIDEBAR_A);
            }
            p.text(18, y + 4, d, ESPRESSO);
        }
        // Icon grid.
        for (i, (icon, name)) in self.items().iter().enumerate() {
            let (cx, cy) = Self::cell(i);
            if self.selected == Some(i) {
                p.rrect(cx - 4, cy - 4, 100, 96, R_ROW, STRAW, SEL_ROW_A);
            }
            let folder = *icon == Icon::Folder;
            let (bg, ba, fg) = if folder {
                (HONEY, FOLDER_A, COCOA)
            } else {
                (CREAM, 255, ESPRESSO)
            };
            icons::draw(p, cx + 12, cy, 56, *icon, bg, ba, fg, !folder);
            p.text_center(cx - 6, 92, cy + 64, name, ESPRESSO, false);
        }
        // Status bar.
        let sy = h - STATUS_H;
        p.fill(0, sy, w, STATUS_H, CREAM);
        p.fill(0, sy, w, 1, SAND);
        let n = self.items().len();
        p.text(
            SIDEBAR_W + 12,
            sy + 4,
            &format!("{n} items, sample data until vfs"),
            COCOA,
        );
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let PointerKind::Down(_) = e.kind else {
            return false;
        };
        if e.x < SIDEBAR_W {
            for i in 0..FAVS.len() + DEVICES.len() {
                let y = if i < FAVS.len() {
                    44 + i as i32 * 28
                } else {
                    222 + (i - FAVS.len()) as i32 * 28
                };
                if hit(&Rect::new(8, y, SIDEBAR_W - 16, 24), e.x, e.y) {
                    self.place = i;
                    return true;
                }
            }
            return false;
        }
        let sel = (0..self.items().len()).find(|&i| {
            let (cx, cy) = Self::cell(i);
            hit(&Rect::new(cx - 4, cy - 4, 100, 96), e.x, e.y)
        });
        if sel != self.selected {
            self.selected = sel;
            return true;
        }
        false
    }
}
