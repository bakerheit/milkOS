//! Milk Drop in the desktop (SPEC "Milk Drop sheet", mockups share.html), built only on the
//! `drop` command line tool (docs/networking.md, "Hook for the desktop"), which the desktop
//! runs on a tty of its own ([`tty::Job`]):
//!
//! * peers: `MILK_TABLE=1 drop list` (name, address, fingerprint, receiving, trust, host),
//!   rescanned about every 3 s while the sheet is open (one scan takes about 1.5 s);
//! * send: `drop send <name> <path>`: its `drop: [####......]  40%  416 KiB / 1024 KiB`
//!   lines drive the progress bar; exit 0 is sent, exit 1 shows the last `drop: <reason>`;
//!   the boxed `KEY HAS CHANGED` warning (trusted / offered fingerprints) becomes the
//!   key-changed panel, whose one button runs `drop forget <name>`;
//! * receive: the Milk Drop window runs `drop recv -k`; `drop: <who> wants to send you
//!   <file> (<size>)` then `accept? [y/N]` becomes the incoming panel, which types `y` or `n`.
//!
//! [`Sheet`] is the sending sheet that Files shows over its body; [`Receiver`] is the
//! milkOS-owned incoming panel (a window, in the Powerbox style).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{Color, Rect};
use crate::sysfont::{Font, Weight};

use super::super::button::{self, DANGER, DANGER_FG, Kind, State};
use super::super::event::Key;
use super::super::icons;
use super::super::paint::{Painter, hit};
use super::super::theme::*;
use super::super::tty::Job;
use super::super::window::{App, KeyInput, PointerInput, PointerKind, WindowContents};
use super::super::{Request, notify_as, request};
use super::fsx;

const DROP: &str = "/bin/drop";
const ENV: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb"];
const ENV_TABLE: &[&str] = &["PATH=/bin", "HOME=/home/milk", "TERM=dumb", "MILK_TABLE=1"];
/// Rescan this long after the last scan finished.
const RESCAN_NS: u64 = 3_000_000_000;

// ---- peers ------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trust {
    New,
    Known,
    Rotated,
    Changed,
}

#[derive(Clone, Debug)]
pub struct Peer {
    pub name: String,
    pub fp: String,
    pub receiving: bool,
    pub trust: Trust,
}

