//! `MessageChannel` and `MessagePort`: the part of them that is not script (the classes
//! themselves are in `js/ports.js`).
//!
//! A port is an id. The ports of one page live in a [`PortTable`] that all its frames share
//! (the hub's, see `hub.rs`), so a port can be given to another frame (`otherWindow
//! .postMessage(x, '*', [port])`, or through another port) and keeps working: the table says
//! which realm owns it (only that realm's script can use it), what it is entangled with, and
//! what is waiting in its queue. A message is copied when it is posted (the structured clone of
//! `postmsg.rs`, as text) and put in the queue of the other end; the realm that owns that end
//! turns it into a `message` event as a task of its own event loop ([`pump`]), in the order
//! the messages were posted across all its ports, and never more than were posted before the
//! pass began, so a page whose scheduler posts a message from every message cannot starve its
//! timers (they run in each pass too).
//!
//! Ports in the transfer list of a message are *in transit*: no realm owns them, their queues
//! keep filling, and the realm that gets the message adopts them ([`adopt_list`]). A message
//! that cannot be delivered closes the ports it carried.
//!
//! The script objects are host objects of [`PortClass`] (data: the id). The realm keeps them
//! only weakly, except while a port is listening (an `onmessage` handler or `message`
//! listener): then it is held, so the other end can still reach it, until it is closed.
//!
//! Limits: [`MAX_PORTS`] open ports in a page, [`MAX_QUEUED`] messages and [`MAX_BYTES`]
//! bytes waiting in all queues; a message past them is dropped (a console note, once).

use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use jsengine::{Attrs, ClassId, HostClass, JsResult, Pinned, Realm, Value, WeakValue};

use super::bridge::{Bridge, arg, dom_error, type_error};

pub const MAX_PORTS: usize = 4096;
pub const MAX_QUEUED: usize = 20_000;
pub const MAX_BYTES: usize = 32 << 20;
/// Messages one pass of the event loop delivers at most.
pub const MAX_PER_PASS: usize = 1000;

/// Who can use a port: one realm (a frame's document).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub frame: u32,
    pub uid: u64,
}

#[derive(Debug)]
pub struct Msg {
    pub seq: u64,
    /// The structured clone (tagged JSON, `postmsg.rs`).
    pub data: String,
    /// The ports it carries (in transit until the receiver adopts them).
    pub ports: Vec<u64>,
}

struct Port {
    peer: Option<u64>,
    owner: Option<Owner>,
    queue: VecDeque<Msg>,
    started: bool,
    /// The port is one end of a pair that spans two threads (a page and its worker): messages
    /// for the far end leave through [`PortTable::take_link_out`] instead of being queued here.
    /// For the end that stayed behind (it has a `peer`) the messages its peer posts wait in its
    /// queue until they are taken; for the end that crossed (no `peer`) it is the posts of
    /// its own owner that leave.
    link: Option<u32>,
}

/// Something a port table has for the other thread of a link.
#[derive(Debug, PartialEq, Eq)]
pub enum LinkOut {
    /// A message for the far end of `port` (the same id names both ends); the ports it
    /// carries are exported with it.
    Msg { link: u32, port: u64, data: String, ports: Vec<u64> },
    /// `port` was closed: the far end closes too.
    Close { link: u32, port: u64 },
}

#[derive(Default)]
struct Inner {
    ports: BTreeMap<u64, Port>,
    next: u64,
    seq: u64,
    bytes: usize,
    queued: usize,
    /// Ports that have something to deliver now: owned, started, and a message waiting.
    ready: BTreeSet<u64>,
    /// Ports that are an end of a link.
    linked: usize,
    /// What leaves for other threads, in order.
    out: VecDeque<LinkOut>,
}

/// Why `post` refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostError {
    /// A port in the transfer list is not this realm's to give away (closed, in transit,
    /// someone else's), is listed twice, or is the port the message is posted on.
    BadTransfer,
}

/// The ports of a page.
#[derive(Default)]
pub struct PortTable {
    inner: RefCell<Inner>,
}

impl Inner {
    /// `id` (in transit) leaves over `link`: it stays here as the end that remembers its
    /// peer, and the far thread gets an end of its own.
    fn export(&mut self, id: u64, link: u32) {
        if let Some(p) = self.ports.get_mut(&id)
            && p.link.is_none()
        {
            p.link = Some(link);
            p.owner = None;
            self.linked += 1;
        }
        self.ready.remove(&id);
    }

    fn refresh(&mut self, id: u64) {
        let ready = self
            .ports
            .get(&id)
            .is_some_and(|p| p.started && p.owner.is_some() && !p.queue.is_empty());
        if ready {
            self.ready.insert(id);
        } else {
            self.ready.remove(&id);
        }
    }

    /// Closes `id`: its queue is dropped (and the ports in transit in it are closed too), its
    /// peer is on its own.
    fn close(&mut self, id: u64) {
        self.close_inner(id, true);
    }

