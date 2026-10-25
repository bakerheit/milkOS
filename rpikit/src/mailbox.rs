//! VideoCore mailbox property messages (the "property tags" interface, channel 8).
//!
//! The ARM asks the firmware running on the VideoCore for things it cannot reach itself: the
//! framebuffer, clock rates (the SD controller's base clock), power domains, the board's
//! MAC address and revision. A message is a 16-byte-aligned buffer of little-endian words:
//!
//! ```text
//! word 0   total size in bytes
//! word 1   request code 0; the firmware answers 0x8000_0000 (ok) or 0x8000_0001 (error)
//! tags...  tag id, value buffer size in bytes, request/response word, value words
//!          (the firmware sets bit 31 of the request/response word and the length of what it
//!          wrote in bits 0..30)
//! 0        end tag
//! ```
//!
//! [`Builder`] writes one and [`Reply`] reads the answer, both on a `&mut [u32]` the caller
//! owns (the kernel keeps one DMA-safe page for it). Nothing here touches hardware; the
//! register dance is in the kernel (`rpi/mailbox.rs`). Everything read back is distrusted:
//! offsets and lengths are checked against the buffer, so a hostile answer yields `None`.

/// Property-tag channel (ARM to VideoCore).
pub const CHANNEL_PROPERTY: u32 = 8;

pub const RESPONSE_OK: u32 = 0x8000_0000;
pub const RESPONSE_ERROR: u32 = 0x8000_0001;

/// Tag ids.
pub mod tag {
    pub const GET_FIRMWARE_REVISION: u32 = 0x0000_0001;
    pub const GET_BOARD_MODEL: u32 = 0x0001_0001;
    pub const GET_BOARD_REVISION: u32 = 0x0001_0002;
    pub const GET_BOARD_MAC: u32 = 0x0001_0003;
    pub const GET_BOARD_SERIAL: u32 = 0x0001_0004;
    pub const GET_ARM_MEMORY: u32 = 0x0001_0005;
    pub const GET_VC_MEMORY: u32 = 0x0001_0006;
    pub const GET_POWER_STATE: u32 = 0x0002_0001;
    pub const SET_POWER_STATE: u32 = 0x0002_8001;
    pub const GET_CLOCK_RATE: u32 = 0x0003_0002;
    pub const SET_CLOCK_RATE: u32 = 0x0003_8002;
    pub const GET_MAX_CLOCK_RATE: u32 = 0x0003_0004;
    pub const GET_TEMPERATURE: u32 = 0x0003_0006;
    pub const GET_THROTTLED: u32 = 0x0003_0046;
    pub const NOTIFY_XHCI_RESET: u32 = 0x0003_0058;
    pub const ALLOCATE_BUFFER: u32 = 0x0004_0001;
    pub const RELEASE_BUFFER: u32 = 0x0004_8001;
    pub const BLANK_SCREEN: u32 = 0x0004_0002;
    pub const GET_PHYSICAL_SIZE: u32 = 0x0004_0003;
    pub const TEST_PHYSICAL_SIZE: u32 = 0x0004_4003;
    pub const SET_PHYSICAL_SIZE: u32 = 0x0004_8003;
    pub const GET_VIRTUAL_SIZE: u32 = 0x0004_0004;
    pub const SET_VIRTUAL_SIZE: u32 = 0x0004_8004;
    pub const GET_DEPTH: u32 = 0x0004_0005;
    pub const SET_DEPTH: u32 = 0x0004_8005;
    pub const GET_PIXEL_ORDER: u32 = 0x0004_0006;
    pub const SET_PIXEL_ORDER: u32 = 0x0004_8006;
    pub const GET_ALPHA_MODE: u32 = 0x0004_0007;
    pub const SET_ALPHA_MODE: u32 = 0x0004_8007;
    pub const GET_PITCH: u32 = 0x0004_0008;
    pub const SET_VIRTUAL_OFFSET: u32 = 0x0004_8009;
    pub const GET_EDID_BLOCK: u32 = 0x0003_0020;
}

/// Clock ids (for [`tag::GET_CLOCK_RATE`]).
pub mod clock {
    pub const EMMC: u32 = 1;
    pub const UART: u32 = 2;
    pub const ARM: u32 = 3;
    pub const CORE: u32 = 4;
    pub const EMMC2: u32 = 12;
}

/// Power domain ids (for [`tag::SET_POWER_STATE`]).
pub mod power {
    pub const SD_CARD: u32 = 0;
    pub const UART0: u32 = 1;
    pub const UART1: u32 = 2;
    pub const USB_HCD: u32 = 3;
}

/// Where a tag's values start inside a message buffer (word index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tag {
    /// Word index of the tag id.
    head: usize,
    /// Words in the value buffer.
    words: usize,
}

