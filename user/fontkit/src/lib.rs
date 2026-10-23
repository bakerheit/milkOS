//! The system fonts' glyph blobs and text measuring, shared by the kernel (the desktop's UI
//! text, `kernel/src/sysfont.rs`) and user space (`appkit::text`, which loads the same blobs
//! from `/System/Fonts/<key>.font`). Pure: no syscalls, no allocation except [`Font::truncate`],
//! host-tested (`scripts/fontkit-test.sh`). Blobs come from files, so nothing here trusts one:
//! [`Family::parse`] checks the header, [`Family::validate`] every record, and every lookup is
//! bounds-checked, so a damaged blob gives missing glyphs, never a panic.
//!
//! # API (stable: appkit and the Browser build on it)
//!
//! - [`Family::parse`]`(&[u8]) -> Result<Family, FontError>` (also `const`, for baked data),
//!   [`Family::validate`], [`Family::glyph`]`(face, code point)`, [`Family::faces`].
//! - [`Source`]: where [`Font`] finds family `i` (0..[`FAMILY_COUNT`] in the Settings order,
//!   [`SYMBOLS`] the drawn symbols); [`KEYS`]/[`NAMES`] name them, [`FALLBACK`] is the order
//!   missing characters are looked for in.
//! - [`Font`]`{ family, scale, weight }`: line boxes of `16 * scale` pixels with the baseline at
//!   `13 * scale` (where the 8x16 Unifont cells sat). [`Font::layout`] calls back with each
//!   glyph and its pen position; [`Font::width`], [`Font::advance`], [`Font::fit`],
//!   [`Font::truncate`], [`Font::tail`], [`Font::index_at`], [`Font::prefix_width`] measure.
//! - [`Glyph`]: `w`, `h`, `left`, `top` (above the baseline), `adv`, and [`Glyph::spans`] (runs of
//!   coverage 0..=255, magnified).
//!
//! # Characters
//!
//! Every family has all of Latin-1 and U+FFFD, and whatever its font has of Latin Extended-A/B
//! and Additional, Greek, Cyrillic, punctuation, arrows and symbols (the generator prints the
//! table). A character goes: the chosen family, then the other families in [`FALLBACK`] order
//! (Fira Sans has the most: all of Greek and nearly all of Cyrillic and Latin Extended), then the
//! drawn symbols, then [`fold`] (the nearest characters some family has: "Tiếng" without a
//! family that has ế becomes "Tieng", "⇒" becomes "=>"), then a visible box. Zero-width
//! characters ([`ignorable`]) and combining marks no family has take no room.
//!
//! Figtree and Atkinson Hyperlegible Next have no Greek or Cyrillic and little of Latin
//! Extended-B; IBM Plex Sans and Ubuntu Sans have most of Greek; no family has more than a few
//! geometric shapes or symbols (the drawn set covers the common ones: ★ ♥ ● ■ ▲ ▶ ✓ ✗ ☐ ☰ ⌘ ...).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::String;

#[rustfmt::skip]
mod generated;

pub use generated::{FALLBACK, KEYS, NAMES};

/// The UI families (Settings > Appearance): `0..FAMILY_COUNT`.
pub const FAMILY_COUNT: usize = KEYS.len() - 1;
/// The drawn symbols' index in a [`Source`].
pub const SYMBOLS: usize = KEYS.len() - 1;
/// The family the desktop uses unless told otherwise (Figtree).
pub const DEFAULT: usize = 0;
/// The code point of the "no font has this" box in every blob.
pub const BOX: u32 = 0xFFFF;
/// Faces a blob may have.
pub const MAX_FACES: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FontError {
    NotAFont,
    Truncated,
    Corrupt(&'static str),
}

/// One baked size and weight.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Face {
    /// The em in pixels.
    pub em: u8,
    /// The line box (16, 32, 64 or 128) and the baseline within it.
    pub line: u8,
    pub ascent: u8,
    pub weight: u16,
    first: u32,
    count: u32,
}

impl Face {
    /// The scale this face draws unmagnified (its line box over 16).
    pub fn tier(&self) -> i32 {
        (self.line / 16).max(1) as i32
    }

    pub fn glyph_count(&self) -> usize {
        self.count as usize
    }
}

/// A glyph record: (code point, data offset, rle, w, h, left, top, adv).
type Record = (u32, usize, bool, u8, u8, i8, i8, u8);

