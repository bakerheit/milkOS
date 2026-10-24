//! Frames in the Browser: `<iframe>`s fetched as navigations, their documents attached to the
//! page's frame table (the engine lays them out, paints, scrolls and hit-tests them), the
//! rules that decide what may be framed and what a frame may do (`scriptnet::frame`), links and
//! forms that target frames, and the joint session history.
//!
//! A tab's [`FrameTable`] keeps, for each frame with a document, where the document came from,
//! what it may do (its sandbox, origin, cookie rights) and where it sits in the history, and for
//! each frame being loaded the fetch job. [`App::sync_frames`] looks at the engine's slots after
//! anything that may have changed them and starts what is wanted: `about:blank`, `srcdoc` and
//! `data:` documents are made on the spot, `http(s)` ones are fetched by a [`Kind::Frame`] job
//! with the page as the `Referer`, the right cookies (only on the same site all the way up) and
//! every redirect hop checked; the answer is judged ([`scriptnet::frame::load::judge`]: framing
//! headers, sandbox, type), and a frame that may not show it shows a page saying so, in the
//! frame, never in the tab.
//!
//! Console lines and requests that come from frames carry the frame's label (its names from the
//! top, `a/b`), on the serial line as `console LEVEL [frame a/b]: ...`.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use browserkit::address::{self, Internal, Target};
use browserkit::cookies::{Context, site_of};
use browserkit::netpolicy::{AddressSpace, ReferrerPolicy};
use browserkit::pages;
use milkweb::http::{Failure, Request, Response};
use milkweb::url::Url;
use scriptnet::frame::ancestors::Blocked;
use scriptnet::frame::history::{DocId, Node, Reload, key_for, restore_for};
use scriptnet::frame::load::{self, Answer, Ask, Source};
use scriptnet::frame::nav::{self, Choice, Denied, FrameTree};
use scriptnet::frame::sandbox::Sandbox;
use webengine::{ConsoleLevel, FrameId, FrameSec, FrameSlot, Submission};

use crate::net::{JobId, Outcome};
use crate::tab::{PageKind, Tab};
use crate::web::Doc;
use crate::{App, MAX_TABS, now_ms};

/// What kind of document a frame shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FrameKind {
    Blank,
    Srcdoc,
    Data,
    Web,
    /// A page the Browser made in place of one that couldn't be shown.
    Error,
}

/// What the Browser knows about a frame's document.
pub struct Meta {
    pub doc_id: DocId,
    pub kind: FrameKind,
    /// The address the document came from (None: blank, srcdoc, data, error).
    pub url: Option<Url>,
    /// The address that stands for it among its frames' ancestors: its own, or the creator's for
    /// a blank or srcdoc document; None for a `data:` document and an error page.
    pub ancestor_url: Option<Url>,
    /// The localStorage key of its origin (None: an opaque origin, no storage).
    pub storage_key: Option<String>,
    /// `scheme://host:port` of its origin ("null" if opaque), for Sec-Fetch-Site.
    pub origin: String,
    /// The restrictions it runs under (its `sandbox`, its parents', its response's CSP).
    pub sandbox: Sandbox,
    /// The site its subresources' cookies are judged against (a site no cookie belongs to
    /// when a cross-site frame is somewhere above it).
    pub sub_site: String,
    /// `document.cookie` works.
    pub cookies_ok: bool,
    pub ref_policy: ReferrerPolicy,
    pub key: String,
    pub path: Vec<String>,
    /// The frame's names from the top ("a/b"), for the console.
    pub label: String,
    pub generation: u32,
    pub space: Option<AddressSpace>,
    pub status: u16,
    /// The `load` event was fired at its `<iframe>`.
    pub notified: bool,
    pub shown: BTreeSet<String>,
    /// Stylesheets and images it still waits for.
    pub waiting: usize,
    pub title: String,
}

pub enum How {
    /// The frame's first document (its `src`, or a restore's) or a script-set `src`.
    Initial,
    /// A link, a form or `location`.
    Navigate { replace: bool },
    /// Back or forward asked for this node.
    Restore(Node),
}

pub struct Pending {
    pub job: JobId,
    pub generation: Option<u32>,
    pub how: How,
    pub address: String,
    pub parent: FrameId,
    pub source: FrameId,
    pub started: u64,
}

#[derive(Default)]
pub struct FrameTable {
    pub meta: BTreeMap<FrameId, Meta>,
    pub loads: BTreeMap<FrameId, Pending>,
    /// Slot generations that were dealt with without a load (a refusal, shown as a page).
    pub done: BTreeMap<FrameId, u32>,
    /// Where a traversal says the frames below a reloaded frame go.
    pub restore: Vec<Reload>,
    /// Slots that want a document and have not got one yet (not loading, not refused).
    pub wanting: BTreeSet<FrameId>,
    /// Documents whose `load` is held back while their frames load.
    pub held: BTreeSet<FrameId>,
}

impl FrameTable {
    pub fn busy(&self) -> bool {
        !self.loads.is_empty()
    }
}

/// The engine's reading of a `sandbox` attribute as the rules' flag set.
pub fn sandbox_of(s: Option<webengine::Sandbox>) -> Sandbox {
    let Some(s) = s else { return Sandbox::NONE };
    let mut f = Sandbox::FULL.0;
    let mut clear = |on: bool, bit: u32| {
        if on {
            f &= !bit;
        }
    };
    clear(s.scripts, Sandbox::SCRIPTS);
    clear(s.same_origin, Sandbox::ORIGIN);
    clear(s.forms, Sandbox::FORMS);
    clear(s.popups, Sandbox::POPUPS);
    clear(s.popups_to_escape_sandbox, Sandbox::POPUPS_INHERIT);
    clear(s.modals, Sandbox::MODALS);
    clear(s.downloads, Sandbox::DOWNLOADS);
    clear(s.pointer_lock, Sandbox::POINTER_LOCK);
    clear(s.orientation_lock, Sandbox::ORIENTATION_LOCK);
    clear(s.presentation, Sandbox::PRESENTATION);
    clear(s.storage_access_by_user_activation, Sandbox::STORAGE_ACCESS);
    if s.top_navigation {
        f &= !(Sandbox::TOP_NAVIGATION | Sandbox::TOP_NAVIGATION_NEEDS_ACTIVATION);
    } else if s.top_navigation_by_user_activation {
        f |= Sandbox::TOP_NAVIGATION | Sandbox::TOP_NAVIGATION_NEEDS_ACTIVATION;
    }
    Sandbox(f)
}