/// Writes a property message into `buf`.
pub struct Builder<'a> {
    buf: &'a mut [u32],
    pos: usize,
}

impl<'a> Builder<'a> {
    /// Start a message in `buf` (which must be 16-byte aligned in memory when sent).
    pub fn new(buf: &'a mut [u32]) -> Builder<'a> {
        let b = Builder { buf, pos: 2 };
        if b.buf.len() >= 2 {
            b.buf[0] = 0;
            b.buf[1] = 0;
        }
        b
    }

    /// Add a tag with the request words `request`; the firmware's answer may be up to
    /// `response_words` words (the value buffer is the larger of the two). `None` if the message
    /// is full.
    pub fn tag(&mut self, id: u32, request: &[u32], response_words: usize) -> Option<Tag> {
        let words = request.len().max(response_words);
        let end = self.pos.checked_add(3)?.checked_add(words)?;
        // Leave room for the end tag.
        if end + 1 > self.buf.len() {
            return None;
        }
        self.buf[self.pos] = id;
        self.buf[self.pos + 1] = (words * 4) as u32;
        self.buf[self.pos + 2] = 0; // request
        for i in 0..words {
            self.buf[self.pos + 3 + i] = request.get(i).copied().unwrap_or(0);
        }
        let t = Tag {
            head: self.pos,
            words,
        };
        self.pos = end;
        Some(t)
    }

    /// Close the message (end tag, total size). Returns its length in words, or `None` if it
    /// does not fit.
    pub fn finish(self) -> Option<usize> {
        if self.pos + 1 > self.buf.len() {
            return None;
        }
        self.buf[self.pos] = 0;
        let words = self.pos + 1;
        // Padded to 16 bytes, as the firmware likes it.
        let padded = words.next_multiple_of(4).min(self.buf.len());
        for w in &mut self.buf[words..padded] {
            *w = 0;
        }
        self.buf[0] = (words * 4) as u32;
        Some(padded.max(words))
    }
}

/// Reads the firmware's answer in the same buffer.
pub struct Reply<'a> {
    buf: &'a [u32],
}

/// Why an answer was not usable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The firmware did not answer (the response code is still 0) or said "error".
    NotAnswered,
    /// The firmware answered the message with an error code.
    Failed(u32),
    /// The tag was not processed (its response bit is clear).
    TagNotAnswered,
    /// A length or offset in the answer points outside the buffer.
    Malformed,
}

impl<'a> Reply<'a> {
    pub fn new(buf: &'a [u32]) -> Reply<'a> {
        Reply { buf }
    }

    /// Did the firmware accept the message as a whole?
    pub fn check(&self) -> Result<(), Error> {
        match self.buf.get(1).copied() {
            Some(RESPONSE_OK) => Ok(()),
            Some(0) | None => Err(Error::NotAnswered),
            Some(c) => Err(Error::Failed(c)),
        }
    }

    /// The values the firmware wrote for `tag`: the words of its answer (at most the value
    /// buffer). Fails if the tag was not answered or the lengths lie.
    pub fn values(&self, tag: Tag) -> Result<&'a [u32], Error> {
        self.check()?;
        let head = tag.head;
        if head + 3 + tag.words > self.buf.len() {
            return Err(Error::Malformed);
        }
        let ctrl = self.buf[head + 2];
        if ctrl & 0x8000_0000 == 0 {
            return Err(Error::TagNotAnswered);
        }
        let got = ((ctrl & 0x7fff_ffff) as usize).div_ceil(4);
        let n = got.min(tag.words);
        Ok(&self.buf[head + 3..head + 3 + n])
    }

    /// One answer word (index `i` of the tag's values).
    pub fn word(&self, tag: Tag, i: usize) -> Result<u32, Error> {
        self.values(tag)?.get(i).copied().ok_or(Error::Malformed)
    }
}

/// The ARM-side address word the kernel writes to the mailbox: the 16-byte-aligned bus address
/// of the message ORed with the channel.
pub fn mail_word(bus_addr: u32, channel: u32) -> Option<u32> {
    if bus_addr & 0xf != 0 || channel > 0xf {
        return None;
    }
    Some(bus_addr | channel)
}

/// Split a mailbox word read back into (data address, channel).
pub fn split_mail(word: u32) -> (u32, u32) {
    (word & !0xf, word & 0xf)
}

/// The bus aliases of ARM RAM worth trying for a message buffer, in order of preference. On the BCM2711 the
/// VideoCore reaches the first GiB of ARM RAM at 0xc000_0000 + physical (the alias Linux uses; no VideoCore
/// L2 caching), 0x4000_0000 + physical (the L2 allocating alias), and, as older chips did, 0x8000_0000 and
/// 0x0000_0000. All four name the same bytes; they differ in the VideoCore's caching of them, which is why
/// the first is preferred and the rest are only for a board where the first is not answered.
pub const BUS_ALIASES: [u32; 4] = [0xc000_0000, 0x4000_0000, 0x8000_0000, 0x0000_0000];

