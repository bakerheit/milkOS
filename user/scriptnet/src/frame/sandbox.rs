//! The `sandbox` attribute of an `<iframe>` and the `sandbox` directive of a response's
//! `Content-Security-Policy`: the HTML standard's "sandboxing flag set".
//!
//! A [`Sandbox`] is the set of things a framed document is **not** allowed to do. An
//! `<iframe>` without the attribute has [`Sandbox::NONE`] (nothing restricted). The attribute
//! alone (`sandbox` or `sandbox=""`) restricts everything; each `allow-*` token takes one
//! restriction away. A frame inherits its parent document's restrictions (a sandboxed page
//! cannot hand out powers it does not have), and a response's CSP `sandbox` directive adds to
//! them. [`Sandbox::merge`] is the union of restrictions.
//!
//! # What the Browser enforces
//!
//! | token | restriction taken away | enforced as |
//! | --- | --- | --- |
//! | `allow-scripts` | scripts | no script of the frame runs: not `<script>`, not `on*` attributes, not timers |
//! | `allow-same-origin` | opaque origin | without it the document's origin is opaque: no cookies, no storage, never same-origin with anything, `postMessage` events say `null` |
//! | `allow-forms` | forms | a form in the frame is not sent (submit buttons, `form.submit()`) |
//! | `allow-popups` | popups | `window.open` and `target=_blank` do nothing |
//! | `allow-popups-to-escape-sandbox` | popups inherit the sandbox | a popup (a new tab) from the frame is opened without the sandbox: **not tracked**, the Browser opens popups as plain tabs, so this token has no visible effect (it is parsed and kept) |
//! | `allow-top-navigation` | top navigation | `target=_top`, `top.location = ...` |
//! | `allow-top-navigation-by-user-activation` | top navigation without a click | top navigation only from a click or key the user just made |
//! | `allow-top-navigation-to-custom-protocols` | custom protocols | parsed; the Browser only has http(s) targets |
//! | `allow-modals` | `alert`, `confirm`, `prompt` | the call returns at once (`undefined`, `false`, `null`) |
//! | `allow-downloads` | downloads | an answer that would be a download is refused in the frame |
//! | `allow-pointer-lock`, `allow-orientation-lock`, `allow-presentation`, `allow-storage-access-by-user-activation` | features the Browser does not have | parsed and kept |
//!
//! And always, for a sandboxed document: it may navigate only itself, its descendants and (with
//! the top-navigation tokens) the top page ([`super::nav`]).
//!
//! Unknown tokens are ignored (and reported, for the console). Tokens are ASCII
//! case-insensitive and separated by ASCII white space. A sandbox with both `allow-scripts`
//! and `allow-same-origin` can remove its own `sandbox` attribute: [`Sandbox::warnings`] says
//! so, as browsers do.

use alloc::string::String;
use alloc::vec::Vec;

/// The restrictions on a document, one bit each (set: restricted).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Sandbox(pub u32);

impl Sandbox {
    pub const SCRIPTS: u32 = 1 << 0;
    pub const ORIGIN: u32 = 1 << 1;
    pub const FORMS: u32 = 1 << 2;
    pub const POPUPS: u32 = 1 << 3;
    pub const TOP_NAVIGATION: u32 = 1 << 4;
    /// Top navigation is allowed, but only after the user's click or key.
    pub const TOP_NAVIGATION_NEEDS_ACTIVATION: u32 = 1 << 5;
    pub const MODALS: u32 = 1 << 6;
    pub const DOWNLOADS: u32 = 1 << 7;
    pub const POINTER_LOCK: u32 = 1 << 8;
    pub const ORIENTATION_LOCK: u32 = 1 << 9;
    pub const PRESENTATION: u32 = 1 << 10;
    pub const STORAGE_ACCESS: u32 = 1 << 11;
    /// A popup opened by the document inherits the sandbox.
    pub const POPUPS_INHERIT: u32 = 1 << 12;
    pub const CUSTOM_PROTOCOLS: u32 = 1 << 13;
    /// Navigating contexts other than itself, its descendants and the top (always on in a
    /// sandbox; there is no token to lift it).
    pub const NAVIGATION: u32 = 1 << 14;

    const ALL: u32 = (1 << 15) - 1;

    /// No sandbox: nothing is restricted.
    pub const NONE: Sandbox = Sandbox(0);
    /// `sandbox=""`: everything is restricted.
    pub const FULL: Sandbox = Sandbox(Self::ALL & !Self::TOP_NAVIGATION_NEEDS_ACTIVATION);

    pub fn is_sandboxed(self) -> bool {
        self.0 != 0
    }

    pub fn has(self, flag: u32) -> bool {
        self.0 & flag != 0
    }

    /// Scripts may not run.
    pub fn no_scripts(self) -> bool {
        self.has(Self::SCRIPTS)
    }

    /// The document's origin is opaque.
    pub fn opaque_origin(self) -> bool {
        self.has(Self::ORIGIN)
    }

    pub fn no_forms(self) -> bool {
        self.has(Self::FORMS)
    }

