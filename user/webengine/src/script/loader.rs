//! Which scripts run when (HTML §4.12.1 "The script element", §13.2.6.4.7 "the end").
//!
//! The page is parsed whole before any script runs (there is no pausing parser and so no
//! `document.write` into the stream: the bindings insert written markup after the running
//! script instead). Then, in order:
//!
//! 1. Parser-inserted scripts that block (inline ones, and external ones with neither
//!    `defer` nor `async`), in tree order, each waiting for its body.
//! 2. `readyState` becomes "interactive"; `defer` scripts in tree order.
//! 3. `DOMContentLoaded`.
//! 4. The `load` event once nothing is pending (scripts, stylesheets, images).
//!
//! `async` scripts run as soon as their body is in, at any point. Scripts a page inserts
//! later run on insertion (inline ones at once, external ones as soon as fetched; with
//! `async = false` in insertion order).
//!
//! `type=module` is reported as unsupported (stage 2) and not run; `nomodule` scripts do run
//! (this browser has no modules). Data blocks (`type=application/json`, `text/template`, ...)
//! never run.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::vec::Vec;

use crate::atoms::tag;
use crate::dom::{Dom, NONE, NodeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Classic,
    /// `type=module`: not run (yet).
    Module,
    /// Some other type: a data block, never run.
    Data,
}

