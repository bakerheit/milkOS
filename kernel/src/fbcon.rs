//! Text console on a framebuffer: 8x16 cells (or 16x32, 24x48, ... with a scale), cursor, wrapping, scrolling, and the basic ANSI
//! SGR colours (`ESC [ 30..37 m` etc.) plus `ESC [ H` and `ESC [ 2 J`.

use crate::font::{self, GLYPH_H, GLYPH_W};
use crate::gfx::{Color, DrawTarget, Surface, rgb};

/// Padding around the text area, in pixels.
const MARGIN: usize = 8;

pub const DEFAULT_FG: Color = rgb(0xEC, 0xE6, 0xD8); // warm milk white
pub const DEFAULT_BG: Color = rgb(0x16, 0x15, 0x13); // espresso black

/// The 16 ANSI colours: 0..8 normal, 8..16 bright. Softened a little so they sit well on the dark bg.
const PALETTE: [Color; 16] = [
    rgb(0x1E, 0x1D, 0x1A), // black
    rgb(0xCC, 0x55, 0x4F), // red
    rgb(0x7F, 0xB0, 0x69), // green
    rgb(0xD8, 0xB0, 0x5A), // yellow
    rgb(0x5E, 0x8F, 0xCF), // blue
    rgb(0xB0, 0x7A, 0xBC), // magenta
    rgb(0x5F, 0xB3, 0xB0), // cyan
    rgb(0xC9, 0xC4, 0xB8), // white
    rgb(0x6B, 0x68, 0x60), // bright black
    rgb(0xF0, 0x7A, 0x72), // bright red
    rgb(0xA3, 0xD4, 0x8C), // bright green
    rgb(0xF2, 0xD2, 0x7E), // bright yellow
    rgb(0x85, 0xB4, 0xF0), // bright blue
    rgb(0xD2, 0x9E, 0xDE), // bright magenta
    rgb(0x86, 0xD6, 0xD2), // bright cyan
    rgb(0xFF, 0xFC, 0xF2), // bright white
];

enum Parse {
    Ground,
    Esc,
    Csi,
}

pub struct Console {
    /// What is drawn on: the screen itself, or (with a shadow) a buffer in RAM.
    surface: Surface<'static>,
    /// The real screen, when `surface` is a RAM shadow of it: [`flush`](Console::flush) copies the rows
    /// that changed. Reading video memory (scrolling does) is very slow on a board where it is not
    /// cacheable, writing it is not, so the console reads and writes RAM and only writes the screen.
    shown: Option<Surface<'static>>,
    /// Pixel rows `[lo, hi)` changed since the last flush.
    dirty: Option<(usize, usize)>,
    /// Pixel rows `[lo, hi)` copied to `shown` by flushes since [`take_flushed`](Console::take_flushed): what the screen
    /// still has to be told about (the present transform converts only these).
    flushed: Option<(usize, usize)>,
    /// Glyph magnification: a cell is `GLYPH_W * scale` by `GLYPH_H * scale` pixels.
    scale: usize,
    cols: usize,
    rows: usize,
    cx: usize,
    cy: usize,
    fg: Color,
    bg: Color,
    bold: bool,
    reverse: bool,
    fg_idx: Option<u8>,
    bg_idx: Option<u8>,
    cursor_drawn: bool,
    /// Instead of scrolling, the cursor goes back to the top after the last row and each new line
    /// replaces the old one there (for a screen that is slow to redraw, the SPI panel).
    wrap: bool,
    state: Parse,
    params: [u16; 8],
    nparams: usize,
    have_param: bool,
}

impl Console {
    pub fn new(surface: Surface<'static>) -> Self {
        Self::with_scale(surface, 1)
    }

    /// A console whose characters are `scale` times the size of the 8x16 font (1..=4), for
    /// screens that are read from across a room.
    pub fn with_scale(surface: Surface<'static>, scale: usize) -> Self {
        let scale = scale.clamp(1, 4);
        let cols = ((surface.width().saturating_sub(2 * MARGIN)) / (GLYPH_W * scale)).max(1);
        let rows = ((surface.height().saturating_sub(2 * MARGIN)) / (GLYPH_H * scale)).max(1);
        let mut c = Console {
            surface,
            shown: None,
            dirty: None,
            flushed: None,
            scale,
            cols,
            rows,
            cx: 0,
            cy: 0,
            fg: DEFAULT_FG,
            bg: DEFAULT_BG,
            bold: false,
            reverse: false,
            fg_idx: None,
            bg_idx: None,
            cursor_drawn: false,
            wrap: false,
            state: Parse::Ground,
            params: [0; 8],
            nparams: 0,
            have_param: false,
        };
        c.clear();
        c
    }

