//! Which framebuffer size to ask the firmware for (docs/raspberry-pi.md, "HDMI displays"). The
//! firmware has set the HDMI mode from config.txt (or the monitor's EDID) before the kernel runs,
//! and the mailbox's "get physical width/height" says what that mode is. The kernel follows it:
//! an 800x480 panel gets an 800x480 desktop. The boot option `milk.fbsize=WxH` forces a size and
//! `milk.fbsize=auto` (the default) follows the firmware. When the firmware's answer says "nothing
//! attached" (0x0, or its fallback 720x576 PAL mode) or is not a size a desktop can use, the
//! kernel falls back to [`FALLBACK`], 1024x768, as it always did.
//!
//! Pure: the mailbox call and the allocation are in the kernel (`rpi/fb.rs`).

/// The size used when the firmware's answer is no use.
pub const FALLBACK: (u32, u32) = (1024, 768);
/// The mode the firmware falls back to with no display attached (720x576, PAL): it means
/// "nothing is plugged in", not "this is the screen".
pub const NO_DISPLAY: (u32, u32) = (720, 576);
/// The smallest and largest framebuffer the desktop is made for.
pub const MIN: (u32, u32) = (640, 400);
pub const MAX: (u32, u32) = (4096, 2160);

/// What `milk.fbsize` asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    /// Follow the firmware's display mode (the default).
    Auto,
    /// This size, whatever the firmware says.
    Forced(u32, u32),
}

/// A size the desktop can use: [`MIN`]..=[`MAX`] in both directions.
pub fn sane(w: u32, h: u32) -> bool {
    w >= MIN.0 && h >= MIN.1 && w <= MAX.0 && h <= MAX.1
}

/// `WxH` (also `W*H`, `WXH`) as two decimal numbers.
pub fn parse_size(v: &[u8]) -> Option<(u32, u32)> {
    let s = core::str::from_utf8(v).ok()?.trim();
    let (w, h) = s.split_once(['x', 'X', '*'])?;
    let num = |t: &str| {
        (!t.is_empty() && t.bytes().all(|b| b.is_ascii_digit()) && t.len() <= 5)
            .then(|| t.parse::<u32>().ok())
            .flatten()
    };
    Some((num(w)?, num(h)?))
}

/// The option's value. `Err` carries what to say about a value that is no use (the caller logs it
/// and follows the firmware instead).
pub fn parse_option(v: Option<&[u8]>) -> Result<Request, &'static str> {
    let Some(v) = v else {
        return Ok(Request::Auto);
    };
    if v.is_empty() || v.eq_ignore_ascii_case(b"auto") {
        return Ok(Request::Auto);
    }
    match parse_size(v) {
        Some((w, h)) if sane(w, h) => Ok(Request::Forced(w, h)),
        Some(_) => Err("milk.fbsize is outside 640x400..4096x2160, using auto"),
        None => Err("milk.fbsize must be WxH (for example 800x480) or auto, using auto"),
    }
}

/// Where the chosen size came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Source {
    /// `milk.fbsize=WxH`.
    Forced,
    /// The firmware's display mode.
    Firmware,
    /// The firmware did not answer.
    NoAnswer,
    /// The firmware says 0x0 or its 720x576 no-display mode: nothing is attached.
    NothingAttached,
    /// The firmware's size is smaller than 640x400 or larger than 4096x2160.
    OutOfRange,
}

impl Source {
    /// A few words for the log.
    pub fn describe(self) -> &'static str {
        match self {
            Source::Forced => "forced by milk.fbsize",
            Source::Firmware => "the firmware's display mode",
            Source::NoAnswer => "fallback: the firmware did not say what the display is",
            Source::NothingAttached => "fallback: the firmware reports no display attached",
            Source::OutOfRange => "fallback: the firmware's size is not one the desktop can use",
        }
    }
}

