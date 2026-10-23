//! milkweb: what milkOS needs to fetch and show a web page, without any I/O of its own.
//!
//! - [`url`]: `http://` / `https://` addresses, joining links against a page.
//! - [`http`]: an HTTP/1.1 GET client over a [`http::Transport`] (a byte stream) opened by a
//!   [`http::Net`] (name lookup + connect, and the `https://` hook for TLS): chunked and
//!   Content-Length bodies, redirects with a loop guard, gzip / deflate, time and size limits.
//! - [`gzip`]: gzip (RFC 1952) and HTTP's "deflate" over user/imgdec's inflate.
//! - [`dns`]: DNS messages (A, AAAA, CNAME, compression, truncation, TTLs), a small TTL cache
//!   and the resolver's logic (UDP, then TCP for truncated answers; CNAME chains) over a
//!   [`dns::Host`] that does the sending.
//! - [`html`]: the Browser's HTML subset (blocks and styled runs), entities, charsets.
//! - [`search`]: the search engines the Browser's address bar can use, and their addresses.
//! - [`secure`]: `https://` over the `tls` crate (TLS 1.3), for a [`http::Net::secure`].
//!
//! Used by the kernel (the Browser app, `net::dns`) and user space (`rt::web`, `network`).

#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod dns;
pub mod gzip;
pub mod html;
pub mod http;
pub mod search;
pub mod secure;
pub mod url;

// The DEFLATE decoder Preview's PNG reader uses, shared as it is.
#[allow(dead_code)]
#[path = "../../user/imgdec/src/inflate.rs"]
pub mod inflate;

#[cfg(test)]
mod tests;
