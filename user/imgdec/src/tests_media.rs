//! JPEG, GIF, WebP and animation tests: the fixtures from scripts/gen_img_fixtures.py against
//! the reference decoders' pixels (libjpeg-turbo, Pillow's GIF reader, libwebp), plus hand-made,
//! truncated, mutated and hostile files.

use crate::anim::Animation;
use crate::{Format, ImgError, MAX_PIXELS, decode, jpeg, probe, sniff};
use std::path::PathBuf;
use std::time::Instant;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn read(name: &str) -> Vec<u8> {
    std::fs::read(fixtures().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn argb(name: &str) -> Vec<u32> {
    read(&format!("{name}.argb"))
        .chunks(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn names(ext: &str) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(ext))
                .map(String::from)
        })
        .collect();
    v.sort();
    v
}

/// (largest channel difference, pixels that differ at all). Fully transparent pixels are equal
/// whatever their colour.
fn diff(got: &[u32], want: &[u32]) -> (u32, usize) {
    let (mut max, mut n) = (0, 0);
    for (&g, &w) in got.iter().zip(want) {
        if g >> 24 == 0 && w >> 24 == 0 {
            continue;
        }
        let d = [24, 16, 8, 0]
            .iter()
            .map(|&s| ((g >> s) & 0xFF).abs_diff((w >> s) & 0xFF))
            .max()
            .unwrap_or(0);
        n += (d > 0) as usize;
        max = max.max(d);
    }
    (max, n)
}

fn check_fixture(name: &str, ext: &str, max_diff: u32, max_share: f64) {
    let file = read(&format!("{name}{ext}"));
    let img = decode(&file).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    let want = argb(name);
    assert_eq!(img.px.len(), img.w * img.h, "{name}");
    assert_eq!(img.px.len(), want.len(), "{name}: {}x{}", img.w, img.h);
    let (m, n) = diff(&img.px, &want);
    assert!(
        m <= max_diff && n as f64 <= max_share * want.len() as f64,
        "{name}: channels differ by up to {m}, {n} of {} pixels differ",
        want.len()
    );
    let info = probe(&file).unwrap_or_else(|e| panic!("{name}: probe {e:?}"));
    assert_eq!((info.w, info.h), (img.w, img.h), "{name}: probe size");
}

// ---- JPEG ---------------------------------------------------------------------------------

#[test]
fn every_jpeg_fixture_matches_libjpeg_within_rounding() {
    let all = names(".jpg");
    assert!(all.len() >= 20, "fixtures are missing: {all:?}");
    for n in &all {
        // The IDCT and colour conversion round a little differently from libjpeg-turbo's
        // (2 or 3 levels on a few per cent of pixels); the upsampling is the same.
        check_fixture(n, ".jpg", 3, 0.05);
    }
}

#[test]
fn jpeg_orientation_turns_the_picture_and_the_probe() {
    let up = decode(&read("jpeg-orient1.jpg")).unwrap();
    for o in 2..=8 {
        let f = read(&format!("jpeg-orient{o}.jpg"));
        let img = decode(&f).unwrap();
        let info = probe(&f).unwrap();
        if o >= 5 {
            assert_eq!((img.w, img.h), (up.h, up.w), "orientation {o}");
        } else {
            assert_eq!((img.w, img.h), (up.w, up.h), "orientation {o}");
        }
        assert_eq!((info.w, info.h), (img.w, img.h));
        // As stored, every one is the same picture.
        assert_eq!(
            jpeg::decode_oriented(&f, false).unwrap(),
            up,
            "orientation {o}"
        );
    }
}

#[test]
fn jpeg_unsupported_kinds_say_so() {
    let good = read("jpeg-444.jpg");
    // Turn the SOF0 into SOF9 (arithmetic), SOF3 (lossless), and its precision into 12 bits.
    let sof = good.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    for (m, want) in [(0xC9u8, "arithmetic"), (0xC3, "Lossless")] {
        let mut b = good.clone();
        b[sof + 1] = m;
        let e = decode(&b).unwrap_err();
        assert!(
            matches!(e, ImgError::Unsupported(s) if s.contains(want)),
            "{e:?}"
        );
    }
    let mut b = good.clone();
    b[sof + 4] = 12;
    assert!(matches!(decode(&b).unwrap_err(), ImgError::Unsupported(_)));
}