/// A family's blob: the header read, the faces known, the glyphs looked up on demand.
#[derive(Clone, Copy, Debug)]
pub struct Family<'a> {
    blob: &'a [u8],
    key: &'a str,
    name: &'a str,
    faces: [Face; MAX_FACES],
    nfaces: usize,
    records: usize,
    rec_off: usize,
}

const fn le16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

const fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// `b[from..from + len]` as text, in a const fn (bounds already checked).
const fn text(b: &[u8], from: usize, len: usize) -> Result<&str, FontError> {
    let (_, rest) = b.split_at(from);
    let (s, _) = rest.split_at(len);
    match core::str::from_utf8(s) {
        Ok(s) => Ok(s),
        Err(_) => Err(FontError::Corrupt("The font's name isn't text.")),
    }
}

impl<'a> Family<'a> {
    /// Read a blob's header and face table (see `scripts/gen_sysfont.py` for the layout).
    pub const fn parse(blob: &'a [u8]) -> Result<Family<'a>, FontError> {
        if blob.len() < 24 {
            return Err(FontError::Truncated);
        }
        let magic = b"MILKFONT";
        let mut i = 0;
        while i < 8 {
            if blob[i] != magic[i] {
                return Err(FontError::NotAFont);
            }
            i += 1;
        }
        if le16(blob, 8) != 1 {
            return Err(FontError::Corrupt("The font file is a newer version."));
        }
        let nfaces = blob[10] as usize;
        let (kl, nl) = (blob[11] as usize, blob[12] as usize);
        let records = le32(blob, 16) as usize;
        let rec_off = le32(blob, 20) as usize;
        let head = 24 + kl + nl;
        let faces_at = head + (4 - head % 4) % 4;
        if nfaces == 0 || nfaces > MAX_FACES {
            return Err(FontError::Corrupt(
                "The font has an impossible number of faces.",
            ));
        }
        if blob.len() < faces_at + 16 * nfaces || rec_off < faces_at + 16 * nfaces {
            return Err(FontError::Truncated);
        }
        // records * 12 can't overflow: records came from 32 bits and usize is 64.
        if rec_off > blob.len() || (blob.len() - rec_off) / 12 < records {
            return Err(FontError::Truncated);
        }
        let key = match text(blob, 24, kl) {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        let name = match text(blob, 24 + kl, nl) {
            Ok(s) => s,
            Err(e) => return Err(e),
        };
        let mut faces = [Face {
            em: 0,
            line: 0,
            ascent: 0,
            weight: 0,
            first: 0,
            count: 0,
        }; MAX_FACES];
        let mut f = 0;
        while f < nfaces {
            let o = faces_at + 16 * f;
            let face = Face {
                em: blob[o],
                line: blob[o + 1],
                ascent: blob[o + 2],
                weight: le16(blob, o + 4),
                first: le32(blob, o + 8),
                count: le32(blob, o + 12),
            };
            if face.first as usize > records || face.count as usize > records - face.first as usize
            {
                return Err(FontError::Corrupt("A font face points past its glyphs."));
            }
            if face.line == 0 || face.em == 0 {
                return Err(FontError::Corrupt("A font face has no size."));
            }
            faces[f] = face;
            f += 1;
        }
        Ok(Family {
            blob,
            key,
            name,
            faces,
            nfaces,
            records,
            rec_off,
        })
    }

    /// A blob baked into the program: checked when it is compiled.
    pub const fn baked(blob: &'static [u8]) -> Family<'static> {
        match Family::parse(blob) {
            Ok(f) => f,
            Err(_) => panic!("a baked font blob is damaged"),
        }
    }

    /// Check every glyph record: sorted within its face, its data inside the blob and long
    /// enough for its size.
    pub fn validate(&self) -> Result<(), FontError> {
        for fc in self.faces() {
            let mut last: i64 = -1;
            for r in fc.first as usize..(fc.first + fc.count) as usize {
                let rec = self.record(r).ok_or(FontError::Truncated)?;
                if (rec.0 as i64) <= last {
                    return Err(FontError::Corrupt("The font's glyphs are out of order."));
                }
                last = rec.0 as i64;
                let g = self
                    .glyph_at(r)
                    .ok_or(FontError::Corrupt("A glyph points outside the font."))?;
                let mut px = 0i64;
                let mut ok = true;
                g.spans(1, |x, y, len, _| {
                    ok &= x >= 0 && y >= 0 && len > 0 && x + len <= g.w && y < g.h;
                    px += len as i64;
                });
                if !ok || px > (g.w * g.h) as i64 || g.data_short() {
                    return Err(FontError::Corrupt("A glyph's data is damaged."));
                }
            }
        }
        Ok(())
    }

    pub fn key(&self) -> &'a str {
        self.key
    }

    pub fn name(&self) -> &'a str {
        self.name
    }

    pub fn faces(&self) -> &[Face] {
        &self.faces[..self.nfaces]
    }

    /// The blob's size in bytes.
    pub fn size(&self) -> usize {
        self.blob.len()
    }

    /// The whole blob (what `/System/Fonts/<key>.font` holds).
    pub fn blob(&self) -> &'a [u8] {
        self.blob
    }

