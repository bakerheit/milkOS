//! Kernel-wide input events: keyboard, pointer, buttons, scroll.
//!
//! Consumers (the console demo today, the window server next) either drain the queue with
//! [`poll`] from thread context, or [`subscribe`] a hook that sees every event as it is
//! produced. Producers (drivers such as `virtio::input`) feed raw reports in with
//! [`report_key`], [`report_pointer`] and [`report_scroll`]; this module tracks modifier,
//! pointer and button state and turns key reports into typed [`KeyEvent`]s with the US
//! layout applied.
//!
//! Coordinates are screen pixels, (0, 0) top left, clamped to [`screen_size`] (the display
//! size by default).
//!
//! Locking: one [`SpinLock`] around the state and queue. Producers run in IRQ context (IRQs
//! already masked); [`poll`] masks IRQs for the few instructions it holds the lock.

// Public API surface for the window server; not all of it is used yet.
#![allow(dead_code)]

pub mod keymap;

use core::sync::atomic::{AtomicU64, Ordering};

pub use keymap::KeyCode;

use crate::display;
use crate::sync::{FnSlot, SpinLock};

/// Modifier state carried by every key event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers(u8);

impl Modifiers {
    pub const NONE: Modifiers = Modifiers(0);
    pub const SHIFT: Modifiers = Modifiers(1 << 0);
    pub const CTRL: Modifiers = Modifiers(1 << 1);
    pub const ALT: Modifiers = Modifiers(1 << 2);
    /// Cmd / Super / Windows.
    pub const META: Modifiers = Modifiers(1 << 3);
    /// Caps Lock is toggled on (a lock state, not a held key).
    pub const CAPS_LOCK: Modifiers = Modifiers(1 << 4);

    pub const fn bits(self) -> u8 {
        self.0
    }
    pub const fn contains(self, other: Modifiers) -> bool {
        self.0 & other.0 == other.0
    }
    pub const fn union(self, other: Modifiers) -> Modifiers {
        Modifiers(self.0 | other.0)
    }
    pub const fn shift(self) -> bool {
        self.contains(Self::SHIFT)
    }
    pub const fn ctrl(self) -> bool {
        self.contains(Self::CTRL)
    }
    pub const fn alt(self) -> bool {
        self.contains(Self::ALT)
    }
    pub const fn meta(self) -> bool {
        self.contains(Self::META)
    }
    pub const fn caps_lock(self) -> bool {
        self.contains(Self::CAPS_LOCK)
    }
}

impl core::ops::BitOr for Modifiers {
    type Output = Modifiers;
    fn bitor(self, rhs: Modifiers) -> Modifiers {
        self.union(rhs)
    }
}

/// A key press or release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyEvent {
    pub key: KeyCode,
    /// What the key types on a US layout with Shift / Caps Lock applied (Ctrl, Alt and Meta
    /// ignored). `None` for keys that type nothing (arrows, F-keys, modifiers).
    pub ch: Option<char>,
    /// Modifiers in effect for this event (after it, for modifier keys themselves).
    pub mods: Modifiers,
    /// Auto-repeat of a held key (always a `KeyDown`).
    pub repeat: bool,
}

impl KeyEvent {
    /// The text this event inserts into a text field: [`ch`](Self::ch) unless Ctrl, Alt or
    /// Meta is held (those make it a shortcut).
    pub fn text(&self) -> Option<char> {
        if self.mods.ctrl() || self.mods.alt() || self.mods.meta() {
            None
        } else {
            self.ch
        }
    }

