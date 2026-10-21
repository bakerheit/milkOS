//! Text console on a framebuffer: 8x16 cells, cursor, wrapping, scrolling, and the basic ANSI
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
    surface: Surface<'static>,
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
    state: Parse,
    params: [u16; 8],
    nparams: usize,
    have_param: bool,
}

impl Console {
    pub fn new(surface: Surface<'static>) -> Self {
        let cols = (surface.width() - 2 * MARGIN) / GLYPH_W;
        let rows = (surface.height() - 2 * MARGIN) / GLYPH_H;
        let mut c = Console {
            surface,
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
            state: Parse::Ground,
            params: [0; 8],
            nparams: 0,
            have_param: false,
        };
        c.clear();
        c
    }

    pub fn size(&self) -> (usize, usize) {
        (self.cols, self.rows)
    }

    /// Clear the screen and home the cursor.
    pub fn clear(&mut self) {
        self.cursor_drawn = false;
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
    }

    fn cell_xy(&self, col: usize, row: usize) -> (i32, i32) {
        (
            (MARGIN + col * GLYPH_W) as i32,
            (MARGIN + row * GLYPH_H) as i32,
        )
    }

    /// Cursor is an inverted block; XORing twice restores the pixels.
    fn toggle_cursor(&mut self) {
        let (x, y) = self.cell_xy(self.cx.min(self.cols - 1), self.cy);
        let stride = self.surface.stride();
        let px = self.surface.pixels_mut();
        for row in 0..GLYPH_H {
            let base = (y as usize + row) * stride + x as usize;
            for p in &mut px[base..base + GLYPH_W] {
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

    fn newline(&mut self) {
        self.cx = 0;
        if self.cy + 1 < self.rows {
            self.cy += 1;
        } else {
            let (x, y) = self.cell_xy(0, 0);
            self.surface.scroll_up(
                x,
                y,
                (self.cols * GLYPH_W) as i32,
                (self.rows * GLYPH_H) as i32,
                GLYPH_H as i32,
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
        let (fg, bg) = if self.reverse {
            (self.bg, self.fg)
        } else {
            (self.fg, self.bg)
        };
        font::draw_glyph(&mut self.surface, x, y, c, fg, Some(bg), 1);
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
                        self.surface.clear(DEFAULT_BG);
                        self.cx = 0;
                        self.cy = 0;
                    }
                    'K' => {
                        let (x, y) = self.cell_xy(self.cx.min(self.cols), self.cy);
                        let end = MARGIN + self.cols * GLYPH_W;
                        self.surface
                            .fill_rect(x, y, end as i32 - x, GLYPH_H as i32, DEFAULT_BG);
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