#[test]
fn jpeg_size_claims_are_checked_before_allocating() {
    let good = read("jpeg-444.jpg");
    let sof = good.windows(2).position(|w| w == [0xFF, 0xC0]).unwrap();
    let mut b = good.clone();
    b[sof + 5..sof + 9].copy_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);
    assert_eq!(decode(&b).unwrap_err(), ImgError::TooBig);
    let mut b = good.clone();
    b[sof + 5..sof + 7].copy_from_slice(&[0, 0]);
    assert!(matches!(decode(&b).unwrap_err(), ImgError::Unsupported(_)));
}

#[test]
fn jpeg_cut_short_still_shows_the_top() {
    let good = read("jpeg-big.jpg");
    let img = decode(&good).unwrap();
    // Half the file of a progressive JPEG: the first scans are all there.
    let half = decode(&good[..good.len() / 2]).unwrap();
    assert_eq!((half.w, half.h), (img.w, img.h));
    assert!(decode(&good[..200]).is_err());
}

// ---- GIF ----------------------------------------------------------------------------------

#[test]
fn every_gif_fixture_matches_exactly() {
    let all: Vec<String> = names(".gif")
        .into_iter()
        .filter(|n| !n.starts_with("gif-anim"))
        .collect();
    assert!(all.len() >= 5, "fixtures are missing: {all:?}");
    for n in &all {
        check_fixture(n, ".gif", 0, 0.0);
    }
}

#[test]
fn gif_animation_composites_with_every_disposal() {
    let data = read("gif-anim.gif");
    let info = probe(&data).unwrap();
    assert!(info.animated);
    let mut a = Animation::new(data).unwrap();
    assert_eq!((a.width(), a.height(), a.frame_count()), (32, 24, 4));
    assert_eq!(a.plays(), 0);
    let delays = [200, 150, 100, 500];
    let mut first = None;
    for (i, &delay) in delays.iter().enumerate() {
        let f = a.next_frame().unwrap().unwrap();
        assert_eq!((f.index, f.delay_ms), (i, delay));
        let want = argb(&format!("gif-anim.{i}"));
        assert_eq!(diff(&f.image.px, &want), (0, 0), "frame {i}");
        if i == 0 {
            first = Some(f.image.clone());
        }
    }
    assert!(a.next_frame().is_none());
    assert!(a.should_loop());
    a.rewind();
    let f = a.next_frame().unwrap().unwrap();
    assert_eq!(Some(f.image.clone()), first);
    // decode() is the first frame.
    assert_eq!(Some(decode(&read("gif-anim.gif")).unwrap()), first);
}

#[test]
fn gif_animation_written_by_pillow_matches_its_frames() {
    let mut a = Animation::new(read("gif-anim-slow.gif")).unwrap();
    assert_eq!(a.frame_count(), 2);
    for i in 0..2 {
        let f = a.next_frame().unwrap().unwrap();
        assert_eq!(f.delay_ms, 1500);
        assert_eq!(
            diff(&f.image.px, &argb(&format!("gif-anim-slow.{i}"))),
            (0, 0),
            "frame {i}"
        );
    }
}

/// A GIF made by hand: header, palette, one image of `w` x `h` with `data` as its LZW blocks.
fn gif(w: u16, h: u16, fw: u16, fh: u16, min_code: u8, data: &[u8]) -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend_from_slice(&w.to_le_bytes());
    g.extend_from_slice(&h.to_le_bytes());
    g.extend_from_slice(&[0x80, 0, 0, 0, 0, 0, 255, 255, 255]);
    g.push(0x2C);
    g.extend_from_slice(&[0, 0, 0, 0]);
    g.extend_from_slice(&fw.to_le_bytes());
    g.extend_from_slice(&fh.to_le_bytes());
    g.push(0);
    g.push(min_code);
    for c in data.chunks(255) {
        g.push(c.len() as u8);
        g.extend_from_slice(c);
    }
    g.push(0);
    g.push(0x3B);
    g
}

