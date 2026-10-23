//! Drawing for Preview: toolbar, thumbnail strip, the picture (stepped zoom, pan, mini map), cards
//! for PDFs, errors and the empty window, the info panel and the status bar.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use appkit::widgets::{self, Kind};
use appkit::{Rect, Theme};
use gui::{Window, str_width};
use imgdec::Image;
use textbuf::{Buffer, view};

use crate::*;

fn human(n: u64) -> String {
    if n >= 10 << 20 {
        format!("{} MB", n >> 20)
    } else if n >= 1 << 20 {
        format!("{}.{} MB", n >> 20, ((n & 0xFFFFF) * 10) >> 20)
    } else if n >= 10 << 10 {
        format!("{} KB", n >> 10)
    } else if n >= 1 << 10 {
        format!("{}.{} KB", n >> 10, ((n & 0x3FF) * 10) >> 10)
    } else {
        format!("{n} bytes")
    }
}

fn ago(mtime: i64) -> String {
    if mtime <= 0 {
        return String::from("unknown");
    }
    let d = wall_secs() - mtime;
    if d < 60 {
        String::from("just now")
    } else if d < 3600 {
        format!("{} min ago", d / 60)
    } else if d < 86400 {
        format!("{} h ago", d / 3600)
    } else {
        format!("{} days ago", d / 86400)
    }
}

fn zoom_label(z: Zoom) -> &'static str {
    match z {
        Zoom::Fit => "Fit",
        Zoom::P25 => "25%",
        Zoom::P50 => "50%",
        Zoom::P100 => "100%",
        Zoom::P200 => "200%",
    }
}

/// Wrap `text` to `w` pixels of Unifont: the lines.
fn card_lines(text: &str, w: i32) -> Vec<String> {
    let per = (w / CELL).max(8) as usize;
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        for word in para.split(' ') {
            if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > per {
                lines.push(core::mem::take(&mut cur));
            }
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(word);
        }
        lines.push(cur);
    }
    lines
}

const CELL: i32 = 8;

impl App {
    pub fn draw(&mut self) {
        self.th = Theme::current();
        self.hits.clear();
        let th = self.th;
        let (w, h) = (self.win.w, self.win.h);
        self.win.fill(0, 0, w, h, th.oat);
        widgets::toolbar(&mut self.win, &th);
        let (cx, cy, cw, ch) = self.canvas();
        if self.strip_visible() {
            self.draw_strip();
        }
        self.overflow = (false, false);
        // Take the content out so drawing can borrow self freely, then put it back.
        let content = core::mem::replace(&mut self.content, Content::Nothing);
        match &content {
            Content::Nothing => self.draw_card(
                Rect::new(cx, cy, cw, ch),
                "Open a picture",
                "Choose a PNG, JPEG, GIF, WebP, PPM or BMP, or drop one on this window.",
                &[("Open...", Kind::Primary, ID_OPEN, true)],
            ),
            Content::Failed(m) => {
                let body = format!("{}\n{}", self.name, m);
                self.draw_card(
                    Rect::new(cx, cy, cw, ch),
                    "Can not open this image",
                    &body,
                    &[("Open another...", Kind::Secondary, ID_OPEN, true)],
                )
            }
            Content::Pdf {
                info,
                text_view,
                scroll,
            } => {
                if *text_view {
                    self.draw_text_view(Rect::new(cx, cy, cw, ch), &info.text, *scroll);
                } else {
                    let mut body = format!("{}\n", self.name);
                    if info.encrypted {
                        body.push_str("This PDF is encrypted, so its text can't be read.");
                    } else {
                        let pages = match info.pages {
                            Some(1) => String::from("1 page"),
                            Some(n) => format!("{n} pages"),
                            None => String::from("page count unknown"),
                        };
                        body.push_str(&format!(
                            "{pages}, {} characters of text",
                            info.text.chars().count()
                        ));
                        if info.limited {
                            body.push_str("\nSome text may be missing: this PDF uses fonts or compression Preview can't read.");
                        }
                    }
                    let has_text = !info.text.is_empty();
                    self.draw_card(
                        Rect::new(cx, cy, cw, ch),
                        "PDF preview is not available yet",
                        &body,
                        &[
                            ("Show text", Kind::Primary, ID_SHOWTEXT, has_text),
                            ("Save text as...", Kind::Secondary, ID_SAVETEXT, has_text),
                        ],
                    );
                }
            }
            Content::Picture { img, .. } => self.draw_picture(img, Rect::new(cx, cy, cw, ch)),
        }
        self.content = content;
        self.draw_toolbar();
        self.draw_status();
        if self.info_open {
            self.draw_info();
        }
        self.win.commit();
    }

