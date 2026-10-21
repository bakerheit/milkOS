//! Drag and drop of files, started in Files (`WindowContents::drag_start`).
//!
//! While dragging, a ghost (icon + name) follows the pointer and whatever would take the
//! file is outlined. Dropping on:
//! * Notes: opens it (text files); the Versions window: shows its versions; Music: plays
//!   it (WAV); the Terminal: types the quoted path at the prompt (no Enter);
//! * a userland window: the user's grant of that one file to the process, through
//!   `proc::grant_file` (`caps.with_fs_grant`, like the powerbox; audited as powerbox.grant
//!   via=drop), delivered as an open fd in a Drop event;
//! * a dock tile: opens it with that app (Notes, Music, Files shows its folder, Terminal a
//!   tab in its folder; a packaged window app starts sandboxed with the file granted and
//!   its path as the argument).
//!
//! Anywhere else the drop does nothing.

use alloc::format;
use alloc::string::String;

use crate::gfx::Rect;

use super::apps::{fsx, music, notes};
use super::icons::{self, Icon};
use super::paint::{Painter, grow, hit};
use super::shell::{self, DockAction};
use super::theme::*;
use super::window::App;
use super::{Desktop, Request, request};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Target {
    Window(u32),
    Dock(usize),
}

pub struct Dnd {
    pub path: String,
    pub name: String,
    pub icon: Icon,
    pub from: u32,
    pub pos: (i32, i32),
    pub target: Option<Target>,
    /// The outline drawn around the target.
    pub outline: Option<Rect>,
}

fn ghost_rect(pos: (i32, i32), name: &str) -> Rect {
    let w = super::fontx::prop_width(name, false).max(40) + 20;
    Rect::new(pos.0 + 8, pos.1 + 6, w.max(56), 74)
}

/// The Terminal's quoting: single quotes, a quote inside becomes '\''.
pub fn quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

fn is_text(path: &str) -> bool {
    fsx::is_text(fsx::basename(path))
}

fn is_wav(path: &str) -> bool {
    path.to_ascii_lowercase().ends_with(".wav")
}

impl Desktop {
    /// A drag that started in window `from` turned out to be a file being dragged.
    pub(super) fn dnd_start(&mut self, from: u32, path: String) {
        let name = String::from(fsx::basename(&path));
        let icon = if fsx::is_dir(&path) {
            Icon::Folder
        } else if is_text(&path) {
            Icon::Note
        } else if is_wav(&path) {
            Icon::Music
        } else {
            Icon::Doc
        };
        self.dnd = Some(Dnd {
            path,
            name,
            icon,
            from,
            pos: self.pointer,
            target: None,
            outline: None,
        });
        let (x, y) = self.pointer;
        self.dnd_move(x, y);
    }

    /// What a drop at (x, y) would go to, if anything takes this file.
    fn dnd_target(&self, x: i32, y: i32, path: &str) -> Option<(Target, Rect)> {
        let dir = fsx::is_dir(path);
        if hit(&shell::dock_rect(), x, y) {
            let i = shell::dock_index(x - shell::dock_rect().x)?;
            let ok = match shell::dock_action(i)? {
                DockAction::App(App::Notes) => is_text(path),
                DockAction::App(App::Music) => is_wav(path),
                DockAction::App(App::Files) | DockAction::App(App::Terminal) => true,
                DockAction::Pkg(k) => !dir && super::packages::get(k).is_some_and(|p| p.window),
                _ => false,
            };
            let (cx, cy) = shell::dock_icon_center(i);
            let s = dock_icon();
            return ok.then_some((Target::Dock(i), Rect::new(cx - s / 2, cy - s / 2, s, s)));
        }
        let id = self.window_at(x, y)?;
        if self.dnd.as_ref().is_some_and(|d| d.from == id) {
            return None; // back onto where it came from: nothing
        }
        let w = self.win(id)?;
        let ok = match w.app {
            App::Notes => is_text(path),
            App::Music => is_wav(path),
            App::Terminal => true,
            App::Versions => !dir,
            App::Client(_) => !dir,
            _ => false,
        };
        ok.then_some((Target::Window(id), w.rect))
    }

