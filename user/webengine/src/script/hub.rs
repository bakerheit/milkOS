//! The windows of one page reaching each other: the part of the frame bindings that every
//! other file builds on.
//!
//! Each frame's document has a realm of its own, and a realm's values mean nothing in another.
//! A script that touches `parent.document` or `iframe.contentWindow.foo` therefore gets a
//! *proxy*: a host object ([`super::remote`]) that forwards every operation to the real object
//! in the other realm, by entering that frame's [`Runtime`] for the length of the operation
//! ([`enter`]). Numbers, strings and the like are copied across; objects, functions and windows
//! become proxies in the receiving realm. This is how same-origin frames share a DOM without a
//! shared heap: `parent.document.getElementById("x").textContent = "y"` is three entries into
//! the parent's realm.
//!
//! # How the frames find each other
//!
//! The top document owns the table of frames ([`crate::frames::Frames`]); the documents of the
//! frames are boxed in its slots. A [`Hub`] is shared by all the runtimes of one page and holds
//! a raw pointer to the top [`Document`], which the engine refreshes whenever it syncs the
//! frame table (`Document::frame_slots` and friends). A cross-frame operation finds the other
//! document through it. The pointer is valid while the top document is where it was when the
//! host last called into the frames: **the host keeps the top document in a `Box`** (the
//! Browser does) so it never moves. Like the page and host pointers of [`super::bridge`], the
//! pointers are used in short scopes: nothing here keeps a reference across a call into script.
//!
//! # Limits of this design
//!
//! * A frame further up the call stack is entered again, nested (a listener of the parent on
//!   the child's button reads the event it is given, which lives in the child's realm), up to
//!   [`MAX_NEST`] deep; deeper calls fail with an error ("the other frame is busy").
//! * Proxies are not instances of the other realm's classes (`parent.document instanceof
//!   Document` is false), `Array.isArray(proxy)` is false, and symbols (other than
//!   `Symbol.iterator`) do not cross.
//! * Every object that crosses stays alive in its own realm until the proxy is collected;
//!   function proxies cannot tell, so they stay for the life of the realm. At most
//!   [`MAX_EXPORTS`] per realm; beyond that an operation fails with an error.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::sync::atomic::{AtomicU64, Ordering};

use jsengine::{ClassId, Pinned, Realm, Value, WeakValue};

use super::Runtime;
use super::bridge::Bridge;
use crate::api::*;
use crate::page::Page;

/// Objects one realm may have handed to others at a time.
pub const MAX_EXPORTS: usize = 16 * 1024;

static NEXT_UID: AtomicU64 = AtomicU64::new(1);

/// What the Browser tells a frame's document about itself: the things only it knows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameSec {
    /// The document's origin as `event.origin` writes it (`scheme://host[:port]`); None: take
    /// it from the document's address. A `srcdoc` or `about:blank` document has its creator's.
    pub origin: Option<String>,
    /// An opaque origin (a sandbox without `allow-same-origin`, a `data:` document): the origin
    /// reads as `"null"` and is the same as no other.
    pub opaque: bool,
    /// What `location.href` says when it is not the address the document was parsed with
    /// (`about:srcdoc`, `about:blank`, a `data:` address).
    pub location: Option<String>,
    /// `alert`, `confirm` and `prompt` do nothing (a sandbox without `allow-modals`).
    pub no_modals: bool,
    /// `window.open` does nothing (a sandbox without `allow-popups`).
    pub no_popups: bool,
}

impl Default for FrameSec {
    fn default() -> FrameSec {
        FrameSec {
            origin: None,
            opaque: false,
            location: None,
            no_modals: false,
            no_popups: false,
        }
    }
}

/// A change to a handle's reference count, for the realm that owns it, applied the next time
/// that realm is entered.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Adjust {
    pub frame: u32,
    pub uid: u64,
    pub handle: u32,
    pub delta: i32,
}

