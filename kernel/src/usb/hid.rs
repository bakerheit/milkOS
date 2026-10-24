//! HID boot protocol devices: a keyboard or a mouse's reports turned into `crate::input` events.
//! The parsing and auto-repeat are `rpikit::usb`'s (host-tested); this is the glue.

use rpikit::usb::{HidKind, Keyboard, parse_mouse};

use crate::input;

/// evdev button codes (what `input::report_key` takes).
const BTN_LEFT: u16 = 0x110;
const BTN_RIGHT: u16 = 0x111;
const BTN_MIDDLE: u16 = 0x112;

pub struct Hid {
    kind: HidKind,
    kb: Keyboard,
    buttons: u8,
    errors: u32,
}

fn now_ms() -> u64 {
    crate::timer::now_ns() / 1_000_000
}

impl Hid {
    pub fn new(kind: HidKind) -> Hid {
        Hid {
            kind,
            kb: Keyboard::new(),
            buttons: 0,
            errors: 0,
        }
    }

    pub fn name(&self) -> &'static str {
        match self.kind {
            HidKind::Keyboard => "keyboard",
            HidKind::Mouse => "mouse",
        }
    }

    /// One report from the device.
    pub fn report(&mut self, r: &[u8]) {
        self.errors = 0;
        match self.kind {
            HidKind::Keyboard => {
                self.kb
                    .report(r, now_ms(), |c| input::report_key(c.evdev, c.value));
            }
            HidKind::Mouse => {
                let Some(m) = parse_mouse(r) else { return };
                let changed = m.buttons ^ self.buttons;
                self.buttons = m.buttons;
                for (bit, code) in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE].into_iter().enumerate() {
                    if changed & (1 << bit) != 0 {
                        input::report_key(code, (m.buttons >> bit & 1) as i32);
                    }
                }
                if m.dx != 0 || m.dy != 0 {
                    input::report_pointer_rel(m.dx, m.dy);
                }
                if m.wheel != 0 {
                    input::report_scroll(0, m.wheel);
                }
            }
        }
    }

    /// Time passes (key auto-repeat).
    pub fn tick(&mut self) {
        if self.kind == HidKind::Keyboard {
            self.kb
                .tick(now_ms(), |c| input::report_key(c.evdev, c.value));
        }
    }

    /// The endpoint stopped with an error (not a short packet).
    pub fn error(&mut self, code: u8) {
        self.errors += 1;
        if self.errors == 1 {
            crate::kprintln!(
                "[usb] {} endpoint stopped, completion code {code}",
                self.name()
            );
        }
    }

    /// The device was unplugged: let go of everything it held down.
    pub fn unplugged(&mut self) {
        match self.kind {
            HidKind::Keyboard => self.kb.release_all(|c| input::report_key(c.evdev, c.value)),
            HidKind::Mouse => {
                for (bit, code) in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE].into_iter().enumerate() {
                    if self.buttons & (1 << bit) != 0 {
                        input::report_key(code, 0);
                    }
                }
                self.buttons = 0;
            }
        }
    }
}