/// The frame tree as the navigation rules see it.
struct TabTree<'a> {
    doc: &'a Doc,
}

impl FrameTree for TabTree<'_> {
    fn top(&self) -> u32 {
        0
    }
    fn parent(&self, f: u32) -> Option<u32> {
        self.doc
            .frame(FrameId(f))
            .and_then(|d| d.frame_info().parent)
            .map(|p| p.0)
    }
    fn children(&self, f: u32) -> Vec<u32> {
        self.doc.children(FrameId(f)).into_iter().map(|c| c.0).collect()
    }
    fn name(&self, f: u32) -> String {
        self.doc.window_name(FrameId(f))
    }
}

/// The frame tree of a page for the navigation rules.
pub fn tree_of(doc: &Doc) -> impl FrameTree + '_ {
    TabTree { doc }
}

/// A navigation of a frame the app asks for.
pub struct Nav {
    pub url: Url,
    pub fragment: Option<String>,
    pub post: Option<Request>,
    pub replace: bool,
    /// The frame whose script or link asked (its address is the `Referer`).
    pub source: FrameId,
    pub how: Option<How>,
}

const SEC_DEST: &str = "iframe";

/// The most lines of one console entry that go to the serial log.
const MAX_SERIAL_LINES: usize = 40;

/// One console entry on the serial log: `[browser] console HEAD: first line`, and every
/// further line (an error's stack, its source line and caret) on a line of its own with the
/// same start, indented, so the log can be read and searched line by line.
pub(crate) fn print_console(head: &str, text: &str) {
    for (i, line) in text.split('\n').enumerate() {
        if i >= MAX_SERIAL_LINES {
            rt::println!("[browser] console {head}:   ...");
            break;
        }
        let line = browserkit::clean_line(line);
        if i == 0 {
            rt::println!("[browser] console {head}: {line}");
        } else {
            rt::println!("[browser] console {head}:   {}", line.trim_end());
        }
    }
}