/// Shared by the runtimes of one page.
pub struct Hub {
    /// The page's message ports (`ports.rs`).
    pub ports: Rc<super::ports::PortTable>,
    top: Cell<*mut Document>,
    /// The documents of the frames (boxed in the frame table, so their addresses hold even
    /// while the host takes one out of its slot to run it), by frame id: (the realm's uid, the
    /// document). A runtime removes its entry when it is dropped.
    docs: RefCell<BTreeMap<u32, (u64, *mut Document)>>,
    pending: RefCell<Vec<Adjust>>,
    /// Cross-frame calls in progress.
    nest: Cell<u32>,
}

/// Cross-frame calls inside each other (`a` calls `b` calls `a` ...).
pub const MAX_NEST: u32 = 8;

impl Hub {
    pub fn new() -> Rc<Hub> {
        Rc::new(Hub {
            ports: Rc::new(super::ports::PortTable::new()),
            top: Cell::new(core::ptr::null_mut()),
            docs: RefCell::new(BTreeMap::new()),
            pending: RefCell::new(Vec::new()),
            nest: Cell::new(0),
        })
    }

    /// The top document is at `d` (called by the engine when it syncs the frame table).
    pub fn set_top(&self, d: *mut Document) {
        self.top.set(d);
    }

    /// The document of frame `id`, if it has one.
    pub(crate) fn doc(&self, id: u32) -> Option<*mut Document> {
        if id == 0 {
            let p = self.top.get();
            return (!p.is_null()).then_some(p);
        }
        self.docs.borrow().get(&id).map(|(_, p)| *p)
    }

    pub(crate) fn register(&self, id: u32, uid: u64, d: *mut Document) {
        self.docs.borrow_mut().insert(id, (uid, d));
    }

    pub(crate) fn unregister(&self, id: u32, uid: u64) {
        let mut m = self.docs.borrow_mut();
        if m.get(&id).is_some_and(|(u, _)| *u == uid) {
            m.remove(&id);
        }
    }

    pub(crate) fn push(&self, a: Adjust) {
        let mut p = self.pending.borrow_mut();
        if p.len() < 100_000 {
            p.push(a);
        }
    }

    pub(crate) fn take_for(&self, frame: u32, uid: u64) -> Vec<Adjust> {
        let mut p = self.pending.borrow_mut();
        let mut mine = Vec::new();
        p.retain(|a| {
            if a.frame == frame && a.uid == uid {
                mine.push(*a);
                false
            } else {
                true
            }
        });
        mine
    }
}

/// What the other frames may see of a frame without entering it.
#[derive(Clone, Debug)]
pub(crate) struct Peek {
    pub uid: u64,
    pub origin: super::xorigin::Origin,
    pub parent: Option<u32>,
}

/// What one realm keeps for the frame bindings.
pub struct XFrame {
    pub uid: u64,
    pub hub: RefCell<Option<Rc<Hub>>>,
    pub sec: RefCell<FrameSec>,
    /// Objects this realm handed out: handle, the pinned value, how many proxies hold it.
    pub exports: RefCell<Exports>,
    /// This realm's host objects that stand for objects of other realms.
    pub remotes: RefCell<Vec<Option<RemoteRef>>>,
    pub proxies: RefCell<BTreeMap<RemoteKey, WeakValue>>,
    /// Remote host objects the collector freed (their handles are given back at the next
    /// operation).
    pub freed: Rc<RefCell<Vec<u64>>>,
    pub class: Cell<Option<ClassId>>,
    /// The frame this realm's document is in, once it has joined a hub.
    pub my_frame: Cell<u32>,
    /// The host is still loading frames of this document: `load` waits.
    pub hold_load: Cell<bool>,
    /// The tagged-JSON cloners of postMessage, compiled at first use.
    pub cloner: RefCell<Option<(Pinned, Pinned)>>,
    pub message_proto: RefCell<Option<Pinned>>,
    /// The message ports of a realm that is in no hub (`ports.rs`).
    pub ports: RefCell<Option<Rc<super::ports::PortTable>>>,
    /// Child-frame windows that were handed out (so `a.frames[0] === a.frames[0]`) live in
    /// `proxies`.
    pub frames_obj: RefCell<Option<Pinned>>,
}

#[derive(Default)]
pub struct Exports {
    next: u32,
    map: BTreeMap<u32, (Pinned, u32)>,
}