#[test]
fn gif_hostile_headers() {
    // A frame claiming 65535 x 65535 is refused; so is a screen that big.
    assert_eq!(
        decode(&gif(10, 10, 65535, 65535, 2, &[0x44, 0x01])).unwrap_err(),
        ImgError::TooBig
    );
    assert_eq!(
        decode(&gif(65535, 65535, 1, 1, 2, &[0x44, 0x01])).unwrap_err(),
        ImgError::TooBig
    );
    // Bad minimum code sizes and garbage LZW give a (transparent) picture, not a crash.
    for mc in [0u8, 12, 255] {
        let img = decode(&gif(4, 4, 4, 4, mc, &[0xFF; 20])).unwrap();
        assert!(img.px.iter().all(|&p| p == 0));
    }
    let img = decode(&gif(4, 4, 4, 4, 2, &[0xFF; 300])).unwrap();
    assert_eq!(img.px.len(), 16);
    // No image at all.
    assert!(decode(b"GIF89a\x04\x00\x04\x00\x00\x00\x00\x3B").is_err());
    // A zero-sized screen takes the frame's size.
    let img = decode(&gif(0, 0, 3, 2, 2, &[0x84, 0x1D, 0x81, 0x7A, 0x50, 0x00])).unwrap();
    assert_eq!((img.w, img.h), (3, 2));
}

#[test]
fn gif_lzw_bombs_are_bounded() {
    // Every code repeats the longest string: output is capped at the frame size.
    let t = Instant::now();
    let data = vec![0u8; 200_000];
    let img = decode(&gif(100, 100, 100, 100, 8, &data)).unwrap();
    assert_eq!(img.px.len(), 10_000);
    assert!(t.elapsed().as_secs() < 10);
}

// ---- WebP ---------------------------------------------------------------------------------

#[test]
fn every_webp_fixture_matches_libwebp_exactly() {
    let all: Vec<String> = names(".webp")
        .into_iter()
        .filter(|n| n != "webp-anim")
        .collect();
    assert!(all.len() >= 10, "fixtures are missing: {all:?}");
    for n in &all {
        check_fixture(n, ".webp", 0, 0.0);
    }
}

#[test]
fn webp_animation_matches_libwebp() {
    let data = read("webp-anim.webp");
    assert_eq!(sniff(&data), Format::WebP);
    assert!(probe(&data).unwrap().animated);
    let mut a = Animation::new(data).unwrap();
    assert_eq!(a.frame_count(), 3);
    assert_eq!(a.plays(), 0);
    for (i, delay) in [80, 120, 60].into_iter().enumerate() {
        let f = a.next_frame().unwrap().unwrap();
        assert_eq!(f.delay_ms, delay);
        let want = argb(&format!("webp-anim.{i}"));
        let (m, n) = diff(&f.image.px, &want);
        assert_eq!((m, n), (0, 0), "frame {i}");
    }
    assert!(a.next_frame().is_none());
}

#[test]
fn webp_size_claims_are_checked() {
    // VP8X with a 16M x 16M canvas.
    let mut b = b"RIFF\x00\x00\x00\x00WEBPVP8X\x0a\x00\x00\x00\x00\x00\x00\x00".to_vec();
    b.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    let n = (b.len() - 8) as u32;
    b[4..8].copy_from_slice(&n.to_le_bytes());
    assert_eq!(decode(&b).unwrap_err(), ImgError::TooBig);
    // VP8L at 16384 x 16384 is 268 megapixels.
    let mut l = b"RIFF\x00\x00\x00\x00WEBPVP8L\x05\x00\x00\x00\x2f\xff\xff\xff\x0f".to_vec();
    let n = (l.len() - 8) as u32;
    l[4..8].copy_from_slice(&n.to_le_bytes());
    assert_eq!(decode(&l).unwrap_err(), ImgError::TooBig);
    assert!(16384 * 16384 > MAX_PIXELS);
}

// ---- everything: truncation and mutation never panic -----------------------------------------

