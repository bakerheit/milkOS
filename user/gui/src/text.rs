//! The system font in user space: the desktop's own families (the kernel's baked glyphs, read
//! from `/System/Fonts/<key>.font`), proportional and anti-aliased, in the family Settings >
//! Appearance > Font names. [`crate::Window::text`] and friends draw with it; Unifont 8x16 is
//! only for monospace content ([`crate::Window::mono`]).
//!
//! # API (stable: appkit and the Browser build on it)
//!
//! - [`Font`]`{ family, scale, weight, oblique }`: [`Font::ui`]`(scale, weight)` is the current
//!   family; scale 1 is 13 px in a 16 px line, 2 is 26 px (headings), 4 is 52 px (big numbers);
//!   [`Font::for_px`] picks the scale nearest a CSS pixel size. Measuring: [`Font::width`],
//!   [`Font::advance`], [`Font::fit`], [`Font::truncate`], [`Font::tail`], [`Font::index_at`],
//!   [`Font::prefix_width`], [`Font::line_height`], [`Font::baseline`].
//! - Drawing: [`Font::draw`] into any 0x00RRGGBB pixel buffer with a clip rectangle (a window
//!   or an offscreen surface), `y` the top of the line box; returns the end x.
//! - [`preload`]: read the font files now. A sandboxed app calls it before `sandbox()` or
//!   grants `fs=/System/Fonts:r` (other families are read when a character needs them).
//! - [`current_family`] follows the desktop's choice (`font <key>` in `/dev/wm`, re-read at most
//!   once a second), so a Settings change shows on the app's next redraw.
//! - [`available`]: false if the font files can't be read; text then falls back to Unifont
//!   everywhere, measured and drawn alike, so layouts stay consistent.
//!
//! Characters: see `fontkit` (Latin-1 in every family, Latin Extended, Greek, Cyrillic and the
//! rest from whichever family has them, a fold to similar characters, then a box).

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use fontkit::{FAMILY_COUNT, Family, KEYS, Source};
pub use fontkit::{Glyph, NAMES, Weight};
use rt::sys;

use crate::{Color, blend};

const N: usize = KEYS.len();
static FAMS: [AtomicPtr<Family<'static>>; N] = [const { AtomicPtr::new(core::ptr::null_mut()) }; N];
static TRIED: [AtomicBool; N] = [const { AtomicBool::new(false) }; N];
/// The desktop's family (usize::MAX: not read yet), and when it was last read (ms).
static CURRENT: AtomicUsize = AtomicUsize::new(usize::MAX);
static CHECKED: AtomicU64 = AtomicU64::new(0);
/// The font files can't be used: everything is Unifont.
static MONO_ONLY: AtomicBool = AtomicBool::new(false);

/// Read and check `/System/Fonts/<key>.font`; the blob lives as long as the program.
fn load(i: usize) -> Option<&'static Family<'static>> {
    let p = FAMS[i].load(Ordering::Acquire);
    if !p.is_null() {
        // SAFETY: only ever set to a leaked, never-freed Box below.
        return Some(unsafe { &*p });
    }
    if TRIED[i].swap(true, Ordering::AcqRel) {
        // Tried and failed (or another thread is loading it right now: it will be there next time).
        let p = FAMS[i].load(Ordering::Acquire);
        // SAFETY: as above.
        return (!p.is_null()).then(|| unsafe { &*p });
    }
    let path = alloc::format!("/System/Fonts/{}.font", KEYS[i]);
    let bytes: Vec<u8> = sys::read_file(&path).ok()?;
    if bytes.len() > 16 << 20 {
        return None;
    }
    let blob: &'static [u8] = Vec::leak(bytes);
    let fam = Family::parse(blob).ok()?;
    if fam.key() != KEYS[i] || fam.validate().is_err() {
        return None;
    }
    let p = alloc::boxed::Box::into_raw(alloc::boxed::Box::new(fam));
    FAMS[i].store(p, Ordering::Release);
    // SAFETY: just leaked.
    Some(unsafe { &*p })
}

struct Loaded;

impl Source<'static> for Loaded {
    fn family(&self, i: usize) -> Option<&'static Family<'static>> {
        if i < N { load(i) } else { None }
    }
}

