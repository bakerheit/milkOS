//! Key codes and the US keyboard layout.
//!
//! [`KeyCode`] is our own enum. Variants are named after (and converted from) the Linux evdev
//! `KEY_*` codes that virtio-input reports, so other sources (USB HID, PS/2) map onto it
//! too. Anything we have no name for comes through as [`KeyCode::Unknown`] with the raw
//! evdev code.

macro_rules! keycodes {
    ($($(#[$m:meta])* $name:ident = $code:expr),* $(,)?) => {
        /// A physical key, independent of layout and modifiers.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum KeyCode {
            $($(#[$m])* $name,)*
            /// A key we do not name; the value is its evdev code.
            Unknown(u16),
        }

        impl KeyCode {
            /// From a Linux evdev `KEY_*` code.
            pub const fn from_evdev(code: u16) -> KeyCode {
                match code {
                    $($code => KeyCode::$name,)*
                    other => KeyCode::Unknown(other),
                }
            }

            /// Back to the evdev code.
            pub const fn evdev(self) -> u16 {
                match self {
                    $(KeyCode::$name => $code,)*
                    KeyCode::Unknown(c) => c,
                }
            }
        }
    };
}

keycodes! {
    Escape = 1,
    Num1 = 2, Num2 = 3, Num3 = 4, Num4 = 5, Num5 = 6,
    Num6 = 7, Num7 = 8, Num8 = 9, Num9 = 10, Num0 = 11,
    Minus = 12, Equal = 13, Backspace = 14, Tab = 15,
    Q = 16, W = 17, E = 18, R = 19, T = 20, Y = 21, U = 22, I = 23, O = 24, P = 25,
    LeftBracket = 26, RightBracket = 27, Enter = 28, LeftCtrl = 29,
    A = 30, S = 31, D = 32, F = 33, G = 34, H = 35, J = 36, K = 37, L = 38,
    Semicolon = 39, Apostrophe = 40, Grave = 41, LeftShift = 42, Backslash = 43,
    Z = 44, X = 45, C = 46, V = 47, B = 48, N = 49, M = 50,
    Comma = 51, Dot = 52, Slash = 53, RightShift = 54, KpAsterisk = 55, LeftAlt = 56,
    Space = 57, CapsLock = 58,
    F1 = 59, F2 = 60, F3 = 61, F4 = 62, F5 = 63, F6 = 64, F7 = 65, F8 = 66, F9 = 67, F10 = 68,
    NumLock = 69, ScrollLock = 70,
    Kp7 = 71, Kp8 = 72, Kp9 = 73, KpMinus = 74, Kp4 = 75, Kp5 = 76, Kp6 = 77, KpPlus = 78,
    Kp1 = 79, Kp2 = 80, Kp3 = 81, Kp0 = 82, KpDot = 83,
    /// The extra key left of Z on ISO keyboards.
    IsoBackslash = 86,
    F11 = 87, F12 = 88,
    KpEnter = 96, RightCtrl = 97, KpSlash = 98, PrintScreen = 99, RightAlt = 100,
    Home = 102, Up = 103, PageUp = 104, Left = 105, Right = 106, End = 107, Down = 108,
    PageDown = 109, Insert = 110, Delete = 111,
    Mute = 113, VolumeDown = 114, VolumeUp = 115, Power = 116, KpEqual = 117, Pause = 119,
    /// Cmd on a Mac keyboard, the Windows / Super key elsewhere.
    LeftMeta = 125,
    RightMeta = 126,
    /// The context-menu key.
    Menu = 127,
}

impl KeyCode {
    pub const fn is_modifier(self) -> bool {
        matches!(
            self,
            KeyCode::LeftShift
                | KeyCode::RightShift
                | KeyCode::LeftCtrl
                | KeyCode::RightCtrl
                | KeyCode::LeftAlt
                | KeyCode::RightAlt
                | KeyCode::LeftMeta
                | KeyCode::RightMeta
                | KeyCode::CapsLock
        )
    }

    /// 'a'..='z' for letter keys.
    pub const fn letter(self) -> Option<char> {
        use KeyCode::*;
        Some(match self {
            A => 'a',
            B => 'b',
            C => 'c',
            D => 'd',
            E => 'e',
            F => 'f',
            G => 'g',
            H => 'h',
            I => 'i',
            J => 'j',
            K => 'k',
            L => 'l',
            M => 'm',
            N => 'n',
            O => 'o',
            P => 'p',
            Q => 'q',
            R => 'r',
            S => 's',
            T => 't',
            U => 'u',
            V => 'v',
            W => 'w',
            X => 'x',
            Y => 'y',
            Z => 'z',
            _ => return None,
        })
    }
}

/// The character `key` types on a US layout. Letters are upper case when exactly one of
/// `shift` / `caps` is on; other keys only look at `shift`. Enter, Tab, Backspace, Escape and
/// Delete give `'\n'`, `'\t'`, `'\x08'`, `'\x1b'`, `'\x7f'`. Ctrl/Alt/Meta are not applied
/// here (see `KeyEvent::text` and `KeyEvent::ctrl_char`).
pub fn us_char(key: KeyCode, shift: bool, caps: bool) -> Option<char> {
    use KeyCode::*;
    if let Some(c) = key.letter() {
        return Some(if shift != caps {
            c.to_ascii_uppercase()
        } else {
            c
        });
    }
    let (plain, shifted) = match key {
        Num1 => ('1', '!'),
        Num2 => ('2', '@'),
        Num3 => ('3', '#'),
        Num4 => ('4', '$'),
        Num5 => ('5', '%'),
        Num6 => ('6', '^'),
        Num7 => ('7', '&'),
        Num8 => ('8', '*'),
        Num9 => ('9', '('),
        Num0 => ('0', ')'),
        Minus => ('-', '_'),
        Equal => ('=', '+'),
        LeftBracket => ('[', '{'),
        RightBracket => (']', '}'),
        Backslash | IsoBackslash => ('\\', '|'),
        Semicolon => (';', ':'),
        Apostrophe => ('\'', '"'),
        Grave => ('`', '~'),
        Comma => (',', '<'),
        Dot => ('.', '>'),
        Slash => ('/', '?'),
        Space => (' ', ' '),
        Enter | KpEnter => ('\n', '\n'),
        Tab => ('\t', '\t'),
        Backspace => ('\x08', '\x08'),
        Escape => ('\x1b', '\x1b'),
        Delete => ('\x7f', '\x7f'),
        // Keypad: always the num-lock-on meaning (we do not drive the LEDs yet).
        Kp0 => ('0', '0'),
        Kp1 => ('1', '1'),
        Kp2 => ('2', '2'),
        Kp3 => ('3', '3'),
        Kp4 => ('4', '4'),
        Kp5 => ('5', '5'),
        Kp6 => ('6', '6'),
        Kp7 => ('7', '7'),
        Kp8 => ('8', '8'),
        Kp9 => ('9', '9'),
        KpDot => ('.', '.'),
        KpSlash => ('/', '/'),
        KpAsterisk => ('*', '*'),
        KpMinus => ('-', '-'),
        KpPlus => ('+', '+'),
        KpEqual => ('=', '='),
        _ => return None,
    };
    Some(if shift { shifted } else { plain })
}
