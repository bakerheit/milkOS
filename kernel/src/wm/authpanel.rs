//! The trusted administrator password panel (docs/security.md, "Authorisation tickets"):
//! "<exe> (pid N) wants to make changes. Enter your password to allow this." Drawn only by
//! the window system, like the Powerbox, never by the app that asks.
//!
//! The wm registers [`prompt`] with `proc::auth::set_prompt`. A process asking for a ticket
//! (MILK_AUTH op 0, e.g. Disk Utility before a rollback) runs it on its own thread: it
//! queues the request, posts `Request::Auth` to the desktop and sleeps until the panel
//! answers. The panel checks the password itself (`login::authenticate`, so a typo can be
//! fixed on the spot, three tries), then hands (user, password) back to the kernel, which
//! grants the ticket; the password goes nowhere else and never reaches the asking process.
//! It is typed into a masked field (dots), and the panel's copy is wiped once answered.
//! Cancel, Esc or closing the window answers "no".

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::Rect;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

use super::button::{self, Kind, State};
use super::event::Key;
use super::paint::{Painter, hit};
use super::theme::*;
use super::window::{KeyInput, PointerInput, PointerKind, WindowContents};
use super::{Request, request};

struct Req {
    id: u64,
    pid: u32,
    exe: String,
    reason: String,
    user: String,
}

struct Broker {
    next: u64,
    queue: VecDeque<Req>,
    active: Option<u64>,
    answers: Vec<(u64, Option<(String, String)>)>,
}

static BROKER: SpinLock<Broker> = SpinLock::new(Broker {
    next: 1,
    queue: VecDeque::new(),
    active: None,
    answers: Vec::new(),
});
static ANSWERED: WaitQueue = WaitQueue::new();

/// Registered with `proc::auth::set_prompt`: runs on the asking process's thread and blocks
/// until the user answers (None: cancelled, or the process was signalled).
pub fn prompt(pid: u32, reason: &str, default_user: &str) -> Option<(String, String)> {
    let exe = crate::proc::get(pid)
        .map(|p| String::from(p.inner.lock().name.as_str()))
        .unwrap_or_else(|| format!("pid {pid}"));
    let id = {
        let mut b = BROKER.lock();
        let id = b.next;
        b.next += 1;
        b.queue.push_back(Req {
            id,
            pid,
            exe,
            reason: reason.chars().take(256).collect(),
            user: String::from(default_user),
        });
        id
    };
    request(Request::Auth);
    loop {
        let got = {
            let mut b = BROKER.lock();
            let i = b.answers.iter().position(|a| a.0 == id);
            i.map(|i| b.answers.remove(i).1)
        };
        if let Some(a) = got {
            return a;
        }
        if crate::proc::signal::interrupted() {
            let mut b = BROKER.lock();
            b.queue.retain(|r| r.id != id);
            return None;
        }
        ANSWERED.wait_until_timeout(
            || BROKER.lock().answers.iter().any(|a| a.0 == id),
            100_000_000,
        );
    }
}

/// Is a request waiting for its panel?
pub fn pending() -> bool {
    let b = BROKER.lock();
    b.active.is_none() && !b.queue.is_empty()
}

fn take_next() -> Option<Req> {
    let mut b = BROKER.lock();
    if b.active.is_some() {
        return None;
    }
    let r = b.queue.pop_front()?;
    b.active = Some(r.id);
    Some(r)
}

fn answer(id: u64, a: Option<(String, String)>) {
    let more = {
        let mut b = BROKER.lock();
        if b.active == Some(id) {
            b.active = None;
        }
        b.answers.push((id, a));
        !b.queue.is_empty()
    };
    ANSWERED.wake_all();
    if more {
        request(Request::Auth);
    }
}

/// Overwrite a secret before it is dropped.
fn wipe(s: &mut String) {
    // SAFETY: zero bytes are valid UTF-8.
    unsafe { s.as_bytes_mut().fill(0) };
    s.clear();
}