impl Exports {
    pub fn get(&self, h: u32) -> Option<&Pinned> {
        self.map.get(&h).map(|(p, _)| p)
    }

    /// A handle for `v`, with one more reference on it. None if the table is full.
    pub fn add(&mut self, r: &mut Realm, v: Value) -> Option<u32> {
        let found = self
            .map
            .iter()
            .find(|(_, (p, _))| r.pinned(p).is_same_handle(&v))
            .map(|(h, _)| *h);
        if let Some(h) = found {
            if let Some(e) = self.map.get_mut(&h) {
                e.1 = e.1.saturating_add(1);
            }
            return Some(h);
        }
        if self.map.len() >= MAX_EXPORTS {
            return None;
        }
        self.next = self.next.wrapping_add(1).max(1);
        let h = self.next;
        let p = r.pin(v);
        self.map.insert(h, (p, 1));
        Some(h)
    }

    pub fn adjust(&mut self, r: &mut Realm, h: u32, delta: i32) {
        let gone = match self.map.get_mut(&h) {
            Some(e) => {
                let n = e.1 as i64 + delta as i64;
                e.1 = n.clamp(0, u32::MAX as i64) as u32;
                e.1 == 0
            }
            None => false,
        };
        if gone && let Some((p, _)) = self.map.remove(&h) {
            r.unpin(p);
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }
}

/// What a remote host object stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Target {
    /// The window (WindowProxy) of the frame.
    Window,
    /// The `location` of the window of the frame, as another window may use it.
    Location,
    /// An object in the frame's realm, by handle.
    Handle(u32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RemoteKey {
    pub frame: u32,
    pub uid: u64,
    pub target: Target,
}

#[derive(Clone, Copy, Debug)]
pub struct RemoteRef {
    pub key: RemoteKey,
    /// This proxy holds a reference on the handle (given back when it is collected).
    pub counted: bool,
}

impl XFrame {
    pub fn new() -> XFrame {
        XFrame {
            uid: NEXT_UID.fetch_add(1, Ordering::Relaxed),
            hub: RefCell::new(None),
            sec: RefCell::new(FrameSec::default()),
            exports: RefCell::new(Exports::default()),
            remotes: RefCell::new(Vec::new()),
            proxies: RefCell::new(BTreeMap::new()),
            freed: Rc::new(RefCell::new(Vec::new())),
            class: Cell::new(None),
            my_frame: Cell::new(0),
            hold_load: Cell::new(false),
            cloner: RefCell::new(None),
            message_proto: RefCell::new(None),
            ports: RefCell::new(None),
            frames_obj: RefCell::new(None),
        }
    }
}

impl Default for XFrame {
    fn default() -> XFrame {
        XFrame::new()
    }
}

impl Runtime {
    /// Join the page's hub as frame `id` whose document is at `doc` (called by the engine when
    /// the frame table is made and when a document is attached; the top document is frame 0).
    pub(crate) fn link_hub(&self, hub: Rc<Hub>, id: u32, doc: *mut Document) {
        self.b.xf.my_frame.set(id);
        if id != 0 {
            hub.register(id, self.b.xf.uid, doc);
        }
        *self.b.xf.hub.borrow_mut() = Some(hub);
    }

    /// Leave the hub (the document is being taken out of the frame table).
    pub(crate) fn unlink_hub(&self) {
        super::ports::release(&self.b, None);
        if let Some(h) = self.b.xf.hub.borrow_mut().take() {
            h.unregister(self.b.xf.my_frame.get(), self.b.xf.uid);
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.unlink_hub();
    }
}

impl Bridge {
    /// This frame's id in its page (0 for the top).
    pub fn frame_id(&self) -> u32 {
        self.xf.my_frame.get()
    }

    pub fn hub(&self) -> Option<Rc<Hub>> {
        self.xf.hub.borrow().clone()
    }

    /// This document's origin.
    pub fn origin(&self) -> super::xorigin::Origin {
        let sec = self.xf.sec.borrow();
        let text = if sec.opaque {
            String::from("null")
        } else if let Some(o) = &sec.origin {
            o.clone()
        } else {
            self.page()
                .map(|p| super::xorigin::origin_of(p.address()))
                .unwrap_or_else(|| String::from("null"))
        };
        super::xorigin::Origin {
            text,
            doc: self.xf.uid,
        }
    }
}

/// Why another frame could not be entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gone {
    /// The frame, or its document, is gone (navigated away, removed, stopped).
    Dead,
    /// It is running further up the call stack.
    Busy,
}

/// A look at frame `frame` (if its document is `uid`) without entering it.
pub(crate) fn peek(from: &Bridge, frame: u32) -> Option<Peek> {
    let hub = from.hub()?;
    let dp = hub.doc(frame)?;
    // SAFETY: a short scope, no script runs.
    let d = unsafe { &*dp };
    let js = d.js.as_ref()?;
    let origin = js.b.origin_with_page(&d.page);
    Some(Peek {
        uid: js.b.xf.uid,
        origin,
        parent: if d.page.frame.id.is_top() {
            None
        } else {
            Some(d.page.frame.parent.0)
        },
    })
}

impl Bridge {
    fn origin_with_page(&self, page: &Page) -> super::xorigin::Origin {
        let sec = self.xf.sec.borrow();
        let text = if sec.opaque {
            String::from("null")
        } else if let Some(o) = &sec.origin {
            o.clone()
        } else {
            super::xorigin::origin_of(page.address())
        };
        super::xorigin::Origin {
            text,
            doc: self.xf.uid,
        }
    }
}

/// Another frame's host, for the length of one operation: the caller's, with the viewport and
/// scroll of the frame being entered.
struct FrameHost<'a> {
    inner: &'a mut dyn ScriptHost,
    vp: Viewport,
    scroll: (i32, i32),
}

impl ScriptHost for FrameHost<'_> {
    fn measurer(&self) -> &dyn TextMeasurer {
        self.inner.measurer()
    }
    fn viewport(&self) -> Viewport {
        self.vp
    }
    fn scroll(&self) -> (i32, i32) {
        self.scroll
    }
    fn now(&self) -> f64 {
        self.inner.now()
    }
    fn time(&self) -> f64 {
        self.inner.time()
    }
    fn tz_offset(&self) -> i32 {
        self.inner.tz_offset()
    }
    fn user_agent(&self) -> String {
        self.inner.user_agent()
    }
    fn cookies(&mut self) -> String {
        self.inner.cookies()
    }
    fn set_cookie(&mut self, line: &str) {
        self.inner.set_cookie(line)
    }
    fn storage(&mut self, session: bool) -> Option<&mut dyn Storage> {
        self.inner.storage(session)
    }
    fn alert(&mut self, msg: &str) {
        self.inner.alert(msg)
    }
    fn confirm(&mut self, msg: &str) -> bool {
        self.inner.confirm(msg)
    }
    fn prompt(&mut self, msg: &str, default: &str) -> Option<String> {
        self.inner.prompt(msg, default)
    }
    fn slow(&mut self, running_ms: f64) -> bool {
        self.inner.slow(running_ms)
    }
}