impl App {
    fn tab_of_epoch(&self, uid: u64, epoch: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.uid == uid && t.epoch == epoch)
    }

    /// The addresses of the ancestors of a child of `parent`, top page first, `parent` last.
    pub fn parent_chain(&self, ti: usize, parent: FrameId) -> Vec<Option<Url>> {
        let t = &self.tabs[ti];
        let mut up: Vec<Option<Url>> = Vec::new();
        let mut cur = parent;
        for _ in 0..16 {
            if cur.is_top() {
                up.push(t.page.url.clone());
                break;
            }
            up.push(t.frames.meta.get(&cur).and_then(|m| m.ancestor_url.clone()));
            cur = t
                .page
                .doc
                .frame(cur)
                .and_then(|d| d.frame_info().parent)
                .unwrap_or(FrameId::TOP);
        }
        up.reverse();
        up
    }

    /// The restrictions frame `f` runs under (none for the page).
    pub fn sandbox_of_frame(&self, ti: usize, f: FrameId) -> Sandbox {
        self.tabs[ti]
            .frames
            .meta
            .get(&f)
            .map_or(Sandbox::NONE, |m| m.sandbox)
    }

    /// Keys and paths of every slot (the history's names for frames).
    fn frame_paths(&self, ti: usize, slots: &[FrameSlot]) -> BTreeMap<FrameId, (String, Vec<String>)> {
        let mut out: BTreeMap<FrameId, (String, Vec<String>)> = BTreeMap::new();
        let mut taken: BTreeMap<FrameId, Vec<String>> = BTreeMap::new();
        let mut index: BTreeMap<FrameId, usize> = BTreeMap::new();
        let _ = ti;
        for s in slots {
            let i = index.entry(s.parent).or_insert(0);
            let t = taken.entry(s.parent).or_default();
            let key = key_for(&s.name, *i, t);
            *i += 1;
            t.push(key.clone());
            let mut path = out
                .get(&s.parent)
                .map(|p| p.1.clone())
                .unwrap_or_default();
            path.push(key.clone());
            out.insert(s.id, (key, path));
        }
        out
    }

    fn frame_label(path: &[String]) -> String {
        path.join("/")
    }

    /// Say something on the console of frame `f` (the tab's, with the frame's label).
    pub fn frame_console(&mut self, ti: usize, f: FrameId, level: ConsoleLevel, text: &str) {
        let label = self.tabs[ti]
            .frames
            .meta
            .get(&f)
            .map(|m| m.label.clone())
            .unwrap_or_default();
        self.console_line(ti, &label, level, text);
    }

    pub fn console_line(&mut self, ti: usize, label: &str, level: ConsoleLevel, text: &str) {
        let name = match level {
            ConsoleLevel::Log => "log",
            ConsoleLevel::Info => "info",
            ConsoleLevel::Warn => "warn",
            ConsoleLevel::Error => "error",
            ConsoleLevel::Debug => "debug",
        };
        if label.is_empty() {
            print_console(name, text);
        } else {
            print_console(&format!("{name} [frame {label}]"), text);
        }
        let text = browserkit::clean_line(text);
        let t = &mut self.tabs[ti];
        if t.console.len() >= crate::tab::CONSOLE_LINES {
            t.console.pop_front();
        }
        let shown = if label.is_empty() {
            text
        } else {
            format!("[{label}] {text}")
        };
        t.console.push_back((level, shown));
    }

    // ---- looking at the slots ----

    /// Bring the frames in step with the page's `<iframe>` elements: forget closed ones, start
    /// what the slots want. Cheap when nothing changed.
    pub fn sync_frames(&mut self, ti: usize) {
        if ti >= self.tabs.len() || self.tabs[ti].page.kind != PageKind::Web {
            return;
        }
        let slots = self.tabs[ti].page.doc.slots();
        for id in self.tabs[ti].page.doc.take_closed() {
            self.forget_frame(ti, id);
        }
        if slots.is_empty() {
            return;
        }
        let paths = self.frame_paths(ti, &slots);
        let (_, vh) = self.page_view(&self.tabs[ti]);
        {
            let t = &mut self.tabs[ti];
            t.frames.wanting = slots
                .iter()
                .filter(|s| s.wants_load() && t.frames.done.get(&s.id) != Some(&s.generation))
                .map(|s| s.id)
                .collect();
        }
        for s in &slots {
            // The slots below a closed frame are gone too.
            if !s.wants_load() {
                continue;
            }
            let t = &self.tabs[ti];
            if let Some(p) = t.frames.loads.get(&s.id) {
                if p.generation == Some(s.generation) || p.generation.is_none() {
                    continue;
                }
                // A script pointed it elsewhere while it was loading: start over.
                self.cancel_frame_load(ti, s.id);
            }
            if self.tabs[ti].frames.done.get(&s.id) == Some(&s.generation) {
                continue;
            }
            // Installing an earlier slot's document looks at the slots again: this one may have
            // been done meanwhile (the list above is from before).
            if self.tabs[ti]
                .frames
                .meta
                .get(&s.id)
                .is_some_and(|m| m.generation == s.generation)
            {
                continue;
            }
            // loading=lazy: not until it is near the view.
            if s.lazy {
                let t = &self.tabs[ti];
                let near = s.rect.y < t.scroll + vh + 600 && s.rect.bottom() > t.scroll - 600;
                if !near || !s.rendered {
                    continue;
                }
            }
            // Its parent has to have its document (a grandchild's slot only exists then).
            if !s.parent.is_top() && !self.tabs[ti].frames.meta.contains_key(&s.parent) {
                continue;
            }
            self.start_slot_load(ti, s, &paths);
        }
        self.update_holds(ti);
    }

    fn cancel_frame_load(&mut self, ti: usize, id: FrameId) {
        if let Some(p) = self.tabs[ti].frames.loads.remove(&id) {
            self.fetch.cancel(p.job);
            self.frame_jobs.remove(&p.job);
        }
    }

    /// A frame's slot or document is gone.
    fn forget_frame(&mut self, ti: usize, id: FrameId) {
        self.cancel_frame_load(ti, id);
        let t = &mut self.tabs[ti];
        if let Some(m) = t.frames.meta.remove(&id) {
            t.history.frame_removed(&m.path);
        }
        t.frames.done.remove(&id);
    }

    /// Cancel every load of tab `ti`'s frames (the tab goes elsewhere).
    pub fn cancel_all_frames(&mut self, ti: usize) {
        let ids: Vec<FrameId> = self.tabs[ti].frames.loads.keys().copied().collect();
        for id in ids {
            self.cancel_frame_load(ti, id);
        }
    }

    /// Drop frame loads whose tab shows another page (or is gone).
    pub fn frames_sweep(&mut self) {
        let stale: Vec<JobId> = self
            .frame_jobs
            .iter()
            .filter(|(_, (uid, epoch, _))| self.tab_of_epoch(*uid, *epoch).is_none())
            .map(|(j, _)| *j)
            .collect();
        for j in stale {
            self.fetch.cancel(j);
            self.frame_jobs.remove(&j);
        }
    }

    fn start_slot_load(
        &mut self,
        ti: usize,
        s: &FrameSlot,
        paths: &BTreeMap<FrameId, (String, Vec<String>)>,
    ) {
        let (key, path) = paths.get(&s.id).cloned().unwrap_or_default();
        let parent = s.parent;
        let chain = self.parent_chain(ti, parent);
        let base = self.tabs[ti].page.doc.base_url_of(parent);
        let frames_now = {
            let t = &self.tabs[ti];
            t.frames.meta.len() + t.frames.loads.len()
        };
        let existing = self.tabs[ti].frames.meta.contains_key(&s.id);
        // A traversal says what this frame showed.
        let restore = restore_for(&self.tabs[ti].frames.restore, &path).cloned();
        let how = match &restore {
            Some(n) => How::Restore(n.clone()),
            None if existing => How::Navigate { replace: false },
            None => How::Initial,
        };
        let ask = Ask {
            src: s.src.as_deref(),
            srcdoc: s.srcdoc.as_deref(),
            base: base.as_ref(),
            ancestors: &chain,
            frames_now,
        };
        let src_text = s.src.clone().or_else(|| s.srcdoc.as_ref().map(|_| String::from("srcdoc"))).unwrap_or_default();
        let mut source = load::source(&ask);
        // Back to an http address the frame had been navigated to.
        if let (Some(n), Ok(Source::Http(_))) = (&restore, &source) {
            let _ = n;
        }
        if let Some(n) = &restore
            && !n.url.starts_with("about:")
            && !n.url.starts_with("data:")
            && s.srcdoc.is_none()
            && let Target::Web(u, _) = address::resolve(None, &n.url)
            && load::check_hop(&chain, &u).is_ok()
        {
            source = Ok(Source::Http(u));
        }
        let sandbox_attr = sandbox_of(s.sandbox);
        match source {
            Err(why) => {
                self.frame_console(ti, parent, ConsoleLevel::Error, &why.console(&src_text));
                self.install_error(
                    ti,
                    s.id,
                    parent,
                    Some(s.generation),
                    how,
                    "This frame isn't shown",
                    &why.reason(),
                    "",
                    &src_text,
                    key,
                    path,
                    sandbox_attr,
                );
            }
            Ok(Source::Http(url)) => {
                let fragment = None;
                let inherited = self.sandbox_of_frame(ti, parent).merge(sandbox_attr);
                for w in sandbox_attr.warnings() {
                    self.frame_console(ti, parent, ConsoleLevel::Warn, w);
                }
                let nav = Nav {
                    url,
                    fragment,
                    post: None,
                    replace: false,
                    source: parent,
                    how: Some(how),
                };
                self.begin_http(
                    ti,
                    s.id,
                    parent,
                    Some(s.generation),
                    nav,
                    inherited,
                    Some(s.referrer_policy.as_str()),
                    key,
                    path,
                );
            }
            Ok(other) => {
                let inherited = self.sandbox_of_frame(ti, parent).merge(sandbox_attr);
                self.install_inline(ti, s, parent, other, how, inherited, key, path);
            }
        }
    }

    // ---- documents made in place ----

    #[allow(clippy::too_many_arguments)]
    fn install_inline(
        &mut self,
        ti: usize,
        s: &FrameSlot,
        parent: FrameId,
        source: Source,
        how: How,
        sandbox: Sandbox,
        key: String,
        path: Vec<String>,
    ) {
        let pmeta_url = self.parent_doc_url(ti, parent);
        let (pm_origin, pm_key, pm_ancestor, pm_site, pm_cookies, pm_pol) = self.parent_identity(ti, parent);
        let base_text = pmeta_url
            .as_ref()
            .map(|u| u.to_text())
            .unwrap_or_else(|| String::from("about:blank"));
        let (kind, bytes, ct, location, address) = match source {
            Source::Blank => (
                FrameKind::Blank,
                b"<!DOCTYPE html><html><head></head><body></body></html>".to_vec(),
                String::from("text/html"),
                String::from("about:blank"),
                String::from("about:blank"),
            ),
            Source::Srcdoc(d) => (
                FrameKind::Srcdoc,
                d.into_bytes(),
                String::from("text/html"),
                String::from("about:srcdoc"),
                String::from("about:srcdoc"),
            ),
            Source::Data(d) => (
                FrameKind::Data,
                d.body,
                d.mime,
                String::from("data:"),
                String::from("data:"),
            ),
            Source::Http(_) => return,
        };
        let opaque = sandbox.opaque_origin() || kind == FrameKind::Data;
        let scripts = self.js_on && !sandbox.no_scripts();
        let base = if kind == FrameKind::Data { "data:".into() } else { base_text };
        let doc = Doc::parse_frame(&bytes, &base, &ct, scripts);
        let mut doc = doc;
        doc.set_frame_security(FrameSec {
            origin: (!opaque).then(|| pm_origin.clone()),
            opaque,
            location: Some(location),
            no_modals: sandbox.no_modals(),
            no_popups: sandbox.no_popups(),
        });
        let doc_id = self.next_doc_id();
        let meta = Meta {
            doc_id,
            kind,
            url: None,
            ancestor_url: if kind == FrameKind::Data { None } else { pm_ancestor },
            storage_key: if opaque { None } else { pm_key },
            origin: if opaque { String::from("null") } else { pm_origin },
            sandbox,
            sub_site: pm_site,
            cookies_ok: pm_cookies && !opaque,
            ref_policy: pm_pol,
            label: Self::frame_label(&path),
            key,
            path,
            generation: s.generation,
            space: None,
            status: 200,
            notified: false,
            shown: BTreeSet::new(),
            waiting: 0,
            title: String::new(),
        };
        self.install_doc(ti, s.id, Some(s.generation), doc, meta, how, &address);
    }

    /// The address a frame's children resolve against and are judged by: its document's
    /// (the creator's for a blank or srcdoc one).
    fn parent_doc_url(&self, ti: usize, parent: FrameId) -> Option<Url> {
        let t = &self.tabs[ti];
        if parent.is_top() {
            return t.page.url.clone();
        }
        t.frames.meta.get(&parent).and_then(|m| m.ancestor_url.clone())
    }

    /// What a child that has no address of its own takes from its parent: (origin text,
    /// localStorage key, ancestor address, subresource site, cookie rights, referrer policy).
    #[allow(clippy::type_complexity)]
    fn parent_identity(
        &self,
        ti: usize,
        parent: FrameId,
    ) -> (String, Option<String>, Option<Url>, String, bool, ReferrerPolicy) {
        let t = &self.tabs[ti];
        if parent.is_top() {
            let origin = t
                .page
                .url
                .as_ref()
                .map(|u| crate::js::origin(u))
                .unwrap_or_else(|| String::from("null"));
            let site = t.page.url.as_ref().map(|u| site_of(&u.host)).unwrap_or_default();
            let ok = t.page.url.is_some();
            let text = t
                .page
                .url
                .as_ref()
                .map(|u| webengine::script::xorigin::origin_of(&u.to_text()))
                .unwrap_or_else(|| String::from("null"));
            let _ = origin;
            let key = t.page.url.as_ref().map(crate::js::origin);
            return (text, key, t.page.url.clone(), site, ok, t.page.ref_policy);
        }
        match t.frames.meta.get(&parent) {
            Some(m) => (
                m.origin.clone(),
                m.storage_key.clone(),
                m.ancestor_url.clone(),
                m.sub_site.clone(),
                m.cookies_ok,
                m.ref_policy,
            ),
            None => (
                String::from("null"),
                None,
                None,
                String::new(),
                false,
                ReferrerPolicy::DEFAULT,
            ),
        }
    }

    pub fn next_doc_id(&mut self) -> DocId {
        self.next_doc += 1;
        self.next_doc
    }

    // ---- fetching ----

    #[allow(clippy::too_many_arguments)]
    fn begin_http(
        &mut self,
        ti: usize,
        id: FrameId,
        parent: FrameId,
        generation: Option<u32>,
        nav: Nav,
        inherited: Sandbox,
        attr_policy: Option<&str>,
        key: String,
        path: Vec<String>,
    ) {
        let chain = self.parent_chain(ti, parent);
        let address = match &nav.fragment {
            Some(f) => format!("{}#{f}", nav.url.to_text()),
            None => nav.url.to_text(),
        };
        let how = nav.how.unwrap_or(How::Navigate { replace: nav.replace });
        // The first address is judged like every redirect hop.
        if let Err(why) = load::check_hop(&chain, &nav.url) {
            self.frame_console(ti, parent, ConsoleLevel::Error, &why.console(&address));
            self.install_error(
                ti,
                id,
                parent,
                generation,
                how,
                "This frame isn't shown",
                &why.reason(),
                "",
                &address,
                key,
                path,
                inherited,
            );
            return;
        }
        let ctx: Context = load::cookie_context(&chain, &nav.url);
        let mut req = nav.post.unwrap_or_else(|| Request::get(&nav.url));
        let (src_url, src_policy) = self.referrer_source(ti, nav.source);
        if let Some(r) = load::referer(src_url.as_ref(), &nav.url, attr_policy, src_policy) {
            req.headers.push((String::from("Referer"), r));
        }
        let site = self.fetch_site(ti, parent, &nav.url);
        req.headers.push((String::from("Sec-Fetch-Dest"), String::from(SEC_DEST)));
        req.headers.push((String::from("Sec-Fetch-Mode"), String::from("navigate")));
        req.headers.push((String::from("Sec-Fetch-Site"), String::from(site)));
        let job = self.fetch.start_frame(req, ctx, chain);
        let (uid, epoch) = (self.tabs[ti].uid, self.tabs[ti].epoch);
        self.frame_jobs.insert(job, (uid, epoch, id));
        // The key and path are worked out again when the answer comes (the tree may have
        // changed meanwhile); `how` carries what the history needs.
        let _ = (key, path);
        self.tabs[ti].frames.loads.insert(
            id,
            Pending {
                job,
                generation,
                how,
                address,
                parent,
                source: nav.source,
                started: now_ms(),
            },
        );
        self.pending_sandbox.insert(job, inherited);
        if self.log {
            rt::println!("[browser] frame {id:?} loads {}", nav.url.to_text());
        }
    }

    /// The address and referrer policy of the document a request comes from.
    fn referrer_source(&self, ti: usize, f: FrameId) -> (Option<Url>, ReferrerPolicy) {
        let t = &self.tabs[ti];
        if f.is_top() {
            return (t.page.url.clone(), t.page.ref_policy);
        }
        match t.frames.meta.get(&f) {
            Some(m) => (m.ancestor_url.clone(), m.ref_policy),
            None => (None, ReferrerPolicy::DEFAULT),
        }
    }

    /// `Sec-Fetch-Site` of a frame request to `url` from a document in `parent`.
    fn fetch_site(&self, ti: usize, parent: FrameId, url: &Url) -> &'static str {
        let from = self.parent_doc_url(ti, parent);
        match from {
            None => "cross-site",
            Some(f) => {
                if f.scheme == url.scheme && f.host.eq_ignore_ascii_case(&url.host) && f.port == url.port {
                    "same-origin"
                } else if f.scheme == url.scheme && site_of(&f.host) == site_of(&url.host) {
                    "same-site"
                } else {
                    "cross-site"
                }
            }
        }
    }

    /// A frame's job ended.
    pub fn frame_finished(
        &mut self,
        job: JobId,
        out: Outcome,
        cert: Option<browserkit::certinfo::CertInfo>,
        peer: Option<Vec<u8>>,
    ) {
        let inherited = self.pending_sandbox.remove(&job).unwrap_or(Sandbox::NONE);
        let Some((uid, epoch, id)) = self.frame_jobs.remove(&job) else {
            return;
        };
        let Some(ti) = self.tab_of_epoch(uid, epoch) else {
            return;
        };
        let Some(p) = self.tabs[ti].frames.loads.get(&id) else {
            return;
        };
        if p.job != job {
            return;
        }
        let p = self.tabs[ti].frames.loads.remove(&id).unwrap();
        // The slot must still be there, for the generation this load was for.
        let slots = self.tabs[ti].page.doc.slots();
        let Some(slot) = slots.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        if let Some(g) = p.generation
            && g != slot.generation
        {
            return;
        }
        let paths = self.frame_paths(ti, &slots);
        let (key, path) = paths.get(&id).cloned().unwrap_or_default();
        let parent = p.parent;
        match out {
            Outcome::Page(resp) => self.frame_response(ti, id, p, resp, cert, peer, inherited, key, path),
            Outcome::Failed(f) => self.frame_failed(ti, id, p, f, inherited, key, path),
            Outcome::Refused(why) => {
                let address = p.address.clone();
                self.frame_console(ti, parent, ConsoleLevel::Error, &why.console(&address));
                self.install_error(
                    ti,
                    id,
                    parent,
                    p.generation,
                    p.how,
                    "This frame isn't shown",
                    &why.reason(),
                    "",
                    &address,
                    key,
                    path,
                    inherited,
                );
            }
            _ => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn frame_failed(
        &mut self,
        ti: usize,
        id: FrameId,
        p: Pending,
        f: Failure,
        inherited: Sandbox,
        key: String,
        path: Vec<String>,
    ) {
        if f.error == milkweb::http::Error::Cancelled {
            return;
        }
        let msg = format!("{}", f.error);
        rt::println!("[browser] frame {}: {msg}", f.url);
        self.frame_console(ti, p.parent, ConsoleLevel::Error, &format!("Failed to load the frame {}: {msg}", f.url.to_text()));
        let addr = f.url.to_text();
        self.install_error(
            ti,
            id,
            p.parent,
            p.generation,
            p.how,
            f.error.title(),
            &msg,
            crate::hint(&f.error),
            &addr,
            key,
            path,
            inherited,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn frame_response(
        &mut self,
        ti: usize,
        id: FrameId,
        p: Pending,
        resp: Response,
        cert: Option<browserkit::certinfo::CertInfo>,
        peer: Option<Vec<u8>>,
        inherited: Sandbox,
        key: String,
        path: Vec<String>,
    ) {
        let _ = cert;
        let parent = p.parent;
        let chain = self.parent_chain(ti, parent);
        let url = resp.url.clone();
        let address = match p.address.split_once('#') {
            Some((_, f)) => format!("{}#{f}", url.to_text()),
            None => url.to_text(),
        };
        let verdict = load::judge(&resp.headers, &url, &chain, inherited);
        for n in &verdict.notes {
            self.frame_console(ti, parent, ConsoleLevel::Warn, n);
        }
        rt::println!(
            "[browser] frame {}: {} {}, {} bytes",
            url,
            resp.status,
            resp.reason,
            resp.body.len()
        );
        if let Some(b) = verdict.blocked {
            self.frame_console(ti, parent, ConsoleLevel::Error, &b.console(&url));
            rt::println!("[browser] frame {}: blocked: {}", url, b.reason());
            self.install_error(
                ti,
                id,
                parent,
                p.generation,
                p.how,
                "This frame is blocked",
                &b.reason(),
                "",
                &address,
                key,
                path,
                verdict.sandbox,
            );
            let _ = Blocked::FrameAncestors(String::new());
            return;
        }
        let ct = String::from(resp.content_type());
        let sandbox = verdict.sandbox;
        let (bytes, ct): (Vec<u8>, String) = match load::answer_kind(&ct, resp.header("content-disposition")) {
            Answer::Page => (resp.body.clone(), ct.clone()),
            Answer::Image => (
                format!(
                    "<!DOCTYPE html><html><body style=\"margin:0\"><img src=\"{}\"></body></html>",
                    url.to_text().replace('"', "%22")
                )
                .into_bytes(),
                String::from("text/html"),
            ),
            Answer::Download => {
                let why = if sandbox.no_downloads() {
                    "A sandboxed frame can't download a file."
                } else {
                    "A file can't be shown in a frame."
                };
                self.frame_console(ti, parent, ConsoleLevel::Warn, &format!("Not showing {} in a frame: it is a file to download.", url.to_text()));
                self.install_error(
                    ti,
                    id,
                    parent,
                    p.generation,
                    p.how,
                    "This frame can't show a file",
                    why,
                    "Open the address in a tab of its own to download it.",
                    &address,
                    key,
                    path,
                    sandbox,
                );
                return;
            }
        };
        for w in sandbox.warnings() {
            self.frame_console(ti, parent, ConsoleLevel::Warn, w);
        }
        let opaque = sandbox.opaque_origin();
        let scripts = self.js_on && !sandbox.no_scripts();
        let mut doc = Doc::parse_frame(&bytes, &url.to_text(), &ct, scripts);
        doc.set_frame_security(FrameSec {
            origin: None,
            opaque,
            location: None,
            no_modals: sandbox.no_modals(),
            no_popups: sandbox.no_popups(),
        });
        let doc_id = self.next_doc_id();
        let cookies_ok = load::cookies_allowed(&chain, &url, sandbox);
        let sub_site = match load::cookie_context(&chain, &url) {
            Context::Subresource { page_site } => page_site,
            _ => String::new(),
        };
        let ref_policy = resp
            .header("referrer-policy")
            .and_then(ReferrerPolicy::parse_header)
            .unwrap_or_else(|| self.parent_identity(ti, parent).5);
        let title = String::new();
        let meta = Meta {
            doc_id,
            kind: FrameKind::Web,
            url: Some(url.clone()),
            ancestor_url: Some(url.clone()),
            storage_key: (!opaque).then(|| crate::js::origin(&url)),
            origin: if opaque {
                String::from("null")
            } else {
                webengine::script::xorigin::origin_of(&url.to_text())
            },
            sandbox,
            sub_site,
            cookies_ok,
            ref_policy,
            label: Self::frame_label(&path),
            key,
            path,
            generation: p.generation.unwrap_or(0),
            space: peer.as_deref().and_then(browserkit::netpolicy::address_space_of_ip),
            status: resp.status,
            notified: false,
            shown: BTreeSet::new(),
            waiting: 0,
            title,
        };
        self.install_doc(ti, id, p.generation, doc, meta, p.how, &address);
    }

    #[allow(clippy::too_many_arguments)]
    fn install_error(
        &mut self,
        ti: usize,
        id: FrameId,
        parent: FrameId,
        generation: Option<u32>,
        how: How,
        title: &str,
        message: &str,
        hint: &str,
        address: &str,
        key: String,
        path: Vec<String>,
        sandbox: Sandbox,
    ) {
        let _ = sandbox;
        let html = pages::error(title, message, hint, address);
        let mut doc = Doc::parse_frame(html.as_bytes(), "about:blank", "text/html", false);
        doc.set_frame_security(FrameSec {
            origin: None,
            opaque: true,
            location: Some(String::from(address)),
            no_modals: true,
            no_popups: true,
        });
        let doc_id = self.next_doc_id();
        let meta = Meta {
            doc_id,
            kind: FrameKind::Error,
            url: None,
            ancestor_url: None,
            storage_key: None,
            origin: String::from("null"),
            sandbox: Sandbox::FULL,
            sub_site: String::new(),
            cookies_ok: false,
            ref_policy: ReferrerPolicy::DEFAULT,
            label: Self::frame_label(&path),
            key,
            path,
            generation: generation.unwrap_or(0),
            space: None,
            status: 0,
            notified: false,
            shown: BTreeSet::new(),
            waiting: 0,
            title: String::from(title),
        };
        let g = generation;
        if let Some(g) = g {
            self.tabs[ti].frames.done.insert(id, g);
        }
        let _ = parent;
        self.install_doc(ti, id, generation, doc, meta, how, address);
    }

    /// Put a document into frame `id` and record it: history, the frame's facts, its resources
    /// and its scripts.
    #[allow(clippy::too_many_arguments)]
    fn install_doc(
        &mut self,
        ti: usize,
        id: FrameId,
        generation: Option<u32>,
        doc: webengine::Document,
        meta: Meta,
        how: How,
        address: &str,
    ) {
        let doc_id = meta.doc_id;
        let path = meta.path.clone();
        if self.log {
            rt::println!("[browser] frame {id:?} gets a document: {address}");
        }
        if !self.tabs[ti].page.doc.attach(id, doc, generation) {
            return; // the slot moved on
        }
        {
            let t = &mut self.tabs[ti];
            // A window keeps its name across navigations in the frame (the engine does).
            match &how {
                How::Initial => t.history.frame_loaded(&path, doc_id, address, true, false),
                How::Navigate { replace } => {
                    t.history.frame_loaded(&path, doc_id, address, false, *replace)
                }
                How::Restore(node) => t.history.rebind(node.doc, doc_id),
            }
            t.frames.meta.insert(id, meta);
            t.page.laid_w = 0;
        }
        self.dirty = true;
        // Its own frames first: they hold its `load` before its scripts get a turn.
        self.sync_frames(ti);
        self.feed(ti);
        self.run_frame(ti, id);
        self.sync_frames(ti);
    }

    // ---- navigation ----

    /// Navigate frame `id` of tab `ti`.
    pub fn navigate_frame(&mut self, ti: usize, id: FrameId, mut nav: Nav) {
        if id.is_top() {
            return;
        }
        let Some(parent) = self.tabs[ti].page.doc.frame(id).and_then(|d| d.frame_info().parent) else {
            return;
        };
        self.cancel_frame_load(ti, id);
        let slots = self.tabs[ti].page.doc.slots();
        let paths = self.frame_paths(ti, &slots);
        let (key, path) = paths.get(&id).cloned().unwrap_or_default();
        let Some(slot) = slots.iter().find(|s| s.id == id) else {
            return;
        };
        let sandbox_attr = sandbox_of(slot.sandbox);
        let inherited = self.sandbox_of_frame(ti, parent).merge(sandbox_attr);
        // A new navigation supersedes what a traversal planned.
        self.tabs[ti].frames.restore.clear();
        if nav.how.is_none() {
            nav.how = Some(How::Navigate { replace: nav.replace });
        }
        self.tabs[ti].frames.done.remove(&id);
        let policy = slot.referrer_policy.clone();
        let attr = (nav.source == parent).then_some(policy.as_str());
        self.begin_http(ti, id, parent, None, nav, inherited, attr, key, path);
    }

    /// A link was followed in frame `from` (by the user or a script): choose the frame or tab
    /// by `target`, check the sandbox, go.
    #[allow(clippy::too_many_arguments)]
    pub fn follow_in(
        &mut self,
        from: FrameId,
        href: &str,
        target_attr: Option<&str>,
        new_tab: bool,
        save: bool,
        activated: bool,
    ) {
        let ti = self.cur;
        let base = self.tabs[ti]
            .page
            .doc
            .base_url_of(from)
            .or_else(|| self.tabs[ti].page.url.clone());
        let site_from = self.referrer_source(ti, from).0.map(|u| site_of(&u.host));
        let target = address::resolve(base.as_ref(), href);
        if save {
            if let Target::Web(u, _) = &target {
                self.start_download(u, site_from);
            }
            return;
        }
        let choice = if new_tab {
            Choice::NewTab
        } else {
            let tree = TabTree {
                doc: &self.tabs[ti].page.doc,
            };
            nav::choose(&tree, from.0, target_attr.unwrap_or(""))
        };
        // Which frame, and may the asker send it there?
        let sb = self.sandbox_of_frame(ti, from);
        let verdict = {
            let tree = TabTree {
                doc: &self.tabs[ti].page.doc,
            };
            nav::allowed(&tree, from.0, &choice, sb, activated)
        };
        if let Err(d) = verdict {
            self.refuse_navigation(ti, from, d);
            return;
        }
        match choice {
            Choice::NewTab => {
                if self.tabs.len() >= MAX_TABS {
                    self.say("That's a lot of tabs: close one first");
                    return;
                }
                let p = self.internal_page(Internal::NewTab);
                self.tabs.push(Tab::new(p));
                let n = self.tabs.len() - 1;
                self.go(n, target, true, site_from, None);
                self.say("Opened in a new tab (Ctrl-Tab to go to it)");
            }
            Choice::Frame(f) if f == FrameId::TOP.0 => {
                self.go(ti, target, true, site_from, None);
            }
            Choice::Frame(f) => match target {
                Target::Web(url, fragment) => {
                    self.navigate_frame(
                        ti,
                        FrameId(f),
                        Nav {
                            url,
                            fragment,
                            post: None,
                            replace: false,
                            source: from,
                            how: None,
                        },
                    );
                }
                Target::Bad(why) => self.say(&why),
                _ => {}
            },
        }
    }

    pub fn refuse_navigation(&mut self, ti: usize, from: FrameId, d: Denied) {
        self.frame_console(ti, from, ConsoleLevel::Error, &d.console());
        self.say("A sandboxed frame tried to navigate: refused");
    }

    /// A form was submitted in frame `from`: its target (a frame, `_top`, a new tab) decides.
    pub fn submit_from(&mut self, from: FrameId, sub: Submission) {
        let ti = self.cur;
        let sb = self.sandbox_of_frame(ti, from);
        if sb.no_forms() {
            self.frame_console(
                ti,
                from,
                ConsoleLevel::Error,
                "Blocked a form submission from a sandboxed frame: the sandbox does not have 'allow-forms'.",
            );
            self.say("A sandboxed frame tried to send a form: refused");
            return;
        }
        let Ok(req) = browserkit::form::request(&sub) else {
            self.say("The form can't be sent");
            return;
        };
        let choice = {
            let tree = TabTree {
                doc: &self.tabs[ti].page.doc,
            };
            nav::choose(&tree, from.0, sub.target.as_deref().unwrap_or(""))
        };
        let verdict = {
            let tree = TabTree {
                doc: &self.tabs[ti].page.doc,
            };
            nav::allowed(&tree, from.0, &choice, sb, true)
        };
        if let Err(d) = verdict {
            self.refuse_navigation(ti, from, d);
            return;
        }
        let site_from = self.referrer_source(ti, from).0.map(|u| site_of(&u.host));
        match choice {
            Choice::Frame(f) if f != 0 => {
                let url = req.url.clone();
                self.navigate_frame(
                    ti,
                    FrameId(f),
                    Nav {
                        url,
                        fragment: None,
                        post: Some(req),
                        replace: false,
                        source: from,
                        how: None,
                    },
                );
            }
            Choice::NewTab => {
                if self.tabs.len() >= MAX_TABS {
                    return;
                }
                let p = self.internal_page(Internal::NewTab);
                self.tabs.push(Tab::new(p));
                self.cur = self.tabs.len() - 1;
                let n = self.cur;
                let target = Target::Web(req.url.clone(), None);
                self.go(n, target, true, site_from, Some(req));
            }
            Choice::Frame(_) => {
                let target = Target::Web(req.url.clone(), None);
                self.go(ti, target, true, site_from, Some(req));
            }
        }
    }

    // ---- the tick ----

    /// Once a tick: new slots, the `load` events of finished frames, the load hold on parents,
    /// and the messages frames posted.
    pub fn frames_tick(&mut self) -> bool {
        let mut changed = false;
        for ti in 0..self.tabs.len() {
            if self.tabs[ti].page.kind != PageKind::Web {
                continue;
            }
            self.sync_frames(ti);
            if self.tabs[ti].frames.meta.is_empty() && self.tabs[ti].frames.loads.is_empty() {
                continue;
            }
            self.frame_budget(ti);
            changed |= self.frame_loads(ti);
            changed |= self.pump_messages(ti);
        }
        changed
    }

    /// `load` events for frames that are done (deepest first), and the hold on their parents.
    fn frame_loads(&mut self, ti: usize) -> bool {
        let mut changed = false;
        // Is anything inside frame `f` (a child slot) still coming?
        fn kids_busy(t: &Tab, f: FrameId) -> bool {
            t.page.doc.children(f).into_iter().any(|c| {
                t.frames.loads.contains_key(&c)
                    || t.frames.wanting.contains(&c)
                    || t.frames.meta.get(&c).is_some_and(|m| !m.notified)
            })
        }
        let mut order: Vec<(u32, FrameId)> = self.tabs[ti]
            .frames
            .meta
            .keys()
            .map(|&f| {
                let d = self.tabs[ti]
                    .page
                    .doc
                    .frame(f)
                    .map_or(0, |d| d.frame_info().depth);
                (d, f)
            })
            .collect();
        order.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, f) in order {
            let t = &self.tabs[ti];
            let Some(m) = t.frames.meta.get(&f) else { continue };
            let done = !m.notified
                && m.waiting == 0
                && t.page.doc.is_loaded(f)
                && !kids_busy(t, f);
            if done {
                let parent = t
                    .page
                    .doc
                    .frame(f)
                    .and_then(|d| d.frame_info().parent)
                    .unwrap_or(FrameId::TOP);
                self.with_doc_in(ti, parent, |d, h| {
                    d.fire_frame_load(h, f);
                });
                if let Some(m) = self.tabs[ti].frames.meta.get_mut(&f) {
                    m.notified = true;
                }
                if self.log {
                    rt::println!("[browser] frame {f:?} loaded");
                }
                changed = true;
            }
        }
        self.update_holds(ti);
        changed
    }

    /// The frames of a tab share a budget: when their scripts together hold too much memory the
    /// greediest frame's scripts end (the page and the other frames carry on).
    fn frame_budget(&mut self, ti: usize) {
        let t = &self.tabs[ti];
        let mut total = 0usize;
        let mut worst: Option<(usize, FrameId)> = None;
        for f in t.page.doc.frame_ids() {
            let st = t.page.doc.script_stats_in(f);
            if st.stopped {
                continue;
            }
            total += st.heap_bytes;
            if worst.is_none_or(|w| st.heap_bytes > w.0) {
                worst = Some((st.heap_bytes, f));
            }
        }
        if total > crate::web::FRAMES_HEAP
            && let Some((_, f)) = worst
        {
            self.frame_console(
                ti,
                f,
                ConsoleLevel::Error,
                "The frames of this page use too much memory: the scripts of this frame were stopped.",
            );
            self.tabs[ti].page.doc.stop_scripts_in(f);
        }
    }

    /// A document's own `load` waits for the frames in it.
    fn update_holds(&mut self, ti: usize) {
        fn kids_busy(t: &Tab, f: FrameId) -> bool {
            t.page.doc.children(f).into_iter().any(|c| {
                t.frames.loads.contains_key(&c)
                    || t.frames.wanting.contains(&c)
                    || t.frames.meta.get(&c).is_some_and(|m| !m.notified)
            })
        }
        let mut docs: Vec<FrameId> = alloc::vec![FrameId::TOP];
        docs.extend(self.tabs[ti].frames.meta.keys().copied());
        for d in docs {
            let busy = kids_busy(&self.tabs[ti], d);
            let was = self.tabs[ti].frames.held.contains(&d);
            if busy != was {
                if busy {
                    self.tabs[ti].frames.held.insert(d);
                } else {
                    self.tabs[ti].frames.held.remove(&d);
                    // Its `load` may go off now: its scripts get a turn.
                    self.tabs[ti].last_run = 0;
                }
            }
            self.tabs[ti].page.doc.set_load_hold(d, busy);
        }
    }

    /// Deliver the messages the frames' scripts posted.
    fn pump_messages(&mut self, ti: usize) -> bool {
        let mut any = false;
        for _ in 0..8 {
            let msgs = self.tabs[ti].page.doc.messages();
            if msgs.is_empty() {
                break;
            }
            for (to, m) in msgs {
                let ran = self.with_doc_in(ti, to, |d, h| d.deliver(h, to, &m));
                any |= ran;
            }
        }
        any
    }

    // ---- history ----

    /// Back or forward by `delta` entries of the joint session history. Returns false if there
    /// is nowhere to go.
    pub fn traverse(&mut self, delta: isize) -> bool {
        let ti = self.cur;
        // Save where every frame was scrolled.
        let ids = self.tabs[ti].frames.meta.keys().copied().collect::<Vec<_>>();
        for id in ids {
            let sc = self.tabs[ti].page.doc.frame_scroll(id).1;
            let path = self.tabs[ti].frames.meta[&id].path.clone();
            self.tabs[ti].history.save_scroll(&path, sc);
        }
        let scroll = self.tabs[ti].scroll;
        let Some(tr) = self.tabs[ti].history.traverse(delta, scroll) else {
            return false;
        };
        self.popup = None;
        if tr.top_reload {
            // The whole page again, with the frames the entry had.
            let root = Node {
                key: String::new(),
                doc: tr.entry.doc,
                url: tr.entry.url.clone(),
                scroll: tr.entry.scroll,
                children: tr.entry.frames.clone(),
            };
            let plan = alloc::vec![Reload { path: Vec::new(), node: root }];
            let target = address::interpret(&tr.entry.url);
            self.go_restoring(ti, target, plan, tr.entry.doc, tr.entry.scroll);
            return true;
        }
        for rl in &tr.reloads {
            // Find the frame by its path and send it back to the address of the entry.
            let found = self.tabs[ti]
                .frames
                .meta
                .iter()
                .find(|(_, m)| m.path == rl.path)
                .map(|(f, _)| *f);
            if let Some(f) = found
                && let Target::Web(u, frag) = address::resolve(None, &rl.node.url)
            {
                self.navigate_frame(
                    ti,
                    f,
                    Nav {
                        url: u,
                        fragment: frag,
                        post: None,
                        replace: false,
                        source: FrameId::TOP,
                        how: Some(How::Restore(rl.node.clone())),
                    },
                );
            }
        }
        self.tabs[ti].frames.restore = tr.reloads.clone();
        for r in &tr.restores {
            if r.path.is_empty() {
                let t = &mut self.tabs[ti];
                t.scroll = r.scroll;
                t.target = r.scroll;
                if r.url_changed
                    && let Ok(u) = Url::parse(&r.url)
                {
                    t.page.url = Some(u);
                    t.page.address = r.url.clone();
                }
            } else if let Some(f) = self.tabs[ti]
                .frames
                .meta
                .iter()
                .find(|(_, m)| m.path == r.path)
                .map(|(f, _)| *f)
            {
                self.tabs[ti].page.doc.set_frame_scroll(f, r.scroll);
                self.tabs[ti].page.laid_w = 0;
            }
        }
        self.dirty = true;
        true
    }

    /// Reload frame `f` (its script said `location.reload()`).
    pub fn reload_frame(&mut self, ti: usize, f: FrameId) {
        let Some(m) = self.tabs[ti].frames.meta.get(&f) else {
            return;
        };
        let Some(u) = m.url.clone() else { return };
        self.navigate_frame(
            ti,
            f,
            Nav {
                url: u,
                fragment: None,
                post: None,
                replace: true,
                source: FrameId::TOP,
                how: None,
            },
        );
    }

    /// The site of the document frame `f` runs (for a navigation's `SameSite` view).
    pub fn referrer_site(&self, ti: usize, f: FrameId) -> Option<String> {
        self.referrer_source(ti, f).0.map(|u| site_of(&u.host))
    }
}