/// How a word read from mailbox 0 relates to the word we sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Echo {
    /// Same channel, same address.
    Exact,
    /// Same channel, same buffer under another alias (the top two address bits differ): the firmware
    /// may hand the address back without the alias bits, or an earlier attempt's answer arrives late.
    SameBuffer,
    /// Same channel, a different buffer.
    OtherBuffer,
    /// Another channel.
    OtherChannel,
}

/// Classify `got` (read from the mailbox) against `sent` (written to it).
pub fn echo(got: u32, sent: u32) -> Echo {
    let (ga, gc) = split_mail(got);
    let (sa, sc) = split_mail(sent);
    if gc != sc {
        Echo::OtherChannel
    } else if ga == sa {
        Echo::Exact
    } else if ga & 0x3fff_ffff == sa & 0x3fff_ffff {
        Echo::SameBuffer
    } else {
        Echo::OtherBuffer
    }
}

/// The ARM physical address behind a VideoCore bus address (the framebuffer the firmware
/// returns is a bus address: the top two bits select an alias of RAM, the rest is the
/// address).
pub fn bus_to_phys(bus: u32) -> u32 {
    bus & 0x3fff_ffff
}

/// A framebuffer as the firmware describes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    pub phys: u32,
    pub size: u32,
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub pitch: u32,
    /// Pixel order as the firmware reports it: 0 = BGR, 1 = RGB.
    pub order: u32,
}

/// Build the message that sets a `w` x `h` x `depth` framebuffer (virtual size = physical size,
/// RGB pixel order 0 = BGR so a little-endian `0x00RRGGBB` word is red-green-blue as drawn) and
/// allocates it with 16-byte alignment. Returns the tags to read the answer with.
pub fn framebuffer_request(buf: &mut [u32], w: u32, h: u32, depth: u32) -> Option<(usize, FbTags)> {
    let mut b = Builder::new(buf);
    let physical = b.tag(tag::SET_PHYSICAL_SIZE, &[w, h], 2)?;
    let virtual_ = b.tag(tag::SET_VIRTUAL_SIZE, &[w, h], 2)?;
    let depth_t = b.tag(tag::SET_DEPTH, &[depth], 1)?;
    let order = b.tag(tag::SET_PIXEL_ORDER, &[0], 1)?;
    let alpha = b.tag(tag::SET_ALPHA_MODE, &[2], 1)?; // 2 = ignored (the X in XRGB)
    let offset = b.tag(tag::SET_VIRTUAL_OFFSET, &[0, 0], 2)?;
    let alloc = b.tag(tag::ALLOCATE_BUFFER, &[16], 2)?;
    let pitch = b.tag(tag::GET_PITCH, &[], 1)?;
    let n = b.finish()?;
    Some((
        n,
        FbTags {
            physical,
            virtual_,
            depth: depth_t,
            order,
            alpha,
            offset,
            alloc,
            pitch,
        },
    ))
}

/// The tags of a [`framebuffer_request`].
#[derive(Clone, Copy, Debug)]
pub struct FbTags {
    pub physical: Tag,
    pub virtual_: Tag,
    pub depth: Tag,
    pub order: Tag,
    pub alpha: Tag,
    pub offset: Tag,
    pub alloc: Tag,
    pub pitch: Tag,
}

/// Read the answer to a [`framebuffer_request`]. Rejects an answer that is not a usable
/// framebuffer (zero size or pitch, an address of 0, a pitch smaller than a row, a buffer smaller
/// than pitch * height, a depth that is not 32).
pub fn framebuffer_reply(buf: &[u32], t: &FbTags) -> Result<Framebuffer, Error> {
    let r = Reply::new(buf);
    let width = r.word(t.physical, 0)?;
    let height = r.word(t.physical, 1)?;
    let depth = r.word(t.depth, 0)?;
    let order = r.word(t.order, 0)?;
    let bus = r.word(t.alloc, 0)?;
    let size = r.word(t.alloc, 1)?;
    let pitch = r.word(t.pitch, 0)?;
    let phys = bus_to_phys(bus);
    if width == 0 || height == 0 || width > 16384 || height > 16384 {
        return Err(Error::Malformed);
    }
    if depth != 32 {
        return Err(Error::Malformed);
    }
    if phys == 0 || pitch < width.saturating_mul(4) {
        return Err(Error::Malformed);
    }
    if (size as u64) < pitch as u64 * height as u64 {
        return Err(Error::Malformed);
    }
    Ok(Framebuffer {
        phys,
        size,
        width,
        height,
        depth,
        pitch,
        order,
    })
}