fn rng(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn hammer(name: &str, cuts: usize, flips: usize) -> usize {
    let good = read(name);
    let mut count = 0;
    let step = (good.len() / cuts).max(1);
    for cut in (0..good.len()).step_by(step) {
        let _ = decode(&good[..cut]);
        let _ = Animation::new(good[..cut].to_vec())
            .map(|mut a| while let Some(Ok(_)) = a.next_frame() {});
        count += 1;
    }
    let mut seed = 0x9E37_79B9_7F4A_7C15u64 ^ good.len() as u64;
    for _ in 0..flips {
        let mut b = good.clone();
        for _ in 0..1 + rng(&mut seed) % 4 {
            let i = (rng(&mut seed) as usize) % b.len();
            b[i] = match rng(&mut seed) % 4 {
                0 => b[i] ^ 0x80,
                1 => 0xFF,
                2 => 0,
                _ => rng(&mut seed) as u8,
            };
        }
        let _ = decode(&b);
        let _ = probe(&b);
        if let Ok(mut a) = Animation::new(b) {
            while let Some(Ok(_)) = a.next_frame() {}
        }
        count += 1;
    }
    count
}

#[test]
fn no_panic_on_truncated_or_mutated_jpegs() {
    let mut n = 0;
    for f in [
        "jpeg-420.jpg",
        "jpeg-progressive.jpg",
        "jpeg-restart.jpg",
        "jpeg-cmyk.jpg",
        "jpeg-orient6.jpg",
        "jpeg-121.jpg",
    ] {
        n += hammer(f, 400, 600);
    }
    assert!(n > 5_000);
}

#[test]
fn no_panic_on_truncated_or_mutated_gifs() {
    let mut n = 0;
    for f in [
        "gif-still.gif",
        "gif-interlaced.gif",
        "gif-anim.gif",
        "gif-transparent.gif",
    ] {
        n += hammer(f, 400, 800);
    }
    assert!(n > 4_000);
}

#[test]
fn no_panic_on_truncated_or_mutated_webps() {
    let mut n = 0;
    for f in [
        "webp-lossy.webp",
        "webp-lossy-alpha.webp",
        "webp-lossless.webp",
        "webp-lossless-alpha.webp",
        "webp-lossless-palette16.webp",
        "webp-anim.webp",
    ] {
        n += hammer(f, 400, 600);
    }
    assert!(n > 5_000);
}

#[test]
fn random_bytes_behind_real_headers_never_panic() {
    let t = Instant::now();
    let mut seed = 42u64;
    let heads: [&[u8]; 5] = [
        &[0xFF, 0xD8, 0xFF, 0xDB, 0x00, 0x43, 0x00],
        b"GIF89a\x20\x00\x20\x00\xF7\x00\x00",
        b"RIFF\xff\xff\x00\x00WEBPVP8L\xf0\xff\x00\x00\x2f\x1f\xc0\x07\x00",
        b"RIFF\xff\xff\x00\x00WEBPVP8 \xf0\xff\x00\x00\x30\x20\x00\x9d\x01\x2a\x20\x00\x20\x00",
        b"RIFF\xff\xff\x00\x00WEBPVP8X\x0a\x00\x00\x00\x12\x00\x00\x00\x1f\x00\x00\x1f\x00\x00",
    ];
    for h in heads {
        for len in [10usize, 100, 1000, 5000] {
            for _ in 0..60 {
                let mut b = h.to_vec();
                b.extend((0..len).map(|_| rng(&mut seed) as u8));
                let _ = decode(&b);
                let _ = probe(&b);
            }
        }
    }
    assert!(
        t.elapsed().as_secs() < 60,
        "hostile input took {:?}",
        t.elapsed()
    );
}

#[test]
fn stills_are_one_frame_animations() {
    let mut a = Animation::new(read("checker.png")).unwrap();
    assert_eq!(a.frame_count(), 1);
    assert!(!a.is_animated() && !a.should_loop());
    let f = a.next_frame().unwrap().unwrap();
    assert_eq!(f.image, &decode(&read("checker.png")).unwrap());
    assert!(a.next_frame().is_none());
    assert!(Animation::new(b"not a picture".to_vec()).is_err());
}

#[test]
fn probe_reports_sizes_without_decoding() {
    let i = probe(&read("checker.png")).unwrap();
    assert_eq!((i.format, i.animated), (Format::Png, false));
    let d = decode(&read("checker.png")).unwrap();
    assert_eq!((i.w, i.h), (d.w, d.h));
    assert!(probe(b"plain").is_err());
    assert_eq!(Format::WebP.name(), "WebP");
}

/// A longer run of the same (`cargo test -p imgdec --release -- --ignored deep_fuzz`).
#[test]
#[ignore]
fn deep_fuzz() {
    let mut n = 0;
    for f in names(".jpg")
        .iter()
        .map(|n| format!("{n}.jpg"))
        .chain(names(".gif").iter().map(|n| format!("{n}.gif")))
        .chain(names(".webp").iter().map(|n| format!("{n}.webp")))
    {
        n += hammer(&f, 2000, 20_000);
    }
    assert!(n > 0);
}