    /// Record `r`: (code point, data offset, rle, w, h, left, top, adv).
    fn record(&self, r: usize) -> Option<Record> {
        if r >= self.records {
            return None;
        }
        let o = self.rec_off + 12 * r;
        let b = self.blob.get(o..o + 12)?;
        let off = le32(b, 2);
        Some((
            le16(b, 0) as u32,
            (off & 0x7FFF_FFFF) as usize,
            off & 0x8000_0000 != 0,
            b[6],
            b[7],
            b[8] as i8,
            b[9] as i8,
            b[10],
        ))
    }

    fn glyph_at(&self, r: usize) -> Option<Glyph<'a>> {
        let (_, off, rle, w, h, left, top, adv) = self.record(r)?;
        let data = self.blob.get(off..).unwrap_or(&[]);
        if data.is_empty() && w as usize * h as usize > 0 {
            return None;
        }
        Some(Glyph {
            w: w as i32,
            h: h as i32,
            left: left as i32,
            top: top as i32,
            adv: adv as i32,
            data,
            rle,
        })
    }

    /// The glyph for `cp` in face `face`, if it has one.
    pub fn glyph(&self, face: usize, cp: u32) -> Option<Glyph<'a>> {
        let fc = self.faces().get(face)?;
        let (mut lo, mut hi) = (fc.first as usize, (fc.first + fc.count) as usize);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let c = self.record(mid)?.0;
            if c == cp {
                return self.glyph_at(mid);
            }
            if c < cp {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        None
    }

    /// The face to draw `scale` in `weight` with, at most `tier` unmagnified, and its tier.
    fn pick(&self, tier: i32, weight: Weight) -> Option<(usize, i32)> {
        let heavy = weight != Weight::Regular;
        let want = weight.value() as i32;
        let mut best: Option<(usize, i32, i32)> = None; // (face, tier, weight distance)
        for (i, f) in self.faces().iter().enumerate() {
            let t = f.tier();
            if t > tier || (f.weight > 400) != heavy {
                continue;
            }
            let d = (f.weight as i32 - want).abs();
            let better = match best {
                None => true,
                Some((_, bt, bd)) => t > bt || (t == bt && d < bd),
            };
            if better {
                best = Some((i, t, d));
            }
        }
        best.map(|(i, t, _)| (i, t))
    }

    /// `cp` at `scale` and `weight` from this family: the best face that has it, and the
    /// magnification. Bigger faces have fewer characters, so smaller ones are tried too.
    pub fn find(&self, cp: u32, scale: i32, weight: Weight) -> Option<(Glyph<'a>, i32)> {
        let scale = scale.max(1);
        let mut tier = scale;
        while tier >= 1 {
            let Some((face, t)) = self.pick(tier, weight) else {
                break;
            };
            if let Some(g) = self.glyph(face, cp) {
                return Some((g, (scale / t).max(1)));
            }
            tier = t - 1;
        }
        // No face of that weight has it: any weight at the smallest size.
        let (i, f) = self
            .faces()
            .iter()
            .enumerate()
            .min_by_key(|(_, f)| (f.tier(), (f.weight as i32 - weight.value() as i32).abs()))?;
        self.glyph(i, cp).map(|g| (g, (scale / f.tier()).max(1)))
    }
}

/// One glyph's coverage bitmap and metrics, in its face's pixels.
#[derive(Clone, Copy, Debug)]
pub struct Glyph<'a> {
    pub w: i32,
    pub h: i32,
    /// Left edge relative to the pen.
    pub left: i32,
    /// Top edge in pixels above the baseline.
    pub top: i32,
    pub adv: i32,
    data: &'a [u8],
    rle: bool,
}