    /// `close`, and for a port the far side of a link closed already: `echo` false says
    /// nothing back.
    fn close_inner(&mut self, id: u64, echo: bool) {
        let mut todo = alloc::vec![(id, echo)];
        while let Some((id, echo)) = todo.pop() {
            let Some(p) = self.ports.remove(&id) else {
                continue;
            };
            self.ready.remove(&id);
            if let Some(link) = p.link {
                self.linked = self.linked.saturating_sub(1);
                if echo && self.out.len() < MAX_QUEUED {
                    self.out.push_back(LinkOut::Close { link, port: id });
                }
            }
            if let Some(peer) = p.peer
                && let Some(q) = self.ports.get_mut(&peer)
            {
                q.peer = None;
                // The stay-behind end of a link has nothing left to talk to: it ends, and so
                // does the far end.
                if q.link.is_some() {
                    todo.push((peer, true));
                }
            }
            for m in p.queue {
                self.bytes = self.bytes.saturating_sub(m.data.len());
                self.queued = self.queued.saturating_sub(1);
                todo.extend(m.ports.into_iter().map(|x| (x, true)));
            }
        }
    }
}

impl PortTable {
    pub fn new() -> PortTable {
        PortTable::default()
    }

    /// A table whose ports are numbered in space `ns` (a worker's: the ports of two tables
    /// that are linked must not share an id).
    pub fn with_namespace(ns: u64) -> PortTable {
        let t = PortTable::default();
        t.inner.borrow_mut().next = ns << 40;
        t
    }

    /// Open ports.
    pub fn len(&self) -> usize {
        self.inner.borrow().ports.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Messages waiting in all queues.
    pub fn queued(&self) -> usize {
        self.inner.borrow().queued
    }

    /// Two entangled ports owned by `owner`; None if the page has too many.
    pub fn new_pair(&self, owner: Owner) -> Option<(u64, u64)> {
        let mut t = self.inner.borrow_mut();
        if t.ports.len() + 2 > MAX_PORTS {
            return None;
        }
        let a = t.next + 1;
        let b = t.next + 2;
        t.next += 2;
        for (id, peer) in [(a, b), (b, a)] {
            t.ports.insert(
                id,
                Port {
                    peer: Some(peer),
                    owner: Some(owner),
                    queue: VecDeque::new(),
                    started: false,
                    link: None,
                },
            );
        }
        Some((a, b))
    }

    /// Is `id` an open port?
    pub fn is_open(&self, id: u64) -> bool {
        self.inner.borrow().ports.contains_key(&id)
    }

    /// Does `owner` own open port `id`?
    pub fn owns(&self, id: u64, owner: Owner) -> bool {
        self.inner
            .borrow()
            .ports
            .get(&id)
            .is_some_and(|p| p.owner == Some(owner))
    }

    /// The sequence number the next message gets (messages with a smaller one were posted).
    pub fn seq(&self) -> u64 {
        self.inner.borrow().seq
    }

    /// Checks a transfer list against `owner` and takes the ports out of its hands (they are
    /// in transit now). `except` is the port a message is posted on.
    pub fn transfer_out(&self, owner: Owner, ids: &[u64], except: Option<u64>) -> Result<(), PostError> {
        let mut t = self.inner.borrow_mut();
        for (i, &id) in ids.iter().enumerate() {
            // (A port that came over a link is not sent on: its far end is on the other thread.)
            let ok = Some(id) != except
                && !ids[..i].contains(&id)
                && t.ports.get(&id).is_some_and(|p| p.owner == Some(owner) && p.link.is_none());
            if !ok {
                return Err(PostError::BadTransfer);
            }
        }
        for &id in ids {
            if let Some(p) = t.ports.get_mut(&id) {
                p.owner = None;
            }
            t.refresh(id);
        }
        Ok(())
    }

    /// `owner` posts a message on port `from`. Nothing happens (and Ok) when the port is
    /// closed or not entangled with anything; the transfer list is still taken out of the
    /// sender's hands and its ports closed, as for a message that goes nowhere.
    pub fn post(
        &self,
        from: u64,
        owner: Owner,
        data: String,
        transfer: &[u64],
    ) -> Result<bool, PostError> {
        // The port must be the sender's to post anything.
        let (peer, link) = {
            let t = self.inner.borrow();
            match t.ports.get(&from) {
                Some(p) if p.owner == Some(owner) => (p.peer, p.link),
                _ => return Ok(false),
            }
        };
        self.transfer_out(owner, transfer, Some(from))?;
        let mut t = self.inner.borrow_mut();
        if let Some(link) = link {
            // The far end is on the other thread of the link: the message leaves, with the
            // ports it carries.
            if t.out.len() >= MAX_QUEUED || t.bytes + data.len() > MAX_BYTES {
                for &id in transfer {
                    t.close(id);
                }
                return Ok(false);
            }
            for &id in transfer {
                t.export(id, link);
            }
            t.bytes += data.len();
            t.out.push_back(LinkOut::Msg { link, port: from, data, ports: transfer.to_vec() });
            return Ok(true);
        }
        let room = t.queued < MAX_QUEUED * 8 && t.bytes + data.len() <= MAX_BYTES;
        let target = peer.filter(|p| t.ports.get(p).is_some_and(|q| q.queue.len() < MAX_QUEUED));
        let Some(peer) = target.filter(|_| room) else {
            // Nowhere to go: what it carried is lost.
            for &id in transfer {
                t.close(id);
            }
            return Ok(false);
        };
        t.seq += 1;
        let seq = t.seq;
        t.bytes += data.len();
        t.queued += 1;
        if let Some(q) = t.ports.get_mut(&peer) {
            q.queue.push_back(Msg {
                seq,
                data,
                ports: transfer.to_vec(),
            });
        }
        t.refresh(peer);
        Ok(true)
    }

    /// `start()` (or `onmessage = ...`): the queue may be delivered.
    pub fn start(&self, id: u64, owner: Owner) {
        let mut t = self.inner.borrow_mut();
        if let Some(p) = t.ports.get_mut(&id)
            && p.owner == Some(owner)
        {
            p.started = true;
        }
        t.refresh(id);
    }

    /// `close()`.
    pub fn close(&self, id: u64) {
        self.inner.borrow_mut().close(id);
    }

    pub fn close_all(&self, ids: &[u64]) {
        let mut t = self.inner.borrow_mut();
        for &id in ids {
            t.close(id);
        }
    }

    // ---- links: ports whose other end is on another thread -------------------------------

    /// Ports in transit (after a transfer list took them from their owner) leave over `link`.
    pub fn export(&self, ids: &[u64], link: u32) {
        let mut t = self.inner.borrow_mut();
        for &id in ids {
            t.export(id, link);
        }
    }

    /// A port that arrived over `link` (its far end is there): a new end here, in transit
    /// until a realm adopts it. False if the id is taken or the table is full.
    pub fn import(&self, id: u64, link: u32) -> bool {
        let mut t = self.inner.borrow_mut();
        if t.ports.contains_key(&id) || t.ports.len() >= MAX_PORTS {
            return false;
        }
        t.ports.insert(
            id,
            Port {
                peer: None,
                owner: None,
                queue: VecDeque::new(),
                started: false,
                link: Some(link),
            },
        );
        t.linked += 1;
        true
    }

    /// A message from the far end of `id` over a link: it goes to the port's peer when it has
    /// one (it is a message the far end posted), else to the port itself. The ports it
    /// carries must be imported first. False if there is no such port or no room.
    pub fn deliver_link(&self, id: u64, data: String, ports: &[u64]) -> bool {
        let mut t = self.inner.borrow_mut();
        let Some(p) = t.ports.get(&id) else {
            for &c in ports {
                t.close(c);
            }
            return false;
        };
        let dest = p.peer.unwrap_or(id);
        let room = t.queued < MAX_QUEUED * 8 && t.bytes + data.len() <= MAX_BYTES;
        let fits = t.ports.get(&dest).is_some_and(|q| q.queue.len() < MAX_QUEUED);
        if !(room && fits) {
            for &c in ports {
                t.close(c);
            }
            return false;
        }
        t.seq += 1;
        let seq = t.seq;
        t.bytes += data.len();
        t.queued += 1;
        if let Some(q) = t.ports.get_mut(&dest) {
            q.queue.push_back(Msg { seq, data, ports: ports.to_vec() });
        }
        t.refresh(dest);
        true
    }

    /// Everything on `link` ends (the thread at the other end is gone): its ports close
    /// without a word, and what was waiting to go is dropped.
    pub fn close_link(&self, link: u32) {
        let mut t = self.inner.borrow_mut();
        let ids: Vec<u64> = t
            .ports
            .iter()
            .filter(|(_, p)| p.link == Some(link))
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            t.close_inner(id, false);
        }
        let mut keep = VecDeque::new();
        while let Some(o) = t.out.pop_front() {
            let (l, data) = match &o {
                LinkOut::Msg { link: l, data, .. } => (*l, data.len()),
                LinkOut::Close { link: l, .. } => (*l, 0),
            };
            if l == link {
                t.bytes = t.bytes.saturating_sub(data);
            } else {
                keep.push_back(o);
            }
        }
        t.out = keep;
    }

