//! Styling: specified declarations ([`props`]), the cascade and computed values
//! ([`cascade`]), the computed-style types ([`types`]) and the UA stylesheet (`ua.css`).

pub mod cascade;
pub mod props;
pub mod types;

/// The user-agent stylesheet (WHATWG HTML §15).
pub const UA_CSS: &str = include_str!("ua.css");
