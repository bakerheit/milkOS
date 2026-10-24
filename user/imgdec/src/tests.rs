use crate::inflate::{InflateError, crc32, inflate, zlib_decompress};
use crate::{Format, Image, ImgError, decode, sniff};
use std::path::PathBuf;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn expected(name: &str) -> Vec<u32> {
    let b = std::fs::read(fixtures().join(format!("{name}.argb"))).unwrap();
    b.chunks(4)
        .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn names() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(fixtures())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".png"))
                .map(String::from)
        })
        .collect();
    v.sort();
    v
}

// ---- PNG: the generated fixtures ---------------------------------------------------------

#[test]
fn every_png_fixture_decodes_to_exactly_the_expected_pixels() {
    let all = names();
    assert!(all.len() >= 20, "fixtures are missing: {all:?}");
    for n in &all {
        let file = std::fs::read(fixtures().join(format!("{n}.png"))).unwrap();
        let img = decode(&file).unwrap_or_else(|e| panic!("{n}: {e:?}"));
        let want = expected(n);
        assert_eq!(img.px.len(), img.w * img.h, "{n}");
        assert_eq!(img.px.len(), want.len(), "{n}: pixel count");
        if let Some(i) = (0..want.len()).find(|&i| img.px[i] != want[i]) {
            panic!(
                "{n}: pixel {i} (x {}, y {}) is {:08x}, want {:08x}",
                i % img.w,
                i / img.w,
                img.px[i],
                want[i]
            );
        }
    }
}

// ---- PNG: the kernel's own encoder (screenshots) -------------------------------------------

use crate::kernel_png as kpng;

#[test]
fn decodes_what_the_kernel_png_encoder_writes() {
    let mut seed = 12345u32;
    let mut rnd = move || {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        seed >> 8
    };
    for &(w, h) in &[(1usize, 1usize), (7, 3), (64, 48), (257, 31), (200, 120)] {
        // Flat areas with some noise, like a screenshot.
        let mut px = vec![0u32; w * h];
        for (i, p) in px.iter_mut().enumerate() {
            *p = if (i / w / 16 + i % w / 16) % 2 == 0 {
                0xFFFDF8
            } else {
                0x6B5846
            };
            if rnd() % 7 == 0 {
                *p = rnd() & 0xFF_FFFF;
            }
        }
        let png = kpng::encode(&px, w, h, w);
        let img = decode(&png).unwrap_or_else(|e| panic!("{w}x{h}: {e:?}"));
        assert_eq!((img.w, img.h), (w, h));
        let want: Vec<u32> = px.iter().map(|p| 0xFF00_0000 | (p & 0xFF_FFFF)).collect();
        assert_eq!(img.px, want, "{w}x{h}");
    }
    // A stride larger than the width: only the visible part is written.
    let px: Vec<u32> = (0..40 * 5).map(|i| 0x10_0000 + i as u32).collect();
    let img = decode(&kpng::encode(&px, 30, 5, 40)).unwrap();
    assert_eq!((img.w, img.h), (30, 5));
    assert_eq!(img.px[31], 0xFF00_0000 | px[40 + 1]);
}

// ---- PNG: bad files ----------------------------------------------------------------------

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u32).to_be_bytes().to_vec();
    v.extend_from_slice(kind);
    v.extend_from_slice(data);
    v.extend_from_slice(&crc32(&[kind, data]).to_be_bytes());
    v
}

fn ihdr(w: u32, h: u32, depth: u8, ctype: u8, il: u8) -> Vec<u8> {
    let mut d = w.to_be_bytes().to_vec();
    d.extend_from_slice(&h.to_be_bytes());
    d.extend_from_slice(&[depth, ctype, 0, 0, il]);
    chunk(b"IHDR", &d)
}

const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

fn png_of(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut v = SIG.to_vec();
    for p in parts {
        v.extend_from_slice(p);
    }
    v
}

/// A zlib stream of stored blocks around `raw`.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut v = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    if raw.is_empty() {
        v.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(c) = chunks.next() {
        v.push(u8::from(chunks.peek().is_none()));
        v.extend_from_slice(&(c.len() as u16).to_le_bytes());
        v.extend_from_slice(&(!(c.len() as u16)).to_le_bytes());
        v.extend_from_slice(c);
    }
    v.extend_from_slice(&crate::inflate::adler32(raw).to_be_bytes());
    v
}

#[test]
fn a_hand_made_png_works() {
    // 2x2 grey, one filter-0 row each.
    let raw = [0u8, 10, 200, 0, 255, 0];
    let png = png_of(&[
        ihdr(2, 2, 8, 0, 0),
        chunk(b"IDAT", &zlib_stored(&raw)),
        chunk(b"IEND", &[]),
    ]);
    let img = decode(&png).unwrap();
    assert_eq!(img.px, vec![0xFF0A0A0A, 0xFFC8C8C8, 0xFFFFFFFF, 0xFF000000]);
}

#[test]
fn big_claims_are_refused_before_allocating() {
    // 100000 x 100000 would be 40 GB.
    let png = png_of(&[ihdr(100_000, 100_000, 8, 6, 0), chunk(b"IEND", &[])]);
    assert_eq!(decode(&png), Err(ImgError::TooBig));
    let png = png_of(&[ihdr(4000, 4000, 8, 6, 0), chunk(b"IEND", &[])]); // 16 MP
    assert_eq!(decode(&png), Err(ImgError::TooBig));
    let png = png_of(&[ihdr(u32::MAX, u32::MAX, 8, 6, 0), chunk(b"IEND", &[])]);
    assert!(matches!(
        decode(&png),
        Err(ImgError::Corrupt(_)) | Err(ImgError::TooBig)
    ));
    // Just inside the limit: allowed to try (and then truncated, there is no data).
    let png = png_of(&[ihdr(2048, 4096, 8, 2, 0), chunk(b"IEND", &[])]);
    assert!(matches!(decode(&png), Err(ImgError::Corrupt(_))));
}

