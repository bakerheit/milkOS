//! Menu bar drop-down menus. (The mockup shows only the titles; the menus use the same glass and
//! row styling as the launcher.) Clicking a title opens its menu, moving along the bar while one
//! is open switches menus, clicking an item runs it, clicking anywhere else closes it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::Rect;

use super::paint::Painter;
use super::shell::{Layer, PanelKey, PanelSpec, Style};
use super::theme::*;
use super::window::App;
use super::{Desktop, Mode};

const ROW_H: i32 = 24;
const SEP_H: i32 = 9;
const MENU_W: i32 = 248;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MenuAction {
    Open(App),
    Close,
    Minimise,
    Zoom,
    Cycle,
    Focus(u32),
    Skim,
    Today,
    Spaces,
    Control,
    NewSpace,
    Help,
    Restart,
    ShutDown,
    Lock,
    /// File > Browse Versions... for the focused window's document.
    Versions,
    /// Send Cmd+<key> to the focused window (Edit menu: cut, copy, paste, select all).
    Key(char),
    /// Shown greyed out (no clipboard yet).
    Disabled,
}

pub struct Item {
    pub label: String,
    pub key: &'static str,
    pub action: Option<MenuAction>,
}

fn item(label: &str, key: &'static str, a: MenuAction) -> Item {
    Item {
        label: String::from(label),
        key,
        action: Some(a),
    }
}

fn sep() -> Item {
    Item {
        label: String::new(),
        key: "",
        action: None,
    }
}

impl Desktop {
    /// Menu bar titles: (x, width, name), index 0 is the app name.
    pub(super) fn menu_titles(&self) -> Vec<(i32, i32, String)> {
        let mut v = Vec::new();
        let name = self.app_title();
        let mut x = 38;
        let w = super::fontx::prop_width(&name, true);
        v.push((x, w, name));
        x += w;
        for m in self.menu_names() {
            x += 24;
            let w = super::fontx::prop_width(m, false);
            v.push((x, w, String::from(*m)));
            x += w;
        }
        v
    }