fn now_ms() -> u64 {
    sys::clock_ns(sys::CLOCK_MONOTONIC) / 1_000_000
}

/// The desktop's family now (`/dev/wm`'s `font` line, else the settings file, else Figtree).
pub fn current_family() -> usize {
    let cur = CURRENT.load(Ordering::Relaxed);
    let now = now_ms();
    if cur != usize::MAX && now.saturating_sub(CHECKED.load(Ordering::Relaxed)) < 1000 {
        return cur;
    }
    CHECKED.store(now, Ordering::Relaxed);
    let read = |p: &str| {
        sys::read_file(p)
            .ok()
            .map(|b| alloc::string::String::from_utf8_lossy(&b).into_owned())
    };
    let key = read("/dev/wm")
        .and_then(|s| {
            s.lines().find_map(|l| {
                l.strip_prefix("font ")
                    .map(|k| alloc::string::String::from(k.trim()))
            })
        })
        .or_else(|| {
            read("/Users/milk/.config/milk/settings").and_then(|s| {
                s.lines().find_map(|l| {
                    let (k, v) = l.split_once('=')?;
                    (k.trim() == "font").then(|| alloc::string::String::from(v.trim()))
                })
            })
        });
    let fam = key
        .and_then(|k| KEYS[..FAMILY_COUNT].iter().position(|x| *x == k))
        .unwrap_or(if cur == usize::MAX {
            fontkit::DEFAULT
        } else {
            cur
        });
    CURRENT.store(fam, Ordering::Relaxed);
    fam
}

/// Use this family from now on (an app that knows better, or a test).
pub fn set_current_family(fam: usize) {
    CURRENT.store(fam.min(FAMILY_COUNT - 1), Ordering::Relaxed);
    CHECKED.store(u64::MAX / 2, Ordering::Relaxed);
}

/// Read the current family's file (and the fallbacks') now, before a sandbox closes the door.
/// True if the system font is usable.
pub fn preload() -> bool {
    let fam = current_family();
    let ok = load(fam).is_some();
    for &i in fontkit::FALLBACK.iter() {
        load(i);
    }
    ok
}

/// The system font can be drawn (else text is Unifont).
pub fn available() -> bool {
    !MONO_ONLY.load(Ordering::Relaxed) && load(current_family()).is_some()
}

/// Draw everything in Unifont (an app that wants the old look, or a test).
pub fn set_mono_only(on: bool) {
    MONO_ONLY.store(on, Ordering::Relaxed);
}

/// A text style. `family` is an index into [`NAMES`]; [`Font::ui`] takes the desktop's.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Font {
    pub family: usize,
    pub scale: i32,
    pub weight: Weight,
    /// Slanted (the families have no italics: drawn sheared, about 12 degrees).
    pub oblique: bool,
}

impl Font {
    /// The desktop's family at `scale` (1: 13 px, 2: 26 px, 4: 52 px).
    pub fn ui(scale: i32, weight: Weight) -> Font {
        Font::of(current_family(), scale, weight)
    }

    /// Body text, 13 px, regular or bold.
    pub fn body(bold: bool) -> Font {
        Font::ui(1, Weight::bold(bold))
    }

    pub fn of(family: usize, scale: i32, weight: Weight) -> Font {
        Font {
            family: family.min(FAMILY_COUNT - 1),
            scale: scale.max(1),
            weight,
            oblique: false,
        }
    }

    /// The desktop's family at the scale nearest an em of `px` pixels (13 per scale).
    pub fn for_px(px: i32, weight: Weight) -> Font {
        Font::ui(((px + 6) / 13).max(1), weight)
    }

    pub fn italic(self) -> Font {
        Font {
            oblique: true,
            ..self
        }
    }

    fn kit(&self) -> fontkit::Font {
        fontkit::Font::new(self.family, self.scale, self.weight)
    }

    fn mono(&self) -> bool {
        MONO_ONLY.load(Ordering::Relaxed) || load(self.family).is_none()
    }