/// Run `f` inside frame `frame`'s realm (its document must still be `uid`). The page and host
/// of that frame are made reachable as for a call from the app; the host is the caller's with
/// that frame's viewport.
pub(crate) fn enter<R>(
    from: &Bridge,
    frame: u32,
    uid: u64,
    f: impl FnOnce(&Rc<Bridge>, &mut Realm) -> R,
) -> Result<R, Gone> {
    let hub = from.hub().ok_or(Gone::Dead)?;
    let dp = hub.doc(frame).ok_or(Gone::Dead)?;
    let host = from.host().ok_or(Gone::Dead)?;
    // SAFETY: the document is boxed in the frame table (or is the boxed top document); it is not
    // the caller's (a frame resolves its own objects locally) and not on the stack (checked
    // below), so nothing else is using its page or runtime now.
    let d = unsafe { &mut *dp };
    let vp = match hub.top_viewport(frame) {
        Some(v) => v,
        None => Viewport::new(300, 150),
    };
    let scroll = hub.top_scroll(frame);
    let page = &mut d.page;
    let js = d.js.as_mut().ok_or(Gone::Dead)?;
    if js.b.xf.uid != uid {
        return Err(Gone::Dead);
    }
    // A frame further up the call stack may be entered again (a listener reading the event it
    // was given): the runtime nests entries. The depth is bounded.
    if hub.nest.get() >= MAX_NEST || js.b.st.try_borrow().is_err() {
        return Err(Gone::Busy);
    }
    hub.nest.set(hub.nest.get() + 1);
    struct Unnest<'a>(&'a Hub);
    impl Drop for Unnest<'_> {
        fn drop(&mut self) {
            self.0.nest.set(self.0.nest.get().saturating_sub(1));
        }
    }
    let _unnest = Unnest(&hub);
    let adj = hub.take_for(frame, uid);
    let run = move |b: &Rc<Bridge>, r: &mut Realm| {
        if !adj.is_empty() {
            let mut ex = b.xf.exports.borrow_mut();
            for a in adj {
                ex.adjust(r, a.handle, a.delta);
            }
        }
        f(b, r)
    };
    // The host that runs that frame's scripts if the app has one (its cookies and storage are the
    // frame's), else the caller's with that frame's viewport.
    // SAFETY: `host` is the caller's host pointer; `frame_host` hands out a borrow of it or of
    // something it owns, used only for this entry.
    let host_ptr: *mut dyn ScriptHost = host;
    if let Some(fh) = unsafe { &mut *host_ptr }.frame_host(FrameId(frame)) {
        return js.entry(page, fh, run).ok_or(Gone::Dead);
    }
    let mut fh = FrameHost {
        inner: unsafe { &mut *host_ptr },
        vp,
        scroll,
    };
    js.entry(page, &mut fh, run).ok_or(Gone::Dead)
}

