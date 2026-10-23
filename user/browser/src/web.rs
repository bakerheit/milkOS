//! The page as the rest of the app uses it: user/webengine's Document (browserkit::engine)
//! behind the few calls the Browser makes, with form controls by index (the engine's
//! ControlId is the index into its controls).

use alloc::string::String;
use alloc::vec::Vec;

use browserkit::engine::{
    ControlId, ControlKind, DisplayList, Document, FormControl, Link, Rect, Resource, Submission,
    Viewport,
};
use browserkit::font::Fonts;
use milkweb::url::Url;

/// Does this control take typing?
pub fn is_text(k: ControlKind) -> bool {
    matches!(
        k,
        ControlKind::Text | ControlKind::Password | ControlKind::TextArea
    )
}

/// Is it a button of some kind?
pub fn is_button(k: ControlKind) -> bool {
    matches!(
        k,
        ControlKind::Submit
            | ControlKind::Reset
            | ControlKind::Button
            | ControlKind::ImageSubmit
            | ControlKind::File
    )
}

pub enum Hit {
    Link(Link),
    Control(usize),
}

pub struct Doc {
    d: Document,
    pub laid: Option<(i32, i32)>,
}

impl Doc {
    /// Parse a page. `url` is its address (relative links resolve against it).
    pub fn parse(bytes: &[u8], url: &str, content_type: &str) -> Doc {
        let ct = (!content_type.is_empty()).then_some(content_type);
        Doc {
            d: Document::parse(bytes, url, ct),
            laid: None,
        }
    }

    pub fn title(&self) -> String {
        self.d.title()
    }

    pub fn base_url(&self) -> Option<Url> {
        Url::parse(self.d.base_url()).ok()
    }

    /// Lay out `w` x `h` page pixels (cached by the engine for the same size).
    pub fn layout(&mut self, w: i32, h: i32, fonts: Fonts) {
        self.d.layout(&Viewport::new(w.max(1), h.max(1)), &fonts);
        self.laid = Some((w, h));
    }

    pub fn display_list(&self) -> &DisplayList {
        self.d.display_list()
    }

    pub fn height(&self) -> i32 {
        self.d.display_list().height
    }

    pub fn controls(&self) -> &[FormControl] {
        self.d.controls()
    }

    pub fn control(&self, i: usize) -> Option<&FormControl> {
        self.d.controls().get(i)
    }

    /// Where control `i` is (page pixels), if it is laid out and shown.
    pub fn control_rect(&self, i: usize) -> Option<Rect> {
        self.control(i).map(|c| c.rect).filter(|r| !r.is_empty())
    }

    fn id(i: usize) -> ControlId {
        ControlId(i as u32)
    }

    /// What is at page point (x, y) when the view is scrolled to `scroll`.
    pub fn hit_test(&self, x: i32, y: i32, scroll: i32) -> Option<Hit> {
        let h = self.d.hit_test(x, y - scroll, (0, scroll));
        if let Some(c) = h.control {
            return Some(Hit::Control(c.0 as usize));
        }
        h.link.map(Hit::Link)
    }

    pub fn anchor(&self, name: &str) -> Option<i32> {
        self.d.fragment_y(name)
    }

    pub fn main_content(&self) -> Option<i32> {
        self.d.main_content().map(|r| r.y)
    }

    /// Find in page: each match's rectangles (a match may wrap across lines).
    pub fn find(&self, q: &str) -> Vec<Vec<Rect>> {
        if q.is_empty() {
            return Vec::new();
        }
        self.d
            .find(q, false)
            .into_iter()
            .take(1000)
            .map(|m| m.rects)
            .collect()
    }

    pub fn set_value(&mut self, i: usize, v: &str) {
        self.d.set_value(Self::id(i), v);
    }

    /// A click on a checkbox (toggles it) or a radio button (checks it).
    pub fn click_check(&mut self, i: usize) {
        let Some(c) = self.control(i) else {
            return;
        };
        if c.disabled {
            return;
        }
        let on = match c.kind {
            ControlKind::Checkbox => !c.checked,
            ControlKind::Radio => true,
            _ => return,
        };
        self.d.set_checked(Self::id(i), on);
    }

    /// The selected option of a select.
    pub fn selected(&self, i: usize) -> usize {
        self.control(i)
            .and_then(|c| c.options.iter().position(|o| o.selected))
            .unwrap_or(0)
    }

    pub fn select(&mut self, i: usize, option: usize) {
        if self
            .control(i)
            .is_some_and(|c| option < c.options.len() && !c.disabled)
        {
            self.d.select_option(Self::id(i), option, true);
        }
    }

    pub fn reset(&mut self, i: usize) {
        self.d.reset(Self::id(i));
    }

    /// Pressing `i` (a submit button, or Enter in a field): what the form sends.
    pub fn submit(&self, i: usize) -> Option<Submission> {
        self.d.submit(Self::id(i), None)
    }

    /// Stylesheets and images still to be fetched.
    pub fn pending(&self) -> Vec<Resource> {
        self.d.pending_resources()
    }

    pub fn supply_stylesheet(&mut self, url: &str, body: Option<&[u8]>) {
        self.d.supply_stylesheet(url, body);
        self.laid = None;
    }

    pub fn supply_image(&mut self, id: browserkit::engine::ImageId, size: Option<(u32, u32)>) {
        self.d.supply_image(id, size);
        self.laid = None;
    }

    pub fn image_url(&self, id: browserkit::engine::ImageId) -> &str {
        self.d.image_url(id)
    }
}
