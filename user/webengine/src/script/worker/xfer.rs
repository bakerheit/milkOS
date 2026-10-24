//! Messages between a page and its worker: the structured clone of `postmsg.rs` plus a
//! transfer list that may hold message ports and `ArrayBuffer`s.
//!
//! Sending ([`encode_post`]): the transfer list is checked (ports the sender owns and has not
//! sent on, buffers that are not detached, nothing twice), the value is cloned (a view of a
//! buffer in the list is written as a view of it), and only then does anything change: the
//! ports leave the sender's hands for the link, the buffers are detached and their bytes
//! move into the message. Receiving ([`decode_post`]): the ports are imported into the
//! receiver's table and adopted by its realm, the buffers become new `ArrayBuffer`s, and the
//! value is rebuilt.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use jsengine::{JsResult, Realm, Value};

use crate::api::{WireMsg, Xfer};
use crate::script::bridge::{Bridge, arg, dom_error};
use crate::script::frames::MAX_MESSAGE_BYTES;
use crate::script::{ports, postmsg};

/// All the bytes of the buffers one message may give away.
pub const MAX_BUFFER_BYTES: usize = 64 << 20;
/// Entries in a transfer list.
const MAX_TRANSFER: usize = 1024;

/// The transfer list of `postMessage(message, transferOrOptions)` as the array of values in
/// it (empty when there is none).
fn list_values(r: &mut Realm, second: Value, who: &str) -> JsResult<Vec<Value>> {
    let mut list = second;
    if list.is_object() && !r.is_array(list) {
        // { transfer: [...] }
        let has_iter = {
            let it = r.well_known_symbol(jsengine::WellKnown::Iterator);
            let f = r.get(list, jsengine::Key::Value(it))?;
            r.is_callable(f)
        };
        if !has_iter {
            list = r.get(list, "transfer")?;
        }
    }
    if list.is_undefined() || list.is_null() {
        return Ok(Vec::new());
    }
    if !list.is_object() {
        return Err(crate::script::bridge::type_error(
            r,
            &alloc::format!("Failed to execute 'postMessage' on '{who}': Value must be an iterable object."),
        ));
    }
    let n = r.get(list, "length")?;
    let n = r.to_number(n)?;
    if !(0.0..=MAX_TRANSFER as f64).contains(&n) {
        return Err(dom_error(r, "DataCloneError", "The transfer list is too long."));
    }
    let mut out = Vec::new();
    for i in 0..n as u32 {
        out.push(r.get(list, i)?);
    }
    Ok(out)
}