    pub(super) fn dnd_move(&mut self, x: i32, y: i32) {
        let Some(d) = self.dnd.as_ref() else {
            return;
        };
        let old = (ghost_rect(d.pos, &d.name), d.outline);
        let path = d.path.clone();
        let t = self.dnd_target(x, y, &path);
        let d = self.dnd.as_mut().unwrap();
        d.pos = (x, y);
        d.target = t.map(|t| t.0);
        d.outline = t.map(|t| t.1);
        let new = (ghost_rect(d.pos, &d.name), d.outline);
        self.damage(grow(&old.0, 4));
        self.damage(grow(&new.0, 4));
        if old.1 != new.1 {
            for r in [old.1, new.1].into_iter().flatten() {
                self.damage(grow(&r, 6));
            }
        }
    }

    pub(super) fn dnd_cancel(&mut self) {
        if let Some(d) = self.dnd.take() {
            self.damage(grow(&ghost_rect(d.pos, &d.name), 4));
            if let Some(r) = d.outline {
                self.damage(grow(&r, 6));
            }
        }
    }

    pub(super) fn dnd_drop(&mut self) {
        let Some(d) = self.dnd.as_ref() else {
            return;
        };
        let (path, target, pos) = (d.path.clone(), d.target, d.pos);
        self.dnd_cancel();
        let name = String::from(fsx::basename(&path));
        match target {
            Some(Target::Window(id)) => {
                let Some(w) = self.win(id) else {
                    return;
                };
                let (app, rect) = (w.app, w.rect);
                self.set_focus(Some(id));
                match app {
                    App::Client(cid) => {
                        let b = super::window::body_rect(rect.w, rect.h);
                        let (bx, by) = (pos.0 - rect.x - b.x, pos.1 - rect.y - b.y);
                        if let Err(e) = super::client::drop_file(cid, bx, by, &path) {
                            self.post_toast(&format!("Can't give {name}"), &e);
                        }
                    }
                    _ => {
                        if let Some(w) = self.win_mut(id)
                            && w.contents.drop_path(&path)
                        {
                            w.dirty = true;
                        }
                    }
                }
            }
            Some(Target::Dock(i)) => match shell::dock_action(i) {
                Some(DockAction::App(App::Notes)) => {
                    notes::open_file(&path);
                    request(Request::OpenHere(App::Notes));
                }
                Some(DockAction::App(App::Music)) => {
                    music::play_path(&path);
                    request(Request::OpenHere(App::Music));
                }
                Some(DockAction::App(App::Files)) => {
                    let dir = if fsx::is_dir(&path) {
                        path.clone()
                    } else {
                        fsx::parent(&path)
                    };
                    super::apps::files::show_path(&dir);
                    request(Request::OpenHere(App::Files));
                }
                Some(DockAction::App(App::Terminal)) => {
                    let dir = if fsx::is_dir(&path) {
                        path.clone()
                    } else {
                        fsx::parent(&path)
                    };
                    self.open_tab(Some(format!("cd {}", quote(&dir))));
                }
                Some(DockAction::Pkg(k)) => {
                    if let Some(p) = super::packages::get(k) {
                        self.launch_package_with(&p, Some(&path));
                    }
                }
                _ => {}
            },
            None => {}
        }
    }
}

/// The ghost and the target's outline.
pub(super) fn draw(d: &Dnd, p: &mut Painter, clip: Rect) {
    if let Some(r) = d.outline
        && grow(&r, 6).intersects(&clip)
    {
        p.rrect_stroke(r.x - 3, r.y - 3, r.w + 6, r.h + 6, R_WINDOW, 3, HONEY, 255);
    }
    let g = ghost_rect(d.pos, &d.name);
    if !grow(&g, 4).intersects(&clip) {
        return;
    }
    let ix = g.x + (g.w - 40) / 2;
    icons::draw(p, ix, g.y, 40, d.icon, CREAM, 230, ESPRESSO, true);
    let lw = super::fontx::prop_width(&d.name, false) + 14;
    let lx = g.x + (g.w - lw) / 2;
    p.rrect(lx, g.y + 46, lw, 22, 8, HONEY, 235);
    p.text_p(lx + 7, g.y + 49, &d.name, INK, false);
}
