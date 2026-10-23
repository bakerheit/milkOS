//! Userland window clients: a process gets a desktop window whose body is a pixel buffer
//! shared with it (mapped into its address space), draws into it, says which part changed
//! (commit), and reads input events. The syscalls (`MILK_WIN_*`, 4100..) live in
//! `proc/syscall.rs` and call into this module; docs/processes.md documents them.
//!
//! Ownership: the buffer is contiguous PMM frames owned here (in [`CLIENTS`]). The process maps
//! them with `UserSpace::map_shared` (which never frees them). They are freed when the process
//! destroys the window (after it unmapped them) or once the process is gone. Closing the
//! window on screen only hides it and sends a Close event; the frames stay until then.
//!
//! Two buffers per window. The *shared* buffer is the client's drawing board: it clears and
//! redraws it at will, mid-frame states and all. The *front* buffer is the compositor's copy,
//! private to the kernel: [`commit`] (the client's "frame done") copies the committed rect from
//! the shared buffer into it, under a lock the compositor also takes while it reads. So the
//! screen only ever shows frames the client called done, never a half-drawn one.
//!
//! The wm thread sees a client window as a normal [`WindowContents`] ([`ClientView`]) that
//! blits the front buffer into the window body.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, Ordering};

use crate::gfx::Rect;
use crate::mmu::{PAGE_SIZE, phys_to_virt};
use crate::sched::{Mutex, WaitQueue};
use crate::sync::SpinLock;
use crate::{pmm, proc};

use super::event::{Button, Key};
use super::paint::Painter;
use super::theme::*;
use super::window::{KeyInput, PointerInput, PointerKind, WindowContents};
use super::{Request, request};

pub const MAX_W: u32 = 1000;
pub const MAX_H: u32 = 700;
/// Event record size (see [`encode`]).
pub const EVENT_LEN: usize = 32;
const MAX_QUEUED: usize = 256;

// Event kinds (2 is reserved for key-up, not sent yet).
pub const EV_KEY_DOWN: u32 = 1;
pub const EV_POINTER_MOVE: u32 = 3;
pub const EV_BUTTON_DOWN: u32 = 4;
pub const EV_BUTTON_UP: u32 = 5;
pub const EV_SCROLL: u32 = 6;
pub const EV_CLOSE: u32 = 7;
/// A button on one of the app's notices was pressed: key = the app's tag, mods = index.
pub const EV_ACTION: u32 = 9;
/// The window's body is now x by y (a resizable client: answer with MILK_WIN_RESIZE).
pub const EV_RESIZE: u32 = 10;

// Key codes for non-character keys (printable keys send their Unicode code point).
pub const K_ENTER: u32 = 0x1_0001;
pub const K_BACKSPACE: u32 = 0x1_0002;
pub const K_TAB: u32 = 0x1_0003;
pub const K_ESCAPE: u32 = 0x1_0004;
pub const K_UP: u32 = 0x1_0005;
pub const K_DOWN: u32 = 0x1_0006;
pub const K_LEFT: u32 = 0x1_0007;
pub const K_RIGHT: u32 = 0x1_0008;
pub const K_DELETE: u32 = 0x1_0009;

/// The compositor's copy of a client window's last committed picture (`w` x `h`, stride `w`).
/// Frames are contiguous PMM memory, given back on drop.
struct Front {
    pa: usize,
    pages: usize,
    w: u32,
    h: u32,
}

impl Front {
    /// A milk-white `w` x `h` picture. `None` if out of memory.
    fn new(w: u32, h: u32) -> Option<Front> {
        let pages = ((w * h * 4) as usize).div_ceil(PAGE_SIZE);
        let pa = pmm::alloc_contiguous_zeroed_as(pmm::Use::WindowBuffers, pages, 1)?;
        let f = Front { pa, pages, w, h };
        // SAFETY: fresh frames we own.
        unsafe { f.pixels_mut().fill(MILK) };
        Some(f)
    }

    fn pixels(&self) -> &[u32] {
        // SAFETY: the frames are ours for as long as the Front lives; only whoever holds the
        // lock around it reaches this.
        unsafe {
            core::slice::from_raw_parts(
                phys_to_virt(self.pa) as *const u32,
                (self.w * self.h) as usize,
            )
        }
    }

