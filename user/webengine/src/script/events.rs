//! Event listeners and the propagation path (DOM §2.9 "Dispatching events"), generic over
//! the script engine's function handle. The bindings walk [`plan`] and, at each stop, call
//! the listeners [`Registry::snapshot`] gives (checking [`Registry::still_there`] before each,
//! so one removed by an earlier listener doesn't run).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::dom::{Dom, NONE, NodeId};

/// The event target id of `window` (node ids are below it).
pub const WINDOW: u32 = u32::MAX - 1;

/// Listeners kept per page, over all targets.
pub const MAX_LISTENERS: usize = 100_000;

#[derive(Clone, Debug)]
pub struct Listener<H> {
    pub id: u64,
    pub cb: H,
    pub capture: bool,
    pub once: bool,
    pub passive: bool,
}

pub struct Registry<H> {
    map: BTreeMap<(u32, String), Vec<Listener<H>>>,
    next: u64,
    count: usize,
}

impl<H: Clone + PartialEq> Default for Registry<H> {
    fn default() -> Self {
        Self::new()
    }
}

/// The event phases (Event.eventPhase).
pub const CAPTURING: u8 = 1;
pub const AT_TARGET: u8 = 2;
pub const BUBBLING: u8 = 3;

impl<H: Clone + PartialEq> Registry<H> {
    pub fn new() -> Registry<H> {
        Registry {
            map: BTreeMap::new(),
            next: 1,
            count: 0,
        }
    }

    /// addEventListener: false if it was already there (same callback and capture) or the
    /// page has too many.
    pub fn add(
        &mut self,
        target: u32,
        ty: &str,
        cb: H,
        capture: bool,
        once: bool,
        passive: bool,
    ) -> bool {
        if self.count >= MAX_LISTENERS {
            return false;
        }
        let v = self.map.entry((target, String::from(ty))).or_default();
        if v.iter().any(|l| l.cb == cb && l.capture == capture) {
            return false;
        }
        v.push(Listener {
            id: self.next,
            cb,
            capture,
            once,
            passive,
        });
        self.next += 1;
        self.count += 1;
        true
    }

    /// removeEventListener. The handle that went away, if one did (to unpin it).
    pub fn remove(&mut self, target: u32, ty: &str, cb: &H, capture: bool) -> Option<H> {
        let key = (target, String::from(ty));
        let v = self.map.get_mut(&key)?;
        let i = v.iter().position(|l| &l.cb == cb && l.capture == capture)?;
        let l = v.remove(i);
        self.count -= 1;
        if v.is_empty() {
            self.map.remove(&key);
        }
        Some(l.cb)
    }

    pub fn remove_id(&mut self, target: u32, ty: &str, id: u64) -> Option<H> {
        let key = (target, String::from(ty));
        let v = self.map.get_mut(&key)?;
        let i = v.iter().position(|l| l.id == id)?;
        let l = v.remove(i);
        self.count -= 1;
        if v.is_empty() {
            self.map.remove(&key);
        }
        Some(l.cb)
    }

    /// The listeners on `target` for `ty`, as they are now.
    pub fn snapshot(&self, target: u32, ty: &str) -> Vec<Listener<H>> {
        self.map
            .get(&(target, String::from(ty)))
            .cloned()
            .unwrap_or_default()
    }

    pub fn still_there(&self, target: u32, ty: &str, id: u64) -> bool {
        self.map
            .get(&(target, String::from(ty)))
            .is_some_and(|v| v.iter().any(|l| l.id == id))
    }

    /// Does anything listen for `ty` anywhere?
    pub fn any(&self, ty: &str) -> bool {
        self.map.keys().any(|(_, t)| t == ty)
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Every handle (for the bindings to unpin when the page goes).
    pub fn all(&self) -> Vec<H> {
        self.map
            .values()
            .flat_map(|v| v.iter().map(|l| l.cb.clone()))
            .collect()
    }

    pub fn clear(&mut self) {
        self.map.clear();
        self.count = 0;
    }
}

/// The propagation path of an event at `target`: the target, its ancestors, and (for a node
/// in the document, or the document itself) the window at the end.
pub fn path(dom: &Dom, target: u32) -> Vec<u32> {
    if target == WINDOW {
        return alloc::vec![WINDOW];
    }
    let mut v = Vec::new();
    let mut n: NodeId = target;
    while n != NONE && (n as usize) < dom.len() {
        v.push(n);
        if v.len() > 1 << 16 {
            break;
        }
        n = dom.parent(n);
    }
    if v.last() == Some(&0) {
        v.push(WINDOW);
    }
    v
}

/// The stops of a dispatch along `path` (target first): capture from the top down to the
/// target's parent, the target, then (if the event bubbles) back up. Each stop says which
/// listeners run there: at the target both kinds, capture ones first.
pub fn plan(path: &[u32], bubbles: bool) -> Vec<(u32, u8)> {
    let mut out = Vec::new();
    if path.is_empty() {
        return out;
    }
    for &n in path[1..].iter().rev() {
        out.push((n, CAPTURING));
    }
    out.push((path[0], AT_TARGET));
    if bubbles {
        for &n in &path[1..] {
            out.push((n, BUBBLING));
        }
    }
    out
}

/// Which listeners run at a stop: during capturing only capture ones, while bubbling only
/// the others, at the target capture ones first, then the others.
pub fn runs_at<H>(l: &Listener<H>, phase: u8, second_pass: bool) -> bool {
    match phase {
        CAPTURING => l.capture,
        BUBBLING => !l.capture,
        _ => l.capture != second_pass,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_and_plan() {
        let mut r: Registry<u32> = Registry::new();
        assert!(r.add(5, "click", 1, false, false, false));
        assert!(!r.add(5, "click", 1, false, false, false));
        assert!(r.add(5, "click", 1, true, false, false));
        assert!(r.add(5, "click", 2, false, true, false));
        assert_eq!(r.snapshot(5, "click").len(), 3);
        assert_eq!(r.remove(5, "click", &1, true), Some(1));
        let s = r.snapshot(5, "click");
        assert!(r.still_there(5, "click", s[1].id));
        r.remove_id(5, "click", s[1].id);
        assert!(!r.still_there(5, "click", s[1].id));
        assert!(r.any("click") && !r.any("keydown"));
        assert_eq!(
            plan(&[7, 3, 0, WINDOW], true),
            [
                (WINDOW, CAPTURING),
                (0, CAPTURING),
                (3, CAPTURING),
                (7, AT_TARGET),
                (3, BUBBLING),
                (0, BUBBLING),
                (WINDOW, BUBBLING)
            ]
        );
        assert_eq!(plan(&[7, 3], false), [(3, CAPTURING), (7, AT_TARGET)]);
    }
}
