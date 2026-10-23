//! Animations (GIF and animated WebP) one composited frame at a time, and still pictures as a
//! one-frame animation, so a caller can treat every picture the same way:
//!
//! ```ignore
//! let mut a = imgdec::anim::Animation::new(bytes)?;
//! loop {
//!     match a.next_frame() {
//!         Some(Ok(f)) => { show(f.image); wait(f.delay_ms); }
//!         Some(Err(e)) => { /* a damaged frame: stop here */ break; }
//!         None if a.should_loop() => a.rewind(), // then next_frame() again
//!         None => break,
//!     }
//! }
//! ```
//!
//! Only the canvas (and, for GIF's "restore to previous", one saved copy) is kept, so a long
//! animation costs no more memory than one frame.

use alloc::vec::Vec;

use crate::gif::{Dispose, Gif};
use crate::webp::WebP;
use crate::{Format, Image, ImgError, sniff};

/// One frame, composited onto the full canvas.
pub struct Frame<'a> {
    pub image: &'a Image,
    /// How long to show it (GIF's 0 and 10 ms are played as 100 ms, as browsers do).
    pub delay_ms: u32,
    pub index: usize,
}

enum Kind {
    Still,
    Gif(Gif),
    WebP(WebP),
}

pub struct Animation {
    data: Vec<u8>,
    kind: Kind,
    canvas: Image,
    /// GIF disposal 3: the canvas before the current frame.
    saved: Option<Image>,
    /// What to do to the canvas before the next frame: (x, y, w, h, how).
    pending: Option<(usize, usize, usize, usize, Dispose)>,
    next: usize,
    /// How many times the whole animation has been played (rewinds).
    played: u32,
}

impl Animation {
    /// Any picture `decode` knows; GIF and WebP animations play all their frames.
    pub fn new(data: Vec<u8>) -> Result<Animation, ImgError> {
        let (kind, canvas) = match sniff(&data) {
            Format::Gif => {
                let g = Gif::parse(&data)?;
                let c = Image::new(g.w, g.h)?;
                (Kind::Gif(g), c)
            }
            Format::WebP => {
                let w = WebP::parse(&data)?;
                if w.animated {
                    let c = Image::new(w.w, w.h)?;
                    (Kind::WebP(w), c)
                } else {
                    (Kind::Still, w.frame_image(&data, 0)?)
                }
            }
            _ => {
                let img = crate::decode(&data)?;
                (Kind::Still, img)
            }
        };
        Ok(Animation {
            data,
            kind,
            canvas,
            saved: None,
            pending: None,
            next: 0,
            played: 0,
        })
    }

    pub fn width(&self) -> usize {
        self.canvas.w
    }

    pub fn height(&self) -> usize {
        self.canvas.h
    }

    pub fn frame_count(&self) -> usize {
        match &self.kind {
            Kind::Still => 1,
            Kind::Gif(g) => g.frames.len(),
            Kind::WebP(w) => w.frames.len(),
        }
    }

    pub fn is_animated(&self) -> bool {
        self.frame_count() > 1
    }

    /// Times the file asks to be played: 0 = for ever, 1 = once.
    pub fn plays(&self) -> u32 {
        match &self.kind {
            Kind::Still => 1,
            Kind::Gif(g) => g.plays,
            Kind::WebP(w) => w.plays,
        }
    }

    /// Every frame has been shown (next_frame would give None).
    pub fn finished(&self) -> bool {
        self.next >= self.frame_count()
    }

    /// After the last frame: whether to [`rewind`](Self::rewind) and play again.
    pub fn should_loop(&self) -> bool {
        self.is_animated() && (self.plays() == 0 || self.played + 1 < self.plays())
    }

    /// Back to the first frame (counts as one more play).
    pub fn rewind(&mut self) {
        self.next = 0;
        self.played = self.played.saturating_add(1);
        self.pending = None;
        self.saved = None;
        if !matches!(self.kind, Kind::Still) {
            self.canvas.px.fill(0);
        }
    }

    /// The next frame, or None after the last one.
    pub fn next_frame(&mut self) -> Option<Result<Frame<'_>, ImgError>> {
        let i = self.next;
        if i >= self.frame_count() {
            return None;
        }
        self.next += 1;
        if let Err(e) = self.render(i) {
            // Nothing after a damaged frame.
            self.next = self.frame_count();
            return Some(Err(e));
        }
        let delay_ms = match &self.kind {
            Kind::Still => 0,
            Kind::Gif(g) => g.frames[i].delay_ms,
            Kind::WebP(w) => match w.frames[i].duration_ms {
                // Browsers play very short WebP frames at 100 ms too.
                0..=10 => 100,
                d => d,
            },
        };
        Some(Ok(Frame {
            image: &self.canvas,
            delay_ms,
            index: i,
        }))
    }

    fn render(&mut self, i: usize) -> Result<(), ImgError> {
        let (cw, ch) = (self.canvas.w, self.canvas.h);
        if let Some((x, y, w, h, how)) = self.pending.take() {
            match how {
                Dispose::Keep => {}
                Dispose::Background => clear(&mut self.canvas, x, y, w, h),
                Dispose::Previous => {
                    if let Some(s) = self.saved.take() {
                        self.canvas = s;
                    }
                }
            }
        }
        match &self.kind {
            Kind::Still => {}
            Kind::Gif(g) => {
                let f = &g.frames[i];
                if f.dispose == Dispose::Previous {
                    self.saved = Some(self.canvas.clone());
                }
                g.draw_frame(&self.data, i, &mut self.canvas)?;
                self.pending = Some((f.x, f.y, f.w, f.h, f.dispose));
            }
            Kind::WebP(w) => {
                let f = &w.frames[i];
                let img = w.frame_image(&self.data, i)?;
                for yy in 0..f.h {
                    let cy = f.y + yy;
                    if cy >= ch {
                        break;
                    }
                    for xx in 0..f.w {
                        let cx = f.x + xx;
                        if cx >= cw {
                            break;
                        }
                        let src = img.px[yy * f.w + xx];
                        let dst = &mut self.canvas.px[cy * cw + cx];
                        *dst = if f.blend { blend(src, *dst) } else { src };
                    }
                }
                let how = if f.dispose {
                    Dispose::Background
                } else {
                    Dispose::Keep
                };
                self.pending = Some((f.x, f.y, f.w, f.h, how));
            }
        }
        Ok(())
    }
}

fn clear(img: &mut Image, x: usize, y: usize, w: usize, h: usize) {
    let (iw, ih) = (img.w, img.h);
    for yy in y.min(ih)..(y.saturating_add(h)).min(ih) {
        let row = &mut img.px[yy * iw..(yy + 1) * iw];
        row[x.min(iw)..x.saturating_add(w).min(iw)].fill(0);
    }
}

/// `src` over `dst`, straight alpha, the way libwebp's animation decoder blends.
fn blend(src: u32, dst: u32) -> u32 {
    let sa = src >> 24;
    match sa {
        0 => return dst,
        255 => return src,
        _ => {}
    }
    let da = dst >> 24;
    let dfa = (da * (256 - sa)) >> 8;
    let ba = sa + dfa;
    let scale = (1u32 << 24) / ba;
    let ch = |s: u32| {
        let v = ((src >> s) & 0xFF) * sa + ((dst >> s) & 0xFF) * dfa;
        ((v * scale) >> 24) & 0xFF
    };
    ba << 24 | ch(16) << 16 | ch(8) << 8 | ch(0)
}