    /// # Safety
    /// The caller has exclusive access (the lock around the Front, or it is not shared yet).
    #[allow(clippy::mut_from_ref)]
    unsafe fn pixels_mut(&self) -> &mut [u32] {
        // SAFETY: see above.
        unsafe {
            core::slice::from_raw_parts_mut(
                phys_to_virt(self.pa) as *mut u32,
                (self.w * self.h) as usize,
            )
        }
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        pmm::free_contiguous(self.pa, self.pages);
    }
}

type SharedFront = Arc<Mutex<Front>>;

struct Client {
    /// What the compositor shows (see the module docs). The Arc is cloned out of [`CLIENTS`]
    /// before the (sleeping) lock inside it is taken.
    front: SharedFront,
    pid: u32,
    w: u32,
    h: u32,
    pa: usize,
    pages: usize,
    title: String,
    /// The title the window was created with: the app's name in the menu bar, whatever
    /// the client later sets the title to (an editor shows its file name there).
    app: String,
    title_gen: u32,
    /// Bounding box of committed changes not yet shown.
    dirty: Option<Rect>,
    events: VecDeque<[u8; EVENT_LEN]>,
    /// Closed on screen (the process has been told).
    closed: bool,
    /// The client handles EV_RESIZE (MILK_WIN_RESIZE with the RESIZABLE flag): its window
    /// can be any size from `min` up, else it stays the size of its buffer.
    resizable: bool,
    min: (u32, u32),
    /// The size last asked for with EV_RESIZE (so it is asked once per size).
    asked: (u32, u32),
}

static CLIENTS: SpinLock<BTreeMap<u32, Client>> = SpinLock::new(BTreeMap::new());
static NEXT_ID: AtomicU32 = AtomicU32::new(1);
/// Clients blocked in MILK_WIN_EVENT wait here.
static EVENT_WQ: WaitQueue = WaitQueue::new();

pub struct Created {
    pub id: u32,
    pub pa: usize,
    pub bytes: usize,
}

/// MILK_WIN_CREATE: a `w` x `h` buffer (stride = w) for process `pid`. The caller maps it.
pub fn create(pid: u32, w: u32, h: u32, title: &str) -> Option<Created> {
    if w == 0 || h == 0 || w > MAX_W || h > MAX_H {
        return None;
    }
    let bytes = (w * h * 4) as usize;
    let pages = bytes.div_ceil(PAGE_SIZE);
    let front = Front::new(w, h)?;
    let pa = pmm::alloc_contiguous_zeroed_as(pmm::Use::WindowBuffers, pages, 1)?;
    // Start milk-white rather than black.
    // SAFETY: fresh frames we own.
    unsafe {
        core::slice::from_raw_parts_mut(phys_to_virt(pa) as *mut u32, (w * h) as usize).fill(MILK)
    };
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    CLIENTS.lock().insert(
        id,
        Client {
            front: Arc::new(Mutex::new(front)),
            pid,
            w,
            h,
            pa,
            pages,
            title: String::from(title),
            app: String::from(title),
            title_gen: 0,
            dirty: None,
            events: VecDeque::new(),
            closed: false,
            resizable: false,
            min: (w, h),
            asked: (w, h),
        },
    );
    request(Request::ClientCreate(id));
    Some(Created { id, pa, bytes })
}

/// MILK_WIN_RESIZE with the RESIZABLE flag: the client handles EV_RESIZE, down to
/// `min_w` x `min_h`.
pub fn set_resizable(pid: u32, id: u32, min_w: u32, min_h: u32) -> bool {
    let mut c = CLIENTS.lock();
    let Some(cl) = c.get_mut(&id).filter(|cl| cl.pid == pid) else {
        return false;
    };
    cl.resizable = true;
    cl.min = (min_w.clamp(1, cl.w), min_h.clamp(1, cl.h));
    drop(c);
    super::WAKE.wake_all();
    true
}