    /// The terminal control character for Ctrl+key (Ctrl+A = `'\x01'` .. Ctrl+Z = `'\x1a'`,
    /// plus Ctrl+[ \ ] ^ _), or `None`.
    pub fn ctrl_char(&self) -> Option<char> {
        if !self.mods.ctrl() {
            return None;
        }
        let c = keymap::us_char(self.key, self.mods.shift(), false)?;
        match c.to_ascii_uppercase() {
            c @ '@'..='_' => Some(((c as u8) & 0x1f) as char),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    /// Back / forward side buttons.
    Back,
    Forward,
}

/// One input event.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    KeyDown(KeyEvent),
    KeyUp(KeyEvent),
    /// The pointer is now at (x, y). Consecutive moves still in the queue are merged.
    PointerMove {
        x: i32,
        y: i32,
    },
    /// A mouse button changed, with the pointer position at the time.
    Button {
        button: MouseButton,
        down: bool,
        x: i32,
        y: i32,
    },
    /// Wheel motion in notches. `dy > 0` scrolls up (away from the user), `dx > 0` right.
    Scroll {
        dx: i32,
        dy: i32,
    },
}

// --- state and queue --------------------------------------------------------------------

const QUEUE_LEN: usize = 256;

// Held-modifier bits (left and right tracked separately, so releasing one keeps the other).
const L_SHIFT: u8 = 1 << 0;
const R_SHIFT: u8 = 1 << 1;
const L_CTRL: u8 = 1 << 2;
const R_CTRL: u8 = 1 << 3;
const L_ALT: u8 = 1 << 4;
const R_ALT: u8 = 1 << 5;
const L_META: u8 = 1 << 6;
const R_META: u8 = 1 << 7;

struct State {
    held: u8,
    caps: bool,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    buttons: u8,
    queue: [Option<Event>; QUEUE_LEN],
    head: usize, // next slot to read
    len: usize,
}

impl State {
    fn mods(&self) -> Modifiers {
        let mut m = Modifiers::NONE;
        if self.held & (L_SHIFT | R_SHIFT) != 0 {
            m = m | Modifiers::SHIFT;
        }
        if self.held & (L_CTRL | R_CTRL) != 0 {
            m = m | Modifiers::CTRL;
        }
        if self.held & (L_ALT | R_ALT) != 0 {
            m = m | Modifiers::ALT;
        }
        if self.held & (L_META | R_META) != 0 {
            m = m | Modifiers::META;
        }
        if self.caps {
            m = m | Modifiers::CAPS_LOCK;
        }
        m
    }

