//! Desktop preferences: theme, wallpaper, brightness, dock size. Kept in
//! `~/.config/milk/settings` as plain `key = value` lines, so they survive reboots once home
//! lives on milkfs. The Settings app edits them through `Request::Prefs`; the desktop applies
//! and saves.

use alloc::format;
use alloc::string::String;

use crate::fs;
use crate::sync::SpinLock;

use super::wallpaper::Kind;

pub const PATH: &str = "/home/milk/.config/milk/settings";

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
}

impl Prefs {
    pub const DEFAULT: Prefs = Prefs {
        dark: false,
        wallpaper: None,
        brightness: 100,
        dock: 48,
        sound: true,
        lock_mins: 0,
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
            "# milkOS desktop settings (written by Settings)\ntheme = {}\nwallpaper = {}\nbrightness = {}\ndock = {}\nsound = {}\nlock = {}\n",
            if self.dark { "chocolate" } else { "whole" },
            self.wallpaper.map_or("auto", |k| k.key()),
            self.brightness,
            self.dock,
            if self.sound { "on" } else { "off" },
            match self.lock_mins {
                0 => String::from("never"),
                m => format!("{m}"),
            }
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
                "lock" => p.lock_mins = v.parse().unwrap_or(0).min(24 * 60),
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
    let _ = fs::mkdir_all("/home/milk/.config/milk", 0o755);
    if let Err(e) = fs::write_file(PATH, p.to_text().as_bytes()) {
        crate::kprintln!("wm: saving {PATH} failed: {e}");
        return;
    }
    // Make it durable now (home may be on milkfs): settings are rare, small writes.
    let _ = fs::sync_all();
}
