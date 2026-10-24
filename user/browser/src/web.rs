//! The page as the rest of the app uses it: user/webengine's Document (browserkit::engine)
//! behind the few calls the Browser makes, with form controls by index (the engine's
//! ControlId is the index into its controls). A page parsed with scripts on runs them
//! through the calls that take a [`ScriptHost`] (js.rs makes one); after any of those the
//! controls may have changed, so the focus is read back from the engine.
//!
//! **Frames.** A page may hold `<iframe>`s, each a document of its own (the engine keeps them in
//! the top document's table: `webengine::api` "Frames"). The calls here that take a control index
//! use *combined* indexes: `frame << 20 | control`, so the top page's controls are numbered as
//! they always were and a frame's are unique across the page. [`join`] and [`split`] convert.
//! A call that runs a frame's scripts takes the host *of that frame* (js.rs builds one per frame).

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use browserkit::engine::{
    ConsoleLine, ControlId, ControlKind, DisplayList, Document, FormControl, KeyInput, KeyMods,
    Limits, Link, PageRequest, Rect, Resource, ScriptHost, ScriptLimits, ScriptStats, Submission,
    Viewport,
};
use browserkit::font::Fonts;
use milkweb::url::Url;
use webengine::{FrameId, FrameSec, FrameSlot, Item, Message, NetOut};

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

/// The script heap one frame may use (bytes).
pub const FRAME_HEAP: usize = 16 << 20;
/// All the frames of a tab together may use this much (bytes); past it the greediest is stopped.
pub const FRAMES_HEAP: usize = 96 << 20;

/// Bits of a combined control index that are the control's own.
const CTL_BITS: u32 = 20;

/// A control of frame `f` as the app numbers it.
pub fn join(f: FrameId, control: usize) -> usize {
    ((f.0 as usize) << CTL_BITS) | (control & ((1 << CTL_BITS) - 1))
}

/// The frame and the engine's control index of a combined one.
pub fn split(i: usize) -> (FrameId, usize) {
    (FrameId((i >> CTL_BITS) as u32), i & ((1 << CTL_BITS) - 1))
}

pub enum Hit {
    Link(Link, FrameId),
    Control(usize),
}

pub struct Doc {
    /// Boxed: the frames' scripts find the top document through its address (script/hub.rs),
    /// so it must not move.
    d: Box<Document>,
    pub laid: Option<(i32, i32)>,
}

impl Doc {
    /// Parse a page. `url` is its address (relative links resolve against it). `scripts`:
    /// run its JavaScript.
    pub fn parse(bytes: &[u8], url: &str, content_type: &str, scripts: bool) -> Doc {
        let ct = (!content_type.is_empty()).then_some(content_type);
        Doc {
            d: Box::new(Document::parse_scripted(
                bytes,
                url,
                ct,
                Limits::default(),
                scripts.then(ScriptLimits::default),
            )),
            laid: None,
        }
    }

    /// A document for a frame (not attached yet): like [`Doc::parse`], as a bare engine
    /// document.
    pub fn parse_frame(bytes: &[u8], url: &str, content_type: &str, scripts: bool) -> Document {
        let ct = (!content_type.is_empty()).then_some(content_type);
        Document::parse_scripted(
            bytes,
            url,
            ct,
            Limits::default(),
            scripts.then(|| ScriptLimits {
                // A frame gets a quarter of what a page does; with 32 frames the tab as a whole
                // is watched too ([`crate::frames`]).
                memory: FRAME_HEAP,
                // (Frames are compiled one at a time, but they are many.)
                compile: 64 << 20,
                ..ScriptLimits::default()
            }),
        )
    }

    // ---- frames ----

    /// Every frame slot of the page, parents before children.
    pub fn slots(&mut self) -> Vec<FrameSlot> {
        self.d.frame_slots()
    }

    /// The frames that have a document (not the top page).
    pub fn frame_ids(&self) -> Vec<FrameId> {
        self.d.frame_ids()
    }