/// A `postMessage(message, transfer)` to the other end of `link` (a page's worker, or a
/// worker's page): the message ready to send. `a` are the call's arguments.
pub fn encode_post(b: &Rc<Bridge>, r: &mut Realm, a: &[Value], link: u32, who: &str) -> JsResult<WireMsg> {
    if a.is_empty() {
        return Err(crate::script::bridge::type_error(
            r,
            &alloc::format!("Failed to execute 'postMessage' on '{who}': 1 argument required, but only 0 present."),
        ));
    }
    let items = list_values(r, arg(a, 1), who)?;
    // What is in the list: ports, and buffers.
    enum Kind {
        Port(u64),
        Buffer,
    }
    let mut kinds: Vec<Kind> = Vec::new();
    for (i, &v) in items.iter().enumerate() {
        if let Some(id) = ports::port_id(b, r, v) {
            kinds.push(Kind::Port(id));
        } else if v.is_object() && r.is_array_buffer(v) {
            if r.buffer_bytes(v).is_none() {
                return Err(dom_error(r, "DataCloneError", "An ArrayBuffer is detached and could not be cloned."));
            }
            kinds.push(Kind::Buffer);
        } else {
            return Err(dom_error(
                r,
                "DataCloneError",
                &alloc::format!("Failed to execute 'postMessage' on '{who}': Value at index {i} does not have a transferable type."),
            ));
        }
        for &w in &items[..i] {
            if r.strict_equals(w, v) {
                return Err(dom_error(
                    r,
                    "DataCloneError",
                    &alloc::format!("Failed to execute 'postMessage' on '{who}': Transferable at index {i} is a duplicate."),
                ));
            }
        }
    }
    let total: usize = items
        .iter()
        .zip(&kinds)
        .filter(|(_, k)| matches!(k, Kind::Buffer))
        .map(|(v, _)| r.buffer_bytes(*v).map_or(0, |x| x.len()))
        .sum();
    if total > MAX_BUFFER_BYTES {
        return Err(dom_error(r, "DataCloneError", "The buffers in the transfer list are too big to send."));
    }
    // The clone (nothing changes if it throws).
    let list = r.new_array(&items);
    let data = postmsg::encode_with(b, r, arg(a, 0), Some(list))?;
    if data.len() > MAX_MESSAGE_BYTES {
        return Err(dom_error(
            r,
            "DataCloneError",
            &alloc::format!("Failed to execute 'postMessage' on '{who}': the message is too large."),
        ));
    }
    // Ports leave the sender's hands.
    let port_ids: Vec<u64> = kinds.iter().filter_map(|k| if let Kind::Port(id) = k { Some(*id) } else { None }).collect();
    let table = b.ports();
    if !port_ids.is_empty() {
        if table.transfer_out(b.port_owner(), &port_ids, None).is_err() {
            return Err(dom_error(
                r,
                "DataCloneError",
                &alloc::format!("Failed to execute 'postMessage' on '{who}': a port in the transfer list cannot be transferred (it is closed, already transferred, came from the other thread, or is listed twice)."),
            ));
        }
        table.export(&port_ids, link);
    }
    // Buffers are given away.
    let mut xfer = Vec::new();
    for (v, k) in items.iter().zip(&kinds) {
        match k {
            Kind::Port(id) => xfer.push(Xfer::Port(*id)),
            Kind::Buffer => {
                let bytes = r.take_buffer_bytes(*v).unwrap_or_default();
                xfer.push(Xfer::Buffer(bytes));
            }
        }
    }
    Ok(WireMsg { data, xfer })
}

/// A message that came over `link`: the value it stands for, and the ports that came with it
/// (adopted by this realm). The buffers are new `ArrayBuffer`s.
pub fn decode_post(b: &Rc<Bridge>, r: &mut Realm, msg: &WireMsg, link: u32) -> JsResult<(Value, Value)> {
    let table = b.ports();
    // Import the ports first (so a message that carries one it cannot use still closes it).
    let mut port_ids: Vec<u64> = Vec::new();
    for x in &msg.xfer {
        if let Xfer::Port(id) = x
            && table.import(*id, link)
        {
            port_ids.push(*id);
        }
    }
    let adopted = ports::adopt_list(b, r, &port_ids)?;
    // The combined list the decoder indexes: ports and buffers, in the sender's order.
    let mut items: Vec<Value> = Vec::new();
    let mut next_port = 0u32;
    for x in &msg.xfer {
        match x {
            Xfer::Port(id) => {
                if port_ids.contains(id) {
                    items.push(r.get(adopted, next_port)?);
                    next_port += 1;
                } else {
                    items.push(Value::UNDEFINED);
                }
            }
            Xfer::Buffer(bytes) => {
                let v = r.new_array_buffer_from_vec(bytes.clone())?;
                items.push(v);
            }
        }
    }
    let xl = r.new_array(&items);
    let data = if msg.data.is_empty() {
        Value::UNDEFINED
    } else {
        postmsg::decode(b, r, &msg.data, Some(xl))?
    };
    Ok((data, adopted))
}

/// Give the ports a message carried back to the table's care when the message cannot be
/// delivered (the receiver is gone): they are closed.
pub fn drop_msg(b: &Bridge, msg: &WireMsg) {
    let ids: Vec<u64> = msg
        .xfer
        .iter()
        .filter_map(|x| if let Xfer::Port(id) = x { Some(*id) } else { None })
        .collect();
    if !ids.is_empty() {
        b.ports().close_all(&ids);
    }
}

#[allow(dead_code)]
fn _unused(_: String) {}
