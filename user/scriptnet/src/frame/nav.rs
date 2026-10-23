//! Where a navigation goes and whether the document asking may send it there: the `target`
//! of a link or form (`_self`, `_parent`, `_top`, `_blank`, a frame's name), and the sandbox
//! rules for navigating other frames and the top page. Pure, over a small tree trait.
//!
//! # Choosing the frame (the HTML standard's "rules for choosing a navigable")
//!
//! * empty, `_self` (any case): the frame itself.
//! * `_parent`: its parent, or itself in the top page.
//! * `_top`: the top page.
//! * `_blank` (any case): a new tab.
//! * a name: the first frame of that name found in this order: the frame itself, its
//!   descendants (tree order), then each ancestor in turn (nearest first) and *its*
//!   descendants. A name nobody has opens a new tab (the Browser has no named windows
//!   beyond that: a second link to the same unknown name opens another tab).
//!
//! # Who may navigate whom ([`allowed`])
//!
//! The HTML standard's "allowed by sandboxing to navigate":
//! 1. a frame may always navigate itself and its descendants;
//! 2. a non-top ancestor may be navigated by its descendant;
//! 3. the top page may be navigated by a descendant when it is not sandboxed against it
//!    (`allow-top-navigation`, or `allow-top-navigation-by-user-activation` after a click or
//!    key);
//! 4. any other frame (a sibling, a cousin) may be navigated unless the source is sandboxed
//!    (a sandbox has no way to lift that: it is the standard's "sandboxed navigation browsing
//!    context flag").
//! A new tab ([`Choice::NewTab`]) needs `allow-popups`.
//!
//! What is *not* checked: the standard's extra rule that a frame may not navigate frames of an
//! unrelated, cross-origin family (all frames here are in one tab, so they are one family).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::sandbox::Sandbox;

/// A frame id (the engine's).
pub type FrameId = u32;

/// The frame tree, as far as these rules look at it.
pub trait FrameTree {
    /// The top page's frame.
    fn top(&self) -> FrameId;
    fn parent(&self, f: FrameId) -> Option<FrameId>;
    /// Direct children, in tree order.
    fn children(&self, f: FrameId) -> Vec<FrameId>;
    /// The frame's `name` ("" if none).
    fn name(&self, f: FrameId) -> String;
}

/// Where a navigation goes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Choice {
    Frame(FrameId),
    /// A new tab (`_blank`, or a name nobody has).
    NewTab,
}

/// Choose the target of a navigation made by `source` (a frame) for the `target` attribute.
pub fn choose(tree: &dyn FrameTree, source: FrameId, target: &str) -> Choice {
    let t = target.trim();
    let lower = t.to_ascii_lowercase();
    match lower.as_str() {
        "" | "_self" => Choice::Frame(source),
        "_parent" => Choice::Frame(tree.parent(source).unwrap_or(source)),
        "_top" => Choice::Frame(tree.top()),
        "_blank" => Choice::NewTab,
        _ => {
            // A name that begins with an underscore is reserved but unknown: a new tab.
            if t.starts_with('_') {
                return Choice::NewTab;
            }
            match find_named(tree, source, t) {
                Some(f) => Choice::Frame(f),
                None => Choice::NewTab,
            }
        }
    }
}

fn descendants_named(tree: &dyn FrameTree, f: FrameId, name: &str, budget: &mut usize) -> Option<FrameId> {
    for c in tree.children(f) {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        if tree.name(c) == name {
            return Some(c);
        }
        if let Some(x) = descendants_named(tree, c, name, budget) {
            return Some(x);
        }
    }
    None
}

/// The frame called `name` nearest to `from` (see the module docs for the order).
pub fn find_named(tree: &dyn FrameTree, from: FrameId, name: &str) -> Option<FrameId> {
    let mut budget = 4096usize;
    let mut cur = Some(from);
    let mut hops = 0;
    while let Some(f) = cur {
        if tree.name(f) == name {
            return Some(f);
        }
        if let Some(x) = descendants_named(tree, f, name, &mut budget) {
            return Some(x);
        }
        hops += 1;
        if hops > 64 {
            return None;
        }
        cur = tree.parent(f);
    }
    None
}

/// The relationship of two frames.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Relation {
    Same,
    /// The target is inside the source.
    Descendant,
    /// The target contains the source (and is not the top page).
    Ancestor,
    /// The target is the top page and contains the source.
    Top,
    /// Neither contains the other.
    Other,
}

pub fn relation(tree: &dyn FrameTree, source: FrameId, target: FrameId) -> Relation {
    if source == target {
        return Relation::Same;
    }
    let mut cur = tree.parent(target);
    let mut hops = 0;
    while let Some(f) = cur {
        if f == source {
            return Relation::Descendant;
        }
        hops += 1;
        if hops > 64 {
            break;
        }
        cur = tree.parent(f);
    }
    let mut cur = tree.parent(source);
    let mut hops = 0;
    while let Some(f) = cur {
        if f == target {
            return if tree.parent(target).is_none() {
                Relation::Top
            } else {
                Relation::Ancestor
            };
        }
        hops += 1;
        if hops > 64 {
            break;
        }
        cur = tree.parent(f);
    }
    Relation::Other
}

/// Why a navigation was refused.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Denied {
    /// A sandboxed frame trying to navigate the top page without the permission.
    TopNavigation,
    /// A sandboxed frame trying to navigate a frame that is not it or inside it.
    Navigation,
    /// A sandboxed frame trying to open a new tab.
    Popups,
}

impl Denied {
    /// The console line.
    pub fn console(&self) -> String {
        match self {
            Denied::TopNavigation => String::from(
                "Unsafe attempt to initiate navigation for frame with origin 'null' from a sandboxed frame: the frame attempting navigation of the top-level window is sandboxed, but the flag of 'allow-top-navigation' or 'allow-top-navigation-by-user-activation' is not set.",
            ),
            Denied::Navigation => String::from(
                "Unsafe attempt to initiate navigation from a sandboxed frame: it may only navigate itself and the frames inside it.",
            ),
            Denied::Popups => String::from(
                "Blocked opening a new tab from a sandboxed frame: the sandbox does not have 'allow-popups'.",
            ),
        }
    }
}

/// May `source`, running under `sandbox`, navigate `choice`? `activated`: the user just
/// clicked or typed (for `allow-top-navigation-by-user-activation`).
pub fn allowed(
    tree: &dyn FrameTree,
    source: FrameId,
    choice: &Choice,
    sandbox: Sandbox,
    activated: bool,
) -> Result<(), Denied> {
    let target = match choice {
        Choice::NewTab => {
            return if sandbox.no_popups() {
                Err(Denied::Popups)
            } else {
                Ok(())
            };
        }
        Choice::Frame(f) => *f,
    };
    match relation(tree, source, target) {
        Relation::Same | Relation::Descendant | Relation::Ancestor => Ok(()),
        Relation::Top => {
            if sandbox.top_navigation_allowed(activated) {
                Ok(())
            } else {
                Err(Denied::TopNavigation)
            }
        }
        Relation::Other => {
            if sandbox.has(Sandbox::NAVIGATION) {
                Err(Denied::Navigation)
            } else {
                Ok(())
            }
        }
    }
}

/// A short description for a log line.
pub fn describe(c: &Choice) -> String {
    match c {
        Choice::Frame(f) => format!("frame {f}"),
        Choice::NewTab => String::from("a new tab"),
    }
}
