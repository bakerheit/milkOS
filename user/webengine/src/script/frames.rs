//! Scripts and frames: what the bindings of one window need to know about the frame tree, the
//! `message` event, and the queue of messages a window posted. The engine side only; the
//! objects scripts see (`window.parent`, `contentWindow`, ...) are built on this by the
//! bindings.

use alloc::string::String;
use alloc::vec::Vec;

use crate::api::*;

/// The `message` event for `msg`, fired at the window of `d`.
pub fn deliver_message(d: &mut Document, host: &mut dyn ScriptHost, msg: &Message) -> bool {
    super::postmsg::deliver(d, host, msg)
}

/// Messages `window.postMessage` queued, for the host.
#[derive(Default)]
pub struct Outbox {
    pub q: Vec<OutMessage>,
}

#[allow(dead_code)]
pub fn unused(_: String) {}