/// MILK_WIN_RESIZE: a new `w` x `h` buffer for window `id` (the old picture copied into its
/// top-left). Returns the new buffer (for the caller to map) and the old one (to unmap and
/// free).
pub fn resize(pid: u32, id: u32, w: u32, h: u32) -> Option<(Created, (usize, usize))> {
    if w == 0 || h == 0 || w > MAX_W || h > MAX_H {
        return None;
    }
    let bytes = (w * h * 4) as usize;
    let pages = bytes.div_ceil(PAGE_SIZE);
    let new_front = Front::new(w, h)?;
    let pa = pmm::alloc_contiguous_zeroed_as(pmm::Use::WindowBuffers, pages, 1)?;
    let mut c = CLIENTS.lock();
    let Some(cl) = c.get_mut(&id).filter(|cl| cl.pid == pid) else {
        drop(c);
        pmm::free_contiguous(pa, pages);
        return None;
    };
    // SAFETY: the new frames are ours; the old buffer is the client's (it may be drawing
    // into it: tearing at worst, as with the compositor).
    unsafe {
        let new = core::slice::from_raw_parts_mut(phys_to_virt(pa) as *mut u32, (w * h) as usize);
        new.fill(MILK);
        let old =
            core::slice::from_raw_parts(phys_to_virt(cl.pa) as *const u32, (cl.w * cl.h) as usize);
        let (cw, ch) = (cl.w.min(w) as usize, cl.h.min(h) as usize);
        for y in 0..ch {
            new[y * w as usize..][..cw].copy_from_slice(&old[y * cl.w as usize..][..cw]);
        }
    }
    let old = (cl.pa, cl.pages);
    // The client chose a new size itself (not an answer to EV_RESIZE): the window follows.
    let own = cl.asked != (w, h);
    cl.asked = (w, h);
    cl.pa = pa;
    cl.pages = pages;
    cl.w = w;
    cl.h = h;
    cl.dirty = Some(Rect::new(0, 0, w as i32, h as i32));
    let front = cl.front.clone();
    drop(c);
    // The compositor's copy follows: the old committed picture in the top left of the new
    // size. (A commit that lands between the two steps sees the sizes differ and waits for the
    // client's next one; the whole window is dirty anyway.)
    {
        let mut f = front.lock();
        let (cw, ch) = (f.w.min(w) as usize, f.h.min(h) as usize);
        // SAFETY: `new_front` is not shared yet.
        let dst = unsafe { new_front.pixels_mut() };
        for y in 0..ch {
            dst[y * w as usize..][..cw].copy_from_slice(&f.pixels()[y * f.w as usize..][..cw]);
        }
        *f = new_front;
    }
    if own {
        request(Request::ClientResized(id));
    }
    super::WAKE.wake_all();
    Some((Created { id, pa, bytes }, old))
}

/// The window's body is `w` x `h` on screen: a resizable client is told (once per size).
pub fn body_size(id: u32, w: i32, h: i32) {
    let (w, h) = (
        w.clamp(1, MAX_W as i32) as u32,
        h.clamp(1, MAX_H as i32) as u32,
    );
    let ask = {
        let mut c = CLIENTS.lock();
        match c.get_mut(&id) {
            Some(cl) if cl.resizable && (cl.w, cl.h) != (w, h) && cl.asked != (w, h) => {
                cl.asked = (w, h);
                true
            }
            _ => false,
        }
    };
    if ask {
        push(id, encode(EV_RESIZE, w as i32, h as i32, 0, 0, 0));
    }
}

