//! Frames (`<iframe>`) in the Browser: the rules, pure and host-tested. No I/O and no clock;
//! the Browser (`user/browser`) fetches and draws, and asks these modules what to do.
//!
//! * [`load`]: what a frame's address means, the depth and count limits, mixed content, the
//!   cookie context and `Referer` of the request, and the verdict on the answer.
//! * [`ancestors`]: `X-Frame-Options` and CSP `frame-ancestors` (who may frame a page), and
//!   the CSP `sandbox` directive.
//! * [`sandbox`]: the `sandbox` attribute's flag set, and what the Browser enforces of it.
//! * [`nav`]: `target=` (`_self`, `_parent`, `_top`, `_blank`, a frame name) and who may
//!   navigate whom.
//! * [`history`]: session history with frames in it (joint session history).
//!
//! `docs/browser.md` (Frames) lists what is enforced and what is skipped.

pub mod ancestors;
pub mod history;
pub mod load;
pub mod nav;
pub mod sandbox;

#[cfg(test)]
mod tests;