    /// The far end of `id` closed.
    pub fn close_from_link(&self, id: u64) {
        self.inner.borrow_mut().close_inner(id, false);
    }

    /// Some port here is an end of a link.
    pub fn has_links(&self) -> bool {
        self.inner.borrow().linked > 0 || !self.inner.borrow().out.is_empty()
    }

    /// What is waiting to go over `link`, at most `max` items: posts made on ports that
    /// crossed, and what the peers of the ports that stayed posted to them.
    pub fn take_link_out(&self, link: u32, max: usize) -> Vec<LinkOut> {
        let mut t = self.inner.borrow_mut();
        let mut out = Vec::new();
        // The posts and closes first, in order.
        let mut keep = VecDeque::new();
        while let Some(o) = t.out.pop_front() {
            let mine = match &o {
                LinkOut::Msg { link: l, .. } | LinkOut::Close { link: l, .. } => *l == link,
            };
            if mine && out.len() < max {
                if let LinkOut::Msg { data, .. } = &o {
                    t.bytes = t.bytes.saturating_sub(data.len());
                }
                out.push(o);
            } else {
                keep.push_back(o);
            }
        }
        t.out = keep;
        // Then what waits in the stay-behind ends.
        if t.linked > 0 {
            let ids: Vec<u64> = t
                .ports
                .iter()
                .filter(|(_, p)| p.link == Some(link) && p.peer.is_some() && !p.queue.is_empty())
                .map(|(id, _)| *id)
                .collect();
            for id in ids {
                while out.len() < max {
                    let Some(m) = t.ports.get_mut(&id).and_then(|p| p.queue.pop_front()) else { break };
                    t.bytes = t.bytes.saturating_sub(m.data.len());
                    t.queued = t.queued.saturating_sub(1);
                    for &c in &m.ports {
                        t.export(c, link);
                    }
                    out.push(LinkOut::Msg { link, port: id, data: m.data, ports: m.ports });
                }
            }
        }
        out
    }