/// Reasons that change or destroy data get the danger button (SPEC Buttons).
fn destructive(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    [
        "delete",
        "rollback",
        "roll back",
        "restore",
        "discard",
        "erase",
        "format",
        "mkfs",
        "remove",
        "lock",
        "promote",
        "receive",
        "repair",
        "prune",
    ]
    .iter()
    .any(|w| r.contains(w))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    User,
    Password,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Hit {
    User,
    Password,
    Cancel,
    Allow,
}

pub struct AuthPanel {
    req: Option<Req>,
    user: String,
    pw: String,
    focus: Field,
    error: Option<&'static str>,
    tries: u32,
    done: bool,
    hits: Vec<(Rect, Hit)>,
    hover: Option<(i32, i32)>,
}

impl AuthPanel {
    pub fn new() -> AuthPanel {
        let req = take_next();
        let user = req.as_ref().map_or_else(String::new, |r| r.user.clone());
        AuthPanel {
            req,
            user,
            pw: String::new(),
            focus: Field::Password,
            error: None,
            tries: 0,
            done: false,
            hits: Vec::new(),
            hover: None,
        }
    }

    fn finish(&mut self, ok: bool) {
        if self.done {
            return;
        }
        self.done = true;
        if let Some(r) = &self.req {
            let a = ok.then(|| (self.user.clone(), self.pw.clone()));
            answer(r.id, a);
        }
        wipe(&mut self.pw);
    }

    /// Allow: check the password here (a typo can be fixed), then answer.
    fn allow(&mut self) {
        if self.pw.is_empty() {
            self.error = Some("Type your password.");
            return;
        }
        match crate::proc::login::authenticate(&self.user, &self.pw) {
            Ok(a) if a.is_admin() => self.finish(true),
            Ok(_) => {
                self.error = Some("That account is not an administrator.");
                wipe(&mut self.pw);
            }
            Err(_) => {
                self.tries += 1;
                wipe(&mut self.pw);
                if self.tries >= 3 {
                    self.finish(false);
                } else {
                    self.error = Some("That password is not right. Try again.");
                }
            }
        }
    }

    fn hov(&self, r: &Rect) -> State {
        State::hover(self.hover.is_some_and(|(x, y)| hit(r, x, y)))
    }
}

impl Drop for AuthPanel {
    fn drop(&mut self) {
        // Closed with the red light (or the desktop going away): that's a cancel.
        self.finish(false);
    }
}

/// A padlock glyph, `s` across, on a honey tile.
fn padlock(p: &mut Painter, x: i32, y: i32, s: i32) {
    p.rrect(x, y, s, s, s * 24 / 100, HONEY, 255);
    let (bw, bh) = (s * 46 / 100, s * 34 / 100);
    let (bx, by) = (x + (s - bw) / 2, y + s * 46 / 100);
    let ring = s * 34 / 100;
    p.ring(
        x + (s - ring) / 2,
        by - ring / 2 - 1,
        ring,
        (s / 16).max(2),
        INK,
        255,
    );
    p.rrect(bx, by, bw, bh, 3, INK, 255);
    p.circle(x + s / 2 - 2, by + bh / 2 - 2, 5, HONEY, 255);
}

impl WindowContents for AuthPanel {
    fn title(&self) -> &str {
        "Authorisation"
    }

    fn menus(&self) -> &'static [&'static str] {
        &["Edit", "Window"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.hits.clear();
        let Some(r) = &self.req else {
            p.text_p(20, 20, "Nothing is asking for authorisation.", COCOA, false);
            return;
        };
        padlock(p, 20, 18, 48);
        p.text_p(
            84,
            20,
            &format!("{} wants to make changes.", r.exe),
            ESPRESSO,
            true,
        );
        p.text_p(84, 40, "Enter your password to allow this.", COCOA, false);
        // What it asked for, as the app put it (just text).
        let rr = Rect::new(20, 80, w - 40, 44);
        p.rrect_r(rr, 10, OAT, 255);
        let max = ((rr.w - 24) / 7).max(8) as usize;
        let reason: String = r.reason.chars().take(max).collect();
        p.text_p(rr.x + 12, rr.y + 6, &reason, ESPRESSO, false);
        p.text_p(
            rr.x + 12,
            rr.y + 24,
            &format!("pid {}", r.pid),
            COCOA,
            false,
        );
        // The fields.
        let fx = 130;
        let fields = [
            (Field::User, "User name", 140, Hit::User),
            (Field::Password, "Password", 178, Hit::Password),
        ];
        for (f, label, fy, hk) in fields {
            p.text_p(20, fy + 6, label, ESPRESSO, false);
            let fr = Rect::new(fx, fy, w - fx - 20, 28);
            let focused = self.focus == f;
            p.rrect_r(fr, 8, MILK, 255);
            p.rrect_stroke(
                fr.x,
                fr.y,
                fr.w,
                fr.h,
                8,
                if focused { 2 } else { 1 },
                if focused { HONEY } else { SAND },
                255,
            );
            let end = match f {
                Field::User => p.text_p(fr.x + 10, fr.y + 6, &self.user, ESPRESSO, false),
                Field::Password => {
                    // Masked: one dot a character.
                    let n = self.pw.chars().count().min(((fr.w - 24) / 12) as usize) as i32;
                    for k in 0..n {
                        p.circle(fr.x + 12 + k * 12, fr.y + 10, 8, ESPRESSO, 255);
                    }
                    fr.x + 12 + n * 12 - 2
                }
            };
            if focused {
                p.fill(end + 2, fr.y + 6, 2, 16, ESPRESSO);
            }
            self.hits.push((fr, hk));
        }
        if let Some(e) = self.error {
            p.text_p(fx, 214, e, button::DANGER, true);
        }
        // Footer: whose panel this is, Cancel and Allow.
        let fy = h - 52;
        p.fill(0, fy, w, 1, SAND);
        p.text_p(
            20,
            fy + 18,
            "This panel is milkOS's, not the app's.",
            COCOA,
            false,
        );
        let by = fy + 12;
        let allow = Rect::new(w - 20 - 96, by, 96, button::DIALOG_H);
        let cancel = Rect::new(allow.x - 8 - 96, by, 96, button::DIALOG_H);
        let kind = if destructive(&r.reason) {
            Kind::Danger
        } else {
            Kind::Primary
        };
        button::paint(p, cancel, "Cancel", Kind::Secondary, self.hov(&cancel));
        button::paint(p, allow, "Allow", kind, self.hov(&allow));
        self.hits.push((cancel, Hit::Cancel));
        self.hits.push((allow, Hit::Allow));
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let PointerKind::Down(_) = e.kind else {
            return false;
        };
        let Some(&(_, h)) = self.hits.iter().find(|(r, _)| hit(r, e.x, e.y)) else {
            return false;
        };
        match h {
            Hit::User => self.focus = Field::User,
            Hit::Password => self.focus = Field::Password,
            Hit::Cancel => self.finish(false),
            Hit::Allow => self.allow(),
        }
        true
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        let at = |h: &Option<(i32, i32)>, hits: &[(Rect, Hit)]| {
            h.and_then(|(x, y)| hits.iter().position(|(r, _)| hit(r, x, y)))
        };
        let before = at(&self.hover, &self.hits);
        self.hover = (x >= 0).then_some((x, y));
        before != at(&self.hover, &self.hits)
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        match k.key {
            Key::Escape => self.finish(false),
            Key::Enter => self.allow(),
            Key::Tab => {
                self.focus = match self.focus {
                    Field::User => Field::Password,
                    Field::Password => Field::User,
                };
            }
            Key::Backspace => {
                match self.focus {
                    Field::User => self.user.pop(),
                    Field::Password => self.pw.pop(),
                };
            }
            _ => match k.ch {
                Some(c) if !c.is_control() && !k.mods.meta && !k.mods.ctrl => {
                    self.error = None;
                    match self.focus {
                        Field::User if self.user.len() < 32 => self.user.push(c),
                        Field::Password if self.pw.len() < 128 => self.pw.push(c),
                        _ => {}
                    }
                }
                _ => return false,
            },
        }
        true
    }

    fn wants_close(&self) -> bool {
        self.done
    }
}