    /// A console drawn on `shadow` (same size as `shown`, in RAM) and copied to `shown` by [`flush`](Self::flush).
    pub fn with_shadow(shown: Surface<'static>, shadow: Surface<'static>, scale: usize) -> Self {
        let mut c = Self::with_scale(shadow, scale);
        c.shown = Some(shown);
        c.mark(0, c.surface.height());
        c.flush();
        c
    }

    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    fn mark(&mut self, y0: usize, y1: usize) {
        self.dirty = Some(match self.dirty {
            Some((lo, hi)) => (lo.min(y0), hi.max(y1)),
            None => (y0, y1),
        });
    }

    /// Wrap around to the top instead of scrolling (see the field).
    #[cfg_attr(not(feature = "raspi"), allow(dead_code))]
    pub fn set_wrap(&mut self, wrap: bool) {
        self.wrap = wrap;
    }

    /// The pixel rows `[lo, hi)` that changed since the last call, for a console that has no
    /// shadow to flush from (the caller sends them wherever the picture goes).
    #[cfg_attr(not(feature = "raspi"), allow(dead_code))]
    pub fn take_dirty(&mut self) -> Option<(usize, usize)> {
        let (lo, hi) = self.dirty.take()?;
        let h = self.surface.height();
        let (lo, hi) = (lo.min(h), hi.min(h));
        (hi > lo).then_some((lo, hi))
    }

    /// Does this console draw in a RAM shadow and copy to `shown` ([`with_shadow`](Self::with_shadow))?
    #[cfg_attr(not(feature = "raspi"), allow(dead_code))]
    pub fn has_shadow(&self) -> bool {
        self.shown.is_some()
    }

    /// The pixel rows `[lo, hi)` flushed to the screen's buffer since the last call.
    #[cfg_attr(not(feature = "raspi"), allow(dead_code))]
    pub fn take_flushed(&mut self) -> Option<(usize, usize)> {
        self.flushed.take()
    }

    /// Copy the rows that changed to the screen (a no-op without a shadow).
    pub fn flush(&mut self) {
        if self.shown.is_none() {
            return;
        }
        let Some((lo, hi)) = self.dirty.take() else {
            return;
        };
        let (w, h) = (self.surface.width(), self.surface.height());
        let (lo, hi) = (lo.min(h), hi.min(h));
        if let Some(shown) = self.shown.as_mut()
            && hi > lo
        {
            shown.blit(
                0,
                lo as i32,
                &self.surface,
                0,
                lo as i32,
                w as i32,
                (hi - lo) as i32,
            );
            self.flushed = Some(match self.flushed {
                Some((a, b)) => (a.min(lo), b.max(hi)),
                None => (lo, hi),
            });
        }
    }

    /// Clear the screen and home the cursor.
    pub fn clear(&mut self) {
        self.cursor_drawn = false;
        let h = self.surface.height();
        self.mark(0, h);
        self.surface.clear(DEFAULT_BG);
        self.cx = 0;
        self.cy = 0;
        self.toggle_cursor();
    }

    pub fn write_str(&mut self, s: &str) {
        self.erase_cursor();
        for c in s.chars() {
            self.put_char(c);
        }
        self.toggle_cursor();
        // A line is done: show it.
        if s.contains('\n') {
            self.flush();
        }
    }

    fn cw(&self) -> usize {
        GLYPH_W * self.scale
    }

    fn ch(&self) -> usize {
        GLYPH_H * self.scale
    }

    fn cell_xy(&self, col: usize, row: usize) -> (i32, i32) {
        (
            (MARGIN + col * self.cw()) as i32,
            (MARGIN + row * self.ch()) as i32,
        )
    }

    /// Cursor is an inverted block; XORing twice restores the pixels.
    fn toggle_cursor(&mut self) {
        let (x, y) = self.cell_xy(self.cx.min(self.cols - 1), self.cy);
        let (cw, ch) = (self.cw(), self.ch());
        self.mark(y as usize, y as usize + ch);
        let stride = self.surface.stride();
        let px = self.surface.pixels_mut();
        for row in 0..ch {
            let base = (y as usize + row) * stride + x as usize;
            for p in &mut px[base..base + cw] {
                *p ^= 0x00FF_FFFF;
            }
        }
        self.cursor_drawn = !self.cursor_drawn;
    }

    fn erase_cursor(&mut self) {
        if self.cursor_drawn {
            self.toggle_cursor();
        }
    }

    /// Blank the row the cursor is on (wrap mode: the old line goes as the new one starts).
    fn clear_row(&mut self) {
        let (x, y) = self.cell_xy(0, self.cy);
        self.mark(y as usize, y as usize + self.ch());
        self.surface.fill_rect(
            x,
            y,
            (self.cols * self.cw()) as i32,
            self.ch() as i32,
            DEFAULT_BG,
        );
    }

