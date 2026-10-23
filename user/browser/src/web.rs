//! The page as the rest of the app uses it: user/webengine's Document (browserkit::engine)
//! behind the few calls the Browser makes, with form controls by index (the engine's
//! ControlId is the index into its controls). A page parsed with scripts on runs them
//! through the calls that take a [`ScriptHost`] (js.rs makes one); after any of those the
//! controls may have changed, so the focus is read back from the engine.

use alloc::string::String;
use alloc::vec::Vec;

use browserkit::engine::{
    ConsoleLine, ControlId, ControlKind, DisplayList, Document, FormControl, KeyInput, KeyMods,
    Limits, Link, PageRequest, Rect, Resource, ScriptHost, ScriptLimits, ScriptStats, Submission,
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
    /// Parse a page. `url` is its address (relative links resolve against it). `scripts`:
    /// run its JavaScript.
    pub fn parse(bytes: &[u8], url: &str, content_type: &str, scripts: bool) -> Doc {
        let ct = (!content_type.is_empty()).then_some(content_type);
        Doc {
            d: Document::parse_scripted(
                bytes,
                url,
                ct,
                Limits::default(),
                scripts.then(ScriptLimits::default),
            ),
            laid: None,
        }
    }

    // ---- scripts ----

    /// It runs scripts (and they haven't been stopped).
    pub fn scripting(&self) -> bool {
        self.d.scripting()
    }

    pub fn pending_scripts(&self) -> Vec<String> {
        self.d.pending_scripts()
    }

    pub fn supply_script(&mut self, url: &str, body: Option<&[u8]>) {
        self.d.supply_script(url, body);
    }

    /// Scripts, timers and load events that are due. True if anything ran.
    pub fn run(&mut self, host: &mut dyn ScriptHost) -> bool {
        self.d.run(host)
    }

    pub fn next_wake(&self) -> Option<f64> {
        self.d.next_wake()
    }

    pub fn wants_frame(&self) -> bool {
        self.d.wants_frame()
    }

    pub fn frame(&mut self, host: &mut dyn ScriptHost) -> bool {
        self.d.frame(host)
    }

    /// A click at page point (x, y) with the view at `scroll`: false if a script cancelled
    /// it (checkboxes and radio buttons are toggled by the engine).
    pub fn click(&mut self, host: &mut dyn ScriptHost, x: i32, y: i32, scroll: i32, m: KeyMods) -> bool {
        self.d.click(host, x, y - scroll, (0, scroll), m)
    }

    /// A key for the page: false if a script cancelled it.
    pub fn key(&mut self, host: &mut dyn ScriptHost, k: &KeyInput, up: bool) -> bool {
        self.d.key(host, k, up)
    }

    pub fn focused(&self) -> Option<usize> {
        self.d.focused().map(|c| c.0 as usize)
    }

    pub fn set_focus(&mut self, host: &mut dyn ScriptHost, i: Option<usize>) {
        self.d.set_focus(host, i.map(Self::id));
    }

    /// The user changed control `i` (input / change events).
    pub fn input(&mut self, host: &mut dyn ScriptHost, i: usize) {
        self.d.input(host, Self::id(i));
    }

    /// Pressing `i` with scripts: validation and the submit event first.
    pub fn submit_form(&mut self, host: &mut dyn ScriptHost, i: usize) -> Option<Submission> {
        self.d.submit_form(host, Self::id(i), None)
    }

    pub fn take_requests(&mut self) -> Vec<PageRequest> {
        self.d.take_requests()
    }

    pub fn take_console(&mut self) -> Vec<ConsoleLine> {
        self.d.take_console()
    }

    /// The network requests the page's scripts made or aborted.
    pub fn take_net(&mut self) -> Vec<webengine::NetOut> {
        self.d.take_net()
    }

    /// An answer for a script request.
    pub fn net_event(&mut self, host: &mut dyn ScriptHost, id: u32, ev: webengine::NetEvent) -> bool {
        self.d.net_event(host, id, ev)
    }

    pub fn stop_scripts(&mut self) {
        self.d.stop_scripts();
    }

    pub fn script_stats(&self) -> ScriptStats {
        self.d.script_stats()
    }

    /// Scripts changed the page since it was laid out.
    pub fn needs_layout(&self) -> bool {
        self.d.needs_layout()
    }

    pub fn title(&self) -> String {
        self.d.title()
    }

    pub fn base_url(&self) -> Option<Url> {
        Url::parse(self.d.base_url()).ok()
    }

    /// Lay out `w` x `h` page pixels (cached by the engine for the same size).
    /// `dark`: the desktop is dark (the engine gives pages that allow it its dark defaults).
    pub fn layout(&mut self, w: i32, h: i32, dark: bool, fonts: Fonts) {
        let vp = Viewport {
            dark,
            ..Viewport::new(w.max(1), h.max(1))
        };
        self.d.layout(&vp, &fonts);
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