    /// The realm goes away: its ports are closed (the ones in transit are not its).
    pub fn close_owned_by(&self, owner: Owner) -> Vec<u64> {
        let mut t = self.inner.borrow_mut();
        let ids: Vec<u64> = t
            .ports
            .iter()
            .filter(|(_, p)| p.owner == Some(owner))
            .map(|(id, _)| *id)
            .collect();
        for &id in &ids {
            t.close(id);
        }
        ids
    }

    /// A realm takes a port that was in transit.
    pub fn adopt(&self, id: u64, owner: Owner) -> bool {
        let mut t = self.inner.borrow_mut();
        let ok = match t.ports.get_mut(&id) {
            Some(p) if p.owner.is_none() => {
                p.owner = Some(owner);
                true
            }
            _ => false,
        };
        t.refresh(id);
        ok
    }

    /// Does `owner` have a message to deliver now?
    pub fn pending_for(&self, owner: Owner) -> bool {
        let t = self.inner.borrow();
        t.ready
            .iter()
            .any(|id| t.ports.get(id).is_some_and(|p| p.owner == Some(owner)))
    }

    /// The oldest message among the ports `owner` has started, if posted by `max_seq`, and
    /// the port it is for.
    pub fn next_for(&self, owner: Owner, max_seq: u64) -> Option<(u64, Msg)> {
        let mut t = self.inner.borrow_mut();
        let mut best: Option<(u64, u64)> = None;
        for id in &t.ready {
            let Some(p) = t.ports.get(id) else { continue };
            if p.owner != Some(owner) {
                continue;
            }
            if let Some(m) = p.queue.front()
                && m.seq <= max_seq
                && best.is_none_or(|(s, _)| m.seq < s)
            {
                best = Some((m.seq, *id));
            }
        }
        let (_, id) = best?;
        let msg = t.ports.get_mut(&id)?.queue.pop_front()?;
        t.bytes = t.bytes.saturating_sub(msg.data.len());
        t.queued = t.queued.saturating_sub(1);
        t.refresh(id);
        Some((id, msg))
    }
}

// ---- script objects --------------------------------------------------------------------------

/// The host class of `MessagePort` objects (data: the port id).
pub struct PortClass;

impl HostClass for PortClass {
    fn name(&self) -> &str {
        "MessagePort"
    }
}

/// What a realm keeps of its ports.
#[derive(Default)]
pub struct PortsLocal {
    class: Option<ClassId>,
    /// The script object of each port this realm has, weakly.
    objs: BTreeMap<u64, WeakValue>,
    /// Listening ports, strongly.
    held: BTreeMap<u64, Pinned>,
    /// A message was dropped for lack of room (said once).
    warned: bool,
}

impl Bridge {
    /// The page's ports (those of the hub, once this realm has joined one).
    pub fn ports(&self) -> Rc<PortTable> {
        if let Some(h) = self.hub() {
            return h.ports.clone();
        }
        // (A realm outside any hub has a table of its own.)
        self.xf
            .ports
            .borrow_mut()
            .get_or_insert_with(|| Rc::new(PortTable::new()))
            .clone()
    }

    pub fn port_owner(&self) -> Owner {
        Owner {
            frame: self.frame_id(),
            uid: self.xf.uid,
        }
    }
}

