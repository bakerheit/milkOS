//! The ACT LED as a boot progress readout (docs/raspberry-pi.md, "The green ACT LED"): the pattern
//! language as pure data, so the host tests can check it and the kernel (`kernel/src/rpi/led.rs`)
//! only has to switch the pin and wait.
//!
//! * A **start marker** first: dark 0.5 s, one long pulse of 1 s, dark 0.5 s. Nothing the Pi's
//!   firmware does looks like it, so it says "the kernel runs and memory is up".
//! * Then one **burst per milestone**, in the order they happen: N pulses of 100 ms on / 100 ms
//!   off, then 300 ms dark (the pause counts as the last pulse's off time). N is the stage's code.
//! * A milestone that **failed** (a bounded wait timed out, the device is not there) shows the
//!   same N, but slow: 400 ms on / 400 ms off, then 800 ms dark.
//! * After the last milestone the kernel turns the LED solid on ("running fine"); a crash
//!   flashes it at about 5 Hz (100 ms on, 100 ms off) forever. Neither is a [`Burst`].
//!
//! Everything is bounded: a burst has at most [`MAX_PULSES`] pulses, so its length is well under a
//! minute and all sums fit in a `u32` of milliseconds.

/// The most pulses in a burst (a code above this is cut to it).
pub const MAX_PULSES: u8 = 15;
/// A normal pulse: on, and off between pulses.
pub const PULSE_MS: u32 = 100;
/// The dark time after a normal burst (includes the last pulse's off time).
pub const PAUSE_MS: u32 = 300;
/// A slow ("failed") pulse: on, and off between pulses.
pub const SLOW_PULSE_MS: u32 = 400;
/// The dark time after a slow burst.
pub const SLOW_PAUSE_MS: u32 = 800;
/// Dark before the start marker's long pulse (the LED is already on from the start of `kmain`).
pub const MARKER_LEAD_MS: u32 = 500;
/// The start marker's long pulse.
pub const MARKER_ON_MS: u32 = 1000;
/// Dark after the start marker.
pub const MARKER_TRAIL_MS: u32 = 500;

/// A boot milestone, numbered in the order the kernel reaches them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// RAM taken from the device tree, page tables and allocator up.
    Memory = 1,
    /// The VideoCore mailbox answered a message.
    Mailbox = 2,
    /// The framebuffer was allocated (failed: no screen).
    Framebuffer = 3,
    /// Interrupt controller and timer initialised.
    Interrupts = 4,
    /// An SD card was found (failed: no card, or it did not answer).
    SdCard = 5,
    /// The scheduler runs and the other CPUs are started.
    Scheduler = 6,
    /// The first user process (init) was started.
    Userspace = 7,
}

impl Stage {
    pub const ALL: [Stage; 7] = [
        Stage::Memory,
        Stage::Mailbox,
        Stage::Framebuffer,
        Stage::Interrupts,
        Stage::SdCard,
        Stage::Scheduler,
        Stage::Userspace,
    ];

    /// The number of pulses.
    pub fn code(self) -> u8 {
        self as u8
    }

    /// The name used in the `led: stage N name` log lines.
    pub fn name(self) -> &'static str {
        match self {
            Stage::Memory => "memory",
            Stage::Mailbox => "mailbox",
            Stage::Framebuffer => "framebuffer",
            Stage::Interrupts => "interrupts",
            Stage::SdCard => "sdcard",
            Stage::Scheduler => "scheduler",
            Stage::Userspace => "userspace",
        }
    }
}

/// The LED level for a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Interval {
    pub on: bool,
    pub ms: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Marker,
    Pulses { n: u8, slow: bool },
}

/// The intervals of one marker or one stage's burst.
#[derive(Clone, Copy, Debug)]
pub struct Burst {
    kind: Kind,
    step: u8,
}

impl Burst {
    /// The start marker.
    pub fn marker() -> Burst {
        Burst {
            kind: Kind::Marker,
            step: 0,
        }
    }

    /// `n` pulses (cut to [`MAX_PULSES`]); `slow` is the "failed" rhythm.
    pub fn pulses(n: u8, slow: bool) -> Burst {
        Burst {
            kind: Kind::Pulses {
                n: n.min(MAX_PULSES),
                slow,
            },
            step: 0,
        }
    }

    /// The burst for a milestone that went well (`ok`) or did not.
    pub fn stage(stage: Stage, ok: bool) -> Burst {
        Burst::pulses(stage.code(), !ok)
    }

    /// How long the whole burst takes, in milliseconds.
    pub fn total_ms(self) -> u32 {
        Burst { step: 0, ..self }.map(|i| i.ms).sum()
    }
}

impl Iterator for Burst {
    type Item = Interval;

    fn next(&mut self) -> Option<Interval> {
        let step = self.step;
        let iv = match self.kind {
            Kind::Marker => match step {
                0 => Interval {
                    on: false,
                    ms: MARKER_LEAD_MS,
                },
                1 => Interval {
                    on: true,
                    ms: MARKER_ON_MS,
                },
                2 => Interval {
                    on: false,
                    ms: MARKER_TRAIL_MS,
                },
                _ => return None,
            },
            Kind::Pulses { n, slow } => {
                let (pulse, pause) = if slow {
                    (SLOW_PULSE_MS, SLOW_PAUSE_MS)
                } else {
                    (PULSE_MS, PAUSE_MS)
                };
                // Steps 0, 2, 4...: pulse k is on; steps 1, 3, 5...: its off time (the pause after the last).
                if u16::from(step) >= 2 * u16::from(n) {
                    return None;
                }
                if step % 2 == 0 {
                    Interval {
                        on: true,
                        ms: pulse,
                    }
                } else if u16::from(step) + 1 == 2 * u16::from(n) {
                    Interval {
                        on: false,
                        ms: pause,
                    }
                } else {
                    Interval {
                        on: false,
                        ms: pulse,
                    }
                }
            }
        };
        self.step = step + 1;
        Some(iv)
    }
}

/// A whole boot's worth: the marker, then a burst per `(stage, ok)`.
pub fn sequence(stages: &[(Stage, bool)]) -> impl Iterator<Item = Interval> + '_ {
    Burst::marker().chain(stages.iter().flat_map(|&(s, ok)| Burst::stage(s, ok)))
}

/// The option `milk.ledcodes`: on unless the value says `0`, `off`, `no` or `false`. `None` (not on the
/// command line) means on.
pub fn enabled(value: Option<&[u8]>) -> bool {
    match value {
        None => true,
        Some(v) => {
            let v = v.trim_ascii();
            !(v == b"0"
                || v.eq_ignore_ascii_case(b"off")
                || v.eq_ignore_ascii_case(b"no")
                || v.eq_ignore_ascii_case(b"false"))
        }
    }
}