#[test]
fn more_picture_data_than_the_image_is_an_error_not_a_growing_buffer() {
    // 1x1 grey but the stream holds a megabyte.
    let big = vec![0u8; 1 << 20];
    let png = png_of(&[
        ihdr(1, 1, 8, 0, 0),
        chunk(b"IDAT", &zlib_stored(&big)),
        chunk(b"IEND", &[]),
    ]);
    assert!(matches!(decode(&png), Err(ImgError::Corrupt(_))));
}

#[test]
fn damaged_png_pieces_are_reported() {
    let good = std::fs::read(fixtures().join("rgb8-filters.png")).unwrap();
    assert!(decode(&good).is_ok());
    // Bad signature.
    let mut b = good.clone();
    b[1] = b'X';
    assert!(matches!(decode(&b), Err(ImgError::Unrecognised(_))));
    // A flipped byte inside a chunk breaks its CRC.
    let mut b = good.clone();
    b[40] ^= 0x55;
    assert!(matches!(decode(&b), Err(ImgError::Corrupt(_))));
    // Cut short anywhere.
    for cut in [
        8,
        12,
        20,
        33,
        41,
        good.len() / 2,
        good.len() - 13,
        good.len() - 1,
    ] {
        assert!(decode(&good[..cut]).is_err(), "cut at {cut}");
    }
    // Header must come first; need picture data; palette image needs a palette.
    assert!(matches!(
        decode(&png_of(&[chunk(b"IEND", &[])])),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(
        decode(&png_of(&[ihdr(1, 1, 8, 0, 0), chunk(b"IEND", &[])])),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(
        decode(&png_of(&[
            ihdr(1, 1, 8, 3, 0),
            chunk(b"IDAT", &zlib_stored(&[0, 0])),
            chunk(b"IEND", &[])
        ])),
        Err(ImgError::Corrupt(_))
    ));
    // Colour type and depth combinations that aren't allowed, and unknown methods.
    assert!(matches!(
        decode(&png_of(&[ihdr(1, 1, 16, 3, 0)])),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(
        decode(&png_of(&[ihdr(1, 1, 4, 2, 0)])),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(
        decode(&png_of(&[ihdr(1, 1, 8, 5, 0)])),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(
        decode(&png_of(&[ihdr(1, 1, 8, 2, 2)])),
        Err(ImgError::Unsupported(_))
    ));
    assert!(matches!(
        decode(&png_of(&[ihdr(0, 5, 8, 2, 0)])),
        Err(ImgError::Corrupt(_))
    ));
}

#[test]
fn a_bad_filter_byte_and_a_palette_index_past_the_palette() {
    let png = png_of(&[
        ihdr(1, 1, 8, 0, 0),
        chunk(b"IDAT", &zlib_stored(&[9, 0])),
        chunk(b"IEND", &[]),
    ]);
    assert!(matches!(decode(&png), Err(ImgError::Corrupt(_))));
    let png = png_of(&[
        ihdr(1, 1, 8, 3, 0),
        chunk(b"PLTE", &[1, 2, 3]),
        chunk(b"IDAT", &zlib_stored(&[0, 5])),
        chunk(b"IEND", &[]),
    ]);
    assert!(matches!(decode(&png), Err(ImgError::Corrupt(_))));
}

#[test]
fn a_wrong_zlib_checksum_is_caught() {
    let mut z = zlib_stored(&[0, 7]);
    let n = z.len();
    z[n - 1] ^= 1;
    let png = png_of(&[ihdr(1, 1, 8, 0, 0), chunk(b"IDAT", &z), chunk(b"IEND", &[])]);
    assert!(matches!(decode(&png), Err(ImgError::Corrupt(_))));
}

// ---- inflate -------------------------------------------------------------------------------

#[test]
fn inflate_rejects_garbage_without_panicking() {
    // Block type 3.
    assert!(matches!(
        inflate(&[0b0000_0111], 100),
        Err(InflateError::Corrupt(_))
    ));
    // Empty input.
    assert_eq!(inflate(&[], 100), Err(InflateError::Truncated));
    // Stored block whose length check fails.
    assert!(matches!(
        inflate(&[1, 5, 0, 0, 0, 1, 2, 3, 4, 5], 100),
        Err(InflateError::Corrupt(_))
    ));
    // Stored block longer than the data.
    assert_eq!(
        inflate(&[1, 5, 0, 0xFA, 0xFF, 1, 2], 100),
        Err(InflateError::Truncated)
    );
    // Stored block over the limit.
    assert_eq!(
        inflate(&[1, 3, 0, 0xFC, 0xFF, 1, 2, 3], 2),
        Err(InflateError::TooBig)
    );
    // Not zlib.
    assert!(zlib_decompress(&[0x00, 0x00, 1], 10).is_err());
    assert!(zlib_decompress(&[0x78], 10).is_err());
}

#[test]
fn inflate_random_bytes_never_panic_and_never_exceed_the_limit() {
    let mut s = 0x1234_5678u32;
    for _ in 0..3000 {
        s = s.wrapping_mul(1664525).wrapping_add(1013904223);
        let n = (s >> 24) as usize % 120;
        let data: Vec<u8> = (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 16) as u8
            })
            .collect();
        if let Ok((out, used)) = inflate(&data, 5000) {
            assert!(out.len() <= 5000 && used <= data.len());
        }
    }
}

#[test]
fn zlib_known_vector() {
    // "hello hello hello" compressed with a length/distance pair (fixed Huffman).
    let z = [
        0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x90, 0x00, 0x3a, 0x2e, 0x06,
        0x7d,
    ];
    assert_eq!(zlib_decompress(&z, 100).unwrap(), b"hello hello hello");
    assert_eq!(zlib_decompress(&z, 5), Err(InflateError::TooBig));
}

// ---- robustness: mutated and truncated real files ----------------------------------------------

fn fix_crcs(file: &mut [u8]) {
    let mut pos = 8;
    while pos + 12 <= file.len() {
        let len =
            u32::from_be_bytes([file[pos], file[pos + 1], file[pos + 2], file[pos + 3]]) as usize;
        if pos + 12 + len > file.len() {
            break;
        }
        let crc = crc32(&[&file[pos + 4..pos + 8], &file[pos + 8..pos + 8 + len]]);
        file[pos + 8 + len..pos + 12 + len].copy_from_slice(&crc.to_be_bytes());
        pos += 12 + len;
    }
}

#[test]
fn no_panic_on_truncated_or_mutated_pngs() {
    let mut count = 0;
    for n in [
        "rgb8-filters",
        "rgb8-interlaced",
        "pal4",
        "rgba16-interlaced",
        "gray1",
        "checker",
        "one-px",
    ] {
        let good = std::fs::read(fixtures().join(format!("{n}.png"))).unwrap();
        for cut in 0..good.len() {
            let _ = decode(&good[..cut]);
            count += 1;
        }
        for i in 8..good.len() {
            for flip in [0x01u8, 0x80, 0xFF] {
                let mut b = good.clone();
                b[i] ^= flip;
                let _ = decode(&b); // CRC catches it
                fix_crcs(&mut b);
                let _ = decode(&b); // and with the CRC repaired, the decoder meets the damage itself
                count += 2;
            }
        }
    }
    assert!(count > 10_000);
}

// ---- PPM / PGM / PBM -----------------------------------------------------------------------

#[test]
fn ppm_p6_p5_p3_p2() {
    let img = decode(b"P6\n2 1\n255\n\xFF\x00\x00\x00\x80\xFF").unwrap();
    assert_eq!((img.w, img.h), (2, 1));
    assert_eq!(img.px, vec![0xFFFF0000, 0xFF0080FF]);
    let img = decode(b"P5 2 2 255 \x00\x40\x80\xFF").unwrap();
    assert_eq!(img.px, vec![0xFF000000, 0xFF404040, 0xFF808080, 0xFFFFFFFF]);
    let img = decode(b"P3\n# a comment\n1 2\n255\n10 20 30 # more\n40 50 60\n").unwrap();
    assert_eq!(img.px, vec![0xFF0A141E, 0xFF28323C]);
}

#[test]
fn ppm_scales_other_maxvals() {
    // maxval 15: 15 -> 255, 0 -> 0, 5 -> 85.
    let img = decode(b"P2 3 1 15 0 5 15").unwrap();
    assert_eq!(img.px, vec![0xFF000000, 0xFF555555, 0xFFFFFFFF]);
    // 16-bit: 65535 -> 255, 32768 -> 128.
    let img = decode(b"P5 2 1 65535 \xFF\xFF\x80\x00").unwrap();
    assert_eq!(img.px, vec![0xFFFFFFFF, 0xFF808080]);
    // Values above maxval in the text forms are an error.
    assert!(matches!(decode(b"P2 1 1 10 11"), Err(ImgError::Corrupt(_))));
}

#[test]
fn pbm_p1_and_p4() {
    // 1 = black. P1 may run the digits together.
    let img = decode(b"P1\n4 2\n0110\n1001\n").unwrap();
    let (w, b) = (0xFFFFFFFF, 0xFF000000);
    assert_eq!(img.px, vec![w, b, b, w, b, w, w, b]);
    let img = decode(b"P1 3 1 1 0 1").unwrap();
    assert_eq!(img.px, vec![b, w, b]);
    // P4: rows padded to bytes. 10 wide = 2 bytes per row.
    let img = decode(b"P4 10 2 \xAA\x80\x55\x40").unwrap();
    assert_eq!(img.w, 10);
    assert_eq!(img.px[0], b);
    assert_eq!(img.px[1], w);
    assert_eq!(img.px[8], b); // second byte, top bit
    assert_eq!(img.px[10 + 1], b); // row 2, 0x55 = 01010101
    assert_eq!(img.px[10], w);
}

#[test]
fn pnm_bad_files() {
    assert_eq!(decode(b"P6 4000 4000 255 "), Err(ImgError::TooBig));
    assert!(matches!(decode(b"P6 0 5 255 "), Err(ImgError::Corrupt(_))));
    assert!(matches!(decode(b"P6 2 2 0 "), Err(ImgError::Corrupt(_))));
    assert!(matches!(
        decode(b"P6 2 2 70000 "),
        Err(ImgError::Corrupt(_))
    ));
    assert_eq!(decode(b"P6 2 2 255 \x00\x00\x00"), Err(ImgError::Truncated));
    assert_eq!(decode(b"P6 2 2"), Err(ImgError::Truncated));
    assert_eq!(decode(b"P3 1 1 255 1 2"), Err(ImgError::Truncated));
    assert!(matches!(
        decode(b"P3 1 1 255 1 x 3"),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(decode(b"P1 1 1 7"), Err(ImgError::Corrupt(_))));
    assert!(matches!(
        decode(b"P6 99999999999 1 255 "),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(
        decode(b"P6 1 1 255X\x00\x00\x00"),
        Err(ImgError::Corrupt(_))
    ));
}

// ---- BMP ---------------------------------------------------------------------------------------

fn bmp(
    w: i32,
    h: i32,
    bpp: u16,
    comp: u32,
    palette: &[[u8; 4]],
    masks: Option<[u32; 3]>,
    pixels: &[u8],
) -> Vec<u8> {
    let hsize: u32 = if masks.is_some() && comp == 3 { 52 } else { 40 };
    let off = 14 + hsize as usize + palette.len() * 4;
    let mut v = b"BM".to_vec();
    v.extend_from_slice(&((off + pixels.len()) as u32).to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    v.extend_from_slice(&(off as u32).to_le_bytes());
    v.extend_from_slice(&hsize.to_le_bytes());
    v.extend_from_slice(&w.to_le_bytes());
    v.extend_from_slice(&h.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&bpp.to_le_bytes());
    v.extend_from_slice(&comp.to_le_bytes());
    v.extend_from_slice(&(pixels.len() as u32).to_le_bytes());
    v.extend_from_slice(&[0; 8]); // ppm x, y
    v.extend_from_slice(&(palette.len() as u32).to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    if hsize == 52 {
        for m in masks.unwrap() {
            v.extend_from_slice(&m.to_le_bytes());
        }
    }
    for p in palette {
        v.extend_from_slice(p);
    }
    v.extend_from_slice(pixels);
    v
}

#[test]
fn bmp_24_bit_bottom_up_with_row_padding() {
    // 2 x 2, rows padded to 8 bytes. Bottom row first: (blue, white), then (red, green).
    let px = [
        0xFF, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0, 0, // bottom row: blue, white (BGR)
        0x00, 0x00, 0xFF, 0x00, 0xFF, 0x00, 0, 0, // top row: red, green
    ];
    let img = decode(&bmp(2, 2, 24, 0, &[], None, &px)).unwrap();
    assert_eq!(img.px, vec![0xFFFF0000, 0xFF00FF00, 0xFF0000FF, 0xFFFFFFFF]);
    // Top-down (negative height): the rows are as stored.
    let img = decode(&bmp(2, -2, 24, 0, &[], None, &px)).unwrap();
    assert_eq!(img.px, vec![0xFF0000FF, 0xFFFFFFFF, 0xFFFF0000, 0xFF00FF00]);
}

#[test]
fn bmp_palettes_1_4_8_bit() {
    let pal = [
        [0, 0, 255, 0],
        [0, 255, 0, 0],
        [255, 0, 0, 0],
        [10, 20, 30, 0],
    ]; // BGRX
    // 8 bit, 3 wide (stride 4).
    let img = decode(&bmp(3, 1, 8, 0, &pal, None, &[2, 0, 3, 0])).unwrap();
    assert_eq!(img.px, vec![0xFF0000FF, 0xFFFF0000, 0xFF1E140A]);
    // 4 bit, 3 wide: nibbles 1, 2, 3.
    let img = decode(&bmp(3, 1, 4, 0, &pal, None, &[0x12, 0x30, 0, 0])).unwrap();
    assert_eq!(img.px, vec![0xFF00FF00, 0xFF0000FF, 0xFF1E140A]);
    // 1 bit, 9 wide: 101010101.
    let img = decode(&bmp(9, 1, 1, 0, &pal[..2], None, &[0xAA, 0x80, 0, 0])).unwrap();
    assert_eq!(img.px[0], 0xFF00FF00);
    assert_eq!(img.px[1], 0xFFFF0000);
    assert_eq!(img.px[8], 0xFF00FF00);
    // An index past the palette.
    assert!(matches!(
        decode(&bmp(1, 1, 8, 0, &pal[..2], None, &[7, 0, 0, 0])),
        Err(ImgError::Corrupt(_))
    ));
}

#[test]
fn bmp_16_and_32_bit() {
    // 16 bit default 5-5-5: 0x7C00 red, 0x03E0 green, 0x001F blue.
    let img = decode(&bmp(
        3,
        1,
        16,
        0,
        &[],
        None,
        &[0x00, 0x7C, 0xE0, 0x03, 0x1F, 0x00, 0, 0],
    ))
    .unwrap();
    assert_eq!(img.px, vec![0xFFFF0000, 0xFF00FF00, 0xFF0000FF]);
    // 16 bit 5-6-5 with masks: green is 6 bits.
    let img = decode(&bmp(
        1,
        1,
        16,
        3,
        &[],
        Some([0xF800, 0x07E0, 0x001F]),
        &[0xE0, 0x07, 0, 0],
    ))
    .unwrap();
    assert_eq!(img.px, vec![0xFF00FF00]);
    // 32 bit BI_RGB: BGRX, the fourth byte is not alpha.
    let img = decode(&bmp(1, 1, 32, 0, &[], None, &[0x10, 0x20, 0x30, 0x00])).unwrap();
    assert_eq!(img.px, vec![0xFF302010]);
    // 32 bit with masks including alpha.
    let img = decode(&bmp(
        1,
        1,
        32,
        3,
        &[],
        Some([0x00FF0000, 0x0000FF00, 0x000000FF]),
        &[1, 2, 3, 4],
    ))
    .unwrap();
    assert_eq!(img.px, vec![0xFF030201]); // no alpha mask given: opaque
}

#[test]
fn bmp_old_core_header() {
    // 12-byte header, 24 bit, 1x1.
    let mut v = b"BM".to_vec();
    v.extend_from_slice(&30u32.to_le_bytes());
    v.extend_from_slice(&[0; 4]);
    v.extend_from_slice(&26u32.to_le_bytes());
    v.extend_from_slice(&12u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&24u16.to_le_bytes());
    v.extend_from_slice(&[1, 2, 3, 0]);
    let img = decode(&v).unwrap();
    assert_eq!(img.px, vec![0xFF030201]);
}

#[test]
fn bmp_bad_files() {
    assert!(matches!(
        decode(&bmp(2, 2, 24, 1, &[], None, &[0; 16])),
        Err(ImgError::Unsupported(_))
    ));
    assert!(matches!(
        decode(&bmp(2, 2, 24, 0, &[], None, &[0; 8])),
        Err(ImgError::Truncated)
    ));
    assert!(matches!(
        decode(&bmp(2, 2, 7, 0, &[], None, &[0; 16])),
        Err(ImgError::Unsupported(_))
    ));
    assert_eq!(
        decode(&bmp(5000, 5000, 24, 0, &[], None, &[0; 16])),
        Err(ImgError::TooBig)
    );
    assert!(matches!(
        decode(&bmp(0, 2, 24, 0, &[], None, &[0; 16])),
        Err(ImgError::Corrupt(_))
    ));
    assert!(matches!(decode(b"BM\x00\x00"), Err(ImgError::Truncated)));
    let good = bmp(2, 2, 24, 0, &[], None, &[0; 16]);
    for cut in 0..good.len() {
        let _ = decode(&good[..cut]);
    }
    for i in 0..good.len() {
        let mut b = good.clone();
        b[i] ^= 0xFF;
        let _ = decode(&b);
    }
}

// ---- sniffing and messages --------------------------------------------------------------------------

#[test]
fn sniffing() {
    assert_eq!(sniff(b"%PDF-1.7\n"), Format::Pdf);
    assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), Format::Jpeg);
    assert_eq!(sniff(b"GIF89a"), Format::Gif);
    assert_eq!(sniff(b"RIFF\x10\0\0\0WEBPVP8 "), Format::WebP);
    assert_eq!(sniff(b"RIFF\x10\0\0\0WAVEfmt "), Format::Unknown);
    assert_eq!(sniff(b"hello"), Format::Unknown);
    assert_eq!(sniff(b"P7\n"), Format::Unknown);
    assert_eq!(sniff(b""), Format::Unknown);
    for f in [&b"%PDF-1.4"[..], b"plain text", b""] {
        let e = decode(f).unwrap_err();
        assert!(matches!(e, ImgError::Unrecognised(_)));
        assert!(!e.message().is_empty());
    }
    // The start of a JPEG or GIF is now just a short file.
    for f in [&[0xFF, 0xD8, 0xFF, 0xE0][..], b"GIF89a"] {
        assert_eq!(decode(f).unwrap_err(), ImgError::Truncated);
    }
    assert!(decode(b"%PDF-1.4").unwrap_err().message().contains("PDF"));
}

// ---- scaling ----------------------------------------------------------------------------------------

fn img(w: usize, h: usize, px: Vec<u32>) -> Image {
    assert_eq!(px.len(), w * h);
    Image { w, h, px }
}

#[test]
fn scaling_down_averages_and_up_repeats() {
    let a = img(2, 2, vec![0xFF000000, 0xFFFFFFFF, 0xFFFFFFFF, 0xFF000000]);
    assert_eq!(a.scaled(2, 2).unwrap(), a);
    assert_eq!(a.scaled(1, 1).unwrap().px, vec![0xFF808080]);
    let up = a.scaled(4, 4).unwrap();
    assert_eq!(
        &up.px[..4],
        &[0xFF000000, 0xFF000000, 0xFFFFFFFF, 0xFFFFFFFF]
    );
    assert_eq!(&up.px[4..8], &up.px[..4]);
    assert_eq!(
        &up.px[8..12],
        &[0xFFFFFFFF, 0xFFFFFFFF, 0xFF000000, 0xFF000000]
    );
    // One axis shrinks while the other grows.
    let b = img(4, 1, vec![0xFF000000, 0xFF000000, 0xFFFFFFFF, 0xFFFFFFFF]);
    let c = b.scaled(2, 3).unwrap();
    assert_eq!(
        c.px,
        vec![
            0xFF000000, 0xFFFFFFFF, 0xFF000000, 0xFFFFFFFF, 0xFF000000, 0xFFFFFFFF
        ]
    );
    // 25 percent of a big flat picture stays flat.
    let flat = img(100, 80, vec![0xFF336699; 8000]);
    assert!(
        flat.scaled(25, 20)
            .unwrap()
            .px
            .iter()
            .all(|&p| p == 0xFF336699)
    );
}

#[test]
fn scaling_weights_by_alpha() {
    // A fully transparent pixel must not tint the opaque one next to it.
    let a = img(2, 1, vec![0x00FF0000, 0xFF0000FF]);
    assert_eq!(a.scaled(1, 1).unwrap().px, vec![0x800000FF]);
    let clear = img(2, 1, vec![0x00FF0000, 0x00FF0000]);
    assert_eq!(clear.scaled(1, 1).unwrap().px, vec![0]);
}

#[test]
fn scaling_limits_and_odd_sizes() {
    let a = img(3, 3, (0..9).map(|i| 0xFF000000 | (i * 0x10101)).collect());
    for (w, h) in [(1, 1), (2, 2), (5, 5), (7, 2), (1, 9), (3, 3)] {
        let s = a.scaled(w, h).unwrap();
        assert_eq!((s.w, s.h, s.px.len()), (w, h, w * h));
    }
    assert!(a.scaled(0, 5).is_err());
    assert_eq!(a.scaled(10_000, 10_000), Err(ImgError::TooBig));
}

#[test]
fn fit_size_keeps_the_shape() {
    let a = img(400, 200, vec![0; 80_000]);
    assert_eq!(a.fit_size(200, 200), (200, 100));
    assert_eq!(a.fit_size(800, 100), (200, 100));
    assert_eq!(a.fit_size(1000, 1000), (1000, 500)); // it also enlarges: callers cap with min() for "fit"
    let tall = img(10, 1000, vec![0; 10_000]);
    assert_eq!(tall.fit_size(100, 100), (1, 100));
    assert_eq!(a.fit_size(0, 0), (1, 1));
}

#[test]
fn flattening_over_a_background() {
    assert_eq!(Image::flatten_pixel(0xFF123456, 0xFFFFFF), 0x123456);
    assert_eq!(Image::flatten_pixel(0x00123456, 0xABCDEF), 0xABCDEF);
    assert_eq!(Image::flatten_pixel(0x80000000, 0xFFFFFF), 0x7F7F7F);
}

// ---- PDF -----------------------------------------------------------------------------------------

mod pdf_tests {
    use crate::pdf::inspect;
    use crate::{ImgError, decode};

    fn pdf(parts: &[&[u8]]) -> Vec<u8> {
        let mut v = b"%PDF-1.4\n".to_vec();
        for p in parts {
            v.extend_from_slice(p);
        }
        v.extend_from_slice(b"\ntrailer << /Root 1 0 R >>\n%%EOF\n");
        v
    }

    const PAGES2: &[u8] = b"1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >> endobj\n3 0 obj << /Type /Page /Parent 2 0 R /Contents 5 0 R >> endobj\n4 0 obj << /Type/Page /Parent 2 0 R >> endobj\n";

    #[test]
    fn counts_pages_and_reads_plain_text() {
        let content = b"BT /F1 12 Tf 72 700 Td (Hello, \\(PDF\\) world) Tj 0 -14 Td [(Sec) 20 (ond) -400 (line)] TJ T* (third) Tj ET";
        let stream = [
            &b"5 0 obj << /Length 99 >>\nstream\n"[..],
            content,
            b"\nendstream\nendobj\n",
        ]
        .concat();
        let info = inspect(&pdf(&[PAGES2, &stream])).unwrap();
        assert_eq!(info.pages, Some(2));
        assert_eq!(info.text, "Hello, (PDF) world\nSecond line\nthird");
        assert!(!info.encrypted && !info.limited);
    }

    #[test]
    fn reads_flate_streams() {
        let z = [
            0x78, 0xda, 0x73, 0x0a, 0x51, 0xd0, 0x77, 0x33, 0x54, 0x30, 0x34, 0x52, 0x08, 0x49,
            0x53, 0x30, 0x37, 0x52, 0x30, 0x37, 0x30, 0x50, 0x08, 0x49, 0x51, 0xd0, 0x70, 0xce,
            0xcf, 0x2d, 0x28, 0x4a, 0x2d, 0x2e, 0x4e, 0x4d, 0x51, 0xc8, 0x48, 0xcd, 0xc9, 0xc9,
            0xd7, 0x54, 0x08, 0xc9, 0x52, 0x30, 0x50, 0xd0, 0x35, 0x34, 0x01, 0x49, 0x47, 0x6b,
            0x84, 0xe7, 0x17, 0x69, 0x2a, 0xe8, 0x1a, 0x03, 0x55, 0x6b, 0xe4, 0xa4, 0x68, 0xc6,
            0x2a, 0x84, 0x78, 0x29, 0xb8, 0x86, 0x00, 0x00, 0x0f, 0x62, 0x14, 0x3d,
        ];
        let mut stream = b"5 0 obj << /Filter /FlateDecode /Length 1 >>\nstream\n".to_vec();
        stream.extend_from_slice(&z);
        stream.extend_from_slice(b"\nendstream\nendobj\n");
        let info = inspect(&pdf(&[PAGES2, &stream])).unwrap();
        assert_eq!(info.text, "Compressed hello\nWor ld");
    }

    #[test]
    fn escapes_and_odd_operators() {
        let content =
            b"BT (a\\101\\n b) Tj (it's) ' ET BT <48656c6c6f> Tj ET % a comment (not text) Tj\n";
        let stream = [&b"5 0 obj << >>\nstream\n"[..], content, b"\nendstream\n"].concat();
        let info = inspect(&pdf(&[&stream])).unwrap();
        assert!(info.text.starts_with("aA\n b"), "{:?}", info.text);
        assert!(info.text.contains("it's"));
        assert!(info.text.contains("Hello"));
        assert!(!info.text.contains("not text"));
    }

    #[test]
    fn glyph_id_fonts_are_flagged_not_shown() {
        let content = b"BT <00480065006C006C006F> Tj ET";
        let stream = [&b"5 0 obj << >>\nstream\n"[..], content, b"\nendstream\n"].concat();
        let info = inspect(&pdf(&[&stream])).unwrap();
        assert!(info.limited);
        assert_eq!(info.text, "");
    }

    #[test]
    fn encrypted_files_are_reported() {
        let info = inspect(&pdf(&[PAGES2, b"6 0 obj << /Encrypt 7 0 R >>\n"])).unwrap();
        assert!(info.encrypted && info.limited);
        assert_eq!(info.pages, Some(2));
        assert_eq!(info.text, "");
    }

    #[test]
    fn images_and_unknown_filters_are_skipped_safely() {
        let img = [
            &b"6 0 obj << /Subtype /Image /Filter /DCTDecode >>\nstream\n"[..],
            &[0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3],
            b"\nendstream\n",
        ]
        .concat();
        let other =
            [&b"7 0 obj << /Filter /LZWDecode >>\nstream\nBT (x) Tj ET\nendstream\n"[..]].concat();
        let info = inspect(&pdf(&[PAGES2, &img, &other])).unwrap();
        assert_eq!(info.text, "");
        assert!(info.limited);
    }

    #[test]
    fn not_a_pdf_and_no_panics_on_garbage() {
        assert!(matches!(inspect(b"hello"), Err(ImgError::Unrecognised(_))));
        let good = pdf(&[PAGES2, b"5 0 obj << >>\nstream\nBT (x) Tj ET\nendstream\n"]);
        for cut in 0..good.len() {
            let _ = inspect(&good[..cut]);
        }
        for i in 0..good.len() {
            let mut b = good.clone();
            b[i] ^= 0xFF;
            let _ = inspect(&b);
        }
        // A flate stream that lies about itself.
        let bad =
            pdf(&[b"5 0 obj << /Filter /FlateDecode >>\nstream\n\x78\x9c\xff\xff\nendstream\n"]);
        assert!(inspect(&bad).unwrap().limited);
        // decode() says what a PDF is.
        assert!(decode(&good).unwrap_err().message().contains("PDF"));
    }
}

// ---- rotation and PNG output ------------------------------------------------------------------------

#[test]
fn rotation_turns_a_quarter_and_four_make_the_original() {
    let a = Image {
        w: 3,
        h: 2,
        px: vec![1, 2, 3, 4, 5, 6],
    };
    // Clockwise: the left column (1, 4) becomes the top row, read upwards.
    let cw = a.rotated(true);
    assert_eq!((cw.w, cw.h), (2, 3));
    assert_eq!(cw.px, vec![4, 1, 5, 2, 6, 3]);
    let ccw = a.rotated(false);
    assert_eq!(ccw.px, vec![3, 6, 2, 5, 1, 4]);
    assert_eq!(cw.rotated(false), a);
    assert_eq!(a.rotated(true).rotated(true).rotated(true).rotated(true), a);
}

#[test]
fn saved_pngs_decode_back_to_the_same_picture() {
    let mut px = vec![0u32; 50 * 40];
    for (i, p) in px.iter_mut().enumerate() {
        *p = 0xFF00_0000 | ((i as u32).wrapping_mul(2654435761) >> 8 & 0xFF_FFFF);
    }
    let a = Image { w: 50, h: 40, px };
    assert_eq!(decode(&a.to_png()).unwrap(), a);
    // Transparent pixels are written over white.
    let t = Image {
        w: 2,
        h: 1,
        px: vec![0x00123456, 0xFF010203],
    };
    assert_eq!(
        decode(&t.to_png()).unwrap().px,
        vec![0xFFFF_FFFF, 0xFF010203]
    );
}

// ---- the resumable decoder ------------------------------------------------------------------------

/// Raw DEFLATE made with zlib (python): a dynamic-Huffman block, a stored block (random bytes) and a
/// fixed-Huffman block, each ended by a sync flush, then a final block. 5510 bytes unpacked.
const MIXED_HEX: &[&str] = &[
    "7c966b52e3301084afe2ab39c1406a03a11ca7a8dc7ed1f448f3b5cdee1fc591e6d1f36ae9797b7bacdb747e5f96fb32",
    "7dbf2fcfe934af9fd3c7e5fa673adfbea7d36ddbaecbf43aaf1ff1ff2985b775bedf251547a7c7b62d6ba985a558c288",
    "4edb6748c741c8a6799983688088059ef4a91526e05a586433633aafcb9c1e7367c4925e43809ae1b7ab0f39ad022697",
    "998b11d6d77cb94201987b021eeb8b2c541a5e2fcbf565986a0209ab7df6ed809846a490f0c2a57690e443603426946d",
    "4302957baf4fcf96e1cba4346d5420f2d59443364cc86a22535ad30661583721f88c3140561306c6c01c276161f80e24",
    "b1a50095b2ea20152e0d55f4215c7d9f80d4c7ac800247aeda21fb2f210b1e8a93c7351086a159dae75415a3a154a13a",
    "842a84041b9148387ba27ab97fa6614e1b47381ad56693d3a45592608ec06425d5ea3954561005ebd3961038c0b056d2",
    "0c55b755769d786ae00e7c415e8a52548e8e1c371acc92f11f5244d7a613d0564f7eb46ea4d20c900bfa585ad3ec1695",
    "04b5fdf9e474611e98506d2312eb6694d2a8660822fb98354c2aeb0c2a72a3f9cf182cc4b4940fb6b6653465b07adecd",
    "3407e9489df47f60cc081a9123a146cf217c2ca4945816dc1eff6849bf667862379262c2c080c832be4e9276c1f56b1a",
    "d0fc26c1584090e68bf48b848ac8c67dc856f31f72086e01c2e6a153d23e5836b38a0827289d5f1cb8baf90ab226eaff",
    "6c53d8597b3e25d87dde0e2564f701b0b24d2a4a4bc1419f011fbe31df20127b49f0458761fab56ae8c151e372c151c5",
    "2c8fa932f3a6e973885b66272c333b2a1a7c6f64cac708eab2e7c43de7d42615c0763c3372460f8d09b397441b43b0d8",
    "fe918c5b0151efdf7c7f010000ffff0090016ffe669f2bf20894ea27e689c66b6b262e4886b8438f39ba76fef8c90c51",
    "01fbe6cf9a48d5b0c0a13da900a6adcb3d64069481be21c9c727b8db8c188f341a924c7f88dfa161bfdb0ecc682919d2",
    "e64692f8194157f1d4af90988285cf7a9af7c93d5552266afe70e7aae6da47627c2e59af2ea37abc84670ad3c4d36bc0",
    "8aad1fff8eb8406e2f8a7fc4cce4dd9f0b4110d9f2fa0025c8efe57f37724f4d37ea2b14004077139b4180df39322499",
    "62c6857200059aeb8ea17cf3787e0ed29d1c0b63ffd7298374d9bd74fc11add7b9ca6503952269fd669f6376ee718797",
    "37fd5f72f8d51c4ac91b6d0c48d41a1e5ec9e6a0392854a8615eef109fc1bfa9e2563701288f29b3d73f6ac2b69edd2c",
    "19f264bee462a5baf20fd27ecf14c011ed201f836320adb98bab1686a28d9801210c7736f3eec580dcfc43fe5d049b4d",
    "78a7a3ebb92865c8517ed02111f6a652da3524872b6a31d7ffe4587744d5eb783e96968f89be828565e07e5f7d784e90",
    "60a721ca807d7633ed123402f376e5bf1496773d19616326be5be5850336b36f13bcae48000000ffff53484a2cca5300",
    "1395f9e9a545250ac919a9a9c5a90ac945a989b9704e69518a42526949496a91424162668e42727eb94279466a254406",
    "aa0a62547e49494e2a4c315406ac07ac3e2db12817c2825a076643554385600e01da01d608e5e766e664c3d860632076",
    "8308b032b03cb2d16045e94589c5c550df4074018d05e1b4ccd41cb8a76082505f8349b07910552861822204f52ec417",
    "50af230503440724a4201ac06e407619dcfd48dec6700024d02126404c435505b613e12ba837807c8825689102d70b73",
    "254800e40e8c2086d805560b752ec40d10a78154805d866c3072508195c1420aaa066231d8bd8848418d0c88eb410e02",
    "4b4214239b0b73225817c412847d0802ec364480438d47a41d987de07404743b4a0206b121d10ab61ee11aa418871805",
    "b61a391c5093334c10a20fac0ea218c95bc8c91612a1d098410a6eb80b2189012365a12ac29230910213294651b23552",
    "ac21656cd422019e5050e20239d5a07a1c492d6ac020bb1bb934802747a40402f1134af022251cb8d731933552f842fc",
    "8b2826901c86942a91d232440372aca3e67d7864408421ca2109032c00f514246891c3139162a07180941490a31f39cd",
    "2082016c03221d41ec822445e43206218c5a0820fb04c53290d9502f230a1d8427d1430791cf90c303a914434e7b88fc",
    "8328af102c8c28468a7e446a84063eac5a40646c78a287f310a1839c90e0fa514a67e40c80520e214b200a0148c98194",
    "bbe0c915b95043842e520c634f5fd0d083051c222522790229bf6296c3483912e159788d805230217c814882488518c2",
    "8368010e525302528c4a0000",
];

fn unhex(parts: &[&str]) -> Vec<u8> {
    let s: String = parts.concat();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// Decode `stream` in pieces cut by `cuts` (sizes, cycled), pumping with `budget`.
fn inflate_in_pieces(stream: &[u8], cuts: &[usize], budget: usize) -> Result<Vec<u8>, InflateError> {
    use crate::inflate::{Inflater, Pump};
    let mut inf = Inflater::new(1 << 24);
    let mut out = Vec::new();
    let mut at = 0;
    let mut k = 0;
    loop {
        let n = cuts[k % cuts.len()].max(1).min(stream.len() - at);
        k += 1;
        inf.feed(&stream[at..at + n]);
        at += n;
        loop {
            match inf.pump(&mut out, budget)? {
                Pump::More => continue,
                Pump::NeedInput => break,
                Pump::Done => return Ok(out),
            }
        }
        if at >= stream.len() {
            return Err(InflateError::Truncated);
        }
    }
}

#[test]
fn the_resumable_inflater_matches_the_whole_one_for_any_cut() {
    let stream = unhex(MIXED_HEX);
    let (whole, used) = inflate(&stream, 1 << 20).unwrap();
    assert_eq!(whole.len(), 5510);
    assert_eq!(used, stream.len());
    for cuts in [&[1usize][..], &[2], &[3], &[7, 1, 13], &[64], &[500, 3], &[1 << 20]] {
        for budget in [1usize, 100, 4096, usize::MAX] {
            assert_eq!(
                inflate_in_pieces(&stream, cuts, budget).unwrap(),
                whole,
                "cuts {cuts:?} budget {budget}"
            );
        }
    }
}

#[test]
fn the_resumable_inflater_waits_for_the_rest_and_hands_back_what_follows() {
    use crate::inflate::{Inflater, Pump};
    let mut stream = unhex(MIXED_HEX);
    let n = stream.len();
    stream.extend_from_slice(b"TRAILER");
    let mut inf = Inflater::new(1 << 20);
    let mut out = Vec::new();
    inf.feed(&stream[..n / 2]);
    assert_eq!(inf.pump(&mut out, usize::MAX), Ok(Pump::NeedInput));
    assert!(!out.is_empty() && out.len() < 5510, "some of it came already: {}", out.len());
    inf.feed(&stream[n / 2..]);
    assert_eq!(inf.pump(&mut out, usize::MAX), Ok(Pump::Done));
    assert_eq!(out.len(), 5510);
    assert_eq!(inf.rest(), b"TRAILER");
    assert_eq!(inf.total_out(), 5510);
    // Done stays done.
    assert_eq!(inf.pump(&mut out, usize::MAX), Ok(Pump::Done));
}

#[test]
fn the_resumable_inflater_keeps_a_window_for_far_back_references() {
    use crate::inflate::{Inflater, Pump};
    // A stored block of 20000 bytes, then a fixed-Huffman block that copies 258 bytes from
    // 20000 back: far past what a small buffer would keep, and over a chunk boundary.
    let data: Vec<u8> = (0..20000u32).map(|i| (i * 7 + i / 13) as u8).collect();
    let mut s = vec![0u8]; // BFINAL=0, BTYPE=00, padded
    s.extend_from_slice(&(20000u16).to_le_bytes());
    s.extend_from_slice(&(!20000u16).to_le_bytes());
    s.extend_from_slice(&data);
    // The second block, bit by bit.
    let mut bits: Vec<u8> = Vec::new();
    for (v, n) in [(1u32, 1u32), (1, 2)] {
        // BFINAL = 1, BTYPE = 01 (LSB first)
        (0..n).for_each(|i| bits.push(((v >> i) & 1) as u8));
    }
    let code = |v: u32, n: u32, bits: &mut Vec<u8>| (0..n).rev().for_each(|i| bits.push(((v >> i) & 1) as u8)); // MSB first
    // length 258 = symbol 285: 8-bit code 0xC0 + (285 - 280)
    code(0xC0 + 5, 8, &mut bits);
    // distance 20000: symbol 28 (base 16385, 13 extra bits = 3615), 5-bit code
    code(28, 5, &mut bits);
    (0..13).for_each(|i| bits.push(((3615u32 >> i) & 1) as u8));
    // end of block: symbol 256, 7-bit code 0
    code(0, 7, &mut bits);
    for chunk in bits.chunks(8) {
        let mut b = 0u8;
        for (i, &bit) in chunk.iter().enumerate() {
            b |= bit << i;
        }
        s.push(b);
    }
    let whole = inflate(&s, 1 << 20).unwrap().0;
    assert_eq!(whole.len(), 20258);
    assert_eq!(&whole[20000..], &data[..258]);
    for cut in [1usize, 5, 4096, 19999, 20005] {
        let mut inf = Inflater::new(1 << 20);
        let mut out = Vec::new();
        for piece in s.chunks(cut) {
            inf.feed(piece);
            while inf.pump(&mut out, 1000).unwrap() == Pump::More {}
        }
        assert_eq!(out, whole, "cut {cut}");
    }
}

#[test]
fn the_resumable_inflater_is_bounded_and_survives_garbage() {
    use crate::inflate::{Inflater, Pump};
    let stream = unhex(MIXED_HEX);
    // The output cap.
    let mut inf = Inflater::new(1000);
    inf.feed(&stream);
    let mut out = Vec::new();
    assert_eq!(inf.pump(&mut out, usize::MAX), Err(InflateError::TooBig));
    assert!(out.len() <= 1000);
    // Bad block type, stored length check, junk.
    for junk in [&[0x07u8][..], &[0x01, 0x05, 0x00, 0x00, 0x00], &[0xff; 64], &[]] {
        let mut inf = Inflater::new(1 << 20);
        inf.feed(junk);
        let mut out = Vec::new();
        let r = inf.pump(&mut out, usize::MAX);
        assert!(matches!(r, Err(_) | Ok(Pump::NeedInput)), "{r:?}");
    }
    // Random damage and random cuts: never a panic, and when it works it agrees with the
    // whole-buffer decoder.
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut rnd = |n: u64| {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s % n
    };
    for _ in 0..400 {
        let mut st = stream.clone();
        for _ in 0..1 + rnd(4) {
            let i = rnd(st.len() as u64) as usize;
            st[i] ^= 1 << rnd(8);
        }
        if rnd(3) == 0 {
            st.truncate(rnd(st.len() as u64) as usize);
        }
        let cuts = [1 + rnd(40) as usize, 1 + rnd(300) as usize];
        let got = inflate_in_pieces(&st, &cuts, 1 + rnd(5000) as usize);
        if let Ok(g) = got
            && let Ok((w, _)) = inflate(&st, 1 << 20)
        {
            assert_eq!(g, w);
        }
    }
}

#[test]
fn checksums_continue() {
    use crate::inflate::{adler32, adler32_update, crc32, crc32_update};
    let data: Vec<u8> = (0..10000u32).map(|i| (i * 31 + 5) as u8).collect();
    let mut c = 0;
    let mut a = 1;
    for p in data.chunks(777) {
        c = crc32_update(c, p);
        a = adler32_update(a, p);
    }
    assert_eq!(c, crc32(&[&data]));
    assert_eq!(a, adler32(&data));
    assert_eq!(crc32_update(0, b""), 0);
    assert_eq!(adler32_update(1, b""), 1);
}
