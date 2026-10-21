//! The window system's own input events, and the adapters that make them.
//!
//! The compositor only ever sees [`Event`]. Three sources feed it:
//! * [`from_input`]: the virtio keyboard/tablet queue (`input::poll()`), once that branch merges.
//!   It is written against the agreed `input::Event` shape and compiled out until then (search
//!   for `MERGE(input)`).
//! * [`UartKeys`]: bytes typed on the serial console (`scripts/run.sh` uses `-serial mon:stdio`),
//!   decoded into key events, so the desktop is usable before the input driver lands.
//! * `script`: canned event sequences for headless demos and benchmarks.

/// Mouse buttons.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Left,
    Right,
    Middle,
}

/// Keys the shell and apps care about. Printable keys arrive as `Char` with the character.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// F1..F12.
    F(u8),
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Cmd / Super / the Windows key.
    pub meta: bool,
}

impl Mods {
    pub const NONE: Mods = Mods {
        shift: false,
        ctrl: false,
        alt: false,
        meta: false,
    };
    pub const CTRL: Mods = Mods {
        ctrl: true,
        ..Mods::NONE
    };
    pub const META: Mods = Mods {
        meta: true,
        ..Mods::NONE
    };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    /// Absolute pointer position in screen pixels (1024x768).
    PointerMove { x: i32, y: i32 },
    Button { button: Button, down: bool },
    /// Wheel: positive `dy` scrolls content down (towards the end).
    Scroll { dy: i32 },
    /// A key press (`down`) or release. `ch` is the text it types, if any.
    Key {
        key: Key,
        ch: Option<char>,
        mods: Mods,
        down: bool,
    },
}

impl Event {
    pub const fn key(key: Key, mods: Mods) -> Event {
        Event::Key {
            key,
            ch: None,
            mods,
            down: true,
        }
    }

    pub const fn char(c: char) -> Event {
        Event::Key {
            key: Key::Char(c),
            ch: Some(c),
            mods: Mods::NONE,
            down: true,
        }
    }
}

// MERGE(input): delete the `#[cfg(any())]` line below once `crate::input` exists, and call
// `from_input` from `wm::poll_sources` (see the MERGE(input) note there). The input agent's queue
// is `input::poll() -> Option<input::Event>` with KeyDown/KeyUp { key, ch, mods }, PointerMove
// { x, y } (absolute, 1024x768), Button { button, down } and Scroll; adjust the names below to
// whatever actually landed.
#[cfg(any())]
pub fn from_input(ev: crate::input::Event) -> Option<Event> {
    use crate::input as i;
    let mods = |m: i::Modifiers| Mods {
        shift: m.shift,
        ctrl: m.ctrl,
        alt: m.alt,
        meta: m.meta,
    };
    let key = |k: i::Key, ch: Option<char>| match k {
        i::Key::Enter => Key::Enter,
        i::Key::Backspace => Key::Backspace,
        i::Key::Delete => Key::Delete,
        i::Key::Tab => Key::Tab,
        i::Key::Escape => Key::Escape,
        i::Key::Up => Key::Up,
        i::Key::Down => Key::Down,
        i::Key::Left => Key::Left,
        i::Key::Right => Key::Right,
        i::Key::Home => Key::Home,
        i::Key::End => Key::End,
        i::Key::PageUp => Key::PageUp,
        i::Key::PageDown => Key::PageDown,
        i::Key::F(n) => Key::F(n),
        _ => match ch {
            Some(c) => Key::Char(c),
            None => Key::Other,
        },
    };
    Some(match ev {
        i::Event::KeyDown { key: k, ch, modifiers } => Event::Key {
            key: key(k, ch),
            ch,
            mods: mods(modifiers),
            down: true,
        },
        i::Event::KeyUp { key: k, ch, modifiers } => Event::Key {
            key: key(k, ch),
            ch,
            mods: mods(modifiers),
            down: false,
        },
        i::Event::PointerMove { x, y } => Event::PointerMove {
            x: x as i32,
            y: y as i32,
        },
        i::Event::Button { button, down } => Event::Button {
            button: match button {
                i::MouseButton::Left => Button::Left,
                i::MouseButton::Right => Button::Right,
                i::MouseButton::Middle => Button::Middle,
            },
            down,
        },
        i::Event::Scroll { dy, .. } => Event::Scroll { dy: dy as i32 },
    })
}

/// Turns serial console bytes into key events: printable ASCII, Enter, Backspace, Tab, Ctrl+letter,
/// and the usual xterm escape sequences (arrows, Home/End, F1..F4 as `ESC O P..S`, F5+ as
/// `ESC [ 15~` etc.). Ctrl+Space (NUL) becomes Cmd+Space, so the launcher works from a terminal.
#[derive(Default)]
pub struct UartKeys {
    state: u8,
    num: u32,
}

impl UartKeys {
    /// Feed one byte; returns a key event when one is complete.
    pub fn feed(&mut self, b: u8) -> Option<Event> {
        match self.state {
            1 => {
                self.state = match b {
                    b'[' => 2,
                    b'O' => 3,
                    _ => 0,
                };
                self.num = 0;
                if self.state == 0 {
                    return Some(Event::key(Key::Escape, Mods::NONE));
                }
                None
            }
            2 => match b {
                b'0'..=b'9' => {
                    self.num = self.num * 10 + (b - b'0') as u32;
                    None
                }
                b';' => None,
                _ => {
                    self.state = 0;
                    let k = match b {
                        b'A' => Key::Up,
                        b'B' => Key::Down,
                        b'C' => Key::Right,
                        b'D' => Key::Left,
                        b'H' => Key::Home,
                        b'F' => Key::End,
                        b'~' => match self.num {
                            1 | 7 => Key::Home,
                            4 | 8 => Key::End,
                            3 => Key::Delete,
                            5 => Key::PageUp,
                            6 => Key::PageDown,
                            15 => Key::F(5),
                            17..=21 => Key::F((self.num - 11) as u8),
                            23 | 24 => Key::F((self.num - 12) as u8),
                            _ => Key::Other,
                        },
                        _ => Key::Other,
                    };
                    Some(Event::key(k, Mods::NONE))
                }
            },
            3 => {
                self.state = 0;
                let k = match b {
                    b'P'..=b'S' => Key::F(b - b'P' + 1),
                    b'H' => Key::Home,
                    b'F' => Key::End,
                    _ => Key::Other,
                };
                Some(Event::key(k, Mods::NONE))
            }
            _ => match b {
                0x1b => {
                    self.state = 1;
                    None
                }
                b'\r' | b'\n' => Some(Event::key(Key::Enter, Mods::NONE)),
                0x7f | 0x08 => Some(Event::key(Key::Backspace, Mods::NONE)),
                b'\t' => Some(Event::key(Key::Tab, Mods::NONE)),
                0 => Some(Event::key(Key::Char(' '), Mods::META)),
                1..=26 => Some(Event::key(Key::Char((b'a' + b - 1) as char), Mods::CTRL)),
                0x20..=0x7e => Some(Event::char(b as char)),
                _ => None,
            },
        }
    }

    /// A lone ESC with nothing after it (by the end of a poll) is the Escape key.
    pub fn flush(&mut self) -> Option<Event> {
        if self.state == 1 {
            self.state = 0;
            return Some(Event::key(Key::Escape, Mods::NONE));
        }
        None
    }
}
