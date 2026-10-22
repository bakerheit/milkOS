//! Desktop preferences: theme, wallpaper, font, brightness, dock size. Kept in
//! `~/.config/milk/settings` as plain `key = value` lines, so they survive reboots once home
//! lives on milkfs. The Settings app edits them through `Request::Prefs`; the desktop applies
//! and saves.

use alloc::format;
use alloc::string::String;

use crate::fs;
use crate::sync::SpinLock;

use super::wallpaper::Kind;

pub const PATH: &str = "/Users/milk/.config/milk/settings";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Prefs {
    /// "Chocolate milk" (dark) instead of "Whole milk".
    pub dark: bool,
    /// None = follow the theme (dawn when light, night when dark).
    pub wallpaper: Option<Kind>,
    /// 20..=100.
    pub brightness: i32,
    /// Dock icon size: 40, 48 or 56.
    pub dock: i32,
    /// System sounds (toasts, crashes). Only audible with a sound device.
    pub sound: bool,
    /// Lock the screen after this many minutes without input; 0 = never.
    pub lock_mins: u32,
    /// Display mode (one of `screen::MODES`).
    pub resolution: (i32, i32),
    /// Show the Welcome window at startup (until "Don't show again").
    pub welcome: bool,
    /// Dock magnification (macOS-style) and how big the icon under the pointer gets, in
    /// percent of the dock size (125..=200).
    pub magnify: bool,
    pub magnify_pct: i32,
    /// The UI font: an index into `sysfont::FAMILIES`, saved by its key (`font = figtree`).
    pub font: usize,
}

impl Prefs {
    pub const DEFAULT: Prefs = Prefs {
        dark: false,
        wallpaper: None,
        brightness: 100,
        dock: 48,
        sound: true,
        lock_mins: 0,
        resolution: (1024, 768),
        welcome: true,
        magnify: true,
        magnify_pct: 175,
        font: crate::sysfont::DEFAULT,
    };

    /// The idle times Settings offers (minutes; 0 = never).
    pub const LOCK_CHOICES: [u32; 5] = [0, 1, 5, 15, 30];

    /// The wallpaper actually shown.
    pub fn wallpaper_kind(&self) -> Kind {
        self.wallpaper
            .unwrap_or(if self.dark { Kind::Night } else { Kind::Dawn })
    }

    fn to_text(self) -> String {
        format!(
            "# milkOS desktop settings (written by Settings)\ntheme = {}\nwallpaper = {}\nbrightness = {}\ndock = {}\nsound = {}\nlock = {}\nresolution = {}x{}\nwelcome = {}\nmagnify = {}\nmagnify_size = {}\nfont = {}\n",
            if self.dark { "chocolate" } else { "whole" },
            self.wallpaper.map_or("auto", |k| k.key()),
            self.brightness,
            self.dock,
            if self.sound { "on" } else { "off" },
            match self.lock_mins {
                0 => String::from("never"),
                m => format!("{m}"),
            },
            self.resolution.0,
            self.resolution.1,
            if self.welcome { "on" } else { "off" },
            if self.magnify { "on" } else { "off" },
            self.magnify_pct,
            crate::sysfont::FAMILIES[self.font.min(crate::sysfont::FAMILIES.len() - 1)].key
        )
    }

    fn parse(text: &str) -> Prefs {
        let mut p = Prefs::DEFAULT;
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "theme" => p.dark = matches!(v, "chocolate" | "dark"),
                "wallpaper" => p.wallpaper = Kind::ALL.iter().copied().find(|kind| kind.key() == v),
                "sound" => p.sound = v != "off",
                "welcome" => p.welcome = v != "off",
                "magnify" => p.magnify = v != "off",
                "font" => p.font = crate::sysfont::find(v).unwrap_or(crate::sysfont::DEFAULT),
                "magnify_size" => p.magnify_pct = v.parse().unwrap_or(175).clamp(125, 200),
                "lock" => p.lock_mins = v.parse().unwrap_or(0).min(24 * 60),
                "resolution" => {
                    if let Some((w, h)) = v.split_once('x')
                        && let (Ok(w), Ok(h)) = (w.trim().parse(), h.trim().parse())
                        && super::screen::MODES.contains(&(w, h))
                    {
                        p.resolution = (w, h);
                    }
                }
                "brightness" => p.brightness = v.parse().unwrap_or(100).clamp(20, 100),
                "dock" => {
                    p.dock = match v.parse().unwrap_or(48) {
                        n if n < 44 => 40,
                        n if n > 52 => 56,
                        _ => 48,
                    }
                }
                _ => {}
            }
        }
        p
    }
}

static PREFS: SpinLock<Prefs> = SpinLock::new(Prefs::DEFAULT);

pub fn get() -> Prefs {
    *PREFS.lock()
}

pub fn set(p: Prefs) {
    *PREFS.lock() = p;
}

/// Read the settings file (defaults if there is none).
pub fn load() -> Prefs {
    let p = fs::read_to_string(PATH).map_or(Prefs::DEFAULT, |t| Prefs::parse(&t));
    set(p);
    p
}

pub fn save(p: &Prefs) {
    let _ = fs::mkdir_all("/Users/milk/.config/milk", 0o755);
    if let Err(e) = fs::write_file(PATH, p.to_text().as_bytes()) {
        crate::kprintln!("wm: saving {PATH} failed: {e}");
        return;
    }
    // Make it durable now (home may be on milkfs): settings are rare, small writes.
    let _ = fs::sync_all();
}
