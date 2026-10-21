//! Signals. (Placeholder until the signal step: nothing is ever pending.)

pub const SIGPIPE: u32 = 13;

/// True if the current thread has a signal that should interrupt a blocking wait.
pub fn interrupted() -> bool {
    false
}

/// Raise `sig` on the calling process.
pub fn send_current(_sig: u32) {}