fn class_of(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<ClassId> {
    if let Some(c) = b.st.borrow().ports.class {
        return Ok(c);
    }
    let c = r.register_class(PortClass);
    b.st(r)?.ports.class = Some(c);
    Ok(c)
}

/// The id of the port a script value is.
pub fn port_id(b: &Rc<Bridge>, r: &mut Realm, v: Value) -> Option<u64> {
    let c = b.st.borrow().ports.class?;
    r.host_data(v, c)
}

/// A script object for port `id` of this realm.
fn make_port(b: &Rc<Bridge>, r: &mut Realm, id: u64, proto: Value) -> JsResult<Value> {
    let c = class_of(b, r)?;
    let v = r.new_host_object(c, id, proto);
    let w = r.weak(v);
    b.st(r)?.ports.objs.insert(id, w);
    Ok(v)
}

/// The prototype of `MessagePort` (from the script side, loading it first if need be).
fn proto(b: &Rc<Bridge>, r: &mut Realm) -> JsResult<Value> {
    let ex = super::net::exports(b, r)?;
    let c = r.get(ex, "MessagePort")?;
    r.get(c, "prototype")
}

/// Script objects for ports that arrived in a message, now this realm's: an array.
pub fn adopt_list(b: &Rc<Bridge>, r: &mut Realm, ids: &[u64]) -> JsResult<Value> {
    let out = r.new_array(&[]);
    if ids.is_empty() {
        return Ok(out);
    }
    let table = b.ports();
    let owner = b.port_owner();
    let p = proto(b, r)?;
    for &id in ids {
        if table.adopt(id, owner) {
            let v = make_port(b, r, id, p)?;
            r.array_push(out, v)?;
        }
    }
    Ok(out)
}

/// A table with too many dead ports: forget the ones whose script object is gone.
fn sweep(b: &Rc<Bridge>, r: &mut Realm) {
    let ids: Vec<(u64, WeakValue)> = match b.st.try_borrow() {
        Ok(st) => st.ports.objs.iter().map(|(k, w)| (*k, *w)).collect(),
        Err(_) => return,
    };
    let table = b.ports();
    for (id, w) in ids {
        let alive = r.upgrade(&w).is_some();
        if !alive && let Ok(mut st) = b.st.try_borrow_mut() {
            let held = st.ports.held.contains_key(&id);
            if !held {
                st.ports.objs.remove(&id);
                drop(st);
                table.close(id);
            }
        }
    }
}

/// The natives the script side calls (on its `native` object).
pub fn define_natives(b: &Rc<Bridge>, r: &mut Realm, n: Value) -> JsResult<()> {
    // channel(proto) -> [port1, port2]
    let b2 = b.clone();
    r.define_method(n, "channel", 1, move |r, _t, a| {
        let table = b2.ports();
        let owner = b2.port_owner();
        let pair = match table.new_pair(owner) {
            Some(p) => p,
            None => {
                sweep(&b2, r);
                match table.new_pair(owner) {
                    Some(p) => p,
                    None => {
                        return Err(dom_error(
                            r,
                            "QuotaExceededError",
                            "Too many message channels are open: close the ones you are done with.",
                        ));
                    }
                }
            }
        };
        let p = arg(a, 0);
        let p1 = make_port(&b2, r, pair.0, p)?;
        let p2 = make_port(&b2, r, pair.1, p)?;
        Ok(r.new_array(&[p1, p2]))
    })?;
    // portPost(port, value, transferList)
    let b2 = b.clone();
    r.define_method(n, "portPost", 3, move |r, _t, a| port_post(&b2, r, a))?;
    // portStart(port), portClose(port), portHold(port, on)
    let b2 = b.clone();
    r.define_method(n, "portStart", 1, move |r, _t, a| {
        let Some(id) = port_id(&b2, r, arg(a, 0)) else {
            return Err(type_error(r, "Illegal invocation"));
        };
        b2.ports().start(id, b2.port_owner());
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(n, "portClose", 1, move |r, _t, a| {
        let Some(id) = port_id(&b2, r, arg(a, 0)) else {
            return Err(type_error(r, "Illegal invocation"));
        };
        // (Only the owner closes it: a port in transit is not the script's any more.)
        if b2.ports().owns(id, b2.port_owner()) {
            b2.ports().close(id);
        }
        let held = b2.st(r)?.ports.held.remove(&id);
        if let Some(p) = held {
            r.unpin(p);
        }
        Ok(Value::UNDEFINED)
    })?;
    let b2 = b.clone();
    r.define_method(n, "portHold", 2, move |r, _t, a| {
        let Some(id) = port_id(&b2, r, arg(a, 0)) else {
            return Err(type_error(r, "Illegal invocation"));
        };
        let on = r.to_boolean(arg(a, 1));
        if on {
            let has = b2.st(r)?.ports.held.contains_key(&id);
            if !has {
                let p = r.pin(arg(a, 0));
                b2.st(r)?.ports.held.insert(id, p);
            }
        } else {
            let held = b2.st(r)?.ports.held.remove(&id);
            if let Some(p) = held {
                r.unpin(p);
            }
        }
        Ok(Value::UNDEFINED)
    })?;
    // isPort(value) -> bool, portOpen(port) -> bool
    let b2 = b.clone();
    r.define_method(n, "isPort", 1, move |r, _t, a| {
        Ok(Value::bool(port_id(&b2, r, arg(a, 0)).is_some()))
    })?;
    // messageEvent(type, data, ports) -> a trusted MessageEvent for a port
    let b2 = b.clone();
    r.define_method(n, "messageEvent", 3, move |r, _t, a| {
        let ty = r.to_string(arg(a, 0))?;
        let proto = {
            let st = b2.st(r)?;
            match st.outbox.message_proto.as_ref() {
                Some(p) => r.pinned(p),
                None => return Err(type_error(r, "MessageEvent is not available")),
            }
        };
        let e = super::events_js::new_event_on(&b2, r, proto, &ty, super::events_js::TRUSTED);
        let empty = r.string("");
        r.define(e, "data", arg(a, 1), Attrs::ENUMERABLE)?;
        r.define(e, "origin", empty, Attrs::ENUMERABLE)?;
        r.define(e, "lastEventId", empty, Attrs::ENUMERABLE)?;
        r.define(e, "source", Value::NULL, Attrs::ENUMERABLE)?;
        r.define(e, "ports", arg(a, 2), Attrs::ENUMERABLE)?;
        Ok(e)
    })?;
    // decode(text, ports) -> the value a message stands for
    let b2 = b.clone();
    r.define_method(n, "decode", 2, move |r, _t, a| {
        let text = r.to_string(arg(a, 0))?;
        let ports = arg(a, 1);
        super::postmsg::decode(&b2, r, &text, if ports.is_object() { Some(ports) } else { None })
    })?;
    Ok(())
}

fn transfer_ids(b: &Rc<Bridge>, r: &mut Realm, list: Value) -> JsResult<Vec<u64>> {
    let mut ids = Vec::new();
    if !list.is_object() {
        return Ok(ids);
    }
    let n = r.get(list, "length")?;
    let n = r.to_number(n)?.clamp(0.0, 65_536.0) as u32;
    for i in 0..n {
        let v = r.get(list, i)?;
        match port_id(b, r, v) {
            Some(id) => ids.push(id),
            None => {
                // (Buffers are copied, not given away; anything else cannot be.)
                if !v.is_object() || !(r.is_array_buffer(v) || r.is_array_buffer_view(v)) {
                    return Err(dom_error(
                        r,
                        "DataCloneError",
                        "Failed to execute 'postMessage': Value at index in the transfer list is not transferable.",
                    ));
                }
            }
        }
    }
    Ok(ids)
}

fn port_post(b: &Rc<Bridge>, r: &mut Realm, a: &[Value]) -> JsResult<Value> {
    let Some(id) = port_id(b, r, arg(a, 0)) else {
        return Err(type_error(r, "Illegal invocation"));
    };
    let ids = transfer_ids(b, r, arg(a, 2))?;
    // Ports among the transferred are replaced by references in the copy.
    let transfer_ports: Vec<Value> = {
        let mut v = Vec::new();
        let n = if arg(a, 2).is_object() {
            let l = r.get(arg(a, 2), "length")?;
            r.to_number(l)?.clamp(0.0, 65_536.0) as u32
        } else {
            0
        };
        for i in 0..n {
            let x = r.get(arg(a, 2), i)?;
            if port_id(b, r, x).is_some() {
                v.push(x);
            }
        }
        v
    };
    let list = r.new_array(&transfer_ports);
    let data = super::postmsg::encode_with(b, r, arg(a, 1), Some(list))?;
    if data.len() > super::frames::MAX_MESSAGE_BYTES {
        return Err(dom_error(r, "DataCloneError", "Failed to execute 'postMessage': the message is too large."));
    }
    let table = b.ports();
    match table.post(id, b.port_owner(), data, &ids) {
        Ok(true) => {}
        Ok(false) => {
            // Dropped: because the port was closed (quietly), or for lack of room (a note).
            if table.owns(id, b.port_owner()) && !b.st(r)?.ports.warned {
                let open = table.is_open(id);
                if open {
                    b.st(r)?.ports.warned = true;
                    b.console(
                        crate::api::ConsoleLevel::Warn,
                        String::from("Too many messages are waiting: a message was dropped."),
                    );
                }
            }
        }
        Err(PostError::BadTransfer) => {
            return Err(dom_error(
                r,
                "DataCloneError",
                "Failed to execute 'postMessage' on 'MessagePort': a port in the transfer list cannot be transferred (it is closed, already transferred, listed twice, or the port the message is sent on).",
            ));
        }
    }
    Ok(Value::UNDEFINED)
}

// ---- the event loop ----------------------------------------------------------------------------

/// Messages are waiting for this realm.
pub fn pending(b: &Bridge) -> bool {
    b.ports().pending_for(b.port_owner())
}

/// One pass: the messages posted before it began (up to [`MAX_PER_PASS`] and the time slice)
/// become `message` events, oldest first. True if any was delivered.
pub fn pump(b: &Rc<Bridge>, r: &mut Realm, start: f64, slice_ms: f64) -> bool {
    let table = b.ports();
    let owner = b.port_owner();
    if !table.pending_for(owner) {
        return false;
    }
    let limit = table.seq();
    let mut did = false;
    for _ in 0..MAX_PER_PASS {
        if b.st.borrow().stopped {
            break;
        }
        let Some((id, msg)) = table.next_for(owner, limit) else {
            break;
        };
        did = true;
        let weak = b.st.borrow().ports.objs.get(&id).copied();
        let obj = weak.and_then(|w| r.upgrade(&w));
        let Some(obj) = obj else {
            // Nobody holds the port any more: it and what it carried are gone.
            table.close(id);
            table.close_all(&msg.ports);
            if let Ok(mut st) = b.st.try_borrow_mut() {
                st.ports.objs.remove(&id);
            }
            continue;
        };
        let ports = adopt_list(b, r, &msg.ports).unwrap_or_else(|_| r.new_array(&[]));
        let ex = match super::net::exports(b, r) {
            Ok(e) => e,
            Err(_) => break,
        };
        if let Ok(f) = r.get(ex, "deliverPort") {
            let text = r.string(&msg.data);
            super::call_report(b, r, f, Value::UNDEFINED, &[obj, text, ports]);
        }
        super::checkpoint(b, r);
        if b.host().map_or(0.0, |h| h.now()) - start > slice_ms {
            break;
        }
    }
    did
}

/// The realm ends: its ports are closed and let go.
pub fn release(b: &Rc<Bridge>, r: Option<&mut Realm>) {
    b.ports().close_owned_by(b.port_owner());
    let held = match b.st.try_borrow_mut() {
        Ok(mut st) => {
            st.ports.objs.clear();
            core::mem::take(&mut st.ports.held)
        }
        Err(_) => return,
    };
    if let Some(r) = r {
        for (_, p) in held {
            r.unpin(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(frame: u32) -> Owner {
        Owner { frame, uid: frame as u64 + 1 }
    }

    #[test]
    fn messages_go_to_the_other_end_in_order() {
        let t = PortTable::new();
        let (a, b) = t.new_pair(o(0)).unwrap();
        assert_eq!(t.post(a, o(0), "1".into(), &[]), Ok(true));
        assert_eq!(t.post(a, o(0), "2".into(), &[]), Ok(true));
        assert_eq!(t.post(b, o(0), "back".into(), &[]), Ok(true));
        // Not started: nothing is ready.
        assert!(!t.pending_for(o(0)));
        t.start(b, o(0));
        assert!(t.pending_for(o(0)));
        let lim = t.seq();
        let (id, m) = t.next_for(o(0), lim).unwrap();
        assert_eq!((id, m.data.as_str()), (b, "1"));
        assert_eq!(t.next_for(o(0), lim).unwrap().1.data, "2");
        assert!(t.next_for(o(0), lim).is_none());
        // Messages posted after the limit wait for the next pass.
        t.post(a, o(0), "3".into(), &[]).unwrap();
        assert!(t.next_for(o(0), lim).is_none());
        assert_eq!(t.next_for(o(0), t.seq()).unwrap().1.data, "3");
        // Another realm sees nothing of it.
        t.start(a, o(0));
        assert!(!t.pending_for(o(1)));
        assert_eq!(t.queued(), 1);
    }

    #[test]
    fn oldest_first_across_ports() {
        let t = PortTable::new();
        let (a, b) = t.new_pair(o(0)).unwrap();
        let (c, d) = t.new_pair(o(0)).unwrap();
        t.start(b, o(0));
        t.start(d, o(0));
        t.post(c, o(0), "c1".into(), &[]).unwrap();
        t.post(a, o(0), "a1".into(), &[]).unwrap();
        t.post(c, o(0), "c2".into(), &[]).unwrap();
        let lim = t.seq();
        let got: Vec<String> = core::iter::from_fn(|| t.next_for(o(0), lim).map(|x| x.1.data)).collect();
        assert_eq!(got, ["c1", "a1", "c2"]);
    }

    #[test]
    fn transfer_moves_a_port_between_realms() {
        let t = PortTable::new();
        let (a, b) = t.new_pair(o(0)).unwrap();
        let (c, d) = t.new_pair(o(0)).unwrap();
        // Frame 0 gives port d to frame 1 through the channel a/b.
        t.start(b, o(0));
        assert_eq!(t.post(a, o(0), "hello".into(), &[d]), Ok(true));
        assert!(!t.owns(d, o(0)), "in transit");
        // A message for the port in transit waits.
        t.start(d, o(0));
        assert_eq!(t.post(c, o(0), "later".into(), &[]), Ok(true));
        assert!(!t.pending_for(o(1)));
        let (_, m) = t.next_for(o(0), t.seq()).unwrap();
        assert_eq!(m.ports, [d]);
        assert!(t.adopt(d, o(1)));
        assert!(!t.adopt(d, o(1)), "once");
        assert!(t.owns(d, o(1)));
        t.start(d, o(1));
        assert!(t.pending_for(o(1)));
        assert_eq!(t.next_for(o(1), t.seq()).unwrap().1.data, "later");
    }

    #[test]
    fn bad_transfers_and_closing() {
        let t = PortTable::new();
        let (a, b) = t.new_pair(o(0)).unwrap();
        let (c, _d) = t.new_pair(o(0)).unwrap();
        assert_eq!(t.post(a, o(0), "x".into(), &[a]), Err(PostError::BadTransfer));
        assert_eq!(t.post(a, o(0), "x".into(), &[c, c]), Err(PostError::BadTransfer));
        assert_eq!(t.post(a, o(0), "x".into(), &[999]), Err(PostError::BadTransfer));
        assert_eq!(t.post(a, o(1), "x".into(), &[]), Ok(false), "not its owner's");
        // A message to a closed port is dropped, and what it carried is closed.
        t.close(b);
        assert_eq!(t.post(a, o(0), "x".into(), &[c]), Ok(false));
        assert!(!t.is_open(c));
        // The peer of a closed port is on its own.
        assert_eq!(t.post(a, o(0), "x".into(), &[]), Ok(false));
        // Closing a port with a message in flight carrying a port closes that too.
        let (e, f) = t.new_pair(o(0)).unwrap();
        let (g, h) = t.new_pair(o(0)).unwrap();
        t.post(e, o(0), "p".into(), &[g]).unwrap();
        t.close(f);
        assert!(!t.is_open(g));
        assert!(t.is_open(h));
        assert_eq!(t.queued(), 0);
    }

    #[test]
    fn a_port_given_to_another_thread_is_one_pair_across_two_tables() {
        // The page's table and a worker's: link 7 on one side, 0 on the other.
        let page = PortTable::new();
        let work = PortTable::with_namespace(8);
        let (a, b) = page.new_pair(o(0)).unwrap();
        // The page gives `b` away.
        page.transfer_out(o(0), &[b], None).unwrap();
        page.export(&[b], 7);
        assert!(page.has_links());
        // `a` posts: the message waits at `b`'s end, and leaves with the next take.
        assert_eq!(page.post(a, o(0), "to worker".into(), &[]), Ok(true));
        let out = page.take_link_out(7, 10);
        assert_eq!(out, [LinkOut::Msg { link: 7, port: b, data: "to worker".into(), ports: vec![] }]);
        // The worker gets an end of its own for the same id, and the message.
        assert!(work.import(b, 0));
        assert!(!work.import(b, 0), "once");
        assert!(work.adopt(b, o(1)));
        assert!(work.deliver_link(b, "to worker".into(), &[]));
        work.start(b, o(1));
        assert_eq!(work.next_for(o(1), work.seq()).unwrap().1.data, "to worker");
        // Its post goes back: out of the worker's table, into `a`'s queue in the page's.
        assert_eq!(work.post(b, o(1), "to page".into(), &[]), Ok(true));
        let back = work.take_link_out(0, 10);
        assert_eq!(back, [LinkOut::Msg { link: 0, port: b, data: "to page".into(), ports: vec![] }]);
        assert!(page.deliver_link(b, "to page".into(), &[]));
        page.start(a, o(0));
        assert_eq!(page.next_for(o(0), page.seq()).unwrap().1.data, "to page");
        // A port that crossed is not sent on.
        let (c, _d) = work.new_pair(o(1)).unwrap();
        assert_eq!(work.post(c, o(1), "x".into(), &[b]), Err(PostError::BadTransfer));
        // Closing the page's end closes the shadow and tells the far end.
        page.close(a);
        assert!(!page.is_open(b));
        let closes = page.take_link_out(7, 10);
        assert_eq!(closes, [LinkOut::Close { link: 7, port: b }]);
        work.close_from_link(b);
        assert!(!work.is_open(b));
        assert!(work.take_link_out(0, 10).is_empty(), "no echo");
    }

    #[test]
    fn ports_that_travel_with_a_message_over_a_link_cross_too() {
        let page = PortTable::new();
        let work = PortTable::with_namespace(3);
        let (a, b) = page.new_pair(o(0)).unwrap();
        let (c, d) = page.new_pair(o(0)).unwrap();
        // `b` goes over; then a message on it carries `d`.
        page.transfer_out(o(0), &[b], None).unwrap();
        page.export(&[b], 1);
        assert!(work.import(b, 0));
        work.adopt(b, o(1));
        // The page's `a` posts a message that carries `d`.
        assert_eq!(page.post(a, o(0), "with port".into(), &[d]), Ok(true));
        let out = page.take_link_out(1, 10);
        let LinkOut::Msg { ports, data, .. } = &out[0] else { panic!("{out:?}") };
        assert_eq!((data.as_str(), ports.as_slice()), ("with port", &[d][..]));
        // d is exported now (its peer c stays on the page).
        assert!(work.import(d, 0));
        assert!(work.deliver_link(b, data.clone(), &[d]));
        work.start(b, o(1));
        let (_, m) = work.next_for(o(1), work.seq()).unwrap();
        assert_eq!(m.ports, [d]);
        assert!(work.adopt(d, o(1)));
        // And what `c` posts reaches the worker's `d`.
        assert_eq!(page.post(c, o(0), "via d".into(), &[]), Ok(true));
        let out = page.take_link_out(1, 10);
        assert_eq!(out, [LinkOut::Msg { link: 1, port: d, data: "via d".into(), ports: vec![] }]);
    }

    #[test]
    fn closing_a_link_drops_everything_on_it() {
        let page = PortTable::new();
        let (a, b) = page.new_pair(o(0)).unwrap();
        page.transfer_out(o(0), &[b], None).unwrap();
        page.export(&[b], 5);
        assert_eq!(page.post(a, o(0), "x".into(), &[]), Ok(true));
        page.close_link(5);
        assert!(!page.is_open(b));
        assert!(page.take_link_out(5, 10).is_empty());
        assert!(!page.has_links());
        assert_eq!(page.queued(), 0);
        // `a` lives, alone.
        assert!(page.is_open(a));
        assert_eq!(page.post(a, o(0), "y".into(), &[]), Ok(false));
        // Messages for a port that is not there are refused, and what they carried closed.
        let (c, d) = page.new_pair(o(0)).unwrap();
        assert!(!page.deliver_link(999, "z".into(), &[c]));
        assert!(!page.is_open(c));
        let _ = d;
    }

    #[test]
    fn limits() {
        let t = PortTable::new();
        for _ in 0..MAX_PORTS / 2 {
            assert!(t.new_pair(o(0)).is_some());
        }
        assert!(t.new_pair(o(0)).is_none());
        let t = PortTable::new();
        let (a, b) = t.new_pair(o(0)).unwrap();
        t.start(b, o(0));
        let mut sent = 0;
        while t.post(a, o(0), "x".into(), &[]) == Ok(true) {
            sent += 1;
            assert!(sent <= MAX_QUEUED);
        }
        assert_eq!(sent, MAX_QUEUED);
        // A big message is refused when the bytes are used up.
        let t = PortTable::new();
        let (a, _b) = t.new_pair(o(0)).unwrap();
        let big = "x".repeat(MAX_BYTES / 2 + 1);
        assert_eq!(t.post(a, o(0), big.clone(), &[]), Ok(true));
        assert_eq!(t.post(a, o(0), big, &[]), Ok(false));
        t.close_owned_by(o(0));
        assert!(t.is_empty());
    }
}
