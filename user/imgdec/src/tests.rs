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

#[allow(dead_code)]
#[path = "../../../kernel/src/wm/png.rs"]
mod kpng;

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
    assert_eq!(sniff(b"hello"), Format::Unknown);
    assert_eq!(sniff(b"P7\n"), Format::Unknown);
    assert_eq!(sniff(b""), Format::Unknown);
    for f in [
        &b"%PDF-1.4"[..],
        &[0xFF, 0xD8, 0xFF, 0xE0],
        b"GIF89a",
        b"plain text",
        b"",
    ] {
        let e = decode(f).unwrap_err();
        assert!(matches!(e, ImgError::Unrecognised(_)));
        assert!(!e.message().is_empty());
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
