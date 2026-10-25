//! Host tests of the 3.5-inch display and its touch chip: the driver runs against `lcdsim`'s model
//! of the panel and the touch chip, byte for byte.

#[path = "../../kernel/src/font_data.rs"]
#[allow(dead_code)]
mod font_data;

use std::string::String;
use std::vec::Vec;

use crate::cmdline;
use crate::lcd::{
    self, Bus, Config, Controller, Dirty, PixelSource, Pixels, Rect, Rotation, Solid, View,
    ViewSource, rgb565, rgb565_to_xrgb,
};
use crate::lcdsim::{SimPanel, SimTouch};
use crate::testpng;
use crate::touch::{self, Cal, Channel, Event, Raw, Tracker};

const KINDS: [Controller; 10] = lcd::CONTROLLERS;
const ROTS: [Rotation; 4] = [Rotation::Deg0, Rotation::Deg90, Rotation::Deg180, Rotation::Deg270];

fn gradient(w: usize, h: usize) -> Vec<u32> {
    let mut v = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let r = x * 255 / (w - 1);
            let g = y * 255 / (h - 1);
            let b = (x * 7 + y * 3) % 256;
            v.push((r as u32) << 16 | (g as u32) << 8 | b as u32);
        }
    }
    v
}

/// What the controller makes of a colour the driver sent.
fn quantize(kind: Controller, c: u32) -> u32 {
    if kind.bytes_per_pixel() == 3 {
        let f = |v: u32| (v & 0xfc) | (v >> 6);
        f(c >> 16 & 0xff) << 16 | f(c >> 8 & 0xff) << 8 | f(c & 0xff)
    } else {
        rgb565_to_xrgb(rgb565(c))
    }
}