impl Hub {
    fn top_viewport(&self, frame: u32) -> Option<Viewport> {
        let p = self.top.get();
        if p.is_null() {
            return None;
        }
        // SAFETY: as in `doc`.
        let top = unsafe { &*p };
        if frame == 0 {
            return None;
        }
        top.frame_viewport(FrameId(frame))
    }

    fn top_scroll(&self, frame: u32) -> (i32, i32) {
        let p = self.top.get();
        if p.is_null() || frame == 0 {
            return (0, 0);
        }
        // SAFETY: as in `doc`.
        let top = unsafe { &*p };
        top.frames
            .as_ref()
            .and_then(|f| f.slot(FrameId(frame)))
            .map_or((0, 0), |s| s.scroll)
    }

    /// The child frames of `of` that have a document, in tree order.
    pub(crate) fn children(&self, of: u32) -> Vec<u32> {
        let p = self.top.get();
        if p.is_null() {
            return Vec::new();
        }
        // SAFETY: as in `doc`.
        let top = unsafe { &*p };
        top.child_frames(FrameId(of))
            .into_iter()
            .map(|f| f.0)
            .collect()
    }

    /// `window.name` of frame `id` (None if it has no document).
    pub(crate) fn name_of(&self, id: u32) -> Option<String> {
        let dp = self.doc(id)?;
        // SAFETY: a short scope, no script runs.
        let d = unsafe { &*dp };
        Some(d.page.window_name.clone())
    }

    /// The element of the parent document that holds frame `id`.
    pub(crate) fn element_of(&self, id: u32) -> Option<(u32, u32)> {
        let p = self.top.get();
        if p.is_null() {
            return None;
        }
        // SAFETY: as in `doc`.
        let top = unsafe { &*p };
        let s = top.frames.as_ref()?.slot(FrameId(id))?;
        Some((s.parent.0, s.node as u32))
    }
}

// ---- What the Browser calls ----

impl Document {
    /// Tell a document what only the Browser knows about it (see [`FrameSec`]). Call it on the
    /// frame's document before it is attached (or on the top document: its origin comes from its
    /// address). No-op without scripts.
    pub fn set_frame_security(&mut self, sec: FrameSec) {
        if let Some(js) = self.js.as_ref() {
            *js.b.xf.sec.borrow_mut() = sec;
        }
    }