/// Unescape a milk-table field (`\\`, `\t`, `\n`).
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse `MILK_TABLE=1 drop list` output (`\x1emilk-table 1`, a header of `name:type`
/// columns, tab-separated rows). Lines before the magic (or with no table) give nothing.
pub fn parse_list(lines: &[String]) -> Vec<Peer> {
    let Some(start) = lines.iter().position(|l| l.contains("milk-table 1")) else {
        return Vec::new();
    };
    let Some(header) = lines.get(start + 1) else {
        return Vec::new();
    };
    let cols: Vec<&str> = header
        .split('\t')
        .map(|c| c.split(':').next().unwrap_or(c))
        .collect();
    let col = |name: &str| cols.iter().position(|c| *c == name);
    let (Some(ni), Some(fi), Some(ri), Some(ti)) = (
        col("name"),
        col("fingerprint"),
        col("receiving"),
        col("trust"),
    ) else {
        return Vec::new();
    };
    lines[start + 2..]
        .iter()
        .filter_map(|l| {
            let f: Vec<String> = l.split('\t').map(unescape).collect();
            let get = |i: usize| f.get(i).cloned().unwrap_or_default();
            let name = get(ni);
            if name.is_empty() || name.starts_with("drop:") {
                return None;
            }
            let t = get(ti);
            let trust = if t.contains("CHANGED") {
                Trust::Changed
            } else if t.starts_with("known") {
                Trust::Known
            } else if t == "rotated" {
                Trust::Rotated
            } else {
                Trust::New
            };
            Some(Peer {
                name,
                fp: get(fi),
                receiving: get(ri) == "yes",
                trust,
            })
        })
        .collect()
}

/// The last `drop: <reason>` line (an error), without the prefix.
fn last_reason(lines: &[String]) -> String {
    lines
        .iter()
        .rev()
        .filter_map(|l| l.trim().strip_prefix("drop: "))
        .find(|l| !l.starts_with('*') && !l.starts_with('['))
        .map_or_else(|| String::from("Milk Drop stopped"), String::from)
}

/// `drop: [####......]  40%  416 KiB / 1024 KiB` -> (40, "416 KiB of 1024 KiB").
fn parse_progress(l: &str) -> Option<(u32, String)> {
    let rest = l.trim().strip_prefix("drop: [")?;
    let (_, after) = rest.split_once(']')?;
    let after = after.trim();
    let (pct, nums) = after.split_once('%')?;
    let pct: u32 = pct.trim().parse().ok()?;
    let nums = nums.trim().replace(" / ", " of ");
    Some((pct, nums))
}

/// A fingerprint as shown: four groups of four.
fn fp_short(fp: &str) -> String {
    let hex: String = fp
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .take(16)
        .collect();
    let mut s = String::new();
    for (i, c) in hex.chars().enumerate() {
        if i > 0 && i % 4 == 0 {
            s.push(' ');
        }
        s.push(c);
    }
    s
}

fn size_text(n: u64) -> String {
    if n >= 1024 * 1024 {
        format!(
            "{}.{} MB",
            n / (1024 * 1024),
            n % (1024 * 1024) * 10 / (1024 * 1024)
        )
    } else {
        fsx::human_size(n)
    }
}

/// A small device tile: a coloured rounded square with a ring and a dot (the mockup's).
fn device_tile(p: &mut Painter, x: i32, y: i32, s: i32, c: Color, dim: bool) {
    let r = s * 24 / 100;
    p.rrect(x, y, s, s, r, if dim { SAND } else { c }, 255);
    let g = if dim { COCOA } else { INK };
    let d = s * 11 / 20;
    p.ring(x + (s - d) / 2, y + (s - d) / 2, d, (s / 14).max(2), g, 255);
    p.circle(x + s / 2 - 3, y + s / 2 - 3, 6, g, 255);
}

fn tile_colour(i: usize) -> Color {
    [BLUE, LAV, MATCHA, HONEY, STRAW][i % 5]
}

/// A small spinner: three dots, one brighter, turning with time.
fn spinner(p: &mut Painter, x: i32, y: i32, now: u64) {
    let k = (now / 250_000_000 % 3) as i32;
    for i in 0..3 {
        let c = if i == k { ESPRESSO } else { SAND };
        p.circle(x + i * 9, y, 6, c, 255);
    }
}

// ---- the sending sheet ------------------------------------------------------------------

enum Phase {
    /// Pick a device (scanning in the background).
    Pick,
    /// `drop send` running.
    Sending {
        job: Job,
        lines: Vec<String>,
        pct: u32,
        nums: String,
        accepted: bool,
    },
    Failed(String),
    /// Sent: the window closes the sheet.
    Sent,
    /// The peer's key does not match the trusted one: (old, new) fingerprints.
    KeyChanged {
        name: String,
        old: String,
        new: String,
    },
    /// `drop forget <name>` running.
    Forgetting(Job),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SHit {
    Row(usize),
    Cancel,
    Send,
    Back,
    Forget,
    LookAgain,
}

/// What the sheet wants from its window after an input.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Out {
    Nothing,
    Redraw,
    Close,
}

pub struct Sheet {
    path: String,
    name: String,
    size: u64,
    peers: Vec<Peer>,
    /// The chosen device, by name (survives rescans).
    sel: Option<String>,
    scan: Option<(Job, Vec<String>)>,
    scanned: bool,
    next_scan: u64,
    phase: Phase,
    hits: Vec<(Rect, SHit)>,
    hover: Option<(i32, i32)>,
    pub sheet_w: i32,
}

impl Sheet {
    pub const W: i32 = 520;
    const HEADER: i32 = 44;
    const ROW: i32 = 64;
    const ROW_GAP: i32 = 12;
    const FOOT: i32 = 56;

    /// Share `path`, with `device` chosen already if given (a file dropped on its name).
    pub fn new(path: &str, device: Option<&str>) -> Sheet {
        let size = crate::fs::stat(path).map_or(0, |s| s.size);
        let mut s = Sheet {
            path: String::from(path),
            name: String::from(fsx::basename(path)),
            size,
            peers: Vec::new(),
            sel: device.map(String::from),
            scan: None,
            scanned: false,
            next_scan: 0,
            phase: Phase::Pick,
            hits: Vec::new(),
            hover: None,
            sheet_w: Self::W,
        };
        s.start_scan();
        s
    }

    fn start_scan(&mut self) {
        if self.scan.is_none() {
            self.scan = Job::start(DROP, &["drop", "list"], ENV_TABLE).map(|j| (j, Vec::new()));
        }
    }

    fn chosen(&self) -> Option<&Peer> {
        let s = self.sel.as_deref()?;
        self.peers.iter().find(|p| p.name == s)
    }

    fn can_send(&self) -> bool {
        self.chosen()
            .is_some_and(|p| p.receiving || p.trust == Trust::Changed)
    }

    /// Background work: scans, the transfer. True if it needs a redraw.
    pub fn tick(&mut self, now: u64) -> bool {
        let mut changed = false;
        // Scanning (while picking).
        if let Some((job, lines)) = &mut self.scan {
            lines.extend(job.output());
            if job.exit().is_some() {
                lines.extend(job.output());
                let peers = parse_list(lines);
                self.scan = None;
                self.scanned = true;
                self.next_scan = now + RESCAN_NS;
                self.peers = peers;
                // Choose the only receiving device when nothing is chosen yet.
                if self.sel.is_none() {
                    let ready: Vec<&Peer> = self.peers.iter().filter(|p| p.receiving).collect();
                    if ready.len() == 1 {
                        self.sel = Some(ready[0].name.clone());
                    }
                }
                changed = true;
            }
        } else if matches!(self.phase, Phase::Pick) && now >= self.next_scan {
            self.start_scan();
            changed = true;
        }
        if self.scan.is_some() {
            changed = true; // the spinner turns
        }
        // Sending.
        let mut next: Option<Phase> = None;
        match &mut self.phase {
            Phase::Sending {
                job,
                lines,
                pct,
                nums,
                accepted,
            } => {
                for l in job.output() {
                    if let Some((p, n)) = parse_progress(&l) {
                        *pct = p;
                        *nums = n;
                        *accepted = true;
                    }
                    lines.push(l);
                    changed = true;
                }
                if let Some(code) = job.exit() {
                    lines.extend(job.output());
                    let peer = self.sel.clone().unwrap_or_default();
                    if code == 0 {
                        notify_as(
                            "Milk Drop",
                            &format!("Sent {}", self.name),
                            &format!("to {peer}, checked with SHA-256"),
                        );
                        next = Some(Phase::Sent);
                    } else if lines.iter().any(|l| l.contains("KEY HAS CHANGED")) {
                        let fp_of = |key: &str| {
                            lines
                                .iter()
                                .find_map(|l| l.split_once(key).map(|(_, f)| fp_short(f)))
                                .unwrap_or_default()
                        };
                        next = Some(Phase::KeyChanged {
                            name: peer,
                            old: fp_of("trusted fingerprint:"),
                            new: fp_of("offered fingerprint:"),
                        });
                    } else {
                        next = Some(Phase::Failed(last_reason(lines)));
                    }
                    changed = true;
                }
            }
            Phase::Forgetting(job) => {
                let _ = job.output();
                if job.exit().is_some() {
                    next = Some(Phase::Pick);
                    self.next_scan = 0;
                    changed = true;
                }
            }
            _ => {}
        }
        if let Some(p) = next {
            self.phase = p;
        }
        changed
    }

    /// Done (sent): the window closes the sheet.
    pub fn finished(&self) -> bool {
        matches!(self.phase, Phase::Sent)
    }

    fn send(&mut self) {
        let Some(peer) = self.chosen().map(|p| p.name.clone()) else {
            return;
        };
        let path = self.path.clone();
        if let Some(job) = Job::start(DROP, &["drop", "send", &peer, &path], ENV) {
            self.scan = None;
            self.phase = Phase::Sending {
                job,
                lines: Vec::new(),
                pct: 0,
                nums: String::new(),
                accepted: false,
            };
        } else {
            self.phase = Phase::Failed(String::from("no free terminal to run drop on"));
        }
    }

    /// The sheet's rect for a body `w` x `h` (hangs from under the toolbar at `top`).
    pub fn rect(&self, w: i32, h: i32, top: i32) -> Rect {
        let n = self.peers.len().max(1) as i32;
        let inner = match &self.phase {
            Phase::Pick if self.scanned && self.peers.is_empty() => 150,
            Phase::Pick => 44 + n * (Self::ROW + Self::ROW_GAP),
            Phase::Sending { .. } => 140,
            Phase::Failed(_) => 110,
            Phase::KeyChanged { .. } => 230,
            Phase::Forgetting(_) | Phase::Sent => 90,
        };
        let sh = (Self::HEADER + inner + Self::FOOT).min(h - top - 8);
        let sw = self.sheet_w.min(w - 16);
        Rect::new((w - sw) / 2, top - 2, sw, sh)
    }

    fn hov(&self, r: &Rect) -> State {
        State::hover(self.hover.is_some_and(|(x, y)| hit(r, x, y)))
    }

    /// Draw over the window body, the sheet at `r` (body coordinates).
    pub fn draw(&mut self, p: &mut Painter, r: Rect, now: u64) {
        self.hits.clear();
        p.shadow_window(r);
        p.rrect_r(r, 14, MILK, 255);
        p.rrect_stroke(r.x, r.y, r.w, r.h, 14, 1, SAND, 255);
        let mut sp = p.sub(r);
        let (w, h) = (r.w, r.h);
        let title = match &self.phase {
            Phase::Sending { .. } => format!("Sending to {}", self.sel.clone().unwrap_or_default()),
            _ => String::from("Milk Drop"),
        };
        sp.rrect(0, 0, w, Self::HEADER, 14, CREAM, 255);
        sp.fill(0, Self::HEADER - 14, w, 14, CREAM);
        sp.fill(0, Self::HEADER, w, 1, SAND);
        let end = sp.text_p(16, 14, &title, ESPRESSO, true);
        if self.scan.is_some() && matches!(self.phase, Phase::Pick) {
            spinner(&mut sp, end + 14, 19, now);
        }
        sp.text_right(w - 16, 14, "Esc", COCOA, 1, false);
        let fy = h - Self::FOOT;
        sp.fill(0, fy, w, 1, SAND);
        let by = fy + 14;
        let mut hits: Vec<(Rect, SHit)> = Vec::new();
        match &self.phase {
            Phase::Pick if self.scanned && self.peers.is_empty() => {
                let cy = Self::HEADER + 18;
                sp.circle(w / 2 - 36, cy, 72, OAT, 255);
                let f = Font::ui(2, Weight::Bold);
                sp.text_center_f(w / 2 - 36, 72, f.center_caps(cy, 72), "?", COCOA, f);
                sp.text_p_center(
                    0,
                    w,
                    cy + 84,
                    "No other milkOS machines found",
                    ESPRESSO,
                    true,
                );
                sp.text_p_center(
                    0,
                    w,
                    cy + 104,
                    "Milk Drop looks on the link to your other milkOS.",
                    COCOA,
                    false,
                );
                let la = Rect::new(16, by, button::dialog_w("Look again"), button::DIALOG_H);
                let cl = Rect::new(w - 16 - 88, by, 88, button::DIALOG_H);
                button::paint(
                    &mut sp,
                    la,
                    "Look again",
                    Kind::Secondary,
                    self.hov_at(r, la),
                );
                button::paint(&mut sp, cl, "Close", Kind::Primary, self.hov_at(r, cl));
                hits.push((la, SHit::LookAgain));
                hits.push((cl, SHit::Cancel));
            }
            Phase::Pick => {
                sp.text_p(
                    16,
                    Self::HEADER + 14,
                    &format!(
                        "Sending {} ({}). Pick a device on this link.",
                        self.name,
                        size_text(self.size)
                    ),
                    COCOA,
                    false,
                );
                if self.peers.is_empty() {
                    sp.text_p(
                        16,
                        Self::HEADER + 52,
                        "Looking for other milkOS machines...",
                        COCOA,
                        false,
                    );
                }
                for (i, pe) in self.peers.iter().enumerate() {
                    let y = Self::HEADER + 44 + i as i32 * (Self::ROW + Self::ROW_GAP);
                    let rr = Rect::new(12, y, w - 24, Self::ROW);
                    if y + Self::ROW > fy {
                        break;
                    }
                    let sel = self.sel.as_deref() == Some(pe.name.as_str());
                    let off = !pe.receiving && pe.trust != Trust::Changed;
                    if sel {
                        sp.rrect_r(rr, 12, STRAW, a(200));
                        sp.rrect_stroke(rr.x, rr.y, rr.w, rr.h, 12, 2, STRAW, 255);
                    } else {
                        sp.rrect_r(rr, 12, if off { CREAM } else { MILK }, 255);
                        sp.rrect_stroke(rr.x, rr.y, rr.w, rr.h, 12, 1, SAND, 255);
                    }
                    device_tile(&mut sp, rr.x + 12, y + 12, 40, tile_colour(i), off);
                    let (c1, c2) = if off {
                        (COCOA, COCOA)
                    } else if sel {
                        (ESPRESSO, ESPRESSO)
                    } else {
                        (ESPRESSO, COCOA)
                    };
                    sp.text_p(rr.x + 64, y + 12, &pe.name, c1, true);
                    sp.text_p(
                        rr.x + 64,
                        y + 34,
                        &format!("fp {}", fp_short(&pe.fp)),
                        c2,
                        false,
                    );
                    // Trust chip (SPEC): known matcha, new honey, key changed danger.
                    let (label, fill, fa, text) = match (pe.trust, pe.receiving) {
                        (Trust::Changed, _) => ("KEY CHANGED", DANGER, 255, DANGER_FG),
                        (_, false) => ("Not receiving", OAT, 255, COCOA),
                        (Trust::New, _) => ("New device", HONEY, a(270), ESPRESSO),
                        (Trust::Rotated, _) => ("Key rotated", MATCHA, a(270), ESPRESSO),
                        (Trust::Known, _) => ("Known", MATCHA, a(270), ESPRESSO),
                    };
                    let cw = crate::sysfont::width(label, true) + 24;
                    let chip = Rect::new(rr.x + rr.w - 12 - cw, y + 9, cw, 24);
                    sp.rrect_r(chip, 12, fill, fa);
                    sp.text_p_center(chip.x, chip.w, chip.y + 4, label, text, true);
                    let sub = if off {
                        "open Milk Drop there"
                    } else if pe.trust == Trust::Changed {
                        "sending blocked"
                    } else {
                        "ready"
                    };
                    sp.text_right(
                        rr.x + rr.w - 14,
                        y + 38,
                        sub,
                        if sel { ESPRESSO } else { COCOA },
                        1,
                        false,
                    );
                    hits.push((rr, SHit::Row(i)));
                }
                sp.text_p(16, by + 6, "Encrypted, checked with SHA-256.", COCOA, false);
                let send = Rect::new(w - 16 - 88, by, 88, button::DIALOG_H);
                let cancel = Rect::new(send.x - 8 - 88, by, 88, button::DIALOG_H);
                button::paint(
                    &mut sp,
                    cancel,
                    "Cancel",
                    Kind::Secondary,
                    self.hov_at(r, cancel),
                );
                let ok = self.can_send();
                let k = if ok { Kind::Primary } else { Kind::Disabled };
                button::paint(&mut sp, send, "Send", k, self.hov_at(r, send));
                hits.push((cancel, SHit::Cancel));
                if ok {
                    hits.push((send, SHit::Send));
                }
            }
            Phase::Sending {
                pct,
                nums,
                accepted,
                ..
            } => {
                let y = Self::HEADER + 16;
                let icon = fsx::icon_for(&fsx::Entry {
                    name: self.name.clone(),
                    kind: crate::fs::InodeType::File,
                    size: self.size,
                });
                icons::draw(&mut sp, 16, y, 56, icon, CREAM, 255, ESPRESSO, true);
                let peer = self.sel.clone().unwrap_or_default();
                let e = sp.text_p(84, y + 4, &self.name, ESPRESSO, true);
                sp.text_p(e + 12, y + 4, &format!("to {peer}"), COCOA, false);
                let n = if nums.is_empty() {
                    format!("{} to send", size_text(self.size))
                } else {
                    nums.clone()
                };
                sp.text_p(84, y + 24, &n, COCOA, false);
                let bw = w - 84 - 16;
                sp.rrect(84, y + 50, bw, 12, 6, SAND, 255);
                let fw = (bw * *pct as i32 / 100).max(if *pct > 0 { 12 } else { 0 });
                if fw > 0 {
                    sp.rrect(84, y + 50, fw, 12, 6, STRAW, 255);
                }
                sp.text_p(84, y + 70, &format!("{pct}%"), ESPRESSO, true);
                let foot = if *accepted {
                    format!("{peer} accepted. Checking SHA-256 when done.")
                } else {
                    format!("Waiting for {peer} to accept...")
                };
                sp.text_p(16, by + 6, &foot, COCOA, false);
                let cancel = Rect::new(w - 16 - 88, by, 88, button::DIALOG_H);
                button::paint(
                    &mut sp,
                    cancel,
                    "Cancel",
                    Kind::Secondary,
                    self.hov_at(r, cancel),
                );
                hits.push((cancel, SHit::Cancel));
            }
            Phase::Failed(reason) => {
                let er = Rect::new(16, Self::HEADER + 16, w - 32, 72);
                sp.rrect_r(er, 12, DANGER, a(120));
                sp.rrect_stroke(er.x, er.y, er.w, er.h, 12, 1, DANGER, 255);
                sp.text_p(er.x + 14, er.y + 12, "Not sent", ESPRESSO, true);
                let t = Font::body(false).truncate(reason, er.w - 28);
                sp.text_p(er.x + 14, er.y + 36, &t, ESPRESSO, false);
                let close = Rect::new(w - 16 - 88, by, 88, button::DIALOG_H);
                let back = Rect::new(close.x - 8 - 88, by, 88, button::DIALOG_H);
                button::paint(&mut sp, back, "Back", Kind::Secondary, self.hov_at(r, back));
                button::paint(
                    &mut sp,
                    close,
                    "Close",
                    Kind::Primary,
                    self.hov_at(r, close),
                );
                hits.push((back, SHit::Back));
                hits.push((close, SHit::Cancel));
            }
            Phase::KeyChanged { name, old, new } => {
                let y = Self::HEADER + 1;
                sp.fill(0, y, w, 64, DANGER);
                sp.circle(16, y + 10, 44, DANGER_FG, 255);
                let f = Font::ui(2, Weight::Bold);
                sp.text_center_f(16, 44, f.center_caps(y + 10, 44), "!", DANGER, f);
                sp.text_p(
                    72,
                    y + 12,
                    &format!("{name}'s key has changed"),
                    DANGER_FG,
                    true,
                );
                sp.text_p(
                    72,
                    y + 34,
                    "Sending is blocked until you decide.",
                    DANGER_FG,
                    false,
                );
                let ty = y + 76;
                sp.text_p(
                    16,
                    ty,
                    &format!("This usually means {name} was reinstalled."),
                    ESPRESSO,
                    false,
                );
                sp.text_p(
                    16,
                    ty + 18,
                    &format!("It could also mean someone is pretending to be {name}."),
                    ESPRESSO,
                    false,
                );
                let bw = (w - 32 - 12) / 2;
                let b1 = Rect::new(16, ty + 48, bw, 64);
                let b2 = Rect::new(16 + bw + 12, ty + 48, bw, 64);
                sp.rrect_r(b1, 12, OAT, 255);
                sp.rrect_r(b2, 12, DANGER, a(100));
                sp.rrect_stroke(b2.x, b2.y, b2.w, b2.h, 12, 1, DANGER, 255);
                sp.text_p(b1.x + 12, b1.y + 10, "Trusted before", COCOA, false);
                sp.text_p(b1.x + 12, b1.y + 34, old, ESPRESSO, true);
                sp.text_p(b2.x + 12, b2.y + 10, "Now", ESPRESSO, false);
                sp.text_p(b2.x + 12, b2.y + 34, new, ESPRESSO, true);
                sp.text_p(
                    16,
                    ty + 126,
                    &format!("Ask {name} to read you their fingerprint, in person or by phone."),
                    COCOA,
                    false,
                );
                let label = format!("Forget {name} and re-verify");
                let fw = button::dialog_w(&label);
                let forget = Rect::new(w - 16 - fw, by, fw, button::DIALOG_H);
                let cancel = Rect::new(forget.x - 8 - 88, by, 88, button::DIALOG_H);
                button::paint(
                    &mut sp,
                    cancel,
                    "Cancel",
                    Kind::Secondary,
                    self.hov_at(r, cancel),
                );
                button::paint(
                    &mut sp,
                    forget,
                    &label,
                    Kind::Danger,
                    self.hov_at(r, forget),
                );
                hits.push((cancel, SHit::Cancel));
                hits.push((forget, SHit::Forget));
            }
            Phase::Forgetting(_) => {
                sp.text_p(
                    16,
                    Self::HEADER + 20,
                    "Forgetting the old key...",
                    COCOA,
                    false,
                );
            }
            Phase::Sent => {
                sp.text_p(16, Self::HEADER + 20, "Sent.", ESPRESSO, true);
            }
        }
        // Hit rects in body coordinates.
        self.hits = hits
            .into_iter()
            .map(|(h, t)| (Rect::new(h.x + r.x, h.y + r.y, h.w, h.h), t))
            .collect();
    }

    /// Hover state for a rect given in sheet coordinates.
    fn hov_at(&self, r: Rect, b: Rect) -> State {
        self.hov(&Rect::new(b.x + r.x, b.y + r.y, b.w, b.h))
    }

    /// The pointer moved (body coordinates). True if a redraw is needed.
    pub fn hover(&mut self, x: i32, y: i32) -> bool {
        let at = |h: &Option<(i32, i32)>, hits: &[(Rect, SHit)]| {
            h.and_then(|(x, y)| hits.iter().position(|(r, _)| hit(r, x, y)))
        };
        let before = at(&self.hover, &self.hits);
        self.hover = (x >= 0).then_some((x, y));
        before != at(&self.hover, &self.hits)
    }

    /// A press in the body (body coordinates).
    pub fn click(&mut self, x: i32, y: i32) -> Out {
        let Some(h) = self
            .hits
            .iter()
            .find(|(r, _)| hit(r, x, y))
            .map(|(_, h)| *h)
        else {
            return Out::Nothing;
        };
        match h {
            SHit::Row(i) => {
                self.sel = self.peers.get(i).map(|p| p.name.clone());
            }
            SHit::Send => self.send(),
            SHit::Cancel => return self.cancel(),
            SHit::Back => self.phase = Phase::Pick,
            SHit::LookAgain => {
                self.scanned = false;
                self.start_scan();
            }
            SHit::Forget => {
                if let Phase::KeyChanged { name, .. } = &self.phase {
                    let name = name.clone();
                    match Job::start(DROP, &["drop", "forget", &name], ENV) {
                        Some(j) => self.phase = Phase::Forgetting(j),
                        None => self.phase = Phase::Pick,
                    }
                }
            }
        }
        Out::Redraw
    }

    /// Cancel / Esc: stop a transfer (Ctrl+C to drop), or close.
    fn cancel(&mut self) -> Out {
        if let Phase::Sending { job, .. } = &self.phase {
            job.interrupt();
            self.phase = Phase::Failed(String::from("cancelled"));
            return Out::Redraw;
        }
        Out::Close
    }

    pub fn key(&mut self, k: &KeyInput) -> Out {
        match k.key {
            Key::Escape => self.cancel(),
            Key::Enter if matches!(self.phase, Phase::Pick) && self.can_send() => {
                self.send();
                Out::Redraw
            }
            Key::Down | Key::Up if matches!(self.phase, Phase::Pick) && !self.peers.is_empty() => {
                let n = self.peers.len();
                let i = self
                    .sel
                    .as_deref()
                    .and_then(|s| self.peers.iter().position(|p| p.name == s));
                let j = match (k.key, i) {
                    (Key::Down, Some(i)) => (i + 1).min(n - 1),
                    (Key::Up, Some(i)) => i.saturating_sub(1),
                    _ => 0,
                };
                self.sel = Some(self.peers[j].name.clone());
                Out::Redraw
            }
            _ => Out::Nothing,
        }
    }
}

// ---- receiving: the Milk Drop window ------------------------------------------------------

enum RState {
    Starting,
    /// Receiving as (name, fingerprint).
    Waiting(String, String),
    /// An offer: who, file, size text, new device?, fingerprint.
    Offer {
        who: String,
        file: String,
        size: String,
        new: bool,
        fp: String,
    },
    Accepting(String),
    /// The last file: (saved path, from).
    Received(String, String),
    Error(String),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RHit {
    Accept,
    Decline,
    Show,
    Open,
    Stop,
}

/// The incoming panel: runs `drop recv -k` while open.
pub struct Receiver {
    job: Option<Job>,
    lines: Vec<String>,
    me: (String, String),
    state: RState,
    hits: Vec<(Rect, RHit)>,
    hover: Option<(i32, i32)>,
    close: bool,
}

/// `drop: alice (NEW peer, fingerprint 3fa2 ...; accepting ...) wants to send you bike.png
/// (1024 KiB), encrypted.`
fn parse_offer(l: &str) -> Option<RState> {
    let rest = l.trim().strip_prefix("drop: ")?;
    let (who_part, file_part) = rest.split_once(" wants to send you ")?;
    let who = who_part.split(" (").next()?.trim();
    let fp = who_part
        .split_once("fingerprint ")
        .map(|(_, f)| fp_short(f.split([';', ')', ',']).next().unwrap_or("")))
        .unwrap_or_default();
    let (file, size) = match file_part.rsplit_once(" (") {
        Some((f, s)) => (f, s.split(')').next().unwrap_or("")),
        None => (file_part, ""),
    };
    let size = size.replace(" KiB", " KB");
    Some(RState::Offer {
        who: String::from(who),
        file: String::from(file),
        size,
        new: who_part.contains("NEW peer"),
        fp,
    })
}

impl Receiver {
    pub fn new() -> Receiver {
        let job = Job::start(DROP, &["drop", "recv", "-k"], ENV);
        let state = if job.is_some() {
            RState::Starting
        } else {
            RState::Error(String::from("no free terminal to run drop on"))
        };
        Receiver {
            job,
            lines: Vec::new(),
            me: (String::new(), String::new()),
            state,
            hits: Vec::new(),
            hover: None,
            close: false,
        }
    }

    fn answer(&mut self, yes: bool) {
        if let Some(j) = &self.job {
            j.write(if yes { "y\n" } else { "n\n" });
        }
        self.state = match &self.state {
            RState::Offer { file, .. } if yes => RState::Accepting(file.clone()),
            _ => RState::Waiting(self.me.0.clone(), self.me.1.clone()),
        };
    }

    fn hov(&self, r: &Rect) -> State {
        State::hover(self.hover.is_some_and(|(x, y)| hit(r, x, y)))
    }
}

impl WindowContents for Receiver {
    fn title(&self) -> &str {
        "Milk Drop"
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.hits.clear();
        let (icon_bg, line1, line2): (Color, String, String) = match &self.state {
            RState::Starting => (BLUE, String::from("Starting Milk Drop..."), String::new()),
            RState::Waiting(name, fp) => (
                BLUE,
                format!("Ready to receive as {name}"),
                format!("Other milkOS machines on the link can send you files. fp {fp}"),
            ),
            RState::Offer {
                who, file, size, ..
            } => (
                HONEY,
                format!("{who} wants to send you {file}"),
                format!("{size}. This panel is milkOS's, not an app's."),
            ),
            RState::Accepting(file) => (
                HONEY,
                format!("Receiving {file}..."),
                String::from("Checking SHA-256 as it arrives."),
            ),
            RState::Received(path, from) => (
                MATCHA,
                format!("Received {} from {from}", fsx::basename(path)),
                String::from("In ~/Downloads, checked with SHA-256."),
            ),
            RState::Error(e) => (SAND, String::from("Milk Drop can't receive"), e.clone()),
        };
        device_tile(
            p,
            16,
            14,
            52,
            icon_bg,
            matches!(self.state, RState::Error(_)),
        );
        p.text_p(84, 18, &line1, ESPRESSO, true);
        let l2 = Font::body(false).truncate(&line2, w - 100);
        p.text_p(84, 40, &l2, COCOA, false);
        p.fill(0, 80, w, 1, SAND);
        let fy = h - 56;
        p.fill(0, fy, w, 1, SAND);
        let by = fy + 14;
        if let RState::Offer { who, new, fp, .. } = &self.state {
            if *new {
                let r = Rect::new(16, 98, w - 32, 128);
                p.rrect_r(r, 12, HONEY, a(270));
                p.rrect_stroke(r.x, r.y, r.w, r.h, 12, 1, HONEY, 255);
                let e = p.text_p(r.x + 16, r.y + 12, "New device", ESPRESSO, true);
                p.text_p(e + 16, r.y + 12, "never seen before", ESPRESSO, false);
                p.text_p(
                    r.x + 16,
                    r.y + 34,
                    &format!("Check this matches what {who} sees on their screen:"),
                    ESPRESSO,
                    false,
                );
                // The fingerprint, big enough to read out loud.
                let groups: Vec<&str> = fp.split(' ').collect();
                let f = Font::ui(2, Weight::Bold);
                let gap = 24;
                let total: i32 = groups.iter().map(|g| f.width(g) + gap).sum::<i32>() - gap;
                let mut x = r.x + (r.w - total) / 2;
                for g in groups {
                    x = p.text_f(x, r.y + 56, g, ESPRESSO, f) + gap;
                }
                p.text_p(
                    r.x + 16,
                    r.y + 98,
                    "Accepting trusts this device from now on.",
                    ESPRESSO,
                    false,
                );
            } else {
                p.text_p(
                    16,
                    104,
                    &format!("{who} is a device you trust (fp {fp})."),
                    ESPRESSO,
                    false,
                );
            }
            p.text_p(16, by + 6, "Saved to Downloads. Encrypted.", COCOA, false);
            let label = if *new {
                format!("Trust {who} and accept")
            } else {
                String::from("Accept")
            };
            let aw = button::dialog_w(&label);
            let acc = Rect::new(w - 16 - aw, by, aw, button::DIALOG_H);
            let dec = Rect::new(acc.x - 8 - 96, by, 96, button::DIALOG_H);
            button::paint(p, dec, "Decline", Kind::Secondary, self.hov(&dec));
            button::paint(p, acc, &label, Kind::Primary, self.hov(&acc));
            self.hits.push((dec, RHit::Decline));
            self.hits.push((acc, RHit::Accept));
            return;
        }
        match &self.state {
            RState::Received(..) => {
                let open = Rect::new(w - 16 - 88, by, 88, button::DIALOG_H);
                let show = Rect::new(open.x - 8 - 120, by, 120, button::DIALOG_H);
                button::paint(p, show, "Show in Files", Kind::Secondary, self.hov(&show));
                button::paint(p, open, "Open", Kind::Primary, self.hov(&open));
                self.hits.push((show, RHit::Show));
                self.hits.push((open, RHit::Open));
                p.text_p(
                    16,
                    104,
                    "Still receiving: anyone on the link can offer another file.",
                    COCOA,
                    false,
                );
            }
            RState::Waiting(..) | RState::Starting | RState::Accepting(_) => {
                p.text_p(
                    16,
                    104,
                    "You are asked before anything is saved.",
                    COCOA,
                    false,
                );
                p.text_p(16, 122, "Files go to ~/Downloads.", COCOA, false);
            }
            _ => {}
        }
        let sw = button::dialog_w("Stop receiving");
        let stop = Rect::new(16, by, sw, button::DIALOG_H);
        button::paint(p, stop, "Stop receiving", Kind::Secondary, self.hov(&stop));
        self.hits.push((stop, RHit::Stop));
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let PointerKind::Down(_) = e.kind else {
            return false;
        };
        let Some(h) = self
            .hits
            .iter()
            .find(|(r, _)| hit(r, e.x, e.y))
            .map(|(_, h)| *h)
        else {
            return false;
        };
        match h {
            RHit::Accept => self.answer(true),
            RHit::Decline => self.answer(false),
            RHit::Stop => self.close = true,
            RHit::Show | RHit::Open => {
                if let RState::Received(path, _) = &self.state {
                    if h == RHit::Show {
                        super::files::show_path(&fsx::parent(path));
                        request(Request::OpenHere(App::Files));
                    } else {
                        request(Request::OpenAny(path.clone()));
                    }
                }
            }
        }
        true
    }

    fn hover(&mut self, x: i32, y: i32) -> bool {
        let at = |h: &Option<(i32, i32)>, hits: &[(Rect, RHit)]| {
            h.and_then(|(x, y)| hits.iter().position(|(r, _)| hit(r, x, y)))
        };
        let before = at(&self.hover, &self.hits);
        self.hover = (x >= 0).then_some((x, y));
        before != at(&self.hover, &self.hits)
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        match (k.key, &self.state) {
            (Key::Enter, RState::Offer { .. }) => self.answer(true),
            (Key::Escape, RState::Offer { .. }) => self.answer(false),
            _ => return false,
        }
        true
    }

    fn wants_close(&self) -> bool {
        self.close
    }

    fn tick(&mut self, _now: u64) -> bool {
        let Some(job) = &mut self.job else {
            return false;
        };
        let mut changed = false;
        for l in job.output() {
            let t = l.trim();
            if let Some(rest) = t.strip_prefix("drop: receiving as ") {
                let name = String::from(rest.split(" (").next().unwrap_or(""));
                let fp = rest
                    .split_once("fingerprint ")
                    .map(|(_, f)| fp_short(f.split(')').next().unwrap_or("")))
                    .unwrap_or_default();
                self.me = (name.clone(), fp.clone());
                self.state = RState::Waiting(name, fp);
            } else if let Some(offer) = parse_offer(t) {
                self.state = offer;
            } else if let Some(rest) = t.strip_prefix("drop: received ") {
                let path = String::from(rest.split(" (").next().unwrap_or(""));
                let from = rest
                    .split_once(" from ")
                    .map(|(_, f)| String::from(f.split(',').next().unwrap_or("")))
                    .unwrap_or_default();
                // The Received toast (SPEC): Show (in Files) and Open.
                super::super::post(super::super::Post {
                    app: String::from("Milk Drop"),
                    title: format!("Received {}", fsx::basename(&path)),
                    sub: format!("from {from}, in Downloads"),
                    sound: true,
                    actions: alloc::vec![
                        super::super::notif::NoticeAction {
                            label: String::from("Show"),
                            cmd: Some(format!("open {}", fsx::parent(&path))),
                        },
                        super::super::notif::NoticeAction {
                            label: String::from("Open"),
                            cmd: Some(format!("open {path}")),
                        },
                    ],
                    owner: 0,
                    tag: 0,
                });
                self.state = RState::Received(path, from);
            } else if t == "drop: declined" {
                self.state = RState::Waiting(self.me.0.clone(), self.me.1.clone());
            } else if matches!(self.state, RState::Accepting(_))
                && t.starts_with("drop: ")
                && !t.starts_with("drop: [")
                && !t.contains("accept")
            {
                // An error during the transfer (-k keeps going).
                self.state = RState::Error(String::from(&t[6..]));
            }
            self.lines.push(l);
            changed = true;
        }
        if let Some(code) = job.exit() {
            let _ = job.output();
            if code != 0 && !matches!(self.state, RState::Error(_)) {
                self.state = RState::Error(last_reason(&self.lines));
            }
            self.job = None;
            changed = true;
        }
        changed
    }
}