fn fnv(px: &[u32]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for p in px {
        for b in p.to_le_bytes() {
            h = (h ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    h
}

fn up(cfg: &Config) -> SimPanel {
    let mut p = SimPanel::new(cfg.controller);
    assert!(lcd::init(cfg, &mut p), "init");
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    p
}

fn full(cfg: &Config) -> Rect {
    Rect::new(0, 0, cfg.width(), cfg.height())
}

// ---- options, geometry ---------------------------------------------------------------------------

#[test]
fn lcd_options_parse() {
    assert_eq!(Controller::parse(b"ili9486"), Some(Controller::Ili9486));
    assert_eq!(Controller::parse(b"ILI9488"), Some(Controller::Ili9488));
    assert_eq!(Controller::parse(b"st7796"), Some(Controller::St7796));
    assert_eq!(Controller::parse(b"st7789"), Some(Controller::St7789));
    assert_eq!(Controller::parse(b"st7789x"), None);
    assert_eq!(Controller::parse(b"ili948"), None);
    assert_eq!(Controller::parse(b""), None);
    // Every controller's own name parses back to it, in any case; the aliases name the same family.
    for k in KINDS {
        assert_eq!(Controller::parse(k.name().as_bytes()), Some(k));
        assert_eq!(Controller::parse(k.name().to_ascii_uppercase().as_bytes()), Some(k));
    }
    assert_eq!(Controller::parse(b"ILI9343"), Some(Controller::Ili9341));
    assert_eq!(Controller::parse(b"hx8357d"), Some(Controller::Hx8357));
    assert_eq!(Controller::parse(b"mpi3501"), Some(Controller::Ili9486P));
    let mut names: Vec<&str> = KINDS.iter().map(|k| k.name()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), KINDS.len());
    assert_eq!(lcd::parse_spi_mode(b"0"), Some(0));
    assert_eq!(lcd::parse_spi_mode(b"3"), Some(3));
    assert_eq!(lcd::parse_spi_mode(b"4"), None);
    assert_eq!(lcd::parse_spi_mode(b"03"), None);
    assert_eq!(lcd::parse_spi_mode(b""), None);
    assert_eq!(Rotation::parse(b"90"), Some(Rotation::Deg90));
    assert_eq!(Rotation::parse(b"45"), None);
    assert_eq!(Rotation::parse(b"090"), None);
    assert_eq!(cmdline::parse_num(b"24"), Some(24));
    assert_eq!(cmdline::parse_num(b"0x36"), Some(0x36));
    assert_eq!(cmdline::parse_num(b"-1"), Some(-1));
    assert_eq!(cmdline::parse_num(b""), None);
    assert_eq!(cmdline::parse_num(b"0x"), None);
    assert_eq!(cmdline::parse_num(b"12ab"), None);
    assert_eq!(cmdline::parse_num(b"99999999999999999999999"), None);
    let line = b"console=tty1 milk.lcd=ili9486 milk.lcd_dc=24 milk.lcd_hz=0x1e84800 milk.x=oops";
    assert_eq!(cmdline::value(line, "lcd"), Some(&b"ili9486"[..]));
    assert_eq!(cmdline::number(line, "lcd_dc"), Some(24));
    assert_eq!(cmdline::number(line, "lcd_hz"), Some(32_000_000));
    assert_eq!(cmdline::number(line, "x"), None);
}

#[test]
fn lcd_config_and_madctl() {
    let c = Config::new(Controller::Ili9486);
    // The default is the kit's landscape.
    assert_eq!((c.width(), c.height()), (480, 320));
    assert!(c.bgr);
    // The four values such a kit takes: mirror/exchange bits plus BGR.
    let m = |rot, bgr| Config { rotation: rot, bgr, ..c }.madctl();
    assert_eq!(m(Rotation::Deg0, true), 0x48);
    assert_eq!(m(Rotation::Deg90, true), 0x28);
    assert_eq!(m(Rotation::Deg180, true), 0x88);
    assert_eq!(m(Rotation::Deg270, true), 0xe8);
    assert_eq!(m(Rotation::Deg90, false), 0x20);
    assert_eq!(Config { madctl: Some(0x5a), ..c }.madctl(), 0x5a);
    let p = Config { rotation: Rotation::Deg180, ..c };
    assert_eq!((p.width(), p.height()), (320, 480));
    // Inversion: the ST7796 module's usual setting is on, the others off, and an option overrides.
    assert!(!c.inverted());
    assert!(Config::new(Controller::St7796).inverted());
    assert!(!Config { invert: Some(false), ..Config::new(Controller::St7796) }.inverted());
    assert!(Config { invert: Some(true), ..c }.inverted());
}

// ---- the byte streams -----------------------------------------------------------------------------

#[test]
fn ili9486_init_stream_is_16_bit_words() {
    let cfg = Config::new(Controller::Ili9486);
    let p = up(&cfg);
    assert_eq!(p.resets, [true, false, true]);
    assert!(p.waited_ms >= 10 + 20 + 120 + 150 + 150);
    // Commands (D/C low) are one word each, 0 then the command.
    let cmds: Vec<u8> = p.log.iter().filter(|(d, _)| !d).map(|(_, b)| b[1]).collect();
    assert_eq!(cmds, [0x01, 0x11, 0x3a, 0xc2, 0xc5, 0xe0, 0xe1, 0x36, 0x20, 0x13, 0x29]);
    for (d, b) in &p.log {
        assert_eq!(b.len() % 2, 0);
        if !*d {
            assert_eq!((b.len(), b[0]), (2, 0));
        } else {
            assert!(b.chunks(2).all(|w| w[0] == 0), "parameter words have a zero high byte");
        }
    }
    // COLMOD takes 0x55 (16 bits per pixel), as one word.
    let i = p.log.iter().position(|(d, b)| !d && b[1] == 0x3a).unwrap();
    assert_eq!(p.log[i + 1], (true, std::vec![0x00, 0x55]));
    // MADCTL: landscape + BGR.
    let i = p.log.iter().position(|(d, b)| !d && b[1] == 0x36).unwrap();
    assert_eq!(p.log[i + 1], (true, std::vec![0x00, 0x28]));
    assert!(p.display_on && !p.sleeping && !p.inverted);
    assert_eq!((p.colmod, p.madctl), (0x55, 0x28));
}

#[test]
fn eight_bit_controllers_init() {
    for kind in KINDS.into_iter().filter(|k| !k.word16()) {
        let cfg = Config::new(kind);
        let p = up(&cfg);
        // 18-bit pixels (COLMOD 0x66) for the controllers that need them over SPI, 16-bit otherwise.
        assert_eq!(p.colmod, if kind.bytes_per_pixel() == 3 { 0x66 } else { 0x55 }, "{kind:?}");
        assert_eq!(p.colmod, kind.colmod());
        assert!(p.display_on && !p.sleeping, "{kind:?}");
        // 8-bit framing: a command is one byte, parameters go out as they are.
        let i = p.log.iter().position(|(d, b)| !d && b == &[0x36]).unwrap();
        assert_eq!(p.log[i + 1], (true, std::vec![0x28]));
        assert_eq!(p.inverted, kind.usually_inverted(), "{kind:?}");
        assert_eq!(p.inverted, matches!(kind, Controller::St7796 | Controller::St7789));
        // Every plain controller starts with a software reset after the hardware one.
        assert_eq!(p.log[0], (false, std::vec![0x01]), "{kind:?}");
        assert_eq!(p.resets, [true, false, true]);
    }
    // The ST7796 unlocks its second command set before using it, and locks it again.
    let p = up(&Config::new(Controller::St7796));
    let f0: Vec<&Vec<u8>> = p.log.iter().filter(|(d, b)| *d && b.len() == 1 && matches!(b[0], 0xc3 | 0x96 | 0x3c | 0x69)).map(|(_, b)| b).collect();
    assert_eq!(f0, [&std::vec![0xc3], &std::vec![0x96], &std::vec![0x3c], &std::vec![0x69]]);
}

#[test]
fn every_init_stream_is_its_own_and_complete() {
    // Every controller's stream differs from every other's (a copy-paste of one stream under two names
    // would make two probe set-ups the same set-up), is bounded, and ends with the common tail.
    let mut streams: Vec<Vec<(bool, Vec<u8>)>> = Vec::new();
    for kind in KINDS {
        let p = up(&Config::new(kind));
        assert!(p.log.len() > 8 && p.log.len() < 120, "{kind:?}: {}", p.log.len());
        assert!(p.waited_ms >= 150 && p.waited_ms < 1200, "{kind:?}: {} ms", p.waited_ms);
        let cmd = |b: &[u8]| *b.last().unwrap();
        let cmds: Vec<u8> = p.log.iter().filter(|(d, _)| !d).map(|(_, b)| cmd(b)).collect();
        for needed in [0x01u8, 0x11, 0x36, 0x13, 0x29] {
            assert!(cmds.contains(&needed), "{kind:?} lacks command {needed:#x}");
        }
        // The display goes on last, after sleep out.
        assert_eq!(*cmds.last().unwrap(), 0x29);
        assert!(cmds.iter().position(|&c| c == 0x11) < cmds.iter().position(|&c| c == 0x29));
        for other in &streams {
            assert!(other != &p.log, "{kind:?} has the stream of another controller");
        }
        streams.push(p.log);
    }
    // The three ILI9486 wirings share a chip, so a common core and nothing else in common.
    let a = up(&Config::new(Controller::Ili9486P));
    let b = up(&Config::new(Controller::Ili9486P18));
    let colmod = |p: &SimPanel| p.log.iter().position(|(d, x)| !d && x == &[0x3a]).map(|i| p.log[i + 1].1.clone());
    assert_eq!(colmod(&a), Some(std::vec![0x55]));
    assert_eq!(colmod(&b), Some(std::vec![0x66]));
}

#[test]
fn small_memory_controllers_draw_320_by_240() {
    for kind in [Controller::Ili9341, Controller::St7789] {
        assert_eq!(kind.native(), (240, 320));
        let cfg = Config::new(kind);
        assert_eq!((cfg.width(), cfg.height()), (320, 240), "{kind:?}");
        let p = Config { rotation: Rotation::Deg0, ..cfg };
        assert_eq!((p.width(), p.height()), (240, 320));
        let mut sim = up(&cfg);
        // A window that only a 480x320 memory has is refused by the driver before anything is sent.
        assert!(!lcd::window(&cfg, &mut sim, Rect::new(0, 0, 480, 320)));
        assert!(!lcd::window(&cfg, &mut sim, Rect::new(0, 0, 321, 1)));
        assert!(lcd::window(&cfg, &mut sim, Rect::new(0, 0, 320, 240)));
        assert!(sim.errors.is_empty(), "{:?}", sim.errors);
    }
    // The model refuses a window past the memory (a driver that forgot the smaller size).
    let mut sim = SimPanel::new(Controller::Ili9341);
    sim.write(false, &[0x2a]);
    sim.write(true, &[0, 0, 0x01, 0xdf]); // column 479
    sim.write(false, &[0x29]);
    assert!(sim.errors.contains(&"window outside the memory"), "{:?}", sim.errors);
    for kind in KINDS.into_iter().filter(|k| k.native() == (320, 480)) {
        assert_eq!((Config::new(kind).width(), Config::new(kind).height()), (480, 320));
    }
}

#[test]
fn window_bytes() {
    let cfg = Config::new(Controller::Ili9486);
    let mut p = SimPanel::new(cfg.controller);
    assert!(lcd::window(&cfg, &mut p, Rect::new(0, 0, 480, 320)));
    let want: [(bool, &[u8]); 5] = [
        (false, &[0, 0x2a]),
        (true, &[0, 0, 0, 0, 0, 1, 0, 0xdf]),
        (false, &[0, 0x2b]),
        (true, &[0, 0, 0, 0, 0, 1, 0, 0x3f]),
        (false, &[0, 0x2c]),
    ];
    assert_eq!(p.log.len(), 5);
    for (got, w) in p.log.iter().zip(want) {
        assert_eq!((got.0, &got.1[..]), w);
    }
    let cfg = Config::new(Controller::St7796);
    let mut p = SimPanel::new(cfg.controller);
    assert!(lcd::window(&cfg, &mut p, Rect::new(10, 20, 30, 40)));
    let want: [(bool, &[u8]); 5] = [
        (false, &[0x2a]),
        (true, &[0, 10, 0, 39]),
        (false, &[0x2b]),
        (true, &[0, 20, 0, 59]),
        (false, &[0x2c]),
    ];
    for (got, w) in p.log.iter().zip(want) {
        assert_eq!((got.0, &got.1[..]), w);
    }
    // Outside the screen, or empty: refused before anything is sent.
    let mut p = SimPanel::new(cfg.controller);
    for r in [Rect::new(0, 0, 481, 1), Rect::new(0, 0, 1, 321), Rect::new(0, 0, 0, 5), Rect::new(479, 319, 2, 1)] {
        assert!(!lcd::window(&cfg, &mut p, r), "{r:?}");
    }
    assert!(p.log.is_empty());
}

#[test]
fn pixel_packing() {
    assert_eq!(rgb565(0x00ff_ffff), 0xffff);
    assert_eq!(rgb565(0x00ff_0000), 0xf800);
    assert_eq!(rgb565(0x0000_ff00), 0x07e0);
    assert_eq!(rgb565(0x0000_00ff), 0x001f);
    assert_eq!(rgb565(0), 0);
    assert_eq!(rgb565_to_xrgb(0xffff), 0x00ff_ffff);
    assert_eq!(rgb565_to_xrgb(0xf800), 0x00ff_0000);
    // Every 565 value survives the trip.
    for v in (0..=0xffffu32).step_by(7) {
        assert_eq!(rgb565(rgb565_to_xrgb(v as u16)), v as u16);
    }
    let mut out = [0u8; 12];
    let n = lcd::pack(Controller::Ili9486, &[0x00ff_0000, 0x0000_00ff, 0x0010_2030], &mut out);
    assert_eq!(&out[..n], [0xf8, 0x00, 0x00, 0x1f, 0x11, 0x06]);
    let n = lcd::pack(Controller::Ili9488, &[0x00ff_0000, 0x0012_3456], &mut out);
    assert_eq!(&out[..n], [0xfc, 0x00, 0x00, 0x10, 0x34, 0x54]);
    // 18-bit pixels: red, green, blue each in the top six bits of its own byte, three bytes a pixel;
    // the plain ILI9486 with 18-bit pixels packs the same way; every other controller takes RGB565.
    let n = lcd::pack(Controller::Ili9486P18, &[0x00ff_ffff, 0x0000_0003, 0x00fc_0000, 0x0001_0203], &mut out);
    assert_eq!(n, 12);
    assert_eq!(&out[..n], [0xfc, 0xfc, 0xfc, 0x00, 0x00, 0x00, 0xfc, 0x00, 0x00, 0x00, 0x00, 0x00]);
    for k in KINDS {
        let n = lcd::pack(k, &[0x00ff_0000, 0x0000_00ff], &mut out);
        assert_eq!(n, 2 * k.bytes_per_pixel(), "{k:?}");
        if k.bytes_per_pixel() == 2 {
            assert_eq!(&out[..n], [0xf8, 0x00, 0x00, 0x1f], "{k:?}");
        } else {
            assert_eq!(&out[..n], [0xfc, 0x00, 0x00, 0x00, 0x00, 0xfc], "{k:?}");
        }
        assert_eq!(k.bytes_per_pixel() == 3, matches!(k, Controller::Ili9488 | Controller::Ili9486P18));
    }
    // Packing never writes past what it reports, and an empty or short output is not a panic.
    assert_eq!(lcd::pack(Controller::Ili9488, &[], &mut out), 0);
    let mut small = [0xaau8; 4];
    let n = lcd::pack(Controller::Ili9486, &[0xffffff, 0xffffff, 0xffffff], &mut small);
    assert_eq!(n, 4, "only the whole pixels that fit");
    assert_eq!(small, [0xff; 4]);
    let mut small = [0xaau8; 5];
    assert_eq!(lcd::pack(Controller::Ili9488, &[0xffffff, 0xffffff], &mut small), 3);
    assert_eq!(small, [0xfc, 0xfc, 0xfc, 0xaa, 0xaa]);
}

// ---- drawing --------------------------------------------------------------------------------------

#[test]
fn every_controller_and_rotation_shows_the_picture_upright() {
    for kind in KINDS {
        for rot in ROTS {
            let cfg = Config { rotation: rot, ..Config::new(kind) };
            let mut p = up(&cfg);
            let (w, h) = (cfg.width(), cfg.height());
            let img = gradient(w, h);
            assert!(lcd::draw_rect(&cfg, &mut p, full(&cfg), &mut Pixels { px: &img, width: w }));
            assert!(p.errors.is_empty(), "{kind:?} {rot:?}: {:?}", p.errors);
            assert_eq!(p.pixels_written, w * h);
            let seen = p.visible_for(&cfg);
            let want: Vec<u32> = img.iter().map(|&c| quantize(kind, c)).collect();
            assert!(seen == want, "{kind:?} {rot:?}: the picture is not upright");
        }
    }
}

#[test]
fn the_four_rotations_are_true_rotations() {
    // The same corner marks, drawn in each rotation, end up at the corners a viewer expects, and
    // text-like asymmetry (a long bar on the top edge) stays on top.
    for rot in ROTS {
        let cfg = Config { rotation: rot, ..Config::new(Controller::Ili9486) };
        let mut p = up(&cfg);
        let (w, h) = (cfg.width(), cfg.height());
        let mut img = std::vec![0u32; w * h];
        img[..w / 2].fill(0xff0000); // red bar along the top left half
        img[(h - 1) * w + w - 1] = 0x00ff00; // green dot, bottom right
        lcd::draw_rect(&cfg, &mut p, full(&cfg), &mut Pixels { px: &img, width: w });
        let (vw, vh, seen) = p.visible(rot);
        assert_eq!((vw, vh), (w, h));
        assert_eq!(seen[0], 0xff0000, "{rot:?}");
        assert_eq!(seen[w / 2 - 1], 0xff0000);
        assert_eq!(seen[w / 2], 0);
        assert_eq!(seen[(h - 1) * w + w - 1], 0x00ff00, "{rot:?}");
        assert_eq!(seen.iter().filter(|&&c| c != 0).count(), w / 2 + 1);
    }
}

#[test]
fn wrong_colour_order_swaps_red_and_blue() {
    let cfg = Config { bgr: false, ..Config::new(Controller::Ili9486) };
    let mut p = up(&cfg);
    lcd::draw_rect(&cfg, &mut p, Rect::new(0, 0, 4, 1), &mut Solid(0x00ff_0000));
    // The kit's glass is BGR: without the bit a red pixel shows blue.
    assert_eq!(p.visible_for(&cfg)[0], 0x0000_f800 >> 11 << 3 | 0x7);
}

#[test]
fn partial_updates_send_only_the_rectangle() {
    let cfg = Config::new(Controller::Ili9486);
    let mut p = up(&cfg);
    let img = gradient(480, 320);
    lcd::draw_rect(&cfg, &mut p, full(&cfg), &mut Pixels { px: &img, width: 480 });
    p.log.clear();
    let r = Rect::new(100, 50, 40, 30);
    lcd::draw_rect(&cfg, &mut p, r, &mut Solid(0x00123456));
    // Window (CASET, PASET, RAMWR) and then exactly the rectangle's pixels, two bytes each.
    let data: usize = p.log.iter().skip(5).map(|(d, b)| { assert!(*d); b.len() }).sum();
    assert_eq!(data, 40 * 30 * 2);
    assert_eq!(p.log[1], (true, std::vec![0, 0, 0, 100, 0, 0, 0, 139]));
    let seen = p.visible_for(&cfg);
    let q = rgb565_to_xrgb(rgb565(0x123456));
    for y in 0..320 {
        for x in 0..480 {
            let inside = (100..140).contains(&x) && (50..80).contains(&y);
            let want = if inside { q } else { quantize(Controller::Ili9486, img[y * 480 + x]) };
            assert_eq!(seen[y * 480 + x], want, "({x},{y})");
        }
    }
    // A rectangle that sticks out is clipped to the screen; one wholly outside sends nothing.
    p.log.clear();
    assert!(lcd::draw_rect(&cfg, &mut p, Rect::new(470, 310, 50, 50), &mut Solid(0xffffff)));
    let data: usize = p.log.iter().skip(5).map(|(_, b)| b.len()).sum();
    assert_eq!(data, 10 * 10 * 2);
    p.log.clear();
    assert!(lcd::draw_rect(&cfg, &mut p, Rect::new(500, 0, 10, 10), &mut Solid(0)));
    assert!(p.log.is_empty());
}

#[test]
fn chunks_stay_small_and_whole_rows() {
    // No single write is larger than the stack buffer, whatever the controller.
    for kind in KINDS {
        let cfg = Config::new(kind);
        let mut p = up(&cfg);
        p.log.clear();
        lcd::draw_rect(&cfg, &mut p, full(&cfg), &mut Solid(0x00aa_bbcc));
        let bpp = kind.bytes_per_pixel();
        let mut total = 0;
        for (d, b) in p.log.iter().skip(5) {
            assert!(*d && b.len() <= 2048 && b.len() % (cfg.width() * bpp) == 0, "{kind:?} {}", b.len());
            total += b.len();
        }
        assert_eq!(total, cfg.width() * cfg.height() * bpp);
    }
}

#[test]
fn a_failing_bus_stops_the_driver_without_a_panic() {
    let cfg = Config::new(Controller::Ili9486);
    // Fail at every point of the bring-up in turn.
    let n = up(&cfg).log.len();
    for k in 0..n {
        let mut p = SimPanel::new(cfg.controller);
        p.fail_at = Some(k);
        assert!(!lcd::init(&cfg, &mut p), "failure at {k}");
    }
    // And during a picture.
    let mut p = up(&cfg);
    let at = p.log.len() + 6;
    p.fail_at = Some(at);
    assert!(!lcd::draw_rect(&cfg, &mut p, full(&cfg), &mut Solid(1)));
    // A bus that never works.
    struct Dead;
    impl Bus for Dead {
        fn write(&mut self, _: bool, _: &[u8]) -> bool {
            false
        }
        fn reset_pin(&mut self, _: bool) {}
        fn delay_ms(&mut self, _: u32) {}
    }
    assert!(!lcd::init(&cfg, &mut Dead));
    assert!(!lcd::draw_rect(&cfg, &mut Dead, full(&cfg), &mut Solid(1)));
}

#[test]
fn the_model_notices_a_wrong_stream() {
    // Not a driver test: proof that the model would have caught a driver that frames wrongly.
    let mut p = SimPanel::new(Controller::Ili9486);
    p.write(false, &[0x11]); // an 8-bit command to a 16-bit kit
    p.write(false, &[0, 0x3a]);
    p.write(true, &[0x55]); // an 8-bit parameter
    p.write(true, &[0x12, 0x55]); // nonzero high byte
    p.write(false, &[0, 0x21]);
    for what in [
        "command not sent as one 16-bit word",
        "odd number of bytes in a 16-bit parameter transfer",
        "16-bit parameter word with a nonzero high byte",
    ] {
        assert!(p.errors.contains(&what), "{what}: {:?}", p.errors);
    }
    let mut p = SimPanel::new(Controller::Ili9488);
    p.write(false, &[0x3a]);
    p.write(true, &[0x55]); // 16-bit pixels over SPI on an 18-bit-only controller
    p.write(false, &[0x29]);
    assert!(p.errors.contains(&"pixel format the controller cannot take over SPI"), "{:?}", p.errors);
}

// ---- dirty rectangles -----------------------------------------------------------------------------

#[test]
fn dirty_rectangles_merge() {
    let mut d = Dirty::new();
    assert!(d.is_empty());
    d.add(Rect::new(0, 0, 0, 10)); // empty: ignored
    assert!(d.is_empty());
    d.add(Rect::new(10, 10, 20, 20));
    d.add(Rect::new(15, 15, 20, 20)); // overlaps: one rectangle
    assert_eq!(d.len(), 1);
    assert_eq!(d.pop(), Some(Rect::new(10, 10, 25, 25)));
    assert_eq!(d.pop(), None);
    // Far apart rectangles stay apart (a window command is cheaper than the pixels between)...
    d.add(Rect::new(0, 0, 40, 40));
    d.add(Rect::new(400, 280, 40, 40));
    assert_eq!(d.len(), 2);
    assert_eq!(d.area(), 3200);
    // ...stacked rows of full width merge into one band...
    let mut d = Dirty::new();
    d.add(Rect::new(0, 0, 480, 16));
    d.add(Rect::new(0, 16, 480, 16));
    assert_eq!((d.len(), d.pop()), (1, Some(Rect::new(0, 0, 480, 32))));
    // ...and a fifth rectangle is folded into the cheapest of four.
    let mut d = Dirty::new();
    for i in 0..4 {
        d.add(Rect::new(i * 110, 0, 30, 30));
    }
    assert_eq!(d.len(), 4);
    d.add(Rect::new(10, 200, 30, 30));
    assert_eq!(d.len(), 4);
    // Whatever was added is covered by what comes out.
    let mut seen = std::vec![false; 480 * 320];
    while let Some(r) = d.pop() {
        for y in r.y..r.y1() {
            for x in r.x..r.x1() {
                seen[y * 480 + x] = true;
            }
        }
    }
    for r in [Rect::new(0, 0, 30, 30), Rect::new(330, 0, 30, 30), Rect::new(10, 200, 30, 30)] {
        for y in r.y..r.y1() {
            for x in r.x..r.x1() {
                assert!(seen[y * 480 + x]);
            }
        }
    }
}

#[test]
fn rect_arithmetic() {
    let a = Rect::new(10, 10, 10, 10);
    assert_eq!(a.intersect(&Rect::new(15, 15, 10, 10)), Rect::new(15, 15, 5, 5));
    assert!(a.intersect(&Rect::new(20, 10, 5, 5)).is_empty());
    assert!(a.touches(&Rect::new(20, 10, 5, 5)));
    assert!(!a.touches(&Rect::new(21, 10, 5, 5)));
    assert_eq!(a.union(&Rect::default()), a);
    assert_eq!(Rect::default().union(&a), a);
    assert_eq!(a.clip(15, 100), Rect::new(10, 10, 5, 10));
    assert!(a.clip(5, 5).is_empty());
}

// ---- showing the desktop on the small screen -----------------------------------------------------------

#[test]
fn scaled_view_geometry() {
    // 1024x768 into 480x320: height limits, 427 wide, centred.
    let v = View::scaled(1024, 768, 480, 320);
    assert_eq!(v.dest(), Rect::new(27, 0, 426, 320));
    // 1440x900 is a clean third, 480x300, with bars above and below.
    let v = View::scaled(1440, 900, 480, 320);
    assert_eq!(v.dest(), Rect::new(0, 10, 480, 300));
    // A picture the size of the panel is shown as it is; a smaller one is centred, not stretched.
    assert_eq!(View::scaled(480, 320, 480, 320).dest(), Rect::new(0, 0, 480, 320));
    assert_eq!(View::scaled(100, 100, 480, 320).dest(), Rect::new(190, 110, 100, 100));
    // Portrait panel.
    let v = View::scaled(1024, 768, 320, 480);
    assert_eq!(v.dest(), Rect::new(0, 120, 320, 240));
}

#[test]
fn scaled_view_maps_rects_and_touches() {
    let v = View::scaled(1440, 900, 480, 320);
    // The whole picture maps to the whole destination (one pixel more on each side, clipped).
    assert_eq!(v.map_rect(Rect::new(0, 0, 1440, 900)), Rect::new(0, 10, 480, 300));
    // A small change maps to a small rectangle that contains where it lands.
    let m = v.map_rect(Rect::new(300, 300, 30, 30));
    assert!(m.x <= 100 && m.x1() >= 110 && m.y <= 110 && m.y1() >= 120, "{m:?}");
    assert!(m.area() < 20 * 20);
    assert!(v.map_rect(Rect::new(5000, 0, 5, 5)).is_empty());
    // Touch: panel corners go to picture corners, and the middle to the middle.
    assert!(v.to_source(0, 0).0 <= 2 && v.to_source(0, 0).1 <= 2); // above the picture: clamped
    let (cx, cy) = v.to_source(479, 319);
    assert!((1436..=1439).contains(&cx) && (896..=899).contains(&cy), "{cx},{cy}");
    let (x, y) = v.to_source(240, 160);
    assert!((x - 720).abs() <= 3 && (y - 450).abs() <= 3, "{x},{y}");
    // Every panel pixel round trips within one box.
    for lx in (0..480).step_by(17) {
        for ly in (10..310).step_by(13) {
            let (sx, sy) = v.to_source(lx, ly);
            let back = v.map_rect(Rect::new(sx as usize, sy as usize, 1, 1));
            assert!(back.x <= lx as usize && back.x1() > lx as usize, "{lx} {sx}");
            assert!(back.y <= ly as usize && back.y1() > ly as usize, "{ly} {sy}");
        }
    }
}

#[test]
fn scaled_view_averages_boxes() {
    // A picture 3x the panel in each direction: every panel pixel is the mean of a 3x3 box.
    let v = View::scaled(1440, 960, 480, 320);
    let mut src = std::vec![0u32; 1440 * 960];
    for y in 0..960 {
        for x in 0..1440 {
            // Checker of 3x3 blocks: black and white; plus one red block.
            src[y * 1440 + x] = if ((x / 3) + (y / 3)) % 2 == 0 { 0xffffff } else { 0 };
        }
    }
    let mut row = [0u32; 480];
    v.render_row(&src, 1440, 12, 0, &mut row);
    assert_eq!(row[0], 0xffffff);
    assert_eq!(row[1], 0);
    // Misaligned boxes give grey: a 2x scale of 1-pixel checkers is mid grey.
    let v = View::scaled(960, 640, 480, 320);
    for y in 0..640 {
        for x in 0..960 {
            src[y * 960 + x] = if (x + y) % 2 == 0 { 0xffffff } else { 0 };
        }
    }
    v.render_row(&src, 960, 0, 0, &mut row);
    assert!(row.iter().all(|&p| p == 0x808080 || p == 0x7f7f7f), "{:x}", row[0]);
    // Bars outside the picture are black.
    let v = View::scaled(1440, 900, 480, 320);
    v.render_row(&src, 1440, 3, 0, &mut row);
    assert!(row.iter().all(|&p| p == 0));
    // A partial row is the same pixels as the full row.
    let src2 = gradient(1440, 900);
    let mut a = [0u32; 480];
    let mut b = [0u32; 50];
    v.render_row(&src2, 1440, 100, 0, &mut a);
    v.render_row(&src2, 1440, 100, 123, &mut b);
    assert_eq!(&a[123..173], &b[..]);
}

#[test]
fn cropped_view_pans_towards_the_edge() {
    let mut v = View::cropped(1024, 768, 480, 320);
    assert_eq!(v.dest(), Rect::new(0, 0, 480, 320));
    assert_eq!(v.to_source(10, 20), (10, 20));
    // Not near an edge: nothing moves.
    assert!(!v.pan_towards(240, 160, 24, 16));
    // Held near the right edge: it moves right, then stops at the end of the picture.
    assert!(v.pan_towards(470, 160, 24, 16));
    assert_eq!(v.origin(), (16, 0));
    assert_eq!(v.to_source(10, 20), (26, 20));
    for _ in 0..100 {
        v.pan_towards(470, 310, 24, 16);
    }
    assert_eq!(v.origin(), (1024 - 480, 768 - 320));
    assert!(!v.pan_towards(470, 310, 24, 16));
    for _ in 0..100 {
        v.pan_towards(2, 3, 24, 16);
    }
    assert_eq!(v.origin(), (0, 0));
    // Rectangle mapping follows the piece shown.
    v.pan_towards(470, 160, 24, 16);
    assert_eq!(v.map_rect(Rect::new(16, 0, 10, 10)), Rect::new(0, 0, 10, 10));
    assert!(v.map_rect(Rect::new(0, 0, 10, 10)).is_empty());
    assert_eq!(v.map_rect(Rect::new(0, 0, 40, 10)), Rect::new(0, 0, 24, 10));
    // A scaled view does not pan.
    let mut s = View::scaled(1024, 768, 480, 320);
    assert!(!s.pan_towards(470, 160, 24, 16));
    // Pixels come out 1:1.
    let src = gradient(1024, 768);
    let mut row = [0u32; 480];
    v.render_row(&src, 1024, 7, 0, &mut row);
    assert_eq!(row[0], src[7 * 1024 + 16]);
    assert_eq!(row[479], src[7 * 1024 + 16 + 479]);
}

// ---- touch ----------------------------------------------------------------------------------------

#[test]
fn xpt2046_commands() {
    assert_eq!(touch::control(Channel::X, 1), 0xd1);
    assert_eq!(touch::control(Channel::Y, 1), 0x91);
    assert_eq!(touch::control(Channel::Z1, 1), 0xb1);
    assert_eq!(touch::control(Channel::Z2, 1), 0xc1);
    assert_eq!(touch::control(Channel::Y, 0), 0x90);
    let b = touch::burst();
    assert_eq!(b.len(), 15);
    assert_eq!(b[0], 0xb1);
    assert_eq!(b[3], 0xc1);
    assert_eq!(b[6], 0x91);
    assert_eq!(b[9], 0xd1);
    assert_eq!(b[12], 0x90);
    assert!(b.iter().enumerate().all(|(i, &v)| i % 3 == 0 || v == 0));
    // The result is bits 14..3 of the two bytes after the control byte.
    assert_eq!(touch::decode(0x7f, 0xf8), 0xfff);
    assert_eq!(touch::decode(0x40, 0x00), 0x800);
    assert_eq!(touch::decode(0x00, 0x08), 1);
    assert_eq!(touch::decode(0xff, 0xff), 0xfff); // the top bit is not data
}

#[test]
fn xpt2046_reading_and_pressure() {
    let mut chip = SimTouch { pen: Some((1800, 2400, 1500)), ..Default::default() };
    let t = touch::read(&mut chip).expect("a touch");
    assert_eq!((t.x, t.y, t.pressure), (1800, 2400, 1500));
    assert_eq!(chip.bursts, touch::SAMPLES);
    // What the chip was asked.
    assert_eq!(&chip.controls[..5], [0xb1, 0xc1, 0x91, 0xd1, 0x90]);
    // No pen: no touch, whatever the position readings say.
    let mut chip = SimTouch::default();
    assert_eq!(touch::read(&mut chip), None);
    // A dead bus is no touch.
    let mut chip = SimTouch { pen: Some((1, 1, 1)), fail: true, ..Default::default() };
    assert_eq!(touch::read(&mut chip), None);
    let r = Raw { x: 0, y: 0, z1: 0, z2: 4095 };
    assert_eq!(r.pressure(), 0);
    assert_eq!(touch::parse_burst(&[0; 14]), None);
}

#[test]
fn touch_filter_takes_the_median_and_drops_noise() {
    let s = |x, y, z1| Raw { x, y, z1, z2: 4095 };
    // One wild reading out of five does not move the median.
    let t = touch::filter(&[s(2000, 2000, 900), s(2004, 1996, 900), s(3900, 100, 900), s(1998, 2002, 900), s(2002, 2001, 900)]);
    // (the wild one spreads the readings beyond the limit: no touch at all)
    assert_eq!(t, None);
    let t = touch::filter(&[s(2000, 2000, 900), s(2040, 1960, 900), s(2100, 2100, 900), s(1980, 2010, 900), s(2020, 1990, 900)]).unwrap();
    assert_eq!((t.x, t.y), (2020, 2000));
    // Light presses and rail readings are dropped, and three good ones of five are enough.
    let t = touch::filter(&[s(2000, 2000, 900), s(2001, 2001, 100), s(2002, 2002, 900), s(0, 2000, 900), s(2003, 2003, 900)]).unwrap();
    assert_eq!((t.x, t.y), (2002, 2002));
    assert_eq!(touch::filter(&[s(2000, 2000, 900), s(2001, 2001, 100), s(2002, 2002, 100), s(0, 2000, 900), s(2003, 2003, 100)]), None);
    assert_eq!(touch::filter(&[]), None);
    assert_eq!(touch::filter(&[s(4095, 4095, 900); 5]), None);
}

#[test]
fn touch_read_survives_noise() {
    let mut chip = SimTouch {
        pen: Some((2000, 1000, 1200)),
        noise: std::vec![(3, -2), (-4, 1), (2, 3), (-1, -3), (0, 0), (5, 5), (-5, -5)],
        ..Default::default()
    };
    for _ in 0..20 {
        let t = touch::read(&mut chip).unwrap();
        assert!((t.x as i32 - 2000).abs() <= 5 && (t.y as i32 - 1000).abs() <= 5, "{t:?}");
    }
}

#[test]
fn calibration_defaults_put_the_corners_in_the_corners() {
    // Raw positions of the glass's corners (from the kit's own numbers), as seen in portrait.
    let (x0, x1, y0, y1) = (300, 3932, 294, 3801);
    for rot in ROTS {
        let c = Cal::default_for(rot);
        let (w, h) = if rot.landscape() { (480, 320) } else { (320, 480) };
        let near = |p: (i32, i32), q: (i32, i32)| (p.0 - q.0).abs() <= 3 && (p.1 - q.1).abs() <= 3;
        // The portrait corners go to the matching corner of the rotated screen.
        let tl = c.apply(x0, y0);
        let br = c.apply(x1, y1);
        let tr = c.apply(x1, y0);
        let bl = c.apply(x0, y1);
        match rot {
            Rotation::Deg0 => assert!(near(tl, (0, 0)) && near(br, (w - 1, h - 1)), "{rot:?} {tl:?} {br:?}"),
            Rotation::Deg90 => assert!(near(tl, (0, h - 1)) && near(tr, (0, 0)) && near(bl, (w - 1, h - 1)), "{rot:?} {tl:?} {tr:?} {bl:?}"),
            Rotation::Deg180 => assert!(near(tl, (w - 1, h - 1)) && near(br, (0, 0)), "{rot:?} {tl:?} {br:?}"),
            Rotation::Deg270 => assert!(near(tl, (w - 1, 0)) && near(tr, (w - 1, h - 1)) && near(bl, (0, 0)), "{rot:?} {tl:?} {tr:?} {bl:?}"),
        }
    }
}

#[test]
fn hdmi_glass_default_puts_the_corners_in_the_corners() {
    // The touch glass of an HDMI panel with no SPI display behind it: raw 200..3900 spans the
    // screen. Each turn sends the raw corners to the matching screen corners.
    let near = |p: (i32, i32), q: (i32, i32)| (p.0 - q.0).abs() <= 2 && (p.1 - q.1).abs() <= 2;
    for (w, h) in [(800, 480), (1024, 768), (1920, 1080)] {
        let (lo, hi) = (touch::GLASS_MIN, touch::GLASS_MAX);
        for rot in ROTS {
            let c = Cal::default_for_screen(w, h, rot);
            let tl = c.apply(lo, lo);
            let tr = c.apply(hi, lo);
            let bl = c.apply(lo, hi);
            let br = c.apply(hi, hi);
            match rot {
                Rotation::Deg0 => assert!(near(tl, (0, 0)) && near(tr, (w - 1, 0)) && near(bl, (0, h - 1)) && near(br, (w - 1, h - 1)), "{w}x{h} {rot:?} {tl:?} {tr:?} {bl:?} {br:?}"),
                Rotation::Deg90 => assert!(near(tl, (0, h - 1)) && near(tr, (0, 0)) && near(bl, (w - 1, h - 1)) && near(br, (w - 1, 0)), "{w}x{h} {rot:?} {tl:?} {tr:?} {bl:?} {br:?}"),
                Rotation::Deg180 => assert!(near(tl, (w - 1, h - 1)) && near(br, (0, 0)) && near(tr, (0, h - 1)), "{w}x{h} {rot:?} {tl:?} {tr:?} {br:?}"),
                Rotation::Deg270 => assert!(near(tl, (w - 1, 0)) && near(tr, (w - 1, h - 1)) && near(bl, (0, 0)) && near(br, (0, h - 1)), "{w}x{h} {rot:?} {tl:?} {tr:?} {bl:?}"),
            }
            // The middle of the glass is the middle of the screen.
            assert!(near(c.apply(2050, 2050), (w / 2, h / 2)) || near(c.apply(2050, 2050), (w / 2 - 1, h / 2 - 1)), "{w}x{h} {rot:?} {:?}", c.apply(2050, 2050));
        }
    }
}

#[test]
fn touch_without_a_spi_panel_gives_pointer_events_in_screen_pixels() {
    // The standalone touch driver (milk.touch=xpt2046, no milk.lcd): the chip model, the filter, the default
    // map for the 800x480 screen and the tracker; no panel anywhere.
    let cal = Cal::default_for_screen(800, 480, Rotation::Deg0);
    let mut chip = SimTouch { pen: Some((2050, 2050, 1500)), ..Default::default() };
    let mut tr = Tracker::new();
    let mut events = Vec::new();
    let mut poll = |chip: &mut SimTouch, tr: &mut Tracker, events: &mut Vec<Event>| {
        let sample = touch::read(chip).map(|t| {
            let (x, y) = cal.apply(t.x as i32, t.y as i32);
            (x.clamp(0, 799), y.clamp(0, 479))
        });
        if let Some(e) = tr.update(sample) {
            events.push(e);
        }
    };
    poll(&mut chip, &mut tr, &mut events);
    assert!(events.is_empty(), "one reading is not a touch yet");
    poll(&mut chip, &mut tr, &mut events);
    let Some(Event::Down(x, y)) = events.first().copied() else { panic!("{events:?}") };
    assert!((x - 400).abs() <= 3 && (y - 240).abs() <= 3, "({x},{y})");
    chip.pen = Some((3900, 3900, 1500));
    poll(&mut chip, &mut tr, &mut events);
    let Some(Event::Move(x, y)) = events.last().copied() else { panic!("{events:?}") };
    assert!((x - 799).abs() <= 2 && (y - 479).abs() <= 2, "({x},{y})");
    chip.pen = None;
    for _ in 0..3 {
        poll(&mut chip, &mut tr, &mut events);
    }
    assert_eq!(events.last(), Some(&Event::Up));
    // A silent chip (nothing wired, as under qemu) makes no events at all.
    let mut none = SimTouch::default();
    let mut tr = Tracker::new();
    let mut ev = Vec::new();
    for _ in 0..50 {
        poll(&mut none, &mut tr, &mut ev);
    }
    assert!(ev.is_empty());
}

#[test]
fn calibration_text_round_trips() {
    let c = Cal { a: 7682, b: 0, c: -26, d: -1, e: 7000, f: 319 };
    let mut buf = [0u8; 96];
    let n = c.write(&mut buf);
    assert_eq!(&buf[..n], b"7682,0,-26,-1,7000,319");
    assert_eq!(Cal::parse(&buf[..n]), Some(c));
    assert_eq!(Cal::parse(b"0x10,0,0,0,0x10,0"), Some(Cal { a: 16, b: 0, c: 0, d: 0, e: 16, f: 0 }));
    for bad in [&b"1,2,3,4,5"[..], b"1,2,3,4,5,6,7", b"1,2,x,4,5,6", b"", b",,,,,", b"1,2,3,4,5,99999999999"] {
        assert_eq!(Cal::parse(bad), None, "{:?}", core::str::from_utf8(bad));
    }
    for rot in ROTS {
        let c = Cal::default_for(rot);
        let n = c.write(&mut buf);
        assert_eq!(Cal::parse(&buf[..n]), Some(c));
    }
}

#[test]
fn calibration_fit_recovers_the_map() {
    // A made-up panel: x from raw y, y from raw x inverted, with a little skew.
    let truth = Cal { a: 1000, b: 8000, c: -30, d: -6500, e: -500, f: 330 };
    let targets = touch::cal_targets(480, 320);
    // Raw readings that land exactly on the targets: invert by search over the raw plane.
    let mut raw = Vec::new();
    for t in targets {
        let mut best = (0, 0, i64::MAX);
        for rx in (0..4096).step_by(4) {
            for ry in (0..4096).step_by(4) {
                let p = truth.apply(rx, ry);
                let d = ((p.0 - t.0).pow(2) + (p.1 - t.1).pow(2)) as i64;
                if d < best.2 {
                    best = (rx, ry, d);
                }
            }
        }
        raw.push((best.0, best.1));
    }
    let fitted = touch::fit(&raw, &targets).expect("fit");
    // The fit reproduces the targets to a pixel, and other points about as well as the truth does.
    for (r, t) in raw.iter().zip(targets) {
        let p = fitted.apply(r.0, r.1);
        assert!((p.0 - t.0).abs() <= 1 && (p.1 - t.1).abs() <= 1, "{p:?} {t:?}");
    }
    let (p, q) = (fitted.apply(2000, 2000), truth.apply(2000, 2000));
    assert!((p.0 - q.0).abs() <= 4 && (p.1 - q.1).abs() <= 4, "{p:?} {q:?}");
    // Noise on the touches (a stylus is not exact) is averaged out by the least squares.
    let noisy: Vec<(i32, i32)> = raw.iter().enumerate().map(|(i, r)| (r.0 + [8, -8, 5, -5][i], r.1 + [-6, 6, -7, 7][i])).collect();
    let f2 = touch::fit(&noisy, &targets).expect("fit");
    for (r, t) in raw.iter().zip(targets) {
        let p = f2.apply(r.0, r.1);
        assert!((p.0 - t.0).abs() <= 3 && (p.1 - t.1).abs() <= 3, "{p:?} {t:?}");
    }
    // Not enough points, or all on one line, or mismatched lists: no answer.
    assert_eq!(touch::fit(&raw[..2], &targets[..2]), None);
    assert_eq!(touch::fit(&raw, &targets[..3]), None);
    let line = [(100, 100), (200, 200), (300, 300), (400, 400)];
    assert_eq!(touch::fit(&line, &targets), None);
    // The default for the kit's rotation is itself recovered from its own corner readings.
    let def = Cal::default_for(Rotation::Deg90);
    let raws = [(300, 3801), (3932, 3801), (3932, 294), (300, 294)];
    let tg: Vec<(i32, i32)> = raws.iter().map(|r| def.apply(r.0, r.1)).collect();
    let back = touch::fit(&raws, &tg).unwrap();
    for r in raws {
        let (a, b) = (back.apply(r.0, r.1), def.apply(r.0, r.1));
        assert!((a.0 - b.0).abs() <= 1 && (a.1 - b.1).abs() <= 1);
    }
}

#[test]
fn calibration_fit_copes_with_swapped_and_flipped_axes() {
    // The owner's glass is turned relative to the picture (milk.fbrotate=90 and the touch chip's axes do not
    // have to follow): whatever the chip's X and Y mean, four touches on the four crosses give a map that is right
    // everywhere. All eight ways round (swap or not, flip X, flip Y), on the logical sizes in use.
    for (w, h) in [(800, 480), (480, 800), (1024, 768), (640, 1067)] {
        let targets = touch::cal_targets(w, h);
        for swap in [false, true] {
            for flip_x in [false, true] {
                for flip_y in [false, true] {
                    // The chip's reading of a point of the screen: normalised position (0..1) turned the way this glass is.
                    let raw_of = |sx: f64, sy: f64| -> (i32, i32) {
                        let (mut u, mut v) = (sx / (w - 1) as f64, sy / (h - 1) as f64);
                        if swap {
                            std::mem::swap(&mut u, &mut v);
                        }
                        if flip_x {
                            u = 1.0 - u;
                        }
                        if flip_y {
                            v = 1.0 - v;
                        }
                        ((300.0 + u * 3600.0) as i32, (290.0 + v * 3500.0) as i32)
                    };
                    let raw: Vec<(i32, i32)> = targets.iter().map(|t| raw_of(t.0 as f64, t.1 as f64)).collect();
                    let cal = touch::fit(&raw, &targets).unwrap_or_else(|| panic!("{w}x{h} swap={swap} fx={flip_x} fy={flip_y}: no fit"));
                    assert!(touch::fit_error(&cal, &raw, &targets) <= 1);
                    // And everywhere else on the glass: a grid of points, each within 2 pixels of where it really is.
                    for gy in (0..h).step_by(37) {
                        for gx in (0..w).step_by(41) {
                            let r = raw_of(gx as f64, gy as f64);
                            let p = cal.apply(r.0, r.1);
                            assert!((p.0 - gx).abs() <= 3 && (p.1 - gy).abs() <= 3, "{w}x{h} swap={swap} fx={flip_x} fy={flip_y}: ({gx},{gy}) -> {p:?}");
                        }
                    }
                    // The printed line parses back into the same map (what the owner copies into cmdline.txt).
                    let mut buf = [0u8; 96];
                    let n = cal.write(&mut buf);
                    assert_eq!(Cal::parse(&buf[..n]), Some(cal));
                }
            }
        }
    }
    // Touches that do not agree (two crosses touched in the wrong order) have a large error the helper can see.
    let targets = touch::cal_targets(800, 480);
    let raw = [(400, 400), (3800, 400), (400, 3800), (3800, 3800)]; // crosses 3 and 4 swapped
    let cal = touch::fit(&raw, &targets).unwrap();
    assert!(touch::fit_error(&cal, &raw, &targets) > 100);
    assert_eq!(touch::fit_error(&cal, &[], &[]), 0);
}

#[test]
fn tracker_makes_clicks_and_drags() {
    let mut t = Tracker::new();
    let p = |x, y| Some((x, y));
    // One reading is a glitch; two make a touch.
    assert_eq!(t.update(p(10, 10)), None);
    assert_eq!(t.update(None), None);
    assert_eq!(t.update(p(10, 10)), None);
    assert_eq!(t.update(p(12, 11)), Some(Event::Down(12, 11)));
    assert!(t.is_down());
    // Jitter under two pixels is ignored; movement is reported.
    assert_eq!(t.update(p(13, 11)), None);
    assert_eq!(t.update(p(20, 15)), Some(Event::Move(20, 15)));
    // A single missed reading does not lift the finger; three do.
    assert_eq!(t.update(None), None);
    assert_eq!(t.update(p(20, 15)), None);
    assert_eq!(t.update(None), None);
    assert_eq!(t.update(None), None);
    assert!(t.is_down());
    assert_eq!(t.update(None), Some(Event::Up));
    assert!(!t.is_down());
    assert_eq!(t.update(None), None);
}

#[test]
fn a_tap_from_chip_to_pointer() {
    // Chip -> filter -> calibration -> tracker -> screen position, for a stylus put on the
    // middle of the glass and then lifted.
    let cal = Cal::default_for(Rotation::Deg90);
    let mut chip = SimTouch { pen: Some((2116, 2047, 1000)), ..Default::default() };
    let mut tr = Tracker::new();
    let mut events = Vec::new();
    for step in 0..8 {
        if step == 5 {
            chip.pen = None;
        }
        let sample = touch::read(&mut chip).map(|t| cal.apply(t.x as i32, t.y as i32));
        if let Some(e) = tr.update(sample) {
            events.push(e);
        }
    }
    assert_eq!(events.len(), 2);
    match events[0] {
        Event::Down(x, y) => assert!((x - 240).abs() <= 6 && (y - 160).abs() <= 6, "{x},{y}"),
        e => panic!("{e:?}"),
    }
    assert_eq!(events[1], Event::Up);
    // And through a scaled desktop view, the same tap lands in the middle of the desktop.
    let v = View::scaled(1440, 900, 480, 320);
    let (sx, sy) = v.to_source(240, 160);
    assert!((sx - 720).abs() <= 3 && (sy - 450).abs() <= 3);
}

// ---- whole pictures through the model ---------------------------------------------------------------

const BG: u32 = 0x0016_1513;

/// The text console as the kernel's, in miniature: 8x16 glyphs of the kernel's font in a 480x320
/// picture with an 8 pixel margin (58 columns, 18 rows).
fn render_text(lines: &[(&str, u32)]) -> Vec<u32> {
    let mut img = std::vec![BG; 480 * 320];
    for (row, (text, colour)) in lines.iter().enumerate().take(18) {
        for (col, ch) in text.chars().enumerate().take(58) {
            let cp = ch as u32;
            let glyph = if (font_data::FIRST_CHAR..=font_data::LAST_CHAR).contains(&cp) {
                &font_data::GLYPHS[(cp - font_data::FIRST_CHAR) as usize]
            } else {
                &font_data::REPLACEMENT
            };
            for (gy, bits) in glyph.iter().enumerate() {
                for gx in 0..8 {
                    if bits & (0x80 >> gx) != 0 {
                        img[(8 + row * 16 + gy) * 480 + 8 + col * 8 + gx] = *colour;
                    }
                }
            }
        }
    }
    img
}

fn through_panel(cfg: &Config, src: &mut impl PixelSource) -> Vec<u32> {
    let mut p = up(cfg);
    assert!(lcd::draw_rect(cfg, &mut p, full(cfg), src));
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    p.visible_for(cfg)
}

fn maybe_save(name: &str, w: usize, h: usize, px: &[u32]) {
    if let Ok(dir) = std::env::var("MILK_LCD_SHOTS") {
        std::fs::write(std::format!("{dir}/{name}"), testpng::encode(w, h, px)).unwrap();
    }
}

fn boot_log() -> Vec<(&'static str, u32)> {
    let w = 0x00c9_c4b8;
    std::vec![
        ("[uart] console up", w),
        ("milkOS 0.2.0 booting...", w),
        ("platform: Raspberry Pi 4 (BCM2711)", w),
        ("lcd: ili9486 480x320 rot 90, SPI0 CE0 16 MHz, DC=24 RST=25", 0x00a3_d48c),
        ("lcd: touch XPT2046 on CE1, pen IRQ GPIO17", 0x00a3_d48c),
        ("mem: RAM 0x0..0x3b400000 (948 MiB, device tree)", w),
        ("[gic] v2 up", w),
        ("[smp] 4 of 4 CPUs online (spin table)", w),
        ("rpi: board revision 0xc03112, firmware 0x5f6a1c2e", w),
        ("[sdhci] sd card ready, 59640 MiB", w),
        ("milkfs: mmcblk0 mounted at /Volumes/Milk", w),
        ("[pcie] link up, xHCI found", w),
        ("[usb] keyboard, mouse", w),
        ("display: rpi mailbox framebuffer 1024x768 XRGB8888", w),
        ("lcd: mirroring the desktop, 427x320 of 1024x768", 0x00a3_d48c),
        ("wm: desktop up in 412 ms", 0x00f2_d27e),
    ]
}

#[test]
fn boot_log_on_the_panel() {
    let img = render_text(&boot_log());
    let want: Vec<u32> = img.iter().map(|&c| quantize(Controller::Ili9486, c)).collect();
    // (The 480x320 picture is for the controllers with 480x320 landscape memory; the small ones have
    // their own test.)
    for kind in KINDS.into_iter().filter(|k| k.native() == (320, 480)) {
        let cfg = Config::new(kind);
        let seen = through_panel(&cfg, &mut Pixels { px: &img, width: 480 });
        let want: Vec<u32> = img.iter().map(|&c| quantize(kind, c)).collect();
        assert_eq!(seen, want, "{kind:?}");
    }
    maybe_save("raspberry-pi-lcd-boot-log.png", 480, 320, &want);
    assert_eq!(fnv(&want), 0x64cbe868b08983c8, "the boot log picture changed");
}

#[test]
fn crash_screen_on_the_panel() {
    let w = 0x00c9_c4b8;
    let r = 0x00cc_554f;
    let mut lines = std::vec![
        ("[sdhci] sd card ready, 59640 MiB", w),
        ("milkfs: mmcblk0 mounted at /Volumes/Milk", w),
        ("wm: desktop up in 412 ms", w),
        ("", w),
        ("*** KERNEL PANIC ***", r),
        ("panicked at kernel/src/wm/mod.rs:1234:9:", w),
        ("a deliberate panic, late", w),
        ("", w),
    ];
    lines.push(("milkOS stopped. The green LED flashes.", 0x00f0_7a72));
    let img = render_text(&lines);
    let cfg = Config::new(Controller::Ili9486);
    let seen = through_panel(&cfg, &mut Pixels { px: &img, width: 480 });
    let want: Vec<u32> = img.iter().map(|&c| quantize(Controller::Ili9486, c)).collect();
    assert_eq!(seen, want);
    maybe_save("raspberry-pi-lcd-crash-screen.png", 480, 320, &seen);
    assert_eq!(fnv(&seen), 0x42d9b5e5f083353, "the crash picture changed");
}

#[test]
fn gradient_on_the_panel() {
    let img = gradient(480, 320);
    let cfg = Config::new(Controller::Ili9486);
    let seen = through_panel(&cfg, &mut Pixels { px: &img, width: 480 });
    maybe_save("raspberry-pi-lcd-gradient.png", 480, 320, &seen);
    let want: Vec<u32> = img.iter().map(|&c| quantize(Controller::Ili9486, c)).collect();
    assert_eq!(seen, want);
    assert_eq!(fnv(&seen), 0xc17f9934aec64605, "the gradient picture changed");
}

/// A plain reference for the box scaler: floating point means over the same boxes.
fn reference_scale(src: &[u32], sw: usize, sh: usize, d: Rect) -> Vec<u32> {
    let mut out = Vec::new();
    for dy in 0..d.h {
        for dx in 0..d.w {
            let (x0, x1) = (dx * sw / d.w, ((dx + 1) * sw / d.w).max(dx * sw / d.w + 1));
            let (y0, y1) = (dy * sh / d.h, ((dy + 1) * sh / d.h).max(dy * sh / d.h + 1));
            let mut acc = [0f64; 3];
            let mut n = 0f64;
            for y in y0..y1.min(sh) {
                for x in x0..x1.min(sw) {
                    let p = src[y * sw + x];
                    acc[0] += (p >> 16 & 0xff) as f64;
                    acc[1] += (p >> 8 & 0xff) as f64;
                    acc[2] += (p & 0xff) as f64;
                    n += 1.0;
                }
            }
            let c = |v: f64| (v / n + 0.5) as u32;
            out.push(c(acc[0]) << 16 | c(acc[1]) << 8 | c(acc[2]));
        }
    }
    out
}

fn desktop_png() -> (usize, usize, Vec<u32>) {
    testpng::decode(include_bytes!("../../docs/screenshots/raspberry-pi-desktop.png")).expect("the desktop screenshot decodes")
}

#[test]
fn the_desktop_on_the_panel() {
    let (sw, sh, desk) = desktop_png();
    assert_eq!((sw, sh), (1024, 768));
    let cfg = Config::new(Controller::Ili9486);
    let view = View::scaled(sw, sh, 480, 320);
    let d = view.dest();
    let seen = through_panel(&cfg, &mut ViewSource { view, src: &desk, stride: sw });
    // Inside the picture the panel shows the box average of the desktop (to the colour depth of
    // the panel); the bars beside it are black.
    let refd = reference_scale(&desk, sw, sh, d);
    let mut worst = 0;
    for y in 0..320 {
        for x in 0..480 {
            let got = seen[y * 480 + x];
            if x < d.x || x >= d.x1() {
                assert_eq!(got, 0, "bar at ({x},{y})");
                continue;
            }
            let want = refd[(y - d.y) * d.w + (x - d.x)];
            for sh in [16, 8, 0] {
                let diff = ((got >> sh & 0xff) as i32 - (want >> sh & 0xff) as i32).abs();
                worst = worst.max(diff);
            }
        }
    }
    // 565 loses up to 7 (red, blue) / 3 (green) levels; the scaler itself is exact to one.
    assert!(worst <= 8, "worst channel difference {worst}");
    // It really is a desktop: many distinct colours.
    let mut colours = seen.clone();
    colours.sort_unstable();
    colours.dedup();
    assert!(colours.len() > 200, "{} colours", colours.len());
    maybe_save("raspberry-pi-lcd-desktop.png", 480, 320, &seen);
}

#[test]
fn the_desktop_panned_on_the_panel() {
    let (sw, sh, desk) = desktop_png();
    let cfg = Config::new(Controller::Ili9486);
    let mut view = View::cropped(sw, sh, 480, 320);
    for _ in 0..12 {
        view.pan_towards(470, 160, 24, 16);
    }
    let seen = through_panel(&cfg, &mut ViewSource { view, src: &desk, stride: sw });
    let (ox, oy) = view.origin();
    assert_eq!((ox, oy), (192, 0));
    for y in (0..320).step_by(5) {
        for x in (0..480).step_by(3) {
            assert_eq!(seen[y * 480 + x], quantize(Controller::Ili9486, desk[(oy + y) * sw + ox + x]));
        }
    }
    maybe_save("raspberry-pi-lcd-desktop-pan.png", 480, 320, &seen);
}

#[test]
fn damage_to_the_desktop_updates_only_its_rectangle() {
    // The compositor changed a 60x40 rectangle of the 1024x768 desktop: the panel gets the
    // rectangle that covers it, not the screen.
    let (sw, sh, mut desk) = desktop_png();
    let cfg = Config::new(Controller::Ili9486);
    let view = View::scaled(sw, sh, 480, 320);
    let mut p = up(&cfg);
    lcd::draw_rect(&cfg, &mut p, full(&cfg), &mut ViewSource { view, src: &desk, stride: sw });
    let changed = Rect::new(500, 300, 60, 40);
    for y in changed.y..changed.y1() {
        for x in changed.x..changed.x1() {
            desk[y * sw + x] = 0x00ff_0000;
        }
    }
    let mut dirty = Dirty::new();
    dirty.add(view.map_rect(changed));
    p.log.clear();
    p.pixels_written = 0;
    while let Some(r) = dirty.pop() {
        assert!(lcd::draw_rect(&cfg, &mut p, r, &mut ViewSource { view, src: &desk, stride: sw }));
    }
    assert!(p.pixels_written > 0 && p.pixels_written < 40 * 25, "{}", p.pixels_written);
    // The panel now equals a fresh draw of the changed desktop.
    let mut fresh = up(&cfg);
    lcd::draw_rect(&cfg, &mut fresh, full(&cfg), &mut ViewSource { view, src: &desk, stride: sw });
    assert!(p.visible_for(&cfg) == fresh.visible_for(&cfg));
}

#[test]
fn garbage_never_panics_the_model_or_the_driver() {
    // Random bytes at the model, and the driver asked to draw anywhere.
    let mut seed = 0x1234_5678u32;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed
    };
    for kind in KINDS {
        let mut p = SimPanel::new(kind);
        for _ in 0..500 {
            let n = (next() % 40) as usize;
            let b: Vec<u8> = (0..n).map(|_| next() as u8).collect();
            p.write(next() % 2 == 0, &b);
        }
        let cfg = Config::new(kind);
        for _ in 0..200 {
            let r = Rect::new((next() % 600) as usize, (next() % 400) as usize, (next() % 300) as usize, (next() % 300) as usize);
            let _ = lcd::draw_rect(&cfg, &mut p, r, &mut Solid(next()));
        }
    }
    // Hostile touch readings.
    for _ in 0..200 {
        let rx: Vec<u8> = (0..touch::BURST_LEN).map(|_| next() as u8).collect();
        let r = touch::parse_burst(&rx).unwrap();
        let _ = touch::filter(&[r; 5]);
    }
    let _: String = String::new();
}

// ---- milk.lcd=probe ------------------------------------------------------------------------------

mod probe {
    use super::*;
    use crate::lcdprobe::{
        self, BACKLIGHT_PINS, BARS, DEFAULT_PINS, FLASHES, HOLD_MS, NumberCard, ProbeCard,
        ProbeOptions, SETUPS, SETUPS_WITHOUT_PIN_ROUND, Setup, WHITE,
    };

    const PINS: (u8, u8) = DEFAULT_PINS;

    fn text_card(i: usize) -> (Config, ProbeCard) {
        let cfg = SETUPS[i].config();
        (cfg, ProbeCard::new(i + 1, SETUPS.len(), &SETUPS[i], PINS, cfg.width(), cfg.height()))
    }

    fn number_card(i: usize) -> (Config, NumberCard) {
        let cfg = SETUPS[i].config();
        (cfg, NumberCard::new(i + 1, SETUPS.len(), &SETUPS[i], cfg.width(), cfg.height()))
    }

    /// What a card looks like once the controller has taken the colours.
    fn expected(cfg: &Config, px: impl Fn(usize, usize) -> u32) -> Vec<u32> {
        let (w, h) = (cfg.width(), cfg.height());
        let mut v = Vec::new();
        for y in 0..h {
            for x in 0..w {
                v.push(quantize(cfg.controller, px(x, y)));
            }
        }
        v
    }

    fn never() -> impl FnMut() -> bool {
        || false
    }

    #[test]
    fn the_matrix_covers_what_it_should_in_the_order_it_should() {
        let n = SETUPS.len();
        assert!((18..=26).contains(&n), "{n} set-ups");
        // Distinct.
        for (i, s) in SETUPS.iter().enumerate() {
            for t in &SETUPS[i + 1..] {
                assert_ne!(s, t);
            }
        }
        // The common 4-inch kit's chip first: st7796 at 4 MHz, then at 16 MHz, then ili9488, ili9486p, ili9486
        // (all SPI mode 0, default pins).
        let first: Vec<(Controller, u32)> = SETUPS[..5].iter().map(|s| (s.controller, s.hz)).collect();
        assert_eq!(
            first,
            [
                (Controller::St7796, lcdprobe::MHZ4),
                (Controller::St7796, lcdprobe::MHZ16),
                (Controller::Ili9488, lcdprobe::MHZ4),
                (Controller::Ili9486P, lcdprobe::MHZ4),
                (Controller::Ili9486, lcdprobe::MHZ4),
            ]
        );
        assert!(SETUPS[..5].iter().all(|s| s.mode == 0 && s.pins.is_none()));
        // Every controller is tried at the slow clock, SPI mode 0, default pins, before any other fast
        // clock, mode 3 or other pins.
        let first_other = SETUPS.iter().position(|s| s.mode != 0 || s.pins.is_some() || s.hz > lcdprobe::MHZ4 && s.controller != Controller::St7796).unwrap();
        for c in lcd::CONTROLLERS {
            let at = SETUPS.iter().position(|s| s.controller == c && s.hz == lcdprobe::MHZ4 && s.mode == 0 && s.pins.is_none());
            assert!(at.is_some_and(|i| i < first_other), "{c:?} at {at:?}, first other {first_other}");
        }
        // The fast clocks only for the likeliest four controllers.
        let mut fast: Vec<Controller> = SETUPS.iter().filter(|s| s.hz > lcdprobe::MHZ4).map(|s| s.controller).collect();
        fast.sort_by_key(|c| c.name());
        fast.dedup();
        assert!(fast.len() <= 4, "{fast:?}");
        // SPI modes 0 and 3 only; clocks 4, 16, 32 MHz only.
        assert!(SETUPS.iter().all(|s| s.mode == 0 || s.mode == 3));
        assert!(SETUPS.iter().any(|s| s.mode == 3));
        assert!(SETUPS.iter().all(|s| [lcdprobe::MHZ4, lcdprobe::MHZ16, lcdprobe::MHZ32].contains(&s.hz)));
        assert!(SETUPS.iter().any(|s| s.hz == lcdprobe::MHZ32));
        // Pin pairs: all in the tail (so `milk.lcd_probe_pins=0` cuts them), none the default or the touch
        // pen line, distinct pins, in the header range, for at most two controllers, slow, mode 0.
        assert!(SETUPS[..SETUPS_WITHOUT_PIN_ROUND].iter().all(|s| s.pins.is_none()));
        assert!(SETUPS[SETUPS_WITHOUT_PIN_ROUND..].iter().all(|s| s.pins.is_some()));
        assert_eq!(SETUPS_WITHOUT_PIN_ROUND, 18);
        let mut pin_ctrls: Vec<Controller> = Vec::new();
        for s in &SETUPS[SETUPS_WITHOUT_PIN_ROUND..] {
            let (dc, rst) = s.pins.unwrap();
            assert_ne!((dc, rst), DEFAULT_PINS);
            assert!(dc != rst && (2..=27).contains(&dc) && (2..=27).contains(&rst));
            assert!(dc != 17 && rst != 17, "17 is the touch pen line");
            assert!(s.is_slow() && s.mode == 0);
            if !pin_ctrls.contains(&s.controller) {
                pin_ctrls.push(s.controller);
            }
        }
        assert_eq!(pin_ctrls.len(), 2);
        // The brief's pairs are in.
        let pairs: Vec<(u8, u8)> = SETUPS.iter().filter_map(|s| s.pins).collect();
        for want in [(22, 27), (25, 24)] {
            assert!(pairs.contains(&want), "{want:?}");
        }
        // The backlight pins are not the default D/C and reset, nor the touch pen line.
        assert!(BACKLIGHT_PINS == [18, 27]);
        assert!(BACKLIGHT_PINS.iter().all(|&b| b != PINS.0 && b != PINS.1 && b != 17));
    }

    #[test]
    fn every_setup_shows_its_three_signs_through_its_own_init_stream() {
        for (i, s) in SETUPS.iter().enumerate() {
            let cfg = s.config();
            let (w, h) = (cfg.width(), cfg.height());
            let mut p = up(&cfg);
            assert_eq!(p.kind, s.controller);
            // The 16-bit framing is the ILI9486's only.
            let first_cmd = p.log.iter().find(|(d, _)| !*d).unwrap();
            assert_eq!(first_cmd.1.len(), if s.controller.word16() { 2 } else { 1 }, "{i}");
            // 1. The number card.
            let (_, mut nc) = number_card(i);
            let rows = lcdprobe::draw_banded(&cfg, &mut p, &mut nc, &mut never());
            assert_eq!(rows, Some(h), "{i}");
            assert_eq!(p.visible_for(&cfg), expected(&cfg, |x, y| nc.pixel(x, y)), "number card {i}");
            // 2. The four flashes: the whole screen one colour each.
            for c in FLASHES {
                let rows = lcdprobe::draw_banded(&cfg, &mut p, &mut lcd::Solid(c), &mut never());
                assert_eq!(rows, Some(h));
                let seen = p.visible_for(&cfg);
                assert_eq!(seen.len(), w * h);
                assert!(seen.iter().all(|&v| v == quantize(s.controller, c)), "flash {c:#08x} at {i}");
            }
            // 3. The text card.
            let (_, mut tc) = text_card(i);
            assert!(lcd::draw_rect(&cfg, &mut p, Rect::new(0, 0, w, h), &mut tc));
            assert!(p.errors.is_empty(), "{:?}", p.errors);
            assert_eq!(p.visible_for(&cfg), expected(&cfg, |x, y| tc.pixel(x, y)), "text card {i}");
            // The text card fills every pixel of the addressed area.
            assert_eq!(p.pixels_written, (2 + FLASHES.len()) * w * h);
        }
    }

    #[test]
    fn the_cards_as_pictures() {
        // The number card of set-up 14 and the text card of set-up 1, for the doc (MILK_LCD_SHOTS=dir).
        let (cfg, nc) = number_card(13);
        let img: Vec<u32> = expected(&cfg, |x, y| nc.pixel(x, y));
        maybe_save("raspberry-pi-lcd-probe-number.png", cfg.width(), cfg.height(), &img);
        let (cfg, tc) = text_card(0);
        let img: Vec<u32> = expected(&cfg, |x, y| tc.pixel(x, y));
        maybe_save("raspberry-pi-lcd-probe-card.png", cfg.width(), cfg.height(), &img);
    }

    #[test]
    fn the_flashes_have_distinct_colours_in_the_order_red_green_blue_white() {
        assert_eq!(FLASHES, [0xff0000, 0x00ff00, 0x0000ff, 0xffffff]);
        // After the controller's rounding they are still four different, saturated colours.
        for k in KINDS {
            let q: Vec<u32> = FLASHES.iter().map(|&c| quantize(k, c)).collect();
            assert_eq!(q, [0xff0000, 0x00ff00, 0x0000ff, 0xffffff], "{k:?}");
        }
    }

    #[test]
    fn the_text_card_has_a_label_options_bars_and_a_border() {
        for i in 0..SETUPS.len() {
            let (cfg, c) = text_card(i);
            let (w, h) = (cfg.width(), cfg.height());
            assert!(w == 480 && h == 320 || w == 320 && h == 240, "{w}x{h}");
            // Border all round.
            for x in [0, 3, w - 1, w - 4] {
                assert_eq!(c.pixel(x, h / 2), WHITE);
            }
            for y in [0, 3, h - 1, h - 4] {
                assert_eq!(c.pixel(w / 2, y), WHITE);
            }
            // Bars: the five colours, left to right, on a row in the middle of them.
            assert!(c.bars_top() + 40 < h, "{i}: bars at {} of {h}", c.bars_top());
            let y = (c.bars_top() + h - 12) / 2;
            let xs: Vec<u32> = (0..BARS.len()).map(|k| c.pixel(12 + (w - 24) * (2 * k + 1) / (2 * BARS.len()), y)).collect();
            assert_eq!(xs, BARS);
            // Text: lit pixels in the name's rows (big), in the option rows, above the bars.
            let lit = |y0: usize, y1: usize| (y0..y1).map(|y| (0..w).filter(|&x| c.pixel(x, y) == WHITE).count()).sum::<usize>();
            assert!(lit(12, 12 + 28) > 400, "name too faint: {}", lit(12, 40));
            assert!(lit(48, c.bars_top() - 2) > 300, "options too faint");
            // The name is wide but fits.
            let name_right = (0..w).rev().find(|&x| (12..40).any(|y| x < w - 5 && c.pixel(x, y) == WHITE)).unwrap();
            assert!(name_right < w - 12, "name runs into the border");
        }
    }

    #[test]
    fn the_number_card_is_a_big_number_on_black() {
        for i in 0..SETUPS.len() {
            let (cfg, c) = number_card(i);
            let (w, h) = (cfg.width(), cfg.height());
            // Big: at least 9 px per font pixel (63 px tall) on the 320x240 area, 12 on the 480x320.
            assert!(c.number_scale() >= if w >= 400 { 12 } else { 9 }, "{i}: scale {}", c.number_scale());
            // White on black with a border; most of the screen is black.
            let total = w * h;
            let white = (0..h).map(|y| (0..w).filter(|&x| c.pixel(x, y) == WHITE).count()).sum::<usize>();
            let black = (0..h).map(|y| (0..w).filter(|&x| c.pixel(x, y) == 0).count()).sum::<usize>();
            assert_eq!(white + black, total);
            assert!(white > total / 25 && white < total / 3, "{white} of {total}");
            // No other colour appears, so it reads on any glass and under any colour order.
            assert_eq!(c.pixel(0, 0), WHITE);
        }
        // Different set-ups look different, and a two-digit number is as wide as the card allows.
        let a = number_card(0).1;
        let b = number_card(12).1;
        assert!((0..320).any(|y| (0..480).any(|x| a.pixel(x, y) != b.pixel(x, y))));
        let c = NumberCard::new(22, 22, &SETUPS[21], 480, 320);
        let right = (0..480 - 8).rev().find(|&x| (12..160).any(|y| c.pixel(x, y) == WHITE)).unwrap();
        assert!(right < 480 - 12);
        // Three digits do not overflow either.
        let c = NumberCard::new(123, 123, &SETUPS[0], 480, 320);
        assert!(c.number_scale() >= 1);
    }

    #[test]
    fn the_options_shown_are_the_ones_that_set_the_same_driver_up() {
        for s in SETUPS {
            let line = lcdprobe::options(&s, PINS);
            let l = line.as_str();
            let value = |name| cmdline::value(l.as_bytes(), name);
            assert_eq!(Controller::parse(value("lcd").unwrap()), Some(s.controller), "{l}");
            assert_eq!(cmdline::number(l.as_bytes(), "lcd_hz"), Some(s.hz as i64), "{l}");
            match value("lcd_spi_mode") {
                None => assert_eq!(s.mode, 0, "{l}"),
                Some(v) => assert_eq!(lcd::parse_spi_mode(v), Some(s.mode), "{l}"),
            }
            match (value("lcd_dc"), value("lcd_rst"), s.pins) {
                (None, None, None) => {}
                (Some(d), Some(r), Some((dc, rst))) => {
                    assert_eq!((cmdline::parse_num(d), cmdline::parse_num(r)), (Some(dc as i64), Some(rst as i64)), "{l}");
                }
                other => panic!("{l}: {other:?}"),
            }
            // No rotation: every set-up is landscape (the default), so nothing to type for it.
            assert_eq!(value("lcd_rot"), None);
            // On a card, the words are split over lines that fit the width at the card's small scale.
            for (w, small) in [(480usize, 2usize), (320, 1)] {
                let chars = (w - 24) / (6 * small);
                let (lines, n) = lcdprobe::option_lines(&s, PINS, chars);
                assert!((1..=4).contains(&n));
                let joined: Vec<&str> = lines[..n].iter().map(|t| t.as_str()).collect::<Vec<_>>();
                assert!(joined.iter().all(|t| t.len() <= chars && !t.is_empty()), "{joined:?} in {chars}");
                assert_eq!(joined.join(" "), l, "no word lost or split");
            }
        }
        // Pins named by the options (not the set-up) are shown when they are not the usual ones.
        let s = SETUPS[0];
        let line = lcdprobe::options(&s, (23, 22));
        assert_eq!(cmdline::number(line.as_str().as_bytes(), "lcd_dc"), Some(23));
        assert_eq!(cmdline::number(line.as_str().as_bytes(), "lcd_rst"), Some(22));
        let line = lcdprobe::options(&SETUPS[18], (23, 22));
        assert_eq!(cmdline::number(line.as_str().as_bytes(), "lcd_dc"), Some(22), "the set-up's own pins win");
    }

    #[test]
    fn labels_and_texts_are_drawable() {
        let mut labels: Vec<String> = Vec::new();
        for s in SETUPS {
            let l = lcdprobe::label(&s);
            assert!(l.as_bytes().len() <= 12, "{}", l.as_str());
            labels.push(String::from(l.as_str()));
            let all = [
                String::from(lcdprobe::options(&s, PINS).as_str()),
                String::from(lcdprobe::detail(&s, PINS).as_str()),
                String::from(lcdprobe::describe(&s, PINS).as_str()),
                String::from(l.as_str()),
            ];
            for t in &all {
                for c in t.bytes() {
                    // Every character has its own glyph (not the box).
                    let g = lcdprobe::glyph(c);
                    assert!(c == b' ' || g != lcdprobe::glyph(0x01), "no glyph for {:?} in {t}", c as char);
                }
            }
            // The log line says all of it.
            let d = lcdprobe::describe(&s, PINS);
            assert!(d.as_str().starts_with(s.controller.name()), "{}", d.as_str());
            assert!(d.as_str().contains(&std::format!("mode {}", s.mode)));
            assert!(d.as_str().contains(&std::format!("{} MHz", s.hz / 1_000_000)));
        }
        for c in b"0123456789 MHZSETUPOF" {
            assert!(*c == b' ' || lcdprobe::glyph(*c) != lcdprobe::glyph(0x01));
        }
        // The names are distinct controllers' names; each controller has at least one set-up.
        for k in KINDS {
            assert!(SETUPS.iter().any(|s| s.controller == k), "{k:?} is never tried");
        }
    }

    #[test]
    fn banded_drawing_stops_at_the_deadline_with_whole_bands() {
        let cfg = SETUPS[0].config();
        let (w, h) = (cfg.width(), cfg.height());
        // Never stopping: everything.
        let mut p = up(&cfg);
        assert_eq!(lcdprobe::draw_banded(&cfg, &mut p, &mut lcd::Solid(0xff0000), &mut never()), Some(h));
        assert!(p.visible_for(&cfg).iter().all(|&v| v == quantize(cfg.controller, 0xff0000)));
        // Stopping at once: still the first band, so there is always something to see.
        let mut p = up(&cfg);
        p.pixels_written = 0;
        let mut calls = 0;
        let rows = lcdprobe::draw_banded(&cfg, &mut p, &mut lcd::Solid(0x00ff00), &mut || {
            calls += 1;
            true
        });
        assert_eq!(rows, Some(lcdprobe::BAND_ROWS));
        assert_eq!(calls, 1);
        assert_eq!(p.pixels_written, w * lcdprobe::BAND_ROWS);
        // The rows drawn are the top ones, in the colour; the rest is untouched.
        let seen = p.visible_for(&cfg);
        let green = quantize(cfg.controller, 0x00ff00);
        assert!(seen[..w * lcdprobe::BAND_ROWS].iter().all(|&v| v == green));
        assert!(seen[w * lcdprobe::BAND_ROWS..].iter().all(|&v| v == 0));
        // Stopping after three bands.
        let mut p = up(&cfg);
        let mut n = 0;
        let rows = lcdprobe::draw_banded(&cfg, &mut p, &mut lcd::Solid(0x0000ff), &mut || {
            n += 1;
            n > 2
        });
        assert_eq!(rows, Some(3 * lcdprobe::BAND_ROWS));
        // A failing bus is reported, not hidden.
        let mut p = up(&cfg);
        p.fail_at = Some(p.log.len() + 5);
        assert_eq!(lcdprobe::draw_banded(&cfg, &mut p, &mut lcd::Solid(1), &mut never()), None);
        // Every controller draws bands that tile exactly its own screen height.
        for k in KINDS {
            let cfg = Config::new(k);
            let mut p = up(&cfg);
            assert_eq!(lcdprobe::draw_banded(&cfg, &mut p, &mut lcd::Solid(0x123456), &mut never()), Some(cfg.height()));
            assert_eq!(p.pixels_written, cfg.width() * cfg.height());
            assert!(p.errors.is_empty(), "{:?}", p.errors);
        }
    }

    #[test]
    fn probe_options_parse_and_clamp() {
        let parse = |line: &'static str| ProbeOptions::parse(|name| cmdline::value(line.as_bytes(), name));
        let d = parse("milk.lcd=probe");
        assert_eq!(d, ProbeOptions::default());
        assert_eq!((d.rounds, d.hold_ms, d.from, d.pins), (1, HOLD_MS, 1, true));
        assert_eq!(d.count(), SETUPS.len());
        let o = parse("milk.lcd=probe milk.lcd_probe_rounds=3 milk.lcd_probe_ms=150 milk.lcd_probe_from=5 milk.lcd_probe_pins=0");
        assert_eq!((o.rounds, o.hold_ms, o.from, o.pins), (3, 150, 5, false));
        assert_eq!(o.count(), SETUPS_WITHOUT_PIN_ROUND);
        // Out of range is clamped, rubbish ignored.
        let o = parse("milk.lcd_probe_rounds=99 milk.lcd_probe_ms=1 milk.lcd_probe_from=999");
        assert_eq!((o.rounds, o.hold_ms, o.from), (10, 50, SETUPS.len()));
        let o = parse("milk.lcd_probe_rounds=0 milk.lcd_probe_ms=999999 milk.lcd_probe_from=0");
        assert_eq!((o.rounds, o.hold_ms, o.from), (1, 20_000, 1));
        let o = parse("milk.lcd_probe_rounds=x milk.lcd_probe_ms= milk.lcd_probe_from=-4 milk.lcd_probe_pins=maybe");
        assert_eq!((o.rounds, o.hold_ms, o.from, o.pins), (1, HOLD_MS, 1, true));
        // `from` is clamped to the set-ups that exist without the pin round when that is off.
        let o = parse("milk.lcd_probe_pins=0 milk.lcd_probe_from=99");
        assert_eq!(o.from, SETUPS_WITHOUT_PIN_ROUND);
        for v in ["off", "no", "false"] {
            let line = std::format!("milk.lcd_probe_pins={v}");
            assert!(!ProbeOptions::parse(|n| cmdline::value(line.as_bytes(), n)).pins);
        }
        // Phase lengths follow the hold: one second, one second, 0.4 s at the default.
        assert_eq!((lcdprobe::number_ms(1000), lcdprobe::flash_ms(1000)), (1000, 400));
        assert_eq!((lcdprobe::number_ms(HOLD_MS), lcdprobe::flash_ms(HOLD_MS)), (1000, 400));
        assert_eq!(lcdprobe::flash_ms(50), 20);
    }

    /// The time the probe takes on a real board, from the byte counts the model saw: the init waits,
    /// the cards and flashes (held for their time, or as long as the wire needs for the number
    /// card), the text card on the wire and then held. SPI clock exactly as asked.
    fn setup_ms(i: usize, hold: u32) -> u64 {
        let s = SETUPS[i];
        let cfg = s.config();
        let (w, h) = (cfg.width(), cfg.height());
        let mut p = up(&cfg);
        let init_ms = u64::from(p.waited_ms);
        let wire = |bytes: usize| bytes as u64 * 8 * 1000 / u64::from(s.hz);
        // What a full-screen draw costs on the wire, and a one-band one at least.
        let bytes_of = |p: &SimPanel, from: usize| p.log[from..].iter().map(|(_, b)| b.len()).sum::<usize>();
        let (_, mut nc) = number_card(i);
        let before = p.log.len();
        lcdprobe::draw_banded(&cfg, &mut p, &mut nc, &mut never()).unwrap();
        let number_wire = wire(bytes_of(&p, before));
        let (_, mut tc) = text_card(i);
        let before = p.log.len();
        assert!(lcd::draw_rect(&cfg, &mut p, Rect::new(0, 0, w, h), &mut tc));
        let text_wire = wire(bytes_of(&p, before));
        let band = wire(w * lcdprobe::BAND_ROWS * cfg.controller.bytes_per_pixel());
        let n = u64::from(lcdprobe::number_ms(hold));
        let f = u64::from(lcdprobe::flash_ms(hold));
        // A phase lasts its time, or the first band's wire time if that is longer; the number card also
        // stops at its time (banded), so it is never longer than a band beyond it.
        let _ = number_wire;
        init_ms + n.max(band) + 4 * f.max(band) + text_wire + u64::from(hold)
    }

    #[test]
    fn the_whole_probe_is_bounded_and_about_a_minute_and_a_half() {
        let total: u64 = (0..SETUPS.len()).map(|i| setup_ms(i, HOLD_MS)).sum();
        std::eprintln!("probe cycle: {} set-ups, {} ms", SETUPS.len(), total);
        assert!(total < 105_000, "{total} ms");
        assert!(total > 60_000, "{total} ms");
        for i in 0..SETUPS.len() {
            let t = setup_ms(i, HOLD_MS);
            assert!((3_000..6_000).contains(&t), "set-up {} takes {t} ms", i + 1);
        }
        // Without the pin round it is shorter by those set-ups.
        let short: u64 = (0..SETUPS_WITHOUT_PIN_ROUND).map(|i| setup_ms(i, HOLD_MS)).sum();
        assert!(short < total);
        // The qemu run (50 ms holds) is a lot shorter: only the init waits and the wire remain.
        let quick: u64 = (0..SETUPS.len()).map(|i| setup_ms(i, 50)).sum();
        assert!(quick < 40_000, "{quick} ms");
    }

    #[test]
    fn a_failing_bus_gives_up_without_a_panic() {
        let (cfg, mut c) = text_card(0);
        let mut p = SimPanel::new(cfg.controller);
        p.fail_at = Some(3);
        assert!(!lcd::init(&cfg, &mut p));
        let mut p = up(&cfg);
        p.fail_at = Some(p.log.len() + 4);
        assert!(!lcd::draw_rect(&cfg, &mut p, Rect::new(0, 0, cfg.width(), cfg.height()), &mut c));
        // Every set-up's init fails cleanly at every one of its transfers (a short sample of points).
        for s in SETUPS {
            let cfg = s.config();
            let n = up(&cfg).log.len();
            for k in [0, 1, n / 2, n - 1] {
                let mut p = SimPanel::new(cfg.controller);
                p.fail_at = Some(k);
                assert!(!lcd::init(&cfg, &mut p));
            }
        }
    }

    #[test]
    fn setups_have_distinct_streams_where_it_matters() {
        // Two set-ups that differ only in clock, mode or pins send the same bytes (the difference is
        // below the bus), but two controllers never do.
        let stream = |s: &Setup| up(&s.config()).log;
        assert_eq!(stream(&SETUPS[0]), stream(&SETUPS[1]));
        for (i, a) in SETUPS.iter().enumerate() {
            for b in &SETUPS[i + 1..] {
                if a.controller != b.controller {
                    assert_ne!(stream(a), stream(b), "{:?} {:?}", a.controller, b.controller);
                }
            }
        }
    }
}