    /// The top page and the frames with documents.
    pub fn all_ids(&self) -> Vec<FrameId> {
        let mut v = alloc::vec![FrameId::TOP];
        v.extend(self.d.frame_ids());
        v
    }

    pub fn attach(&mut self, id: FrameId, doc: Document, generation: Option<u32>) -> bool {
        self.laid = None;
        self.d.attach_frame(id, doc, generation)
    }

    pub fn detach(&mut self, id: FrameId) {
        self.laid = None;
        let _ = self.d.detach_frame(id);
    }

    pub fn take_closed(&mut self) -> Vec<FrameId> {
        self.d.take_closed_frames()
    }

    pub fn frame(&self, id: FrameId) -> Option<&Document> {
        self.d.frame_doc(id)
    }

    pub fn frame_mut(&mut self, id: FrameId) -> Option<&mut Document> {
        self.d.frame_doc_mut(id)
    }

    pub fn viewport_of(&self, id: FrameId) -> Option<Viewport> {
        self.d.frame_viewport(id)
    }

    pub fn frame_at(&self, x: i32, y: i32, scroll: i32) -> FrameId {
        self.d.frame_at(x, y - scroll, (0, scroll))
    }

    pub fn children(&self, of: FrameId) -> Vec<FrameId> {
        self.d.child_frames(of)
    }

    pub fn frames_named(&self, name: &str) -> Vec<FrameId> {
        self.d.frames_named(name)
    }

    pub fn window_name(&self, id: FrameId) -> String {
        self.d
            .frame_doc(id)
            .map(|d| String::from(d.window_name()))
            .unwrap_or_default()
    }

    pub fn set_security(&mut self, id: FrameId, sec: FrameSec) {
        if let Some(d) = self.d.frame_doc_mut(id) {
            d.set_frame_security(sec);
        }
    }

    pub fn origin_of(&self, id: FrameId) -> Option<String> {
        self.d.frame_origin(id)
    }

    pub fn set_load_hold(&mut self, id: FrameId, hold: bool) {
        if let Some(d) = self.d.frame_doc_mut(id) {
            d.set_load_hold(hold);
        }
    }

    pub fn is_loaded(&self, id: FrameId) -> bool {
        self.d.frame_doc(id).is_some_and(|d| d.is_loaded())
    }

    pub fn fire_frame_load(&mut self, host: &mut dyn ScriptHost, id: FrameId) -> bool {
        self.d.fire_frame_load(host, id)
    }

    /// The frame with the keyboard.
    pub fn focused_frame(&self) -> FrameId {
        self.d.focused_frame()
    }

    pub fn set_focused_frame(&mut self, id: FrameId) {
        self.d.set_focused_frame(id);
    }

    /// Scroll offset of a frame's document.
    pub fn frame_scroll(&self, id: FrameId) -> (i32, i32) {
        self.d.frame_scroll(id).unwrap_or((0, 0))
    }

    pub fn set_frame_scroll(&mut self, id: FrameId, y: i32) -> i32 {
        self.d.set_frame_scroll(id, 0, y).1
    }

    /// The wheel over the page at view point (x, y): frames under the pointer scroll first;
    /// what is left (dy) is the page's.
    pub fn wheel(&mut self, x: i32, y: i32, scroll: i32, dy: i32) -> i32 {
        self.d.scroll_frames_at(x, y - scroll, (0, scroll), 0, dy).1
    }

    /// Scroll so that `rect` of frame `id` is in view; its place in the page.
    pub fn reveal(&mut self, id: FrameId, rect: Rect) -> Rect {
        self.d.reveal(id, rect)
    }

    pub fn frame_fragment(&mut self, id: FrameId, frag: &str) -> bool {
        self.d.frame_fragment(id, frag)
    }

    /// The controls of the page and its frames in Tab order, as combined indexes.
    pub fn focus_order(&self) -> Vec<usize> {
        self.d
            .focus_order()
            .into_iter()
            .map(|(f, c)| join(f, c.0 as usize))
            .collect()
    }

