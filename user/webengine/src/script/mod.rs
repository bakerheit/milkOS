//! Scripts in pages: the bindings between the DOM and the JavaScript engine (user/jsengine),
//! and the engine-independent parts they use: which scripts run when ([`loader`]), event
//! listeners and propagation ([`events`]), timers and animation frames ([`timers`]).

pub mod events;
pub mod loader;
pub mod timers;