    /// Hold this document's `load` event back (the frames in it are still loading) or let it
    /// go.
    pub fn set_load_hold(&mut self, hold: bool) {
        if let Some(js) = self.js.as_ref() {
            js.b.xf.hold_load.set(hold);
        }
    }

    /// The document has finished loading: its scripts have run and `load` was fired (or it has
    /// none), and it waits for no stylesheet or image. (Its frames are not counted.)
    pub fn is_loaded(&self) -> bool {
        if !self.page.pending_resources().is_empty() {
            return false;
        }
        match self.js.as_ref() {
            Some(js) if !js.stopped() => js.b.st.try_borrow().map_or(false, |s| s.loaded),
            _ => true,
        }
    }

    /// Fire `load` at the `<iframe>` element that holds frame `id`, in its parent's document
    /// (the frame's document is done). True if a listener ran or the event was fired.
    pub fn fire_frame_load(&mut self, host: &mut dyn ScriptHost, id: FrameId) -> bool {
        let Some(h) = self.frames.as_ref().map(|f| f.hub.clone()) else {
            return false;
        };
        let Some((parent, node)) = h.element_of(id.0) else {
            return false;
        };
        self.touch_hub();
        let Some(dp) = h.doc(parent) else { return false };
        // SAFETY: as in `hub::enter`: the hub's documents are the frame table's.
        let d = unsafe { &mut *dp };
        let Some(js) = d.js.as_mut() else { return false };
        if js.stopped() {
            return false;
        }
        js.entry(&mut d.page, host, |b, r| {
            super::events_js::fire_simple(b, r, node, "load", false, false);
            true
        })
        .unwrap_or(false)
    }

    /// The origin of frame `id`'s window as `event.origin` writes it (`"null"` for an opaque
    /// one). None for an unknown frame or one without scripts.
    pub fn frame_origin(&self, id: FrameId) -> Option<String> {
        let d = self.frame_doc(id)?;
        let js = d.js.as_ref()?;
        Some(js.b.origin_with_page(&d.page).text)
    }

    /// Where a posted message goes and what the target sees, or None if it is dropped
    /// (the target is gone, or its origin is not the `targetOrigin`). The host then calls
    /// [`Document::deliver_message`] on the document of the returned frame. `sender` is the
    /// frame whose script called `postMessage`.
    pub fn route_message(&self, sender: FrameId, m: &OutMessage) -> Option<(FrameId, Message)> {
        let out = self.route_message_inner(sender, m);
        if out.is_none() && !m.ports.is_empty() {
            // Dropped: the ports it carried are lost with it.
            if let Some(js) = self.js.as_ref() {
                js.b.ports().close_all(&m.ports);
            }
        }
        out
    }

    fn route_message_inner(&self, sender: FrameId, m: &OutMessage) -> Option<(FrameId, Message)> {
        let tgt = self.frame_doc(m.target)?;
        let sdoc = self.frame_doc(sender)?;
        let (tjs, sjs) = (tgt.js.as_ref()?, sdoc.js.as_ref()?);
        let target_origin = tjs.b.origin_with_page(&tgt.page);
        let sender_origin = sjs.b.origin_with_page(&sdoc.page);
        let want = if m.target_origin == "/" {
            super::xorigin::Target::SameAsSender
        } else {
            super::xorigin::parse_target(&m.target_origin)?
        };
        if !want.allows(&sender_origin.text, &target_origin) {
            return None;
        }
        Some((
            m.target,
            Message {
                data: m.data.clone(),
                origin: sender_origin.text,
                source: sender,
                ports: m.ports.clone(),
            },
        ))
    }

    /// Everything the frames' scripts posted since the last call, in the order of the frames
    /// (the top page first), as (sender, message).
    pub fn take_all_messages(&mut self) -> Vec<(FrameId, OutMessage)> {
        let mut out = Vec::new();
        let mut ids = alloc::vec![FrameId::TOP];
        ids.extend(self.frame_ids());
        for id in ids {
            if let Some(d) = self.frame_doc_mut(id) {
                for m in d.take_messages() {
                    out.push((id, m));
                }
            }
        }
        out
    }
}

#[allow(dead_code)]
pub(crate) fn _keep(_: Box<()>) {}