    /// Messages the frames' scripts posted, where they go: (target frame, message).
    pub fn messages(&mut self) -> Vec<(FrameId, Message)> {
        let mut out = Vec::new();
        for (from, m) in self.d.take_all_messages() {
            if let Some(x) = self.d.route_message(from, &m) {
                out.push(x);
            }
        }
        out
    }

    pub fn deliver(&mut self, host: &mut dyn ScriptHost, to: FrameId, m: &Message) -> bool {
        match self.d.frame_doc_mut(to) {
            Some(d) => d.deliver_message(host, m),
            None => false,
        }
    }

    // ---- scripts ----

    /// The top page runs scripts (and they haven't been stopped).
    pub fn scripting(&self) -> bool {
        self.d.scripting()
    }

    /// Frame `id`'s document runs scripts.
    pub fn scripting_in(&self, id: FrameId) -> bool {
        self.d.frame_doc(id).is_some_and(|d| d.scripting())
    }

    /// Any document of the page runs scripts.
    pub fn any_scripting(&self) -> bool {
        self.all_ids().into_iter().any(|f| self.scripting_in(f))
    }

    pub fn pending_scripts(&self, id: FrameId) -> Vec<String> {
        self.d
            .frame_doc(id)
            .map(|d| d.pending_scripts())
            .unwrap_or_default()
    }

    pub fn supply_script(&mut self, id: FrameId, url: &str, body: Option<&[u8]>) {
        if let Some(d) = self.d.frame_doc_mut(id) {
            d.supply_script(url, body);
        }
    }

