//! Settings: Appearance (Whole milk / Chocolate milk), Wallpaper, Display (brightness), Dock
//! (icon size), Time Machine (milkfs automatic snapshots, via /sys/fs/milkfs/auto) and About.
//! Changes go to the desktop as `Request::Prefs` (applied at once, saved to
//! ~/.config/milk/settings); Time Machine settings live on the milkfs volume itself.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{Color, Rect, rgb};
use crate::{fs, heap, pmm, timer};

use super::super::buffer::Buffer;
use super::super::paint::{self, Painter, hit};
use super::super::prefs::{self, Prefs};
use super::super::theme::*;
use super::super::wallpaper::{self, Kind};
use super::super::window::{PointerInput, PointerKind, WindowContents};
use super::super::{Request, icons, request};
use super::fsx;

pub const SECTIONS: [&str; 7] = [
    "Appearance",
    "Wallpaper",
    "Display",
    "Dock",
    "Sound",
    "Time Machine",
    "About",
];
const TM: usize = 5;

/// A section someone asked for (Skim's settings results); the Settings window picks it up.
static PENDING: crate::sync::SpinLock<Option<usize>> = crate::sync::SpinLock::new(None);

pub fn show_section(i: usize) {
    *PENDING.lock() = Some(i.min(SECTIONS.len() - 1));
}
const AUTO: &str = "/sys/fs/milkfs/auto";
const TM_SNAPSHOTS: &str = "/sys/fs/milkfs/snapshots";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hit {
    Section(usize),
    Theme(bool),
    Wallpaper(Option<Kind>),
    Brightness,
    Dock(i32),
    TmToggle,
    TmNow,
    TmInterval(&'static str),
    Sound(bool),
    TestSound,
}

pub struct Settings {
    section: usize,
    hits: Vec<(Rect, Hit)>,
    /// Wallpaper previews (built the first time the section is shown).
    thumbs: Vec<(Kind, Buffer)>,
    /// What we last sent (the desktop may not have applied it yet).
    pending: Prefs,
    /// Brightness slider being dragged.
    dragging: bool,
    track: Rect,
    tm_status: String,
    tm_count: usize,
    last_sec: u64,
}

impl Settings {
    pub fn new() -> Settings {
        let mut s = Settings {
            section: PENDING.lock().take().unwrap_or(0),
            hits: Vec::new(),
            thumbs: Vec::new(),
            pending: prefs::get(),
            dragging: false,
            track: Rect::new(0, 0, 0, 0),
            tm_status: String::new(),
            tm_count: 0,
            last_sec: 0,
        };
        s.read_tm();
        s
    }

    fn send(&mut self, p: Prefs) {
        self.pending = p;
        request(Request::Prefs(p));
    }

    fn read_tm(&mut self) {
        self.tm_status = fs::read_to_string(AUTO)
            .map(|s| String::from(s.trim()))
            .unwrap_or_default();
        self.tm_count = fs::read_to_string(TM_SNAPSHOTS)
            .map(|s| s.lines().filter(|l| l.contains("gen=")).count())
            .unwrap_or(0);
    }

    fn build_thumbs(&mut self) {
        if !self.thumbs.is_empty() {
            return;
        }
        let Some(mut full) = Buffer::new(SCREEN_W as usize, SCREEN_H as usize, 0) else {
            return;
        };
        for k in Kind::ALL {
            wallpaper::paint(&mut full, k);
            if let Some(mut t) = Buffer::new(152, 114, 0) {
                paint::downscale(&full, &mut t);
                self.thumbs.push((k, t));
            }
        }
    }

    fn button(&mut self, p: &mut Painter, r: Rect, label: &str, on: bool, h: Hit) {
        let (bg, fg) = if on { (STRAW, MILK) } else { (OAT, ESPRESSO) };
        p.rrect_r(r, R_BUTTON, bg, 255);
        p.text_p_center(r.x, r.w, r.y + (r.h - 16) / 2, label, fg, on);
        self.hits.push((r, h));
    }

    fn heading(p: &mut Painter, x: i32, title: &str, sub: &str) {
        p.text_x(x, 18, title, ESPRESSO, 2, true);
        p.text_p(x, 56, sub, COCOA, false);
    }

    fn draw_appearance(&mut self, p: &mut Painter, x: i32) {
        Self::heading(
            p,
            x,
            "Appearance",
            "The whole desktop follows it: windows, glass, Terminal.",
        );
        let dark_now = self.pending.dark;
        for (i, (name, dark)) in [("Whole milk", false), ("Chocolate milk", true)]
            .into_iter()
            .enumerate()
        {
            let r = Rect::new(x + i as i32 * 212, 92, 196, 136);
            preview(p, r, dark);
            if dark == dark_now {
                p.rrect_stroke(r.x - 4, r.y - 4, r.w + 8, r.h + 8, 16, 3, HONEY, 255);
            }
            p.text_p_center(r.x, r.w, r.y + r.h + 12, name, ESPRESSO, dark == dark_now);
            self.hits.push((grow_h(r, 34), Hit::Theme(dark)));
        }
    }

    fn draw_wallpaper(&mut self, p: &mut Painter, x: i32) {
        Self::heading(
            p,
            x,
            "Wallpaper",
            "Auto follows the theme: dawn by day, night with chocolate.",
        );
        self.build_thumbs();
        let cur = self.pending.wallpaper;
        let mut items: Vec<(Option<Kind>, &str)> = alloc::vec![(None, "Auto")];
        for k in Kind::ALL {
            items.push((Some(k), k.name()));
        }
        for (i, (choice, name)) in items.into_iter().enumerate() {
            let (col, row) = (i as i32 % 2, i as i32 / 2);
            let r = Rect::new(x + col * 176, 88 + row * 150, 152, 114);
            let shown = choice.unwrap_or(if self.pending.dark {
                Kind::Night
            } else {
                Kind::Dawn
            });
            if let Some((_, t)) = self.thumbs.iter().find(|(k, _)| *k == shown) {
                p.blit_rrect(r.x, r.y, t, 0, 0, r.w, r.h, 10, 255);
            }
            if choice.is_none() {
                p.rrect(r.x + 8, r.y + 8, 48, 22, 8, MILK, a(800));
                p.text_p(r.x + 16, r.y + 11, "auto", COCOA, true);
            }
            if choice == cur {
                p.rrect_stroke(r.x - 4, r.y - 4, r.w + 8, r.h + 8, 14, 3, HONEY, 255);
            }
            p.text_p_center(r.x, r.w, r.y + r.h + 8, name, ESPRESSO, choice == cur);
            self.hits.push((grow_h(r, 28), Hit::Wallpaper(choice)));
        }
    }

    fn draw_display(&mut self, p: &mut Painter, x: i32, w: i32) {
        Self::heading(
            p,
            x,
            "Display",
            "Brightness dims the whole screen (same as Control centre).",
        );
        let r = Rect::new(x, 100, w - x - 32, CC_TILE_H);
        p.rrect_r(r, 16, OAT, 255);
        p.text_p(x + 14, 108, "Brightness", ESPRESSO, true);
        let v = self.pending.brightness;
        p.text_right(r.x + r.w - 14, 108, &format!("{v}%"), COCOA, 1, false);
        self.track = Rect::new(x + 14, 136, r.w - 28, CC_SLIDER_H);
        let t = self.track;
        p.rrect(t.x, t.y, t.w, t.h, 4, COCOA, 0x40);
        p.rrect(t.x, t.y, (t.w * v / 100).max(8), t.h, 4, ESPRESSO, 255);
        p.circle(t.x + t.w * v / 100 - 8, t.y - 4, 16, MILK, 255);
        p.ring(t.x + t.w * v / 100 - 8, t.y - 4, 16, 1, SAND, 255);
        self.hits.push((grow_h(r, 0), Hit::Brightness));
    }

    fn draw_dock(&mut self, p: &mut Painter, x: i32) {
        Self::heading(
            p,
            x,
            "Dock",
            "Icon size. The dock stays centred, 12px above the bottom.",
        );
        for (i, (name, px)) in [("Small", 40), ("Medium", 48), ("Large", 56)]
            .into_iter()
            .enumerate()
        {
            let r = Rect::new(x + i as i32 * 116, 100, 108, 96);
            let on = self.pending.dock == px;
            p.rrect_r(r, 14, if on { OAT } else { CREAM }, 255);
            if on {
                p.rrect_stroke(r.x, r.y, r.w, r.h, 14, 2, HONEY, 255);
            }
            let s = px * 3 / 4;
            icons::draw(
                p,
                r.x + (r.w - s) / 2,
                r.y + 14,
                s,
                icons::Icon::Folder,
                HONEY,
                255,
                INK,
                false,
            );
            p.text_p_center(r.x, r.w, r.y + r.h - 26, name, ESPRESSO, on);
            self.hits.push((r, Hit::Dock(px)));
        }
    }

    fn draw_sound(&mut self, p: &mut Painter, x: i32) {
        Self::heading(
            p,
            x,
            "Sound",
            "System sounds: a soft tone for toasts, a low one when an app crashes.",
        );
        let on = self.pending.sound;
        self.button(p, Rect::new(x, 100, 90, 30), "On", on, Hit::Sound(true));
        self.button(
            p,
            Rect::new(x + 98, 100, 90, 30),
            "Off",
            !on,
            Hit::Sound(false),
        );
        self.button(
            p,
            Rect::new(x + 210, 100, 130, 30),
            "Play a sound",
            false,
            Hit::TestSound,
        );
        let dev = if crate::audio::available() {
            "Sound device: virtio-snd"
        } else {
            "No sound device (boot with MILK_SOUND=1 to hear them)"
        };
        p.text_p(x, 150, dev, COCOA, false);
        p.text_p(
            x,
            172,
            "Focus mode in Control centre also keeps toasts quiet.",
            COCOA,
            false,
        );
    }

    fn draw_tm(&mut self, p: &mut Painter, x: i32) {
        Self::heading(
            p,
            x,
            "Time Machine",
            "Automatic milkfs snapshots of your home folder.",
        );
        if self.tm_status.is_empty() {
            p.text_p(
                x,
                100,
                "No milkfs disk is attached, so there is nothing to back up to.",
                COCOA,
                false,
            );
            return;
        }
        let on = self.tm_status.starts_with("on");
        let status = self.tm_status.clone();
        let first = status.lines().next().unwrap_or("");
        p.text_p(x, 96, "Status", COCOA, false);
        p.text_p(x + 96, 96, first, ESPRESSO, false);
        if let Some(next) = status.lines().nth(1) {
            p.text_p(x + 96, 116, next, COCOA, false);
        }
        p.text_p(x, 140, "Snapshots", COCOA, false);
        p.text_p(
            x + 96,
            140,
            &format!("{} on /Volumes/Milk (see Files)", self.tm_count),
            ESPRESSO,
            false,
        );
        let lbl = if on { "Turn off" } else { "Turn on" };
        self.button(p, Rect::new(x, 176, 110, 30), lbl, on, Hit::TmToggle);
        self.button(
            p,
            Rect::new(x + 122, 176, 130, 30),
            "Back up now",
            false,
            Hit::TmNow,
        );
        p.text_p(x, 228, "Every", COCOA, false);
        for (i, iv) in ["15m", "1h", "6h"].into_iter().enumerate() {
            let sel = first.contains(&format!("interval={iv}"));
            self.button(
                p,
                Rect::new(x + 60 + i as i32 * 64, 222, 56, 28),
                iv,
                sel,
                Hit::TmInterval(iv),
            );
        }
    }

    fn draw_about(&mut self, p: &mut Painter, x: i32) {
        icons::drop_shape(p, x + 40, 60, 26, 56, BLUE);
        icons::drop_shape(p, x + 40, 60, 21, 46, MILK);
        let x2 = p.text_x(x + 90, 30, "milk", ESPRESSO, 2, true);
        p.text_x(x2, 30, "OS", BLUE, 2, true);
        p.text_p(
            x + 90,
            70,
            "Version 1.0, kernel 0.0.1, aarch64 on qemu virt",
            COCOA,
            false,
        );
        let s = pmm::stats();
        let h = heap::stats();
        let up = timer::now_ns() / 1_000_000_000;
        let rows = [
            (
                "Memory",
                format!(
                    "{} of {} MiB free",
                    (s.free_frames * 4096) >> 20,
                    (s.total_frames * 4096) >> 20
                ),
            ),
            (
                "Kernel heap",
                format!("{} KiB used of {} KiB", h.used >> 10, h.total >> 10),
            ),
            (
                "Uptime",
                format!("{}:{:02}:{:02}", up / 3600, up / 60 % 60, up % 60),
            ),
            (
                "Home",
                String::from(if fs::stat("/Volumes/Milk").is_ok() {
                    "on milkfs (/Volumes/Milk)"
                } else {
                    "in RAM (tmpfs)"
                }),
            ),
            ("Settings file", String::from(prefs::PATH)),
        ];
        for (i, (k, v)) in rows.iter().enumerate() {
            let y = 132 + i as i32 * 26;
            p.text_p(x, y, k, COCOA, false);
            p.text_p(x + 120, y, v, ESPRESSO, false);
        }
    }

    fn set_brightness_at(&mut self, x: i32) {
        let t = self.track;
        let v = ((x - t.x) * 100 / t.w.max(1)).clamp(20, 100);
        if v != self.pending.brightness {
            let mut p = self.pending;
            p.brightness = v;
            self.send(p);
        }
    }
}

fn grow_h(r: Rect, extra: i32) -> Rect {
    Rect::new(r.x, r.y, r.w, r.h + extra)
}

/// A tiny desktop in theme `dark`: wallpaper colour, a window, a dock.
fn preview(p: &mut Painter, r: Rect, dark: bool) {
    let mut q = p.unthemed();
    let pick = |c: Color| -> Color {
        if !dark {
            return c;
        }
        match c {
            MILK => rgb(0x2E, 0x25, 0x20),
            CREAM => rgb(0x38, 0x2D, 0x27),
            SAND => rgb(0x55, 0x46, 0x3C),
            ESPRESSO => rgb(0xF4, 0xEB, 0xE0),
            c => c,
        }
    };
    let (sky, hill) = if dark {
        (rgb(0x2C, 0x24, 0x40), rgb(0x35, 0x4B, 0x3C))
    } else {
        (rgb(0xF3, 0xC3, 0xB8), rgb(0xB9, 0xD3, 0xB0))
    };
    q.rrect_r(r, 12, sky, 255);
    q.rrect(r.x, r.y + r.h - 46, r.w, 46, 12, hill, 255);
    q.circle(
        r.x + r.w - 52,
        r.y + 14,
        28,
        if dark { rgb(0xF5, 0xEC, 0xD6) } else { HONEY },
        255,
    );
    // A window.
    let wr = Rect::new(r.x + 20, r.y + 22, 120, 76);
    q.rrect_r(wr, 6, pick(MILK), 255);
    q.rrect(wr.x, wr.y, wr.w, 14, 6, pick(CREAM), 255);
    q.fill(wr.x, wr.y + 10, wr.w, 4, pick(CREAM));
    for (i, c) in [STRAW, HONEY, MATCHA].into_iter().enumerate() {
        q.circle(wr.x + 6 + i as i32 * 9, wr.y + 4, 6, c, 255);
    }
    for k in 0..3 {
        q.fill(
            wr.x + 10,
            wr.y + 24 + k * 12,
            70 - k * 14,
            4,
            pick(ESPRESSO),
        );
    }
    q.rrect_stroke(wr.x, wr.y, wr.w, wr.h, 6, 1, pick(SAND), 255);
    // The dock.
    q.rrect(
        r.x + 48,
        r.y + r.h - 22,
        r.w - 96,
        16,
        6,
        pick(MILK),
        a(800),
    );
}

impl WindowContents for Settings {
    fn title(&self) -> &str {
        "Settings"
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "View", "Window", "Help"]
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        self.hits.clear();
        // Sidebar like Files.
        p.fill(0, 0, SIDEBAR_W, h, OAT);
        p.fill(SIDEBAR_W - 1, 0, 1, h, SAND);
        p.text(16, 20, "SETTINGS", COCOA);
        for (i, s) in SECTIONS.iter().enumerate() {
            let y = 44 + i as i32 * 28;
            let r = Rect::new(8, y, SIDEBAR_W - 16, 24);
            if i == self.section {
                p.rrect_r(r, R_BUTTON, STRAW, SEL_SIDEBAR_A);
            }
            p.text_p(18, y + 4, s, ESPRESSO, i == self.section);
            self.hits.push((r, Hit::Section(i)));
        }
        let x = SIDEBAR_W + 28;
        match self.section {
            0 => self.draw_appearance(p, x),
            1 => self.draw_wallpaper(p, x),
            2 => self.draw_display(p, x, w),
            3 => self.draw_dock(p, x),
            4 => self.draw_sound(p, x),
            TM => self.draw_tm(p, x),
            _ => self.draw_about(p, x),
        }
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        if self.dragging {
            match e.kind {
                PointerKind::Drag => {
                    self.set_brightness_at(e.x);
                    return true;
                }
                PointerKind::Up(_) => {
                    self.dragging = false;
                    return false;
                }
                _ => {}
            }
        }
        let PointerKind::Down(_) = e.kind else {
            return false;
        };
        let Some(&(_, h)) = self.hits.iter().find(|(r, _)| hit(r, e.x, e.y)) else {
            return false;
        };
        let mut pr = self.pending;
        match h {
            Hit::Section(i) => {
                self.section = i;
                if i == TM {
                    self.read_tm();
                }
            }
            Hit::Theme(dark) => {
                pr.dark = dark;
                self.send(pr);
            }
            Hit::Wallpaper(k) => {
                pr.wallpaper = k;
                self.send(pr);
            }
            Hit::Brightness => {
                self.dragging = true;
                self.set_brightness_at(e.x);
            }
            Hit::Dock(px) => {
                pr.dock = px;
                self.send(pr);
            }
            Hit::TmToggle => {
                let on = self.tm_status.starts_with("on");
                let _ = fsx::ctl_write(AUTO, if on { "off" } else { "on" });
                self.read_tm();
            }
            Hit::TmNow => {
                let before = self.tm_count;
                let ok = fsx::ctl_write(AUTO, "now").is_ok();
                self.read_tm();
                let msg = match (ok, self.tm_count > before) {
                    (false, _) => "Couldn't take a snapshot",
                    (true, true) => "Snapshot taken",
                    (true, false) => "Nothing changed since the last one",
                };
                super::super::notify("Time Machine", msg);
            }
            Hit::Sound(on) => {
                pr.sound = on;
                self.send(pr);
            }
            Hit::TestSound => crate::audio::play_system(crate::audio::Sound::Chime),
            Hit::TmInterval(iv) => {
                let _ = fsx::ctl_write(AUTO, &format!("interval={iv}"));
                self.read_tm();
            }
        }
        true
    }

    fn tick(&mut self, now: u64) -> bool {
        let asked = PENDING.lock().take();
        if let Some(i) = asked {
            self.section = i;
            if i == TM {
                self.read_tm();
            }
            return true;
        }
        // Live sections (Time Machine countdown, About uptime) once a second.
        let s = now / 1_000_000_000;
        if s != self.last_sec && (self.section == TM || self.section == TM + 1) {
            self.last_sec = s;
            if self.section == TM {
                self.read_tm();
            }
            return true;
        }
        // Follow changes made elsewhere (Control centre brightness).
        let cur = prefs::get();
        if cur != self.pending && !self.dragging {
            self.pending = cur;
            return true;
        }
        false
    }
}