/// MILK_WIN_COMMIT: `r` of the buffer changed (whole buffer if empty). This is the client's
/// "frame done": the rect is copied into the compositor's front buffer, so a frame the client
/// is still drawing is never shown.
pub fn commit(pid: u32, id: u32, r: Rect) -> bool {
    let (front, pa, w, h, r) = {
        let c = CLIENTS.lock();
        let Some(cl) = c.get(&id).filter(|cl| cl.pid == pid) else {
            return false;
        };
        let full = Rect::new(0, 0, cl.w as i32, cl.h as i32);
        let r = if r.is_empty() {
            full
        } else {
            super::paint::intersect(&r, &full)
        };
        (cl.front.clone(), cl.pa, cl.w, cl.h, r)
    };
    if !r.is_empty() {
        // The lock is a sleeping one, so it is taken with CLIENTS released. The compositor
        // holds it while it reads the front buffer, so it never sees a half-copied rect.
        let f = front.lock();
        if f.w == w && f.h == h {
            // SAFETY: the shared buffer is the client's, which is us (it is blocked in this
            // syscall; a second thread of it drawing at the same moment is its own tearing);
            // the front buffer is ours under the lock.
            unsafe {
                let src =
                    core::slice::from_raw_parts(phys_to_virt(pa) as *const u32, (w * h) as usize);
                let dst = f.pixels_mut();
                for y in r.y..r.y + r.h {
                    let at = y as usize * w as usize + r.x as usize;
                    dst[at..at + r.w as usize].copy_from_slice(&src[at..at + r.w as usize]);
                }
            }
        }
    }
    let mut c = CLIENTS.lock();
    if let Some(cl) = c.get_mut(&id).filter(|cl| cl.pid == pid) {
        cl.dirty = Some(cl.dirty.map_or(r, |d| d.union(&r)));
    }
    drop(c);
    super::WAKE.wake_all();
    true
}

pub fn set_title(pid: u32, id: u32, title: &str) -> bool {
    let mut c = CLIENTS.lock();
    let Some(cl) = c.get_mut(&id).filter(|cl| cl.pid == pid) else {
        return false;
    };
    cl.title = String::from(title);
    cl.title_gen += 1;
    drop(c);
    super::WAKE.wake_all();
    true
}

/// MILK_WIN_EVENT: the next event for window `id`, waiting up to `timeout_ns` (0 = don't
/// wait). `Err(())` if the window doesn't exist or isn't the caller's.
pub fn next_event(pid: u32, id: u32, timeout_ns: u64) -> Result<Option<[u8; EVENT_LEN]>, ()> {
    let pop = || -> Result<Option<[u8; EVENT_LEN]>, ()> {
        let mut c = CLIENTS.lock();
        let cl = c.get_mut(&id).filter(|cl| cl.pid == pid).ok_or(())?;
        Ok(cl.events.pop_front())
    };
    if let Some(e) = pop()? {
        return Ok(Some(e));
    }
    if timeout_ns > 0 {
        EVENT_WQ.wait_until_timeout(
            || {
                CLIENTS
                    .lock()
                    .get(&id)
                    .is_none_or(|cl| !cl.events.is_empty())
            },
            timeout_ns,
        );
    }
    pop()
}

/// MILK_WIN_DESTROY, step 1: forget the window; returns the buffer (pa, bytes) so the caller
/// can unmap it from the process, then hand it to [`free`].
pub fn destroy(pid: u32, id: u32) -> Option<(usize, usize)> {
    let mut c = CLIENTS.lock();
    if c.get(&id).is_none_or(|cl| cl.pid != pid) {
        return None;
    }
    let cl = c.remove(&id).unwrap();
    drop(c);
    request(Request::ClientGone(id));
    Some((cl.pa, cl.pages))
}

/// Step 2: give the frames back (the process no longer maps them).
pub fn free(pa: usize, pages: usize) {
    pmm::free_contiguous(pa, pages);
}

/// Called by the wm about once a second: windows of processes that have exited are closed
/// and their buffers freed (an exited process's address space is already gone).
pub fn reap() -> Vec<u32> {
    let alive =
        |pid: u32| proc::get(pid).is_some_and(|p| p.inner.lock().state == proc::PState::Alive);
    // Snapshot (id, pid) first: don't take process locks while holding CLIENTS.
    let owners: Vec<(u32, u32)> = CLIENTS
        .lock()
        .iter()
        .map(|(&id, cl)| (id, cl.pid))
        .collect();
    let dead: Vec<u32> = owners
        .into_iter()
        .filter(|&(_, pid)| !alive(pid))
        .map(|(id, _)| id)
        .collect();
    for &id in &dead {
        if let Some(cl) = CLIENTS.lock().remove(&id) {
            free(cl.pa, cl.pages);
        }
    }
    dead
}

/// The window was closed on screen: tell the client.
pub fn closed(id: u32) {
    if let Some(cl) = CLIENTS.lock().get_mut(&id) {
        cl.closed = true;
    }
    push(id, encode(EV_CLOSE, 0, 0, 0, 0, 0));
}