    /// Scripts, timers and load events of frame `id` that are due. True if anything ran.
    pub fn run(&mut self, id: FrameId, host: &mut dyn ScriptHost) -> bool {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.run(host),
            None => false,
        }
    }

    pub fn next_wake(&self) -> Option<f64> {
        let mut best: Option<f64> = None;
        for id in self.all_ids() {
            if let Some(w) = self.d.frame_doc(id).and_then(|d| d.next_wake()) {
                best = Some(best.map_or(w, |b| b.min(w)));
            }
        }
        best
    }

    pub fn wants_frame(&self) -> bool {
        self.all_ids()
            .into_iter()
            .any(|f| self.d.frame_doc(f).is_some_and(|d| d.wants_frame()))
    }

    pub fn frame_anim(&mut self, id: FrameId, host: &mut dyn ScriptHost) -> bool {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.frame(host),
            None => false,
        }
    }

    pub fn wants_frame_in(&self, id: FrameId) -> bool {
        self.d.frame_doc(id).is_some_and(|d| d.wants_frame())
    }

    /// A click at point (x, y) of frame `id`'s own viewport (`scroll`: its scroll offset):
    /// false if a script cancelled it.
    pub fn click_in(
        &mut self,
        id: FrameId,
        host: &mut dyn ScriptHost,
        x: i32,
        y: i32,
        scroll: (i32, i32),
        m: KeyMods,
    ) -> bool {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.click(host, x, y, scroll, m),
            None => true,
        }
    }

    /// A key for frame `id`: false if a script cancelled it.
    pub fn key(&mut self, id: FrameId, host: &mut dyn ScriptHost, k: &KeyInput, up: bool) -> bool {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.key(host, k, up),
            None => true,
        }
    }

    /// The control with the keyboard, as a combined index: in the frame that has it.
    pub fn focused(&self) -> Option<usize> {
        let f = self.d.focused_frame();
        self.d
            .frame_doc(f)
            .and_then(|d| d.focused())
            .map(|c| join(f, c.0 as usize))
    }

    /// Give control `i` (a combined index) the focus; None: take it away from the frame that
    /// has it. `host` is the host of the frame `i` is in (or, for None, of the focused one).
    pub fn set_focus(&mut self, host: &mut dyn ScriptHost, i: Option<usize>) {
        match i {
            Some(i) => {
                let (f, c) = split(i);
                if let Some(d) = self.d.frame_doc_mut(f) {
                    d.set_focus(host, Some(ControlId(c as u32)));
                }
                self.d.set_focused_frame(f);
            }
            None => {
                let f = self.d.focused_frame();
                if let Some(d) = self.d.frame_doc_mut(f) {
                    d.set_focus(host, None);
                }
            }
        }
    }

    /// The user changed control `i` (input / change events); `host`: that frame's.
    pub fn input(&mut self, host: &mut dyn ScriptHost, i: usize) {
        let (f, c) = split(i);
        if let Some(d) = self.d.frame_doc_mut(f) {
            d.input(host, ControlId(c as u32));
        }
    }

    /// Pressing `i` with scripts: validation and the submit event first.
    pub fn submit_form(&mut self, host: &mut dyn ScriptHost, i: usize) -> Option<Submission> {
        let (f, c) = split(i);
        self.d
            .frame_doc_mut(f)?
            .submit_form(host, ControlId(c as u32), None)
    }

    pub fn take_requests(&mut self, id: FrameId) -> Vec<PageRequest> {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.take_requests(),
            None => Vec::new(),
        }
    }

    pub fn take_console(&mut self, id: FrameId) -> Vec<ConsoleLine> {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.take_console(),
            None => Vec::new(),
        }
    }

    /// The network requests the page's scripts made or aborted.
    pub fn take_net(&mut self, id: FrameId) -> Vec<NetOut> {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.take_net(),
            None => Vec::new(),
        }
    }

    /// An answer for a script request of frame `id`.
    pub fn net_event(
        &mut self,
        id: FrameId,
        host: &mut dyn ScriptHost,
        req: u32,
        ev: webengine::NetEvent,
    ) -> bool {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.net_event(host, req, ev),
            None => false,
        }
    }

    /// What frame `id`'s scripts asked of the app about their workers.
    pub fn take_workers(&mut self, id: FrameId) -> Vec<webengine::WorkerOut> {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.take_workers(),
            None => Vec::new(),
        }
    }

    /// Something happened in worker `worker` of frame `id`.
    pub fn worker_event(
        &mut self,
        id: FrameId,
        host: &mut dyn ScriptHost,
        worker: u32,
        ev: webengine::WorkerEvent,
    ) -> bool {
        match self.d.frame_doc_mut(id) {
            Some(d) => d.worker_event(host, worker, ev),
            None => false,
        }
    }

    pub fn stop_scripts_in(&mut self, id: FrameId) {
        if let Some(d) = self.d.frame_doc_mut(id) {
            d.stop_scripts();
        }
    }

    /// Stop every document's scripts.
    pub fn stop_scripts(&mut self) {
        for id in self.all_ids() {
            self.stop_scripts_in(id);
        }
    }

    pub fn script_stats(&self) -> ScriptStats {
        self.d.script_stats()
    }

    pub fn script_stats_in(&self, id: FrameId) -> ScriptStats {
        self.d
            .frame_doc(id)
            .map(|d| d.script_stats())
            .unwrap_or_default()
    }

    /// Scripts changed the page (or a frame) since it was laid out.
    pub fn needs_layout(&self) -> bool {
        self.all_ids()
            .into_iter()
            .any(|f| self.d.frame_doc(f).is_some_and(|d| d.needs_layout()))
    }

    pub fn title(&self) -> String {
        self.d.title()
    }

    pub fn base_url(&self) -> Option<Url> {
        Url::parse(self.d.base_url()).ok()
    }

    pub fn base_url_of(&self, id: FrameId) -> Option<Url> {
        self.d
            .frame_doc(id)
            .and_then(|d| Url::parse(d.base_url()).ok())
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

    /// The top page's controls only.
    pub fn controls(&self) -> &[FormControl] {
        self.d.controls()
    }

    /// Control `i` (a combined index).
    pub fn control(&self, i: usize) -> Option<&FormControl> {
        let (f, c) = split(i);
        self.d.frame_doc(f)?.controls().get(c)
    }

    /// All the controls of frame `f`'s document.
    pub fn controls_in(&self, f: FrameId) -> &[FormControl] {
        self.d.frame_doc(f).map_or(&[], |d| d.controls())
    }

    /// Where control `i` is in the page (page pixels), if it is laid out and shown. A frame's
    /// control is found in the display list (where it was drawn, scrolled with its frame).
    pub fn control_rect(&self, i: usize) -> Option<Rect> {
        let (f, c) = split(i);
        if f.is_top() {
            return self.control(i).map(|c| c.rect).filter(|r| !r.is_empty());
        }
        let list = self.d.display_list();
        list.entries.iter().find_map(|e| match e.item {
            Item::Control { rect, control } if e.frame == f && control.0 as usize == c => {
                Some(rect)
            }
            _ => None,
        })
    }

    /// What is at page point (x, y) when the view is scrolled to `scroll`. A frame's links
    /// and controls are the frame's: the hit says which.
    pub fn hit_test(&self, x: i32, y: i32, scroll: i32) -> Option<Hit> {
        let h = self.d.hit_test(x, y - scroll, (0, scroll));
        if let Some(c) = h.control {
            return Some(Hit::Control(join(h.frame, c.0 as usize)));
        }
        h.link.map(|l| Hit::Link(l, h.frame))
    }

    /// The frame under the pointer and where the pointer is in it: (frame, point, scroll).
    pub fn pointer(&self, x: i32, y: i32, scroll: i32) -> (FrameId, (i32, i32), (i32, i32)) {
        let h = self.d.hit_test(x, y - scroll, (0, scroll));
        (h.frame, h.point, h.scroll)
    }

    pub fn anchor(&self, name: &str) -> Option<i32> {
        self.d.fragment_y(name)
    }

    pub fn main_content(&self) -> Option<i32> {
        self.d.main_content().map(|r| r.y)
    }

    /// Find in page: each match's rectangles (a match may wrap across lines), and the frame
    /// and its own rectangles.
    pub fn find(&self, q: &str) -> Vec<(Vec<Rect>, FrameId, Vec<Rect>)> {
        if q.is_empty() {
            return Vec::new();
        }
        self.d
            .find(q, false)
            .into_iter()
            .take(1000)
            .map(|m| (m.rects, m.frame, m.frame_rects))
            .collect()
    }

    pub fn set_value(&mut self, i: usize, v: &str) {
        let (f, c) = split(i);
        if let Some(d) = self.d.frame_doc_mut(f) {
            d.set_value(ControlId(c as u32), v);
        }
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
        let (f, k) = split(i);
        if let Some(d) = self.d.frame_doc_mut(f) {
            d.set_checked(ControlId(k as u32), on);
        }
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
            let (f, k) = split(i);
            if let Some(d) = self.d.frame_doc_mut(f) {
                d.select_option(ControlId(k as u32), option, true);
            }
        }
    }

    pub fn reset(&mut self, i: usize) {
        let (f, k) = split(i);
        if let Some(d) = self.d.frame_doc_mut(f) {
            d.reset(ControlId(k as u32));
        }
    }

    /// Pressing `i` (a submit button, or Enter in a field): what the form sends.
    pub fn submit(&self, i: usize) -> Option<Submission> {
        let (f, k) = split(i);
        self.d.frame_doc(f)?.submit(ControlId(k as u32), None)
    }

    /// Stylesheets and images frame `id`'s document still needs fetched.
    pub fn pending(&self, id: FrameId) -> Vec<Resource> {
        self.d
            .frame_doc(id)
            .map(|d| d.pending_resources())
            .unwrap_or_default()
    }

    pub fn supply_stylesheet(&mut self, id: FrameId, url: &str, body: Option<&[u8]>) {
        if let Some(d) = self.d.frame_doc_mut(id) {
            d.supply_stylesheet(url, body);
        }
        self.laid = None;
    }

    pub fn supply_image(
        &mut self,
        id: FrameId,
        img: browserkit::engine::ImageId,
        size: Option<(u32, u32)>,
    ) {
        if let Some(d) = self.d.frame_doc_mut(id) {
            d.supply_image(img, size);
        }
        self.laid = None;
    }

    pub fn image_url(&self, id: FrameId, img: browserkit::engine::ImageId) -> &str {
        match self.d.frame_doc(id) {
            Some(d) => d.image_url(img),
            None => "",
        }
    }
}
