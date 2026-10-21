//! The window system's own input events, and the adapters that make them.
//!
//! The compositor only ever sees [`Event`]. Three sources feed it:
//! * [`from_input`]: the virtio keyboard/tablet queue (`input::poll()`).
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
    PointerMove {
        x: i32,
        y: i32,
    },
    Button {
        button: Button,
        down: bool,
    },
    /// Wheel: positive `dy` scrolls content down (towards the end).
    Scroll {
        dy: i32,
    },
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

/// Map an `input::Event` (virtio keyboard + tablet) to ours. Modifier key presses themselves and
/// side buttons are dropped (modifier state rides on every key event).
pub fn from_input(ev: crate::input::Event) -> Option<Event> {
    use crate::input::{self as i, KeyCode as K};
    let key_ev = |k: i::KeyEvent, down: bool| -> Option<Event> {
        if k.key.is_modifier() {
            return None;
        }
        let key = match k.key {
            K::Enter | K::KpEnter => Key::Enter,
            K::Backspace => Key::Backspace,
            K::Delete => Key::Delete,
            K::Tab => Key::Tab,
            K::Escape => Key::Escape,
            K::Up => Key::Up,
            K::Down => Key::Down,
            K::Left => Key::Left,
            K::Right => Key::Right,
            K::Home => Key::Home,
            K::End => Key::End,
            K::PageUp => Key::PageUp,
            K::PageDown => Key::PageDown,
            K::F1 => Key::F(1),
            K::F2 => Key::F(2),
            K::F3 => Key::F(3),
            K::F4 => Key::F(4),
            K::F5 => Key::F(5),
            K::F6 => Key::F(6),
            K::F7 => Key::F(7),
            K::F8 => Key::F(8),
            K::F9 => Key::F(9),
            K::F10 => Key::F(10),
            K::F11 => Key::F(11),
            K::F12 => Key::F(12),
            _ => match k.ch {
                // Shortcuts match on the unshifted letter.
                Some(c) => Key::Char(c.to_ascii_lowercase()),
                None => Key::Other,
            },
        };
        Some(Event::Key {
            key,
            ch: k.text(),
            mods: Mods {
                shift: k.mods.shift(),
                ctrl: k.mods.ctrl(),
                alt: k.mods.alt(),
                meta: k.mods.meta(),
            },
            down,
        })
    };
    match ev {
        i::Event::KeyDown(k) => key_ev(k, true),
        i::Event::KeyUp(k) => key_ev(k, false),
        i::Event::PointerMove { x, y } => Some(Event::PointerMove { x, y }),
        i::Event::Button { button, down, .. } => Some(Event::Button {
            button: match button {
                i::MouseButton::Left => Button::Left,
                i::MouseButton::Right => Button::Right,
                i::MouseButton::Middle => Button::Middle,
                _ => return None,
            },
            down,
        }),
        // input: dy > 0 is "up" (away from the user); ours: dy > 0 moves content towards its end.
        i::Event::Scroll { dy, .. } if dy != 0 => Some(Event::Scroll { dy: -dy }),
        i::Event::Scroll { .. } => None,
    }
}

/// Turns serial console bytes into key events: printable ASCII, Enter, Backspace, Tab, Ctrl+letter,
/// and the usual xterm escape sequences (arrows, Home/End, F1..F4 as `ESC O P..S`, F5+ as
/// `ESC [ 15~` etc.). Ctrl+Space (NUL) becomes Cmd+Space, so the launcher works from a terminal.
#[derive(Default)]
pub struct UartKeys {
    state: u8,
    num: u32,
    /// When the pending ESC arrived (ns), to tell a lone Escape from a sequence.
    esc_at: u64,
}

impl UartKeys {
    /// Feed one byte; returns a key event when one is complete.
    pub fn feed(&mut self, b: u8, now: u64) -> Option<Event> {
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
                    self.esc_at = now;
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

    /// A lone ESC with nothing after it for 30 ms is the Escape key.
    pub fn flush(&mut self, now: u64) -> Option<Event> {
        if self.state == 1 && now.saturating_sub(self.esc_at) > 30_000_000 {
            self.state = 0;
            return Some(Event::key(Key::Escape, Mods::NONE));
        }
        None
    }
}
