//! Host tests against the real baked blobs (kernel/src/sysfont/*.font).

use super::*;
use std::time::Instant;

static BLOBS: [&[u8]; 6] = [
    include_bytes!("../../../kernel/src/sysfont/figtree.font"),
    include_bytes!("../../../kernel/src/sysfont/plex.font"),
    include_bytes!("../../../kernel/src/sysfont/fira.font"),
    include_bytes!("../../../kernel/src/sysfont/atkinson.font"),
    include_bytes!("../../../kernel/src/sysfont/ubuntu.font"),
    include_bytes!("../../../kernel/src/sysfont/symbols.font"),
];

struct All(Vec<&'static Family<'static>>);

impl Source<'static> for All {
    fn family(&self, i: usize) -> Option<&'static Family<'static>> {
        self.0.get(i).copied()
    }
}

/// Only some families (the others "failed to load").
struct Some_(Vec<Option<&'static Family<'static>>>);

impl Source<'static> for Some_ {
    fn family(&self, i: usize) -> Option<&'static Family<'static>> {
        self.0.get(i).copied().flatten()
    }
}

fn all() -> All {
    All(BLOBS
        .iter()
        .map(|b| &*Box::leak(Box::new(Family::parse(b).unwrap())))
        .collect())
}

/// Which family `c` comes from at body size (None: a fold or the box).
fn source_of(src: &All, fam: usize, c: char) -> Option<usize> {
    let f = Font::new(fam, 1, Weight::Regular);
    let (g, _) = f.find(src, c as u32)?;
    src.0.iter().position(|x| {
        x.faces().iter().enumerate().any(|(i, _)| {
            x.glyph(i, c as u32)
                .is_some_and(|h| core::ptr::eq(h.data.as_ptr(), g.data.as_ptr()))
        })
    })
}

#[test]
fn every_blob_parses_and_validates() {
    let s = all();
    for (i, f) in s.0.iter().enumerate() {
        assert_eq!(f.key(), KEYS[i]);
        assert_eq!(f.name(), NAMES[i]);
        f.validate()
            .unwrap_or_else(|e| panic!("{}: {e:?}", f.key()));
        let n = if i == SYMBOLS { 5 } else { 7 };
        assert_eq!(f.faces().len(), n, "{}", f.key());
        // The box is in every text face.
        for (fi, face) in f.faces().iter().enumerate() {
            if face.line <= 32 {
                assert!(
                    f.glyph(fi, BOX).is_some(),
                    "{} face {fi} has no box",
                    f.key()
                );
            }
        }
    }
}

#[test]
fn latin1_is_everywhere_at_every_scale_and_weight() {
    let s = all();
    for fam in 0..FAMILY_COUNT {
        for scale in [1, 2, 3, 4, 5, 8, 12] {
            for weight in [Weight::Regular, Weight::Medium, Weight::Bold] {
                let f = Font::new(fam, scale, weight);
                for cp in (0x20u32..0x7F).chain(0xA0..0x100).chain([0xFFFD]) {
                    let c = char::from_u32(cp).unwrap();
                    // From the family itself, never a fallback.
                    assert_eq!(source_of(&s, fam, c), Some(fam), "{} U+{cp:04X}", KEYS[fam]);
                    if c != '\u{AD}' {
                        assert!(
                            f.advance(&s, c) > 0,
                            "{} {scale} {weight:?} U+{cp:04X}",
                            KEYS[fam]
                        );
                    }
                }
                let w = f.width(&s, "milkOS 12:45");
                assert!(
                    w >= 40 * scale && w <= 160 * scale,
                    "{} {scale}: {w}",
                    KEYS[fam]
                );
            }
        }
    }
}

#[test]
fn scripts_come_from_the_family_that_has_them() {
    let s = all();
    let figtree = 0;
    let fira = 2;
    // Figtree has no Greek or Cyrillic: Fira's are used (the first fallback).
    for c in ['α', 'Ω', 'Ж', 'я', 'ї', 'ґ'] {
        assert_eq!(source_of(&s, figtree, c), Some(fira), "{c}");
    }
    // Figtree has Ł and ő itself.
    for c in ['Ł', 'ő', 'Ž', '€', '—', '…', '“'] {
        assert_eq!(source_of(&s, figtree, c), Some(figtree), "{c}");
    }
    // Plex has its own Cyrillic.
    assert_eq!(source_of(&s, 1, 'Ж'), Some(1));
    // A Vietnamese letter Fira has.
    assert_eq!(source_of(&s, figtree, 'ế'), Some(fira));
    // The drawn symbols.
    for c in ['★', '♥', '●', '▶', '☐', '⌘'] {
        assert_eq!(source_of(&s, figtree, c), Some(SYMBOLS), "{c}");
    }
    // Every family draws every common letter of these languages somehow (never the box).
    let text = "Ελληνικά Русский Українська Čeština Łódź Türkçe Română Tiếng Việt ½ € → ✓ ★";
    for fam in 0..FAMILY_COUNT {
        let f = Font::new(fam, 1, Weight::Regular);
        for c in text.chars() {
            let mut boxed = false;
            f.glyphs(&s, c, |g, _| {
                boxed |= g.w == f.find(&s, BOX).unwrap().0.w
                    && g.h == f.find(&s, BOX).unwrap().0.h
                    && f.find(&s, c as u32).is_none()
            });
            assert!(!boxed, "{} draws {c} as the box", KEYS[fam]);
        }
    }
}

