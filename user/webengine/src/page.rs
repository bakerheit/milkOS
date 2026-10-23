//! A parsed page: ties the stages together. (Stub while the engine is being built.)

use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;

pub struct Page {
    url: String,
    dl: DisplayList,
}

impl Page {
    pub fn parse(_bytes: &[u8], url: &str, _ct: Option<&str>, _l: Limits) -> Page {
        Page {
            url: url.into(),
            dl: DisplayList::default(),
        }
    }
    pub fn parse_str(_html: &str, url: &str, _l: Limits) -> Page {
        Page {
            url: url.into(),
            dl: DisplayList::default(),
        }
    }
    pub fn title(&self) -> String {
        String::new()
    }
    pub fn base_url(&self) -> &str {
        &self.url
    }
    pub fn resolve(&self, href: &str) -> String {
        href.into()
    }
    pub fn favicon(&self) -> Option<String> {
        None
    }
    pub fn meta_refresh(&self) -> Option<(u32, Option<String>)> {
        None
    }
    pub fn truncated(&self) -> bool {
        false
    }
    pub fn pending_resources(&self) -> Vec<Resource> {
        Vec::new()
    }
    pub fn supply_stylesheet(&mut self, _url: &str, _body: Option<&[u8]>) {}
    pub fn supply_image(&mut self, _id: ImageId, _size: Option<(u32, u32)>) {}
    pub fn image_url(&self, _id: ImageId) -> &str {
        ""
    }
    pub fn load_resources(&mut self, _f: &mut dyn Fetcher) {}
    pub fn layout(&mut self, _vp: &Viewport, _m: &dyn TextMeasurer) -> &DisplayList {
        &self.dl
    }
    pub fn display_list(&self) -> &DisplayList {
        &self.dl
    }
    pub fn hit_test(&self, _x: i32, _y: i32, _scroll: (i32, i32)) -> Hit {
        Hit::default()
    }
    pub fn find(&self, _q: &str, _cs: bool) -> Vec<FindMatch> {
        Vec::new()
    }
    pub fn fragment_y(&self, _f: &str) -> Option<i32> {
        None
    }
    pub fn main_content(&self) -> Option<Rect> {
        None
    }
    pub fn controls(&self) -> &[FormControl] {
        &[]
    }
    pub fn set_value(&mut self, _id: ControlId, _v: &str) {}
    pub fn set_checked(&mut self, _id: ControlId, _c: bool) {}
    pub fn select_option(&mut self, _id: ControlId, _i: usize, _s: bool) {}
    pub fn submit(&self, _id: ControlId, _click: Option<(i32, i32)>) -> Option<Submission> {
        None
    }
    pub fn reset(&mut self, _id: ControlId) {}
    pub fn text(&self) -> String {
        String::new()
    }
    pub fn stats(&self) -> Stats {
        Stats::default()
    }
}

impl TextMeasurer for ApproxMeasurer {
    fn text_width(&self, font: &Font, text: &str) -> i32 {
        (text.chars().count() as i32 * font.size as i32) / 2
    }
    fn metrics(&self, font: &Font) -> FontMetrics {
        let s = font.size as i32;
        FontMetrics {
            ascent: s * 4 / 5,
            descent: s / 5,
            line_height: s * 6 / 5,
        }
    }
}