    pub(super) fn menu_names(&self) -> &'static [&'static str] {
        match self.mode {
            Mode::Desktop => self
                .focused_window()
                .map_or(&["File", "Edit", "View", "Go", "Window", "Help"], |w| {
                    w.contents.menus()
                }),
            _ => &["View", "Window", "Help"],
        }
    }

    pub(super) fn menu_items(&self, idx: usize) -> Vec<Item> {
        let titles = self.menu_titles();
        let Some((_, _, name)) = titles.get(idx) else {
            return Vec::new();
        };
        let name = name.as_str();
        let has_win = self.focus.is_some() && self.mode == Mode::Desktop;
        let win = |a| if has_win { a } else { MenuAction::Disabled };
        if idx == 0 {
            return alloc::vec![
                item("About milkOS", "", MenuAction::Open(App::About)),
                sep(),
                item("Skim", "Ctrl+Space", MenuAction::Skim),
                item("Control centre", "", MenuAction::Control),
                sep(),
                item("Minimise", "Cmd+M", win(MenuAction::Minimise)),
                item("Close", "Cmd+W", win(MenuAction::Close)),
                sep(),
                item("Lock Screen", "Ctrl+Cmd+Q", MenuAction::Lock),
                item("Restart", "", MenuAction::Restart),
                item("Shut Down", "", MenuAction::ShutDown),
            ];
        }
        match name {
            "File" | "Shell" => {
                let mut v = alloc::vec![
                    item("Open Terminal", "", MenuAction::Open(App::Terminal)),
                    item("Open Files", "", MenuAction::Open(App::Files)),
                    item("Open Notes", "", MenuAction::Open(App::Notes)),
                    sep(),
                ];
                let doc = has_win
                    && self
                        .focused_window()
                        .is_some_and(|w| w.contents.document_path().is_some());
                if doc {
                    v.push(item("Browse Versions...", "", MenuAction::Versions));
                    v.push(sep());
                }
                v.push(item("Close Window", "Cmd+W", win(MenuAction::Close)));
                v
            }
            "Edit" | "Format" => alloc::vec![
                item("Undo", "Cmd+Z", MenuAction::Disabled),
                sep(),
                item("Cut", "Cmd+X", win(MenuAction::Key('x'))),
                item("Copy", "Cmd+C", win(MenuAction::Key('c'))),
                item("Paste", "Cmd+V", win(MenuAction::Key('v'))),
                item("Select All", "Cmd+A", win(MenuAction::Key('a'))),
            ],
            "View" => alloc::vec![
                item("Today", "F4", MenuAction::Today),
                item("Spaces", "F3", MenuAction::Spaces),
                sep(),
                item("New Space", "", MenuAction::NewSpace),
            ],
            "Go" => alloc::vec![
                item("Documents", "", MenuAction::Open(App::Files)),
                item("Today", "F4", MenuAction::Today),
            ],
            "Window" => {
                let mut v = alloc::vec![
                    item("Minimise", "Cmd+M", win(MenuAction::Minimise)),
                    item("Zoom", "", win(MenuAction::Zoom)),
                    item("Cycle Windows", "Cmd+Tab", MenuAction::Cycle),
                    sep(),
                ];
                for w in &self.windows {
                    let mark = if Some(w.id) == self.focus { "*" } else { "" };
                    v.push(Item {
                        label: format!("{} {}", w.contents.title(), mark),
                        key: if w.minimized { "minimised" } else { "" },
                        action: Some(MenuAction::Focus(w.id)),
                    });
                }
                v
            }
            "Help" => alloc::vec![item("milkOS Help", "", MenuAction::Help)],
            _ => Vec::new(),
        }
    }

    fn menu_height(items: &[Item]) -> i32 {
        12 + items
            .iter()
            .map(|i| if i.action.is_none() { SEP_H } else { ROW_H })
            .sum::<i32>()
    }

    pub(super) fn menu_layer(&self) -> Option<Layer> {
        let idx = self.menu?;
        let titles = self.menu_titles();
        let x = titles.get(idx)?.0;
        let items = self.menu_items(idx);
        let r = Rect::new(
            (x - 12).min(SCREEN_W - MENU_W - 8),
            MENU_H + 2,
            MENU_W,
            Self::menu_height(&items),
        );
        Some(Layer::Panel(PanelSpec {
            key: PanelKey::Menu,
            rect: r,
            radius: R_BUTTON,
            style: Style::GlassStrong,
            shadow: true,
        }))
    }

    /// Row index under (x, y) in panel coordinates, skipping separators.
    pub(super) fn menu_row_at(&self, y: i32) -> Option<usize> {
        let items = self.menu_items(self.menu?);
        let mut ry = 6;
        for (i, it) in items.iter().enumerate() {
            let h = if it.action.is_none() { SEP_H } else { ROW_H };
            if y >= ry && y < ry + h {
                return it.action.is_some().then_some(i);
            }
            ry += h;
        }
        None
    }

    pub(super) fn paint_menu(&self, p: &mut Painter, w: i32) {
        let Some(idx) = self.menu else {
            return;
        };
        let items = self.menu_items(idx);
        let mut y = 6;
        for (i, it) in items.iter().enumerate() {
            let Some(a) = it.action else {
                p.fill(12, y + 4, w - 24, 1, SAND);
                y += SEP_H;
                continue;
            };
            let disabled = a == MenuAction::Disabled;
            if self.menu_hover == Some(i) && !disabled {
                p.rrect(6, y, w - 12, ROW_H, 6, STRAW, SEL_ROW_A);
            }
            let c = if disabled { LATTE } else { ESPRESSO };
            p.text_p(16, y + 4, &it.label, c, false);
            if !it.key.is_empty() {
                p.text_right(
                    w - 14,
                    y + 4,
                    it.key,
                    if disabled { LATTE } else { COCOA },
                    1,
                    false,
                );
            }
            y += ROW_H;
        }
    }

    /// Run menu row `row` of the open menu (and close the menu).
    pub(super) fn menu_activate(&mut self, row: usize) {
        let Some(idx) = self.menu else {
            return;
        };
        let items = self.menu_items(idx);
        let Some(Some(a)) = items.get(row).map(|i| i.action) else {
            return;
        };
        self.menu = None;
        self.menu_hover = None;
        let focus = self.focus;
        match a {
            MenuAction::Open(app) => self.open(app),
            MenuAction::Close => {
                if let Some(id) = focus {
                    self.close(id);
                }
            }
            MenuAction::Minimise => {
                if let Some(id) = focus {
                    self.minimise(id);
                }
            }
            MenuAction::Zoom => {
                if let Some(id) = focus {
                    self.zoom(id);
                }
            }
            MenuAction::Cycle => self.cycle_windows(),
            MenuAction::Focus(id) => self.show_window(id),
            MenuAction::Skim => self.toggle_launcher(),
            MenuAction::Today => self.toggle_mode(Mode::Home),
            MenuAction::Spaces => self.toggle_mode(Mode::Spaces),
            MenuAction::Control => self.control = true,
            MenuAction::NewSpace => {
                self.nspaces = (self.nspaces + 1).min(6);
                self.set_mode(Mode::Spaces);
            }
            MenuAction::Help => self.post_toast("milkOS Help", "type 'help' in Terminal"),
            MenuAction::Lock => self.lock_screen(),
            MenuAction::Versions => {
                if let Some(p) = self
                    .focused_window()
                    .and_then(|w| w.contents.document_path())
                {
                    super::apps::versions::show(&p);
                }
            }
            MenuAction::Restart => super::power::restart(),
            MenuAction::ShutDown => super::power::shut_down(),
            MenuAction::Key(c) => {
                self.handle(super::Event::Key {
                    key: super::Key::Char(c),
                    ch: None,
                    mods: super::event::Mods::META,
                    down: true,
                });
            }
            MenuAction::Disabled => {}
        }
    }

    /// Which menu title is at menu bar x, if any.
    pub(super) fn menu_title_at(&self, x: i32) -> Option<usize> {
        self.menu_titles()
            .iter()
            .position(|(tx, tw, _)| x >= tx - 8 && x < tx + tw + 8)
    }
}