    pub fn no_popups(self) -> bool {
        self.has(Self::POPUPS)
    }

    pub fn no_modals(self) -> bool {
        self.has(Self::MODALS)
    }

    pub fn no_downloads(self) -> bool {
        self.has(Self::DOWNLOADS)
    }

    /// May the document navigate the top page? `activated`: the user just clicked or typed.
    pub fn top_navigation_allowed(self, activated: bool) -> bool {
        if !self.has(Self::TOP_NAVIGATION) {
            // allow-top-navigation (or no sandbox) lifts both restrictions.
            return true;
        }
        self.has(Self::TOP_NAVIGATION_NEEDS_ACTIVATION) && activated
    }

    /// The union of two sets of restrictions (a frame inside a sandboxed document, or a
    /// response's CSP sandbox on top of the attribute).
    pub fn merge(self, other: Sandbox) -> Sandbox {
        // The "needs activation" bit is a relaxation of TOP_NAVIGATION: when either side
        // forbids top navigation outright, the result does; when one allows it only by
        // activation and the other fully, it is by activation.
        let a = self.top_state();
        let b = other.top_state();
        let top = a.min_state(b);
        let rest = (self.0 | other.0) & !(Self::TOP_NAVIGATION | Self::TOP_NAVIGATION_NEEDS_ACTIVATION);
        Sandbox(rest | top.bits())
    }

    fn top_state(self) -> Top {
        match (
            self.has(Self::TOP_NAVIGATION),
            self.has(Self::TOP_NAVIGATION_NEEDS_ACTIVATION),
        ) {
            (false, _) => Top::Free,
            (true, false) => Top::Forbidden,
            (true, true) => Top::Activated,
        }
    }

    /// Parse the value of a `sandbox` attribute (or CSP directive): the restrictions it
    /// leaves. Returns the set and the tokens it did not know.
    pub fn parse(value: &str) -> (Sandbox, Vec<String>) {
        let mut flags = Self::FULL.0;
        let mut unknown = Vec::new();
        let mut by_activation = false;
        let mut full_top = false;
        for tok in value
            .split(|c: char| matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c'))
            .filter(|t| !t.is_empty())
        {
            let t = tok.to_ascii_lowercase();
            match t.as_str() {
                "allow-scripts" => flags &= !Self::SCRIPTS,
                "allow-same-origin" => flags &= !Self::ORIGIN,
                "allow-forms" => flags &= !Self::FORMS,
                "allow-popups" => flags &= !Self::POPUPS,
                "allow-popups-to-escape-sandbox" => flags &= !Self::POPUPS_INHERIT,
                "allow-top-navigation" => full_top = true,
                "allow-top-navigation-by-user-activation" => by_activation = true,
                "allow-top-navigation-to-custom-protocols" => flags &= !Self::CUSTOM_PROTOCOLS,
                "allow-modals" => flags &= !Self::MODALS,
                "allow-downloads" => flags &= !Self::DOWNLOADS,
                "allow-pointer-lock" => flags &= !Self::POINTER_LOCK,
                "allow-orientation-lock" => flags &= !Self::ORIENTATION_LOCK,
                "allow-presentation" => flags &= !Self::PRESENTATION,
                "allow-storage-access-by-user-activation" => flags &= !Self::STORAGE_ACCESS,
                _ => unknown.push(String::from(tok)),
            }
        }
        flags &= !(Self::TOP_NAVIGATION | Self::TOP_NAVIGATION_NEEDS_ACTIVATION);
        if full_top {
            // Fully allowed: no top restriction at all.
        } else if by_activation {
            flags |= Self::TOP_NAVIGATION | Self::TOP_NAVIGATION_NEEDS_ACTIVATION;
        } else {
            flags |= Self::TOP_NAVIGATION;
        }
        (Sandbox(flags), unknown)
    }

    /// Things worth a console line about this sandbox (the unknown tokens go separately).
    pub fn warnings(self) -> Vec<&'static str> {
        let mut w = Vec::new();
        if self.is_sandboxed() && !self.no_scripts() && !self.opaque_origin() {
            w.push(
                "An iframe which has both allow-scripts and allow-same-origin for its sandbox attribute can remove its sandboxing.",
            );
        }
        w
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Top {
    Free,
    Activated,
    Forbidden,
}

impl Top {
    /// The more restrictive of two.
    fn min_state(self, o: Top) -> Top {
        self.max(o)
    }

    fn bits(self) -> u32 {
        match self {
            Top::Free => 0,
            Top::Activated => Sandbox::TOP_NAVIGATION | Sandbox::TOP_NAVIGATION_NEEDS_ACTIVATION,
            Top::Forbidden => Sandbox::TOP_NAVIGATION,
        }
    }
}

/// Is `attr` (the value of the `sandbox` attribute, None if absent) a sandbox, and which.
pub fn from_attribute(attr: Option<&str>) -> Sandbox {
    match attr {
        None => Sandbox::NONE,
        Some(v) => Sandbox::parse(v).0,
    }
}
