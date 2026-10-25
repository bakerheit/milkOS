//! The BCM2835/BCM2711 power manager's watchdog (the "PM" block): the register values, and the rule for when to kick it,
//! as pure functions and a small state machine the host tests drive with a fake clock. `kernel/src/rpi/watchdog.rs`
//! does the register writes and the timer tick.
//!
//! The hardware: WDOG holds a timeout in 1/65536 s ticks (20 bits, so at most 16 s); writing it starts or restarts
//! the countdown once RSTC has been written with the full-reset configuration; at zero the SoC resets as if
//! powered up again (the firmware boots the kernel again). Every write needs the password 0x5a in the top byte.
//! Writing RSTC with the "reset" value (0x102) stops it.

/// Ticks per second.
pub const TICKS_PER_SEC: u64 = 65536;
/// The largest timeout WDOG holds.
pub const WDOG_TIME_MASK: u32 = 0x000f_ffff;
pub const PASSWORD: u32 = 0x5a00_0000;
pub const RSTC_WRCFG_CLR: u32 = 0xffff_ffcf;
pub const RSTC_WRCFG_FULL_RESET: u32 = 0x20;
/// RSTC value that stops the countdown (what Linux's driver writes to stop the watchdog).
pub const RSTC_STOP: u32 = PASSWORD | 0x102;

/// The WDOG register value for a timeout of `ms` (at least one tick, at most what the register holds).
pub const fn wdog_value(ms: u64) -> u32 {
    let ticks = ms * TICKS_PER_SEC / 1000;
    let ticks = if ticks == 0 { 1 } else { ticks };
    let ticks = if ticks > WDOG_TIME_MASK as u64 { WDOG_TIME_MASK as u64 } else { ticks };
    PASSWORD | ticks as u32
}

/// The RSTC value that makes the countdown end in a full reset, from the register's current value.
pub const fn rstc_full_reset(current: u32) -> u32 {
    (current & RSTC_WRCFG_CLR) | PASSWORD | RSTC_WRCFG_FULL_RESET
}

/// The longest timeout the register can hold, in ms.
pub const MAX_TIMEOUT_MS: u64 = WDOG_TIME_MASK as u64 * 1000 / TICKS_PER_SEC;

/// `milk.watchdog=`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Setting {
    /// Never armed.
    Off,
    /// Armed on the hardware once the boot is over, if the board has the block (the device tree lists it).
    On,
    /// The same rules with no register at all: a CPU that finds the deadline missed resets the board, for qemu, whose
    /// power manager resets at once when RSTC is written instead of counting down.
    Sim,
}

impl Setting {
    /// From the option's value (None: absent, which is `On`). Unknown words are `Err` with the setting used instead.
    pub fn parse(v: Option<&[u8]>) -> Result<Setting, Setting> {
        match v {
            None | Some(b"") | Some(b"on") | Some(b"1") => Ok(Setting::On),
            Some(b"off") | Some(b"0") => Ok(Setting::Off),
            Some(b"sim") => Ok(Setting::Sim),
            Some(_) => Err(Setting::On),
        }
    }
}

/// What the caller has to do with the hardware now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Nothing,
    /// Write WDOG (restart the countdown).
    Kick,
    /// Write WDOG, then RSTC with the full reset configuration (start the countdown).
    Arm,
    /// Write RSTC with [`RSTC_STOP`].
    Stop,
}

/// The rules: armed after the boot, kicked at most every `kick_every_ms`, stopped before a power-off or a crash dump.
#[derive(Clone, Copy, Debug)]
pub struct Watchdog {
    timeout_ms: u64,
    kick_every_ms: u64,
    armed: bool,
    last_kick_ms: u64,
    /// The longest time between two kicks (or the arm and the first kick) seen so far.
    max_gap_ms: u64,
    kicks: u64,
}

impl Watchdog {
    pub const fn new(timeout_ms: u64, kick_every_ms: u64) -> Self {
        Self { timeout_ms, kick_every_ms, armed: false, last_kick_ms: 0, max_gap_ms: 0, kicks: 0 }
    }

    pub const fn armed(&self) -> bool {
        self.armed
    }

    pub const fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }

    pub const fn kicks(&self) -> u64 {
        self.kicks
    }

    pub const fn max_gap_ms(&self) -> u64 {
        self.max_gap_ms
    }

    /// The boot is over: start the countdown. Arming twice does nothing the second time.
    pub fn arm(&mut self, now_ms: u64) -> Action {
        if self.armed {
            return Action::Nothing;
        }
        self.armed = true;
        self.last_kick_ms = now_ms;
        Action::Arm
    }

    /// The timer tick (any rate): kick if armed and `kick_every_ms` has passed since the last kick.
    pub fn tick(&mut self, now_ms: u64) -> Action {
        if !self.armed || now_ms.saturating_sub(self.last_kick_ms) < self.kick_every_ms {
            return Action::Nothing;
        }
        let gap = now_ms - self.last_kick_ms;
        self.max_gap_ms = self.max_gap_ms.max(gap);
        self.last_kick_ms = now_ms;
        self.kicks += 1;
        Action::Kick
    }

    /// Power off, restart or a crash dump: stop it (so the board does not reset under the person reading the screen).
    pub fn disarm(&mut self) -> Action {
        if !self.armed {
            return Action::Nothing;
        }
        self.armed = false;
        Action::Stop
    }

    /// Would the hardware have reset the board by `now_ms`? (For the simulated one: the missed deadline.)
    pub const fn expired(&self, now_ms: u64) -> bool {
        self.armed && now_ms.saturating_sub(self.last_kick_ms) >= self.timeout_ms
    }
}
