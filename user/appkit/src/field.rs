//! A single-line text field's state and key handling.

use alloc::string::String;
use gui::{Key, Mods};

#[derive(Clone, Debug, Default)]
pub struct Field {
    pub text: String,
    /// Caret position in characters.
    pub cur: usize,
    /// Accept digits only (and ':' if `time`).
    pub digits: bool,
    pub max: usize,
}

impl Field {
    pub fn new(text: &str, max: usize) -> Field {
        Field {
            text: String::from(text),
            cur: text.chars().count(),
            digits: false,
            max,
        }
    }

    pub fn numeric(text: &str, max: usize) -> Field {
        Field {
            digits: true,
            ..Field::new(text, max)
        }
    }

    pub fn set(&mut self, s: &str) {
        self.text = String::from(s);
        self.cur = s.chars().count();
    }

    fn byte(&self, ch: usize) -> usize {
        self.text
            .char_indices()
            .nth(ch)
            .map_or(self.text.len(), |(i, _)| i)
    }

    /// Handle a key; true if the text or caret changed.
    pub fn key(&mut self, k: Key, m: Mods) -> bool {
        if m.ctrl || m.meta {
            return false;
        }
        match k {
            Key::Char(c) if !c.is_control() => {
                if self.digits && !c.is_ascii_digit() {
                    return false;
                }
                if self.text.chars().count() >= self.max {
                    return false;
                }
                let i = self.byte(self.cur);
                self.text.insert(i, c);
                self.cur += 1;
                true
            }
            Key::Backspace if self.cur > 0 => {
                let i = self.byte(self.cur - 1);
                self.text.remove(i);
                self.cur -= 1;
                true
            }
            Key::Delete if self.cur < self.text.chars().count() => {
                let i = self.byte(self.cur);
                self.text.remove(i);
                true
            }
            Key::Left if self.cur > 0 => {
                self.cur -= 1;
                true
            }
            Key::Right if self.cur < self.text.chars().count() => {
                self.cur += 1;
                true
            }
            _ => false,
        }
    }
}
