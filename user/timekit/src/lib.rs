//! Time logic shared by Calendar, Clock and `alarmd`. Nothing here touches the system: callers
//! pass the current time in, so every function can be tested on the host.
//!
//! Time on milkOS is the RTC, which holds local time. All "seconds" below are local seconds
//! since 1970-01-01 00:00 (no time zones, no daylight saving).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod cal;
pub mod clock;
pub mod date;
pub mod reminders;
pub mod text;

pub use date::Date;