/// A script element's type (HTML §4.12.1.1 "prepare the script element" steps 7-9).
pub fn kind_of(dom: &Dom, n: NodeId) -> Kind {
    let Some(e) = dom.element(n) else {
        return Kind::Data;
    };
    let ty = match e.attr("type") {
        Some(t) => String::from(t.trim()),
        None => match e.attr("language") {
            Some(l) if !l.is_empty() => alloc::format!("text/{}", l.trim()),
            _ => String::new(),
        },
    };
    let ty = ty.to_ascii_lowercase();
    if ty.is_empty() {
        return Kind::Classic;
    }
    // The JavaScript MIME type essences (MIME Sniffing §4.6), parameters ignored.
    let essence = ty.split(';').next().unwrap_or("").trim();
    match essence {
        "application/ecmascript"
        | "application/javascript"
        | "application/x-ecmascript"
        | "application/x-javascript"
        | "text/ecmascript"
        | "text/javascript"
        | "text/javascript1.0"
        | "text/javascript1.1"
        | "text/javascript1.2"
        | "text/javascript1.3"
        | "text/javascript1.4"
        | "text/javascript1.5"
        | "text/jscript"
        | "text/livescript"
        | "text/x-ecmascript"
        | "text/x-javascript" => Kind::Classic,
        "module" => Kind::Module,
        _ => Kind::Data,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    /// Inline text, or a fetched body.
    Ready(String),
    /// Waiting for the app to fetch it.
    Waiting,
    /// The fetch failed (an `error` event, nothing runs).
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum When {
    /// Parser-inserted, blocking: in order, before DOMContentLoaded.
    Blocking,
    /// `defer`: in order after the blocking ones.
    Defer,
    /// `async`, or inserted by a script: whenever ready.
    Async,
    /// Inserted by a script with `async = false`: in insertion order among those.
    Ordered,
}

#[derive(Clone, Debug)]
pub struct Script {
    pub node: NodeId,
    /// The absolute URL of an external script.
    pub src: Option<String>,
    pub when: When,
    pub body: Body,
    pub done: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// Running the blocking scripts.
    Loading,
    /// readyState "interactive": running defer scripts.
    Interactive,
    /// DOMContentLoaded has fired; waiting for the rest before `load`.
    ContentLoaded,
    /// `load` has fired.
    Complete,
}

pub struct Loader {
    pub scripts: Vec<Script>,
    /// Script elements that have been "prepared" (HTML's "already started"): never again.
    started: BTreeSet<NodeId>,
    pub phase: Phase,
    /// Scripts that turned out to be modules (for the console's note), by node.
    pub modules: Vec<NodeId>,
}

/// More scripts than this on one page are ignored.
pub const MAX_SCRIPTS: usize = 2_000;

impl Default for Loader {
    fn default() -> Self {
        Self::new()
    }
}

impl Loader {
    pub fn new() -> Loader {
        Loader {
            scripts: Vec::new(),
            started: BTreeSet::new(),
            phase: Phase::Loading,
            modules: Vec::new(),
        }
    }

    /// The parser-inserted scripts of a freshly parsed page, in tree order.
    pub fn collect(dom: &Dom, resolve: &dyn Fn(&str) -> String) -> Loader {
        let mut l = Loader::new();
        let mut n = 0;
        while n != NONE {
            if dom.is_html(n, tag::SCRIPT) {
                l.prepare(dom, n, true, false, resolve);
            }
            // Not inside templates (their content is inert).
            n = if dom.is_html(n, tag::TEMPLATE) {
                dom.next_skipping_children(n, 0)
            } else {
                dom.next_in(n, 0)
            };
        }
        l
    }

    /// Mark a script element as started without running it (innerHTML-inserted scripts).
    pub fn mark_started(&mut self, n: NodeId) {
        self.started.insert(n);
    }

    pub fn is_started(&self, n: NodeId) -> bool {
        self.started.contains(&n)
    }

    /// Let a script element be prepared again (`document.write` output, which browsers run).
    pub fn unmark_started(&mut self, n: NodeId) {
        self.started.remove(&n);
    }

    /// A node went back to the DOM (its id will be reused): forget it.
    pub fn forget(&mut self, n: NodeId) {
        self.started.remove(&n);
        self.modules.retain(|&m| m != n);
    }

    /// HTML's "prepare the script element" for a script at `n`: queue it (or not). For a
    /// script a page inserted (`parser` false) the element must be connected; an inline one
    /// with no text and no src yet is left unstarted (it can run once it gets them). `ordered`:
    /// an inserted external script whose `async` was set to false. Returns the index queued.
    pub fn prepare(
        &mut self,
        dom: &Dom,
        n: NodeId,
        parser: bool,
        ordered: bool,
        resolve: &dyn Fn(&str) -> String,
    ) -> Option<usize> {
        if self.started.contains(&n) || !dom.is_connected(n) {
            return None;
        }
        let e = dom.element(n)?;
        let src = e.attr("src").map(String::from);
        let text = dom.text_content(n);
        if src.is_none() && text.is_empty() {
            return None;
        }
        self.started.insert(n);
        match kind_of(dom, n) {
            Kind::Classic => {}
            Kind::Module => {
                self.modules.push(n);
                return None;
            }
            Kind::Data => return None,
        }
        if self.scripts.len() >= MAX_SCRIPTS {
            return None;
        }
        let is_async = e.has_attr("async");
        let when = match (&src, parser) {
            (Some(_), true) if is_async => When::Async,
            (Some(_), true) if e.has_attr("defer") => When::Defer,
            (_, true) => When::Blocking,
            // Inserted: external ones are async unless the page said `async = false`.
            (Some(_), false) if ordered && !is_async => When::Ordered,
            (Some(_), false) => When::Async,
            (None, false) => When::Async,
        };
        let (src, body) = match src {
            Some(s) if s.trim().is_empty() => (None, Body::Failed),
            Some(s) => (Some(resolve(s.trim())), Body::Waiting),
            None => (None, Body::Ready(text)),
        };
        self.scripts.push(Script {
            node: n,
            src,
            when,
            body,
            done: false,
        });
        Some(self.scripts.len() - 1)
    }

    /// External scripts whose bodies are wanted (absolute URLs, each once).
    pub fn pending(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for s in &self.scripts {
            if s.body == Body::Waiting
                && let Some(u) = &s.src
                && !v.contains(u)
            {
                v.push(u.clone());
            }
        }
        v
    }

    /// The body of `url` came (None: it failed). Every script waiting for it gets it.
    pub fn supply(&mut self, url: &str, body: Option<&str>) {
        for s in self.scripts.iter_mut() {
            if s.body == Body::Waiting && s.src.as_deref() == Some(url) {
                s.body = match body {
                    Some(b) => Body::Ready(String::from(b)),
                    None => Body::Failed,
                };
            }
        }
    }

    /// The next script to run (or fail) now, by the ordering rules, if one can.
    pub fn next(&self) -> Option<usize> {
        // Async ones whenever they are ready.
        if let Some(i) = self
            .scripts
            .iter()
            .position(|s| !s.done && s.when == When::Async && s.body != Body::Waiting)
        {
            return Some(i);
        }
        // Ordered (inserted, async = false): the first not done, once ready.
        if let Some(i) = self
            .scripts
            .iter()
            .position(|s| !s.done && s.when == When::Ordered)
            && self.scripts[i].body != Body::Waiting
        {
            return Some(i);
        }
        let first = |w: When| {
            self.scripts
                .iter()
                .position(|s| !s.done && s.when == w)
        };
        match self.phase {
            Phase::Loading => {
                let i = first(When::Blocking)?;
                (self.scripts[i].body != Body::Waiting).then_some(i)
            }
            Phase::Interactive => {
                let i = first(When::Defer)?;
                (self.scripts[i].body != Body::Waiting).then_some(i)
            }
            _ => None,
        }
    }

    /// Nothing more of the current phase can run until something is fetched; or the phase
    /// is over. The next phase, if the current one is over.
    pub fn advance(&mut self) -> Option<Phase> {
        let open = |w: When| self.scripts.iter().any(|s| !s.done && s.when == w);
        match self.phase {
            Phase::Loading if !open(When::Blocking) => {
                self.phase = Phase::Interactive;
                Some(self.phase)
            }
            Phase::Interactive if !open(When::Defer) => {
                self.phase = Phase::ContentLoaded;
                Some(self.phase)
            }
            _ => None,
        }
    }

    /// Scripts not run yet (any kind).
    pub fn unfinished(&self) -> usize {
        self.scripts.iter().filter(|s| !s.done).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(s: &str) -> String {
        alloc::format!("http://t.example/{s}")
    }

    #[test]
    fn order_of_blocking_defer_async() {
        let p = crate::html::parse_document(
            "<script src=a.js></script><script>inline1</script>\
             <script defer src=d1.js></script><script async src=as.js></script>\
             <script defer>inline-defer-runs-now</script><script type=module src=m.js></script>\
             <script type=application/json>{}</script><script defer src=d2.js></script>\
             <template><script>no</script></template><script></script>",
            &crate::api::Limits::default(),
        );
        let mut l = Loader::collect(&p.dom, &res);
        assert_eq!(l.scripts.len(), 6);
        assert_eq!(l.modules.len(), 1);
        assert_eq!(
            l.pending(),
            ["http://t.example/a.js", "http://t.example/d1.js", "http://t.example/as.js", "http://t.example/d2.js"]
        );
        // a.js blocks everything after it (but not the async one).
        assert_eq!(l.next(), None);
        l.supply("http://t.example/as.js", Some("async"));
        let i = l.next().unwrap();
        assert_eq!(l.scripts[i].body, Body::Ready(String::from("async")));
        l.scripts[i].done = true;
        assert_eq!(l.next(), None);
        l.supply("http://t.example/d2.js", Some("d2"));
        l.supply("http://t.example/a.js", None);
        let mut ran = Vec::new();
        loop {
            while let Some(i) = l.next() {
                ran.push(match &l.scripts[i].body {
                    Body::Ready(s) => s.clone(),
                    _ => String::from("(failed)"),
                });
                l.scripts[i].done = true;
            }
            if l.advance().is_none() {
                break;
            }
        }
        // Stuck waiting for d1.js: d2 (after it) waits too.
        assert_eq!(ran, ["(failed)", "inline1", "inline-defer-runs-now"]);
        assert_eq!(l.phase, Phase::Interactive);
        l.supply("http://t.example/d1.js", Some("d1"));
        while let Some(i) = l.next() {
            if let Body::Ready(s) = &l.scripts[i].body {
                ran.push(s.clone());
            }
            l.scripts[i].done = true;
        }
        assert_eq!(l.advance(), Some(Phase::ContentLoaded));
        assert_eq!(&ran[3..], ["d1", "d2"]);
    }

    #[test]
    fn types() {
        let p = crate::html::parse_document(
            "<script type='text/javascript; charset=utf-8'>1</script><script type=TEXT/JAVASCRIPT>2</script>\
             <script language=javascript>3</script><script type=text/template>4</script>",
            &crate::api::Limits::default(),
        );
        let l = Loader::collect(&p.dom, &res);
        assert_eq!(l.scripts.len(), 3);
    }
}