    fn newline(&mut self) {
        self.cx = 0;
        if self.cy + 1 < self.rows {
            self.cy += 1;
            if self.wrap {
                self.clear_row();
            }
        } else if self.wrap {
            self.cy = 0;
            self.clear_row();
        } else {
            let (x, y) = self.cell_xy(0, 0);
            self.mark(MARGIN, MARGIN + self.rows * self.ch());
            self.surface.scroll_up(
                x,
                y,
                (self.cols * self.cw()) as i32,
                (self.rows * self.ch()) as i32,
                self.ch() as i32,
                DEFAULT_BG,
            );
        }
    }

    fn put_char(&mut self, c: char) {
        match self.state {
            Parse::Esc => {
                if c == '[' {
                    self.state = Parse::Csi;
                    self.params = [0; 8];
                    self.nparams = 0;
                    self.have_param = false;
                } else {
                    self.state = Parse::Ground;
                }
                return;
            }
            Parse::Csi => {
                self.csi_char(c);
                return;
            }
            Parse::Ground => {}
        }
        match c {
            '\x1b' => self.state = Parse::Esc,
            '\n' => self.newline(),
            '\r' => self.cx = 0,
            '\x08' => self.cx = self.cx.saturating_sub(1),
            '\t' => {
                let next = (self.cx + 8) & !7;
                while self.cx < next.min(self.cols) {
                    self.draw(' ');
                }
            }
            c if (c as u32) < 0x20 || c == '\x7f' => {}
            c => self.draw(c),
        }
    }

    /// Draw `c` at the cursor and advance, wrapping first if the line is full.
    fn draw(&mut self, c: char) {
        if self.cx >= self.cols {
            self.newline();
        }
        let (x, y) = self.cell_xy(self.cx, self.cy);
        self.mark(y as usize, y as usize + self.ch());
        let (fg, bg) = if self.reverse {
            (self.bg, self.fg)
        } else {
            (self.fg, self.bg)
        };
        font::draw_glyph(&mut self.surface, x, y, c, fg, Some(bg), self.scale as i32);
        self.cx += 1;
    }

    fn csi_char(&mut self, c: char) {
        match c {
            '0'..='9' => {
                if self.nparams < self.params.len() {
                    let p = &mut self.params[self.nparams];
                    *p = p.saturating_mul(10).saturating_add(c as u16 - '0' as u16);
                    self.have_param = true;
                }
            }
            ';' => {
                if self.nparams < self.params.len() {
                    self.nparams += 1;
                }
                self.have_param = false;
            }
            _ => {
                if self.have_param && self.nparams < self.params.len() {
                    self.nparams += 1;
                }
                self.state = Parse::Ground;
                match c {
                    'm' => self.sgr(),
                    'H' | 'f' => {
                        // Rows/cols are 1-based; only "home" is really expected, but honour both.
                        self.cy = (self.params[0].max(1) as usize - 1).min(self.rows - 1);
                        self.cx = (self.params[1].max(1) as usize - 1).min(self.cols - 1);
                    }
                    'J' if self.params[0] == 2 => {
                        let h = self.surface.height();
                        self.mark(0, h);
                        self.surface.clear(DEFAULT_BG);
                        self.cx = 0;
                        self.cy = 0;
                    }
                    'K' => {
                        let (x, y) = self.cell_xy(self.cx.min(self.cols), self.cy);
                        self.mark(y as usize, y as usize + self.ch());
                        let end = MARGIN + self.cols * self.cw();
                        self.surface
                            .fill_rect(x, y, end as i32 - x, self.ch() as i32, DEFAULT_BG);
                    }
                    _ => {}
                }
            }
        }
    }

    fn sgr(&mut self) {
        if self.nparams == 0 {
            self.params[0] = 0;
            self.nparams = 1;
        }
        for i in 0..self.nparams {
            match self.params[i] {
                0 => {
                    self.bold = false;
                    self.reverse = false;
                    self.fg_idx = None;
                    self.bg_idx = None;
                }
                1 => self.bold = true,
                22 => self.bold = false,
                7 => self.reverse = true,
                27 => self.reverse = false,
                n @ 30..=37 => self.fg_idx = Some((n - 30) as u8),
                39 => self.fg_idx = None,
                n @ 40..=47 => self.bg_idx = Some((n - 40) as u8),
                49 => self.bg_idx = None,
                n @ 90..=97 => self.fg_idx = Some((n - 90 + 8) as u8),
                n @ 100..=107 => self.bg_idx = Some((n - 100 + 8) as u8),
                _ => {}
            }
        }
        self.fg = match self.fg_idx {
            Some(i) => PALETTE[(i + if self.bold && i < 8 { 8 } else { 0 }) as usize],
            None => DEFAULT_FG,
        };
        self.bg = self.bg_idx.map_or(DEFAULT_BG, |i| PALETTE[i as usize]);
    }
}