    pub fn line_height(&self) -> i32 {
        16 * self.scale
    }

    pub fn baseline(&self) -> i32 {
        13 * self.scale
    }

    pub fn cap_height(&self) -> i32 {
        (13 * self.scale * 7 + 5) / 10
    }

    pub fn advance(&self, c: char) -> i32 {
        if self.mono() {
            return 8 * self.scale;
        }
        self.kit().advance(&Loaded, c)
    }

    pub fn width(&self, s: &str) -> i32 {
        if self.mono() {
            return s.chars().count() as i32 * 8 * self.scale;
        }
        self.kit().width(&Loaded, s)
    }

    /// How many bytes of `s` fit in `max` pixels.
    pub fn fit(&self, s: &str, max: i32) -> usize {
        let mut w = 0;
        for (i, c) in s.char_indices() {
            w += self.advance(c);
            if w > max {
                return i;
            }
        }
        s.len()
    }

    /// `s` cut to `max` pixels, ending in "..." when it doesn't fit.
    pub fn truncate(&self, s: &str, max: i32) -> alloc::string::String {
        if self.width(s) <= max {
            return alloc::string::String::from(s);
        }
        let n = self.fit(s, (max - self.width("...")).max(0));
        let mut out = alloc::string::String::from(s[..n].trim_end());
        out.push_str("...");
        out
    }

    /// The longest end of `s` that fits in `max` pixels.
    pub fn tail<'s>(&self, s: &'s str, max: i32) -> &'s str {
        let mut w = 0;
        for (i, c) in s.char_indices().rev() {
            w += self.advance(c);
            if w > max {
                return &s[i + c.len_utf8()..];
            }
        }
        s
    }

    /// The char index in `s` nearest pixel offset `x` (caret placement).
    pub fn index_at(&self, s: &str, x: i32) -> usize {
        let mut w = 0;
        for (i, c) in s.chars().enumerate() {
            let a = self.advance(c);
            if x < w + a / 2 {
                return i;
            }
            w += a;
        }
        s.chars().count()
    }

    /// Width of the first `n` chars of `s`.
    pub fn prefix_width(&self, s: &str, n: usize) -> i32 {
        s.chars().take(n).map(|c| self.advance(c)).sum()
    }

    /// Draw `s` into `px` (rows of `stride` pixels, 0x00RRGGBB) with the top of the line box at
    /// `y`, only inside `clip` (x, y, w, h). Returns the end x.
    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        px: &mut [u32],
        stride: usize,
        clip: (i32, i32, i32, i32),
        x: i32,
        y: i32,
        s: &str,
        c: Color,
    ) -> i32 {
        let rows = (px.len() / stride.max(1)) as i32;
        let (cx0, cy0) = (clip.0.max(0), clip.1.max(0));
        let (cx1, cy1) = (
            (clip.0 + clip.2).min(stride as i32),
            (clip.1 + clip.3).min(rows),
        );
        if self.mono() {
            return crate::mono_into(px, stride, (cx0, cy0, cx1, cy1), x, y, s, c, self.scale);
        }
        let base = y + self.baseline();
        let lean = self.oblique;
        self.kit().layout(&Loaded, x, s, |pen, g, mag| {
            let gx = pen + g.left * mag;
            let gy = base - g.top * mag;
            if gy >= cy1 || gy + g.h * mag <= cy0 {
                return;
            }
            g.spans(mag, |sx, sy, len, a| {
                let by = gy + sy;
                if by < cy0 || by >= cy1 {
                    return;
                }
                // Oblique: shift each row right by its height above the baseline / 5.
                let shift = if lean { (base - by) / 5 } else { 0 };
                let x0 = (gx + sx + shift).max(cx0);
                let x1 = (gx + sx + len + shift).min(cx1);
                if x0 >= x1 {
                    return;
                }
                let row = by as usize * stride;
                for p in &mut px[row + x0 as usize..row + x1 as usize] {
                    *p = if a == 255 { c } else { blend(*p, c, a) };
                }
            });
        })
    }
}