/// The name of the process that owns client window `id` (for the dock's running dots).
pub fn process_name(id: u32) -> Option<String> {
    let pid = CLIENTS.lock().get(&id)?.pid;
    let p = proc::get(pid)?;
    let n = p.inner.lock().name;
    Some(String::from(n.as_str()))
}

/// Body size the wm should give a new client window, and its title.
pub fn info(id: u32) -> Option<(u32, u32, String)> {
    CLIENTS
        .lock()
        .get(&id)
        .map(|cl| (cl.w, cl.h, cl.title.clone()))
}

/// A file dropped onto client window `id` at body (x, y): the user's grant of that one
/// file (`proc::grant_file`, read-only, audited as powerbox.grant via=drop), delivered as an
/// open fd in a Drop event (kind 8: x, y, key = fd). The app reads it like any fd.
pub fn drop_file(id: u32, x: i32, y: i32, path: &str) -> Result<(), String> {
    let pid = CLIENTS
        .lock()
        .get(&id)
        .map(|cl| cl.pid)
        .ok_or_else(|| String::from("the window is gone"))?;
    let fd = proc::grant_file(pid, path, false, "drop").map_err(|e| alloc::format!("{e}"))?;
    push(id, encode(8, x, y, fd as u32, 0, 0));
    Ok(())
}

/// A notice button for the process `pid`: an action event to its first window. False if it
/// has none.
pub fn action(pid: u32, tag: u32, index: u32) -> bool {
    let id = CLIENTS
        .lock()
        .iter()
        .filter(|(_, cl)| cl.pid == pid && !cl.closed)
        .map(|(id, _)| *id)
        .min();
    let Some(id) = id else {
        return false;
    };
    push(id, encode(EV_ACTION, 0, 0, tag, index, 0));
    true
}

fn push(id: u32, ev: [u8; EVENT_LEN]) {
    if let Some(cl) = CLIENTS.lock().get_mut(&id) {
        if cl.events.len() >= MAX_QUEUED {
            cl.events.pop_front();
        }
        cl.events.push_back(ev);
    }
    EVENT_WQ.wake_all();
}