    fn draw_toolbar(&mut self) {
        let th = self.th;
        let w = self.win.w;
        let many = self.files.len() > 1;
        widgets::button(
            &mut self.win,
            &th,
            &mut self.hits,
            Rect::new(12, 8, 24, 24),
            "<",
            Kind::Secondary,
            ID_PREV,
            many,
            true,
        );
        widgets::button(
            &mut self.win,
            &th,
            &mut self.hits,
            Rect::new(40, 8, 24, 24),
            ">",
            Kind::Secondary,
            ID_NEXT,
            many,
            true,
        );
        let is_pic = matches!(self.content, Content::Picture { .. });
        let can_save =
            is_pic || matches!(&self.content, Content::Pdf { info, .. } if !info.text.is_empty());
        // From the right: Save as, Info, Rotate, then the zoom control, while they fit.
        let left_limit = 72;
        let mut x = w - 12;
        let save_w = widgets::button_w("Save as...", true);
        if x - save_w > left_limit + 200 {
            x -= save_w;
            widgets::button(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, 8, save_w, 24),
                "Save as...",
                Kind::Secondary,
                ID_SAVE,
                can_save,
                true,
            );
            x -= 8;
        }
        let info_w = widgets::button_w("Info", true);
        if x - info_w > left_limit + 200 {
            x -= info_w;
            widgets::button(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, 8, info_w, 24),
                "Info",
                Kind::Secondary,
                ID_INFO,
                is_pic,
                true,
            );
            x -= 8;
        }
        let rot_w = widgets::button_w("Rotate", true);
        if x - rot_w > left_limit + 200 {
            x -= rot_w;
            widgets::button(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, 8, rot_w, 24),
                "Rotate",
                Kind::Secondary,
                ID_ROT,
                is_pic,
                true,
            );
            x -= 8;
        }
        let labels: [&str; 5] = [
            zoom_label(ZOOMS[0]),
            zoom_label(ZOOMS[1]),
            zoom_label(ZOOMS[2]),
            zoom_label(ZOOMS[3]),
            zoom_label(ZOOMS[4]),
        ];
        let seg_w: i32 = labels.iter().map(|l| str_width(l, 1) + 28).sum::<i32>() + 4;
        let mut name_limit = x;
        if is_pic && x - seg_w > left_limit {
            x -= seg_w;
            let sel = ZOOMS.iter().position(|z| *z == self.zoom).unwrap_or(0);
            widgets::segmented(
                &mut self.win,
                &th,
                &mut self.hits,
                x,
                6,
                &labels,
                sel,
                ID_ZOOM,
            );
            name_limit = x - 12;
        }
        // The name, and "2 of 4".
        let room = name_limit - 76;
        // Nothing open: the title bar already says Preview.
        if room > 40 && !self.name.is_empty() {
            let mut label = String::from(&*self.name);
            let pos = if self.files.len() > 1 {
                format!("  {} of {}", self.idx + 1, self.files.len())
            } else {
                String::new()
            };
            let pos_w = str_width(&pos, 1);
            label = widgets::fit(&label, (room - pos_w).max(16));
            widgets::bold_x(&mut self.win, 76, 12, &label, th.espresso, 1);
            let lw = str_width(&label, 1) + 1;
            if lw + pos_w <= room {
                self.win.text(76 + lw, 12, &pos, th.cocoa);
            }
        }
    }

    fn draw_status(&mut self) {
        let th = self.th;
        let text = match &self.content {
            Content::Picture { img, fmt, anim } => {
                let (dw, _) = self.display_size(img);
                let fmt = match anim {
                    Some(p) => format!("{fmt}, {} frames", p.anim.frame_count()),
                    None => String::from(*fmt),
                };
                let pct = dw * 100 / img.w.max(1);
                let z = if self.zoom == Zoom::Fit {
                    format!("Fit ({pct}%)")
                } else {
                    String::from(zoom_label(self.zoom))
                };
                format!(
                    "{} x {}   {}   {}   Zoom: {}",
                    img.w,
                    img.h,
                    human(self.size),
                    fmt,
                    z
                )
            }
            Content::Pdf { info, .. } => {
                let pages = info
                    .pages
                    .map_or(String::new(), |n| format!("   {n} pages"));
                format!("PDF   {}{}", human(self.size), pages)
            }
            Content::Failed(_) => String::from("Can not open this file"),
            Content::Nothing => String::from("No picture open"),
        };
        let (w, h) = (self.win.w, self.win.h);
        widgets::status_bar(&mut self.win, &th, &widgets::fit(&text, w - 200));
        let right = match &self.msg {
            Some((m, _)) => m.clone(),
            None => String::from("Space: next   Cmd-0: fit"),
        };
        widgets::text_r(&mut self.win, w - 12, h - 20, &right, th.cocoa, 1);
    }

    fn draw_strip(&mut self) {
        let th = self.th;
        let (_, _, _, ch) = self.canvas();
        self.win.fill(0, BAR + 1, STRIP_W, ch, th.oat);
        widgets::vline(&mut self.win, STRIP_W - 1, BAR + 1, ch, th.sand);
        for i in 0..self.files.len() {
            let y = BAR + 8 + i as i32 * THUMB_STEP - self.strip_scroll;
            if y + THUMB_STEP < BAR || y > BAR + ch + 8 {
                continue;
            }
            let r = Rect::new(14, y, THUMB_W + 4, THUMB_H + 4);
            if i == self.idx {
                self.win
                    .rrect(r.x - 2, r.y - 2, r.w + 4, r.h + 4, 10, th.straw);
            }
            self.win.rrect(r.x, r.y, r.w, r.h, 8, th.milk);
            if let Some(Some(t)) = self.thumbs.get(&i) {
                let (tx, ty) = (
                    r.x + 2 + (THUMB_W - t.w as i32) / 2,
                    r.y + 2 + (THUMB_H - t.h as i32) / 2,
                );
                for yy in 0..t.h {
                    for xx in 0..t.w {
                        let (dx, dy) = (tx + xx as i32, ty + yy as i32);
                        if dy > BAR && dy < BAR + ch + 1 {
                            let p = Image::flatten_pixel(t.px[yy * t.w + xx], th.milk);
                            let stride = self.win.w;
                            self.win.pixels()[(dy * stride + dx) as usize] = p;
                        }
                    }
                }
            } else {
                // No thumbnail (not made yet, too big or not a picture): the kind in a tile.
                let kind = if self.files[i].to_ascii_lowercase().ends_with(".pdf") {
                    "PDF"
                } else {
                    "IMG"
                };
                widgets::text_c(
                    &mut self.win,
                    r.x + r.w / 2,
                    r.y + r.h / 2 - 8,
                    kind,
                    th.cocoa,
                    1,
                );
            }
            let name = widgets::fit(base_of(&self.files[i]), THUMB_W + 4);
            widgets::text_c(
                &mut self.win,
                r.x + r.w / 2,
                r.y + r.h + 6,
                &name,
                th.espresso,
                1,
            );
            // The strip draws under the toolbar's band: clip what scrolled above the strip.
            self.hits.add(
                Rect::new(
                    r.x,
                    r.y.max(BAR + 1),
                    r.w,
                    (r.h + 24).min(BAR + ch - r.y.max(BAR + 1)),
                ),
                ID_THUMB + i as u32,
            );
        }
        // Rows scrolled past the top would paint over the toolbar: redraw its band.
        widgets::toolbar(&mut self.win, &th);
    }

    /// The size the picture is shown at.
    fn display_size(&self, img: &Image) -> (usize, usize) {
        let (_, _, cw, ch) = self.canvas();
        match self.zoom {
            Zoom::Fit => {
                let (w, h) = img.fit_size(cw.max(1) as usize, ch.max(1) as usize);
                if w >= img.w || h >= img.h {
                    (img.w, img.h)
                } else {
                    (w, h)
                }
            }
            Zoom::P25 => ((img.w / 4).max(1), (img.h / 4).max(1)),
            Zoom::P50 => ((img.w / 2).max(1), (img.h / 2).max(1)),
            Zoom::P100 => (img.w, img.h),
            Zoom::P200 => (img.w * 2, img.h * 2),
        }
    }

    fn draw_picture(&mut self, img: &Image, r: Rect) {
        let th = self.th;
        let (dw, dh) = self.display_size(img);
        let (dwi, dhi) = (dw as i32, dh as i32);
        self.overflow = (dwi > r.w, dhi > r.h);
        // Where the picture's top-left is, relative to the canvas.
        if self.pan_reset {
            self.pan = ((r.w - dwi) / 2, (r.h - dhi) / 2);
            self.pan_reset = false;
        }
        let ox = if dwi <= r.w {
            (r.w - dwi) / 2
        } else {
            self.pan.0.clamp(r.w - dwi, 0)
        };
        let oy = if dhi <= r.h {
            (r.h - dhi) / 2
        } else {
            self.pan.1.clamp(r.h - dhi, 0)
        };
        if dwi > r.w {
            self.pan.0 = ox;
        }
        if dhi > r.h {
            self.pan.1 = oy;
        }
        // The source: the original, or a cached smaller copy; 200% repeats pixels on the fly.
        let key = (self.zoom, self.version, dw, dh);
        let scaled: Option<&Image> = if (dw, dh) == (img.w, img.h) || self.zoom == Zoom::P200 {
            None
        } else {
            if self.cache.as_ref().is_none_or(|c| c.0 != key) {
                self.cache = img.scaled(dw, dh).ok().map(|s| (key, s));
            }
            self.cache.as_ref().map(|c| &c.1)
        };
        let src: &Image = scaled.unwrap_or(img);
        let zoom2 = self.zoom == Zoom::P200;
        let (x0, y0) = (r.x + ox, r.y + oy);
        let (vx0, vx1) = (x0.max(r.x), (x0 + dwi).min(r.x + r.w));
        let (vy0, vy1) = (y0.max(r.y), (y0 + dhi).min(r.y + r.h));
        let stride = self.win.w as usize;
        let (c1, c2) = (th.milk, th.sand);
        let px = self.win.pixels();
        for y in vy0..vy1 {
            let sy = (y - y0) as usize;
            let sy = if zoom2 { sy / 2 } else { sy };
            let row = &src.px[sy * src.w..(sy + 1) * src.w];
            for x in vx0..vx1 {
                let sx = (x - x0) as usize;
                let sx = if zoom2 { sx / 2 } else { sx };
                let p = row[sx];
                px[y as usize * stride + x as usize] = if p >> 24 == 255 {
                    p & 0xFF_FFFF
                } else {
                    let bg = if ((x - x0) / 8 + (y - y0) / 8) % 2 == 0 {
                        c1
                    } else {
                        c2
                    };
                    Image::flatten_pixel(p, bg)
                };
            }
        }
        if self.overflow.0 || self.overflow.1 {
            self.draw_minimap(img, r, (ox, oy), (dwi, dhi));
        }
    }

    fn draw_minimap(&mut self, img: &Image, r: Rect, off: (i32, i32), disp: (i32, i32)) {
        let th = self.th;
        if self.mini.as_ref().is_none_or(|m| m.0 != self.version) {
            let (w, h) = img.fit_size(130, 90);
            let (w, h) = (w.min(img.w).max(1), h.min(img.h).max(1));
            self.mini = img.scaled(w, h).ok().map(|m| (self.version, m));
        }
        let Some((_, m)) = &self.mini else { return };
        let (mw, mh) = (m.w as i32, m.h as i32);
        let box_r = Rect::new(
            r.x + r.w - 12 - mw - 4,
            r.y + r.h - 12 - mh - 4,
            mw + 4,
            mh + 4,
        );
        widgets::bordered(&mut self.win, box_r, 6, th.milk, th.sand);
        let stride = self.win.w as usize;
        let px = self.win.pixels();
        for y in 0..m.h {
            for x in 0..m.w {
                px[(box_r.y + 2 + y as i32) as usize * stride
                    + (box_r.x + 2 + x as i32) as usize] =
                    Image::flatten_pixel(m.px[y * m.w + x], th.milk);
            }
        }
        // The visible part of the picture, as a box.
        let sx = mw as f32 / disp.0 as f32;
        let sy = mh as f32 / disp.1 as f32;
        let vx = ((-off.0) as f32 * sx) as i32;
        let vy = ((-off.1) as f32 * sy) as i32;
        let vw = ((r.w.min(disp.0)) as f32 * sx) as i32;
        let vh = ((r.h.min(disp.1)) as f32 * sy) as i32;
        let b = Rect::new(
            box_r.x + 2 + vx.max(0),
            box_r.y + 2 + vy.max(0),
            vw.min(mw),
            vh.min(mh),
        );
        widgets::hline(&mut self.win, b.x, b.y, b.w, th.straw);
        widgets::hline(&mut self.win, b.x, b.y + b.h - 1, b.w, th.straw);
        widgets::vline(&mut self.win, b.x, b.y, b.h, th.straw);
        widgets::vline(&mut self.win, b.x + b.w - 1, b.y, b.h, th.straw);
    }

    fn draw_info(&mut self) {
        let th = self.th;
        let Content::Picture { img, fmt, anim } = &self.content else {
            return;
        };
        let mut rows = alloc::vec![
            ("Size", format!("{} x {} pixels", img.w, img.h)),
            ("File", human(self.size)),
            ("Format", String::from(*fmt)),
            ("Modified", ago(self.mtime)),
        ];
        if let Some(p) = anim {
            let plays = match p.anim.plays() {
                0 => String::from(", loops"),
                1 => String::new(),
                n => format!(", plays {n} times"),
            };
            rows.push(("Frames", format!("{}{plays}", p.anim.frame_count())));
        }
        let (cx, cy, cw, _) = self.canvas();
        let r = Rect::new(
            cx + cw - 12 - 250,
            cy + 12,
            250,
            20 + rows.len() as i32 * 22 + 4,
        );
        widgets::bordered(&mut self.win, r, 12, th.cream, th.sand);
        for (i, (k, v)) in rows.iter().enumerate() {
            let y = r.y + 14 + i as i32 * 22;
            self.win.text(r.x + 14, y, k, th.cocoa);
            widgets::text_r(
                &mut self.win,
                r.right() - 14,
                y,
                &widgets::fit(v, 150),
                th.espresso,
                1,
            );
        }
    }

    /// A card in the middle of the canvas: a title, wrapped text and buttons (id, enabled).
    fn draw_card(
        &mut self,
        area: Rect,
        title: &str,
        body: &str,
        buttons: &[(&str, Kind, u32, bool)],
    ) {
        let th = self.th;
        let card_w = 460.min(area.w - 24).max(200);
        let lines = card_lines(body, card_w - 48);
        let card_h = 28 + 24 + lines.len() as i32 * 18 + 24 + 28 + 28;
        let r = Rect::new(
            area.x + (area.w - card_w) / 2,
            area.y + ((area.h - card_h) / 2).max(8),
            card_w,
            card_h,
        );
        widgets::bordered(&mut self.win, r, 12, th.cream, th.sand);
        widgets::bold_x(
            &mut self.win,
            r.x + 24,
            r.y + 28,
            &widgets::fit(title, card_w - 48),
            th.espresso,
            1,
        );
        for (i, l) in lines.iter().enumerate() {
            self.win
                .text(r.x + 24, r.y + 28 + 24 + i as i32 * 18, l, th.cocoa);
        }
        let mut x = r.x + 24;
        let y = r.bottom() - 28 - 28;
        for (label, kind, id, enabled) in buttons {
            let bw = widgets::button_w(label, false);
            widgets::button(
                &mut self.win,
                &th,
                &mut self.hits,
                Rect::new(x, y, bw, 28),
                label,
                *kind,
                *id,
                *enabled,
                false,
            );
            x += bw + 8;
        }
    }

    fn draw_text_view(&mut self, area: Rect, text: &str, scroll: usize) {
        let th = self.th;
        self.win.fill(area.x, area.y, area.w, area.h, th.milk);
        widgets::button(
            &mut self.win,
            &th,
            &mut self.hits,
            Rect::new(area.x + 12, area.y + 8, 64, 24),
            "Back",
            Kind::Secondary,
            ID_BACK,
            true,
            true,
        );
        self.win.text(
            area.x + 88,
            area.y + 12,
            "Text from the PDF (no layout or fonts)",
            th.cocoa,
        );
        widgets::hline(&mut self.win, area.x, area.y + 40, area.w, th.sand);
        let buf = Buffer::from_bytes_lossy(text.as_bytes());
        let cols = (((area.w - 28) / CELL).max(1)) as usize;
        let rows = view::layout(&buf, cols, true);
        let vis = (((area.h - 48) / 18).max(1)) as usize;
        let top = scroll.min(rows.len().saturating_sub(vis));
        if let Content::Pdf { scroll: s, .. } = &mut self.content {
            *s = top;
        }
        for (i, r) in rows.iter().skip(top).take(vis).enumerate() {
            let line: String = buf
                .line(r.line)
                .chars()
                .skip(r.start)
                .take(r.end - r.start)
                .collect();
            self.win
                .text(area.x + 14, area.y + 48 + i as i32 * 18, &line, th.espresso);
        }
    }
}

#[allow(dead_code)]
fn _unused(_: &Window) {}