impl Glyph<'_> {
    /// Call `f(x, y, len, coverage 1..=255)` for every run of non-zero coverage, row by row
    /// (glyph-local coordinates, `mag` times magnified). Stops at the end of the data.
    pub fn spans(&self, mag: i32, mut f: impl FnMut(i32, i32, i32, u8)) {
        let (w, n) = (self.w, self.w * self.h);
        if w <= 0 || n <= 0 {
            return;
        }
        let mag = mag.max(1);
        let mut emit = |i: i32, len: i32, v: u8| {
            if v == 0 || len <= 0 {
                return;
            }
            let a = v * 17;
            let (mut i, mut len) = (i, len);
            while len > 0 {
                let (x, y) = (i % w, i / w);
                let l = len.min(w - x);
                for r in 0..mag {
                    f(x * mag, y * mag + r, l * mag, a);
                }
                i += l;
                len -= l;
            }
        };
        let mut i = 0; // pixel index
        if self.rle {
            for &b in self.data {
                if i >= n {
                    break;
                }
                let len = (b & 15) as i32 + 1;
                emit(i, len.min(n - i), b >> 4);
                i += len;
            }
        } else {
            // Packed nibbles: merge equal neighbours into runs.
            let mut run_start = 0;
            let mut run_v = 0u8;
            for &b in self.data.iter().take((n as usize).div_ceil(2)) {
                for v in [b >> 4, b & 15] {
                    if i >= n {
                        break;
                    }
                    if v != run_v {
                        emit(run_start, i - run_start, run_v);
                        run_start = i;
                        run_v = v;
                    }
                    i += 1;
                }
            }
            emit(run_start, i - run_start, run_v);
        }
    }

    /// The data ends before the bitmap does.
    fn data_short(&self) -> bool {
        let n = (self.w * self.h) as usize;
        if n == 0 {
            return false;
        }
        if self.rle {
            let mut i = 0usize;
            for &b in self.data {
                if i >= n {
                    return false;
                }
                i += (b & 15) as usize + 1;
            }
            i < n
        } else {
            self.data.len() < n.div_ceil(2)
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Weight {
    Regular,
    Medium,
    Bold,
}

impl Weight {
    pub fn bold(b: bool) -> Weight {
        if b { Weight::Bold } else { Weight::Regular }
    }

    pub fn value(self) -> u16 {
        match self {
            Weight::Regular => 400,
            Weight::Medium => 500,
            Weight::Bold => 700,
        }
    }
}

/// Where a [`Font`] gets its families: index `0..FAMILY_COUNT` (Settings order) and
/// [`SYMBOLS`]. None for one that isn't there (not loaded, or damaged): its characters then
/// come from the others.
pub trait Source<'a> {
    fn family(&self, i: usize) -> Option<&'a Family<'a>>;
}

/// Characters that take no room and are never drawn: zero-width spaces and joiners,
/// direction marks, word joiners, the byte-order mark.
pub fn ignorable(c: char) -> bool {
    matches!(c as u32, 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF)
}

/// Combining marks (drawn over the previous character when a family has them).
pub fn combining(c: char) -> bool {
    matches!(c as u32, 0x0300..=0x036F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

/// What to draw instead of `c` when no family has it: the nearest characters some family has
/// ("ế" -> "e", "⇒" -> "=>", "™" -> "TM"), or None.
pub fn fold(c: char) -> Option<&'static str> {
    let cp = c as u32;
    if cp > 0xFFFF {
        return None;
    }
    let i = generated::FOLD_CP.binary_search(&(cp as u16)).ok()?;
    let (a, b) = (
        *generated::FOLD_AT.get(i)? as usize,
        *generated::FOLD_AT.get(i + 1)? as usize,
    );
    generated::FOLD_STR.get(a..b)
}

/// A text style: family, scale (line boxes of `16 * scale` pixels) and weight.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Font {
    pub family: usize,
    pub scale: i32,
    pub weight: Weight,
}

impl Font {
    pub fn new(family: usize, scale: i32, weight: Weight) -> Font {
        Font {
            family: family.min(FAMILY_COUNT - 1),
            scale: scale.max(1),
            weight,
        }
    }

    /// Height of a line box.
    pub fn line_height(&self) -> i32 {
        16 * self.scale
    }

    /// Baseline, from the top of the line box.
    pub fn baseline(&self) -> i32 {
        13 * self.scale
    }

    /// Pixels capitals stand above the baseline (about 0.7 em).
    pub fn cap_height(&self) -> i32 {
        (13 * self.scale * 7 + 5) / 10
    }

    /// The em in pixels (13 per scale).
    pub fn em(&self) -> i32 {
        13 * self.scale
    }

    /// `cp` from this font's family or the ones after it in [`FALLBACK`].
    fn find<'a>(&self, src: &impl Source<'a>, cp: u32) -> Option<(Glyph<'a>, i32)> {
        if let Some(f) = src.family(self.family)
            && let Some(g) = f.find(cp, self.scale, self.weight)
        {
            return Some(g);
        }
        for &i in FALLBACK.iter() {
            if i == self.family {
                continue;
            }
            if let Some(f) = src.family(i)
                && let Some(g) = f.find(cp, self.scale, self.weight)
            {
                return Some(g);
            }
        }
        None
    }

    /// Call `f(glyph, magnification)` for what `c` draws as: usually one glyph, a fold can be
    /// several, an ignorable character none.
    pub fn glyphs<'a>(&self, src: &impl Source<'a>, c: char, mut f: impl FnMut(&Glyph<'a>, i32)) {
        if ignorable(c) {
            return;
        }
        if let Some((g, m)) = self.find(src, c as u32) {
            return f(&g, m);
        }
        if combining(c) {
            return;
        }
        if let Some(s) = fold(c) {
            for d in s.chars() {
                if let Some((g, m)) = self.find(src, d as u32) {
                    f(&g, m);
                }
            }
            return;
        }
        if let Some((g, m)) = self.find(src, BOX) {
            f(&g, m);
        }
    }

    pub fn advance<'a>(&self, src: &impl Source<'a>, c: char) -> i32 {
        let mut a = 0;
        self.glyphs(src, c, |g, m| a += g.adv * m);
        a
    }

    /// Width of `s` in pixels.
    pub fn width<'a>(&self, src: &impl Source<'a>, s: &str) -> i32 {
        s.chars().map(|c| self.advance(src, c)).sum()
    }

    /// How many bytes of `s` fit in `max` pixels (a char boundary).
    pub fn fit<'a>(&self, src: &impl Source<'a>, s: &str, max: i32) -> usize {
        let mut w = 0;
        for (i, c) in s.char_indices() {
            w += self.advance(src, c);
            if w > max {
                return i;
            }
        }
        s.len()
    }

    /// `s` cut to `max` pixels with "..." when it doesn't fit.
    pub fn truncate<'a>(&self, src: &impl Source<'a>, s: &str, max: i32) -> String {
        if self.width(src, s) <= max {
            return String::from(s);
        }
        let dots = self.width(src, "...");
        let n = self.fit(src, s, (max - dots).max(0));
        let mut out = String::from(s[..n].trim_end());
        out.push_str("...");
        out
    }

    /// The longest end of `s` that fits in `max` pixels (a text field scrolled to its caret).
    pub fn tail<'a, 's>(&self, src: &impl Source<'a>, s: &'s str, max: i32) -> &'s str {
        let mut w = 0;
        for (i, c) in s.char_indices().rev() {
            w += self.advance(src, c);
            if w > max {
                return &s[i + c.len_utf8()..];
            }
        }
        s
    }

    /// The char index in `s` nearest pixel offset `x` (caret placement).
    pub fn index_at<'a>(&self, src: &impl Source<'a>, s: &str, x: i32) -> usize {
        let mut w = 0;
        for (i, c) in s.chars().enumerate() {
            let a = self.advance(src, c);
            if x < w + a / 2 {
                return i;
            }
            w += a;
        }
        s.chars().count()
    }

    /// Width of the first `n` chars of `s` (where a caret after them goes).
    pub fn prefix_width<'a>(&self, src: &impl Source<'a>, s: &str, n: usize) -> i32 {
        s.chars().take(n).map(|c| self.advance(src, c)).sum()
    }

    /// Call `f(pen x, glyph, magnification)` for each glyph of `s` starting at pen `x` (empty
    /// glyphs, like spaces, are skipped); returns the pen after the last one.
    pub fn layout<'a>(
        &self,
        src: &impl Source<'a>,
        x: i32,
        s: &str,
        mut f: impl FnMut(i32, &Glyph<'a>, i32),
    ) -> i32 {
        let mut pen = x;
        for c in s.chars() {
            self.glyphs(src, c, |g, m| {
                if g.w > 0 {
                    f(pen, g, m);
                }
                pen += g.adv * m;
            });
        }
        pen
    }
}

#[cfg(test)]
mod tests;
