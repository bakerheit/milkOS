//! Who is using this session. One place for the user's name, so nothing personal is
//! hardcoded anywhere in the window system. Today every session is the built-in "milk"
//! user; when real accounts land (/System/Settings/passwd, milk is uid 501) this returns the logged-in
//! user's name instead and every caller follows.

/// The session user's display name (Terminal titles and prompt, `users me`, the lock screen,
/// the Today greeting, the drawer's Mentions chip).
pub fn user_name() -> &'static str {
    "milk"
}