/// 32-byte event: kind u32, x i32, y i32, key u32, mods u32, extra i32 (button / scroll dy),
/// 8 bytes reserved. All little endian.
fn encode(kind: u32, x: i32, y: i32, key: u32, mods: u32, extra: i32) -> [u8; EVENT_LEN] {
    let mut b = [0u8; EVENT_LEN];
    for (k, v) in [kind, x as u32, y as u32, key, mods, extra as u32]
        .iter()
        .enumerate()
    {
        b[k * 4..k * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    b
}

/// What the wm holds for a client window.
pub struct ClientView {
    id: u32,
    front: Option<SharedFront>,
    title: String,
    app: String,
    title_gen: u32,
    /// The pointer is pressed inside (so moves are sent as drags too).
    last: (i32, i32),
}

impl ClientView {
    pub fn new(id: u32) -> Box<ClientView> {
        let title = info(id).map_or(String::from("App"), |i| i.2);
        let (front, app) = {
            let c = CLIENTS.lock();
            let cl = c.get(&id);
            (
                cl.map(|cl| cl.front.clone()),
                cl.map_or_else(|| title.clone(), |cl| cl.app.clone()),
            )
        };
        Box::new(ClientView {
            id,
            front,
            title,
            app,
            title_gen: 0,
            last: (-1, -1),
        })
    }
}

fn mods_bits(m: super::event::Mods) -> u32 {
    (m.shift as u32) | (m.ctrl as u32) << 1 | (m.alt as u32) << 2 | (m.meta as u32) << 3
}

impl WindowContents for ClientView {
    fn title(&self) -> &str {
        &self.title
    }

    fn app_name(&self) -> &str {
        &self.app
    }

    fn menus(&self) -> &'static [&'static str] {
        &["File", "Edit", "Window", "Help"]
    }

    fn min_body(&self) -> Option<(i32, i32)> {
        // A resizable client goes down to its minimum; the others keep their buffer size.
        CLIENTS.lock().get(&self.id).map(|cl| {
            let (w, h) = if cl.resizable { cl.min } else { (cl.w, cl.h) };
            (w as i32, h as i32)
        })
    }

    fn draw(&mut self, p: &mut Painter, w: i32, h: i32, _focused: bool) {
        // A resizable client follows its window (a resize drag, a tab group).
        body_size(self.id, w, h);
        // The window is gone (its process destroyed it): nothing to show.
        let Some(front) = self
            .front
            .as_ref()
            .filter(|_| CLIENTS.lock().contains_key(&self.id))
        else {
            p.fill(0, 0, w, h, MILK);
            return;
        };
        // The front buffer holds the last committed frame; the lock keeps a commit from
        // rewriting it while it is copied out.
        let f = front.lock();
        let (cw, ch) = (f.w as i32, f.h as i32);
        p.blit_raw(0, 0, f.pixels(), cw as usize, cw, ch);
        drop(f);
        if w > cw {
            p.fill(cw, 0, w - cw, h, MILK);
        }
        if h > ch {
            p.fill(0, ch, cw.min(w), h - ch, MILK);
        }
    }

    fn key(&mut self, k: &KeyInput) -> bool {
        let code = match k.key {
            Key::Enter => K_ENTER,
            Key::Backspace => K_BACKSPACE,
            Key::Tab => K_TAB,
            Key::Escape => K_ESCAPE,
            Key::Up => K_UP,
            Key::Down => K_DOWN,
            Key::Left => K_LEFT,
            Key::Right => K_RIGHT,
            Key::Delete => K_DELETE,
            Key::Char(c) => k.ch.unwrap_or(c) as u32,
            _ => return false,
        };
        push(
            self.id,
            encode(EV_KEY_DOWN, 0, 0, code, mods_bits(k.mods), 0),
        );
        false
    }

    fn pointer(&mut self, e: &PointerInput) -> bool {
        let (kind, extra) = match e.kind {
            PointerKind::Down(b) => (EV_BUTTON_DOWN, button(b)),
            PointerKind::Up(b) => (EV_BUTTON_UP, button(b)),
            PointerKind::Drag => (EV_POINTER_MOVE, 1),
        };
        if kind == EV_POINTER_MOVE && (e.x, e.y) == self.last {
            return false;
        }
        self.last = (e.x, e.y);
        push(self.id, encode(kind, e.x, e.y, 0, 0, extra));
        false
    }

    fn scroll(&mut self, dy: i32) -> bool {
        push(self.id, encode(EV_SCROLL, 0, 0, 0, 0, dy));
        false
    }

    fn tick(&mut self, _now: u64) -> bool {
        let mut c = CLIENTS.lock();
        let Some(cl) = c.get_mut(&self.id) else {
            return false;
        };
        let mut redraw = cl.dirty.take().is_some();
        if cl.title_gen != self.title_gen {
            self.title_gen = cl.title_gen;
            self.title = cl.title.clone();
            redraw = true;
        }
        redraw
    }
}

fn button(b: Button) -> i32 {
    match b {
        Button::Left => 1,
        Button::Right => 2,
        Button::Middle => 3,
    }
}

/// Air kept between a new window's bottom edge and the top of the dock.
const DOCK_CLEARANCE: i32 = 8;
/// Air kept between a new window's right edge and the screen's.
const EDGE_CLEARANCE: i32 = 16;

/// Where a new client window goes: centred in the area between the menu bar and the dock,
/// nudged 24 px down-right per window already placed (n) so they don't stack exactly. The
/// bottom edge stays DOCK_CLEARANCE above the dock and the right edge EDGE_CLEARANCE inside the
/// screen (a window that would end beyond slides back), and the window stays on-screen (one bigger
/// than the area sits at its top left, below the bar).
pub fn place(n: i32, w: i32, h: i32) -> Rect {
    let area = super::Desktop::work_area();
    let step = (n % 4) * 24;
    let x = area.x + (area.w - w) / 2 + step;
    let y = area.y + (area.h - h) / 2 + step;
    let x = x.min(screen_w() - w - EDGE_CLEARANCE).max(0);
    let y = y.min(area.y + area.h - DOCK_CLEARANCE - h).max(MENU_H);
    Rect::new(x, y, w, h)
}