    /// Queue `ev`; merges into a trailing PointerMove; drops (and counts) when full.
    fn push(&mut self, ev: Event) {
        if let Event::PointerMove { .. } = ev
            && self.len > 0
        {
            let last = (self.head + self.len - 1) % QUEUE_LEN;
            if let Some(Event::PointerMove { .. }) = self.queue[last] {
                self.queue[last] = Some(ev);
                return;
            }
        }
        if self.len == QUEUE_LEN {
            DROPPED.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.queue[(self.head + self.len) % QUEUE_LEN] = Some(ev);
        self.len += 1;
    }

    fn pop(&mut self) -> Option<Event> {
        if self.len == 0 {
            return None;
        }
        let ev = self.queue[self.head].take();
        self.head = (self.head + 1) % QUEUE_LEN;
        self.len -= 1;
        ev
    }
}

static STATE: SpinLock<State> = SpinLock::new(State {
    held: 0,
    caps: false,
    x: display::WIDTH as i32 / 2,
    y: display::HEIGHT as i32 / 2,
    width: display::WIDTH as i32,
    height: display::HEIGHT as i32,
    buttons: 0,
    queue: [None; QUEUE_LEN],
    head: 0,
    len: 0,
});

// Events dropped because the queue was full.
static DROPPED: AtomicU64 = AtomicU64::new(0);

const MAX_SUBSCRIBERS: usize = 4;
static SUBSCRIBERS: [FnSlot; MAX_SUBSCRIBERS] = [const { FnSlot::new() }; MAX_SUBSCRIBERS];

/// A subscriber hook. Runs in the producer's context, which is IRQ context for the virtio
/// drivers: keep it short, do not block, do not draw. Typical use: wake the window server.
pub type Subscriber = fn(&Event);

/// All subscriber slots are taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubscribersFull;

// --- consumer API -----------------------------------------------------------------------

/// Next queued event, oldest first, or `None`. Never blocks.
pub fn poll() -> Option<Event> {
    STATE.lock().pop()
}

/// Number of queued events.
pub fn pending() -> usize {
    STATE.lock().len
}

/// Events lost because the queue was full.
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}

/// Get every event as it is produced (in addition to it being queued for [`poll`]).
pub fn subscribe(hook: Subscriber) -> Result<(), SubscribersFull> {
    let _g = crate::sync::IrqGuard::new();
    for s in &SUBSCRIBERS {
        if s.get::<Subscriber>().is_none() {
            s.set(hook);
            return Ok(());
        }
    }
    Err(SubscribersFull)
}

pub fn unsubscribe(hook: Subscriber) {
    let _g = crate::sync::IrqGuard::new();
    for s in &SUBSCRIBERS {
        if s.get::<Subscriber>()
            .is_some_and(|f| core::ptr::fn_addr_eq(f, hook))
        {
            s.clear();
        }
    }
}

/// Current modifier state.
pub fn modifiers() -> Modifiers {
    STATE.lock().mods()
}

/// Current pointer position.
pub fn pointer() -> (i32, i32) {
    let s = STATE.lock();
    (s.x, s.y)
}

/// Pointer coordinate space (defaults to the boot display size).
pub fn screen_size() -> (i32, i32) {
    let s = STATE.lock();
    (s.width, s.height)
}

/// Change the pointer coordinate space (e.g. on a mode change). Re-clamps the pointer.
pub fn set_screen_size(width: i32, height: i32) {
    let mut s = STATE.lock();
    s.width = width.max(1);
    s.height = height.max(1);
    s.x = s.x.clamp(0, s.width - 1);
    s.y = s.y.clamp(0, s.height - 1);
}

// --- producer API -----------------------------------------------------------------------

fn emit(ev: Option<Event>) {
    let Some(ev) = ev else { return };
    for s in &SUBSCRIBERS {
        if let Some(hook) = s.get::<Subscriber>() {
            hook(&ev);
        }
    }
}

// evdev button codes.
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;
const BTN_SIDE: u16 = 0x113;
const BTN_EXTRA: u16 = 0x114;
const BTN_FORWARD: u16 = 0x115;
const BTN_BACK: u16 = 0x116;
const BTN_TOUCH: u16 = 0x14a;
const BTN_GEAR_DOWN: u16 = 0x150;
const BTN_GEAR_UP: u16 = 0x151;

/// Feed an evdev `EV_KEY` report: `value` 1 = press, 0 = release, 2 = auto-repeat. Handles
/// both keyboard keys and pointer buttons (`BTN_*`).
pub fn report_key(code: u16, value: i32) {
    let button = match code {
        BTN_LEFT | BTN_TOUCH => Some(MouseButton::Left),
        BTN_RIGHT => Some(MouseButton::Right),
        BTN_MIDDLE => Some(MouseButton::Middle),
        BTN_SIDE | BTN_BACK => Some(MouseButton::Back),
        BTN_EXTRA | BTN_FORWARD => Some(MouseButton::Forward),
        // Some qemu versions report the wheel as gear buttons instead of REL_WHEEL.
        BTN_GEAR_UP if value == 1 => return report_scroll(0, 1),
        BTN_GEAR_DOWN if value == 1 => return report_scroll(0, -1),
        BTN_GEAR_UP | BTN_GEAR_DOWN => return,
        _ => None,
    };
    if let Some(b) = button {
        return report_button(b, value != 0);
    }

    let key = KeyCode::from_evdev(code);
    let ev = {
        let mut s = STATE.lock();
        let bit = match key {
            KeyCode::LeftShift => L_SHIFT,
            KeyCode::RightShift => R_SHIFT,
            KeyCode::LeftCtrl => L_CTRL,
            KeyCode::RightCtrl => R_CTRL,
            KeyCode::LeftAlt => L_ALT,
            KeyCode::RightAlt => R_ALT,
            KeyCode::LeftMeta => L_META,
            KeyCode::RightMeta => R_META,
            _ => 0,
        };
        match value {
            0 => s.held &= !bit,
            1 => {
                s.held |= bit;
                if key == KeyCode::CapsLock {
                    s.caps = !s.caps;
                }
            }
            _ => {}
        }
        let mods = s.mods();
        let ke = KeyEvent {
            key,
            ch: keymap::us_char(key, mods.shift(), mods.caps_lock()),
            mods,
            repeat: value == 2,
        };
        let ev = if value == 0 {
            Event::KeyUp(ke)
        } else {
            Event::KeyDown(ke)
        };
        s.push(ev);
        ev
    };
    emit(Some(ev));
}

/// Feed a pointer button change.
pub fn report_button(button: MouseButton, down: bool) {
    let ev = {
        let mut s = STATE.lock();
        let bit = 1 << button as u8;
        let was = s.buttons & bit != 0;
        if was == down {
            None
        } else {
            if down {
                s.buttons |= bit;
            } else {
                s.buttons &= !bit;
            }
            let ev = Event::Button {
                button,
                down,
                x: s.x,
                y: s.y,
            };
            s.push(ev);
            Some(ev)
        }
    };
    emit(ev);
}

/// Feed an absolute pointer position in screen pixels (clamped to the screen).
pub fn report_pointer(x: i32, y: i32) {
    let ev = {
        let mut s = STATE.lock();
        let (x, y) = (x.clamp(0, s.width - 1), y.clamp(0, s.height - 1));
        if (x, y) == (s.x, s.y) {
            None
        } else {
            s.x = x;
            s.y = y;
            let ev = Event::PointerMove { x, y };
            s.push(ev);
            Some(ev)
        }
    };
    emit(ev);
}

/// Feed a relative pointer motion (mice). Not produced by the tablet; here for completeness.
pub fn report_pointer_rel(dx: i32, dy: i32) {
    let (x, y) = pointer();
    report_pointer(x + dx, y + dy);
}

/// Feed wheel motion in notches (`dy > 0` = up / away, `dx > 0` = right).
pub fn report_scroll(dx: i32, dy: i32) {
    if dx == 0 && dy == 0 {
        return;
    }
    let ev = Event::Scroll { dx, dy };
    STATE.lock().push(ev);
    emit(Some(ev));
}