#[test]
fn folding_and_the_box() {
    assert_eq!(fold('ⅰ'), Some("i"));
    assert_eq!(fold('Ḁ').map(|s| s.chars().next()), Some(Some('A')));
    assert_eq!(fold('a'), None);
    assert_eq!(fold('\u{1F600}'), None);
    let s = all();
    let f = Font::new(0, 1, Weight::Regular);
    // ⇒ is drawn (symbols); a character nothing has and nothing folds becomes the box.
    let mut n = 0;
    f.glyphs(&s, '中', |g, _| {
        n += 1;
        assert!(g.w > 0);
    });
    assert_eq!(n, 1);
    assert_eq!(f.advance(&s, '中'), f.advance(&s, '\u{FFFF}'));
    // Emoji (outside the BMP): the box too.
    assert_eq!(f.advance(&s, '😀'), f.advance(&s, '\u{FFFF}'));
    // Zero-width characters take no room; a combining mark nobody has is dropped.
    assert_eq!(f.width(&s, "a\u{200B}b\u{FEFF}"), f.width(&s, "ab"));
    assert_eq!(f.advance(&s, '\u{1DC0}'), 0);
    // A fold with several characters.
    let w3 = f.width(&s, "...");
    if fold('⋯').is_some() {
        assert_eq!(f.advance(&s, '⋯'), w3);
    }
}

#[test]
fn missing_families_fall_back_quietly() {
    let a = all();
    // Only Figtree loaded: Greek folds or boxes, never panics; Latin-1 is unchanged.
    let only = Some_(vec![Some(a.0[0]), None, None, None, None, None]);
    let f = Font::new(0, 1, Weight::Regular);
    assert_eq!(f.width(&only, "Hello"), f.width(&a, "Hello"));
    assert!(f.width(&only, "αβγ ★") > 0);
    // Nothing loaded at all: everything measures 0 and nothing is drawn.
    let none = Some_(vec![None; 6]);
    assert_eq!(f.width(&none, "Hello"), 0);
    assert_eq!(f.layout(&none, 5, "Hello", |_, _, _| panic!("drew")), 5);
}

#[test]
fn measuring() {
    let s = all();
    let f = Font::new(0, 1, Weight::Regular);
    let t = "Grüße aus Αθήνα";
    let w = f.width(&s, t);
    assert_eq!(f.prefix_width(&s, t, t.chars().count()), w);
    assert_eq!(f.fit(&s, t, w), t.len());
    assert!(f.fit(&s, t, w / 2) < t.len());
    assert!(t.is_char_boundary(f.fit(&s, t, w / 2)));
    let cut = f.truncate(&s, t, w / 2);
    assert!(cut.ends_with("...") && f.width(&s, &cut) <= w / 2);
    let tail = f.tail(&s, t, w / 3);
    assert!(t.ends_with(tail) && f.width(&s, tail) <= w / 3);
    assert_eq!(f.index_at(&s, t, 0), 0);
    assert_eq!(f.index_at(&s, t, w + 50), t.chars().count());
    assert_eq!(f.layout(&s, 10, t, |_, _, _| {}), 10 + w);
    // Bigger scales are wider.
    assert!(Font::new(0, 2, Weight::Bold).width(&s, t) > w);
}

#[test]
fn hostile_blobs_never_panic() {
    let t = Instant::now();
    let good = BLOBS[0];
    assert_eq!(Family::parse(b"").unwrap_err(), FontError::Truncated);
    assert_eq!(Family::parse(&[0u8; 64]).unwrap_err(), FontError::NotAFont);
    let mut seed = 7u64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut parsed = 0;
    // Truncations, and mutations of the header, the face table, records and data.
    for cut in (0..good.len()).step_by(997).chain(0..400) {
        let b: &'static [u8] = Box::leak(good[..cut].to_vec().into_boxed_slice());
        if let Ok(f) = Family::parse(b) {
            let _ = f.validate();
            let _ = f.glyph(0, 'A' as u32);
        }
    }
    for i in 0..3000 {
        let mut b = good.to_vec();
        let at = if i % 2 == 0 {
            (rnd() as usize) % 2000
        } else {
            (rnd() as usize) % b.len()
        };
        for k in 0..1 + rnd() % 3 {
            let j = (at + k as usize) % b.len();
            b[j] = rnd() as u8;
        }
        let b: &'static [u8] = Box::leak(b.into_boxed_slice());
        if let Ok(f) = Family::parse(b) {
            parsed += 1;
            let _ = f.validate();
            let fam: &'static Family<'static> = Box::leak(Box::new(f));
            let src = Some_(vec![Some(fam), None, None, None, None, None]);
            for scale in [1, 2, 4, 8] {
                let font = Font::new(0, scale, Weight::Bold);
                font.layout(&src, 0, "Hello, Ωμέγα ★ 12:45", |_, g, m| {
                    g.spans(m, |_, _, _, _| {})
                });
            }
        }
    }
    assert!(parsed > 1000);
    // A guard against runaway parsing, not a benchmark: a debug build on a loaded Mac can take
    // over a minute here.
    assert!(t.elapsed().as_secs() < 240);
}

#[test]
fn keys_and_fallback_order() {
    assert_eq!(KEYS.len(), NAMES.len());
    assert_eq!(FALLBACK.len(), KEYS.len());
    assert_eq!(*FALLBACK.last().unwrap(), SYMBOLS);
    let mut seen = FALLBACK.to_vec();
    seen.sort();
    assert_eq!(seen, (0..KEYS.len()).collect::<Vec<_>>());
    assert_eq!(KEYS[DEFAULT], "figtree");
}