/// The size to allocate: `req`, the firmware's physical size (`None` if it did not answer).
pub fn choose(req: Request, firmware: Option<(u32, u32)>) -> ((u32, u32), Source) {
    if let Request::Forced(w, h) = req {
        return ((w, h), Source::Forced);
    }
    match firmware {
        None => (FALLBACK, Source::NoAnswer),
        Some((0, _) | (_, 0)) => (FALLBACK, Source::NothingAttached),
        Some(NO_DISPLAY) => (FALLBACK, Source::NothingAttached),
        Some((w, h)) if sane(w, h) => ((w, h), Source::Firmware),
        Some(_) => (FALLBACK, Source::OutOfRange),
    }
}

/// The preferred mode in an EDID base block (the first detailed timing descriptor): `(width,
/// height, pixel clock in kHz)`. `None` if the block is not an EDID (header, checksum) or has no
/// detailed timing first.
pub fn edid_preferred(block: &[u8]) -> Option<(u32, u32, u32)> {
    if block.len() < 128 || block[..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
        return None;
    }
    if block[..128].iter().fold(0u8, |s, &b| s.wrapping_add(b)) != 0 {
        return None;
    }
    let d = &block[54..72];
    let clock = u32::from(u16::from_le_bytes([d[0], d[1]])) * 10;
    if clock == 0 {
        return None;
    }
    let w = u32::from(d[2]) | (u32::from(d[4] & 0xf0) << 4);
    let h = u32::from(d[5]) | (u32::from(d[7] & 0xf0) << 4);
    (w > 0 && h > 0).then_some((w, h, clock))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::vec::Vec;

    #[test]
    fn parse_the_option() {
        assert_eq!(parse_option(None), Ok(Request::Auto));
        assert_eq!(parse_option(Some(b"")), Ok(Request::Auto));
        assert_eq!(parse_option(Some(b"auto")), Ok(Request::Auto));
        assert_eq!(parse_option(Some(b"AUTO")), Ok(Request::Auto));
        assert_eq!(parse_option(Some(b"800x480")), Ok(Request::Forced(800, 480)));
        assert_eq!(parse_option(Some(b"1920X1080")), Ok(Request::Forced(1920, 1080)));
        assert_eq!(parse_option(Some(b"1024*768")), Ok(Request::Forced(1024, 768)));
        assert!(parse_option(Some(b"800x")).is_err());
        assert!(parse_option(Some(b"x480")).is_err());
        assert!(parse_option(Some(b"800")).is_err());
        assert!(parse_option(Some(b"-800x480")).is_err());
        assert!(parse_option(Some(b"800x480x2")).is_err());
        assert!(parse_option(Some(b"0x0")).is_err());
        assert!(parse_option(Some(b"320x240")).is_err()); // too small for the desktop
        assert!(parse_option(Some(b"8192x4320")).is_err()); // too big
        assert!(parse_option(Some(b"99999999x480")).is_err());
    }

    #[test]
    fn choice_table() {
        use Source::*;
        let auto = Request::Auto;
        // (request, what the firmware said, size, source)
        let table: &[(Request, Option<(u32, u32)>, (u32, u32), Source)] = &[
            // The 4-inch HDMI panel, a monitor, a TV, a tall one.
            (auto, Some((800, 480)), (800, 480), Firmware),
            (auto, Some((1024, 768)), (1024, 768), Firmware),
            (auto, Some((1280, 720)), (1280, 720), Firmware),
            (auto, Some((1920, 1080)), (1920, 1080), Firmware),
            (auto, Some((3840, 2160)), (3840, 2160), Firmware),
            (auto, Some((1080, 1920)), (1080, 1920), Firmware),
            // The edges of the sane range.
            (auto, Some((640, 400)), (640, 400), Firmware),
            (auto, Some((4096, 2160)), (4096, 2160), Firmware),
            (auto, Some((639, 480)), FALLBACK_OUT, OutOfRange),
            (auto, Some((640, 399)), FALLBACK_OUT, OutOfRange),
            (auto, Some((4097, 2160)), FALLBACK_OUT, OutOfRange),
            (auto, Some((4096, 2161)), FALLBACK_OUT, OutOfRange),
            // 640x480 is a size a desktop can use.
            (auto, Some((640, 480)), (640, 480), Firmware),
            // Nothing attached: the PAL default, or nothing at all.
            (auto, Some((720, 576)), FALLBACK_OUT, NothingAttached),
            (auto, Some((0, 0)), FALLBACK_OUT, NothingAttached),
            (auto, Some((800, 0)), FALLBACK_OUT, NothingAttached),
            (auto, Some((0, 480)), FALLBACK_OUT, NothingAttached),
            (auto, Some((7680, 4320)), FALLBACK_OUT, OutOfRange),
            (auto, Some((u32::MAX, u32::MAX)), FALLBACK_OUT, OutOfRange),
            (auto, Some((320, 240)), FALLBACK_OUT, OutOfRange),
            (auto, None, FALLBACK_OUT, NoAnswer),
            // Forced: whatever the firmware says.
            (Request::Forced(800, 480), Some((1920, 1080)), (800, 480), Forced),
            (Request::Forced(800, 480), Some((720, 576)), (800, 480), Forced),
            (Request::Forced(800, 480), None, (800, 480), Forced),
            (Request::Forced(1920, 1080), Some((800, 480)), (1920, 1080), Forced),
        ];
        for (req, fw, size, src) in table {
            assert_eq!(choose(*req, *fw), (*size, *src), "{req:?} firmware {fw:?}");
        }
    }

    const FALLBACK_OUT: (u32, u32) = FALLBACK;

    /// An EDID base block with `w`x`h` at `khz` as its first detailed timing.
    fn edid(w: u32, h: u32, khz: u32) -> Vec<u8> {
        let mut b = std::vec![0u8; 128];
        b[..8].copy_from_slice(&[0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0]);
        b[126] = 0;
        let c = (khz / 10) as u16;
        b[54..56].copy_from_slice(&c.to_le_bytes());
        b[56] = (w & 0xff) as u8;
        b[58] = ((w >> 8) as u8) << 4;
        b[59] = (h & 0xff) as u8;
        b[61] = ((h >> 8) as u8) << 4;
        let sum = b[..127].iter().fold(0u8, |s, &x| s.wrapping_add(x));
        b[127] = 0u8.wrapping_sub(sum);
        b
    }

    #[test]
    fn edid_preferred_mode() {
        assert_eq!(edid_preferred(&edid(1920, 1080, 148_500)), Some((1920, 1080, 148_500)));
        assert_eq!(edid_preferred(&edid(800, 480, 33_300)), Some((800, 480, 33_300)));
        // 4096 wide: the high nibble matters.
        assert_eq!(edid_preferred(&edid(3840, 2160, 594_000)), Some((3840, 2160, 594_000)));
        // Not an EDID: wrong header, bad checksum, short, no timing in the first slot.
        let mut bad_header = edid(800, 480, 33_300);
        bad_header[1] = 0;
        assert_eq!(edid_preferred(&bad_header), None);
        let mut bad_sum = edid(800, 480, 33_300);
        bad_sum[10] ^= 1;
        assert_eq!(edid_preferred(&bad_sum), None);
        assert_eq!(edid_preferred(&edid(800, 480, 33_300)[..100]), None);
        assert_eq!(edid_preferred(&[0u8; 128]), None);
        assert_eq!(edid_preferred(&edid(800, 480, 0)), None);
    }

    #[test]
    fn every_source_has_words() {
        for s in [
            Source::Forced,
            Source::Firmware,
            Source::NoAnswer,
            Source::NothingAttached,
            Source::OutOfRange,
        ] {
            assert!(!s.describe().is_empty());
        }
    }
}
