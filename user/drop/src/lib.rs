//! milkdrop: what Milk Drop (`drop`) and the remote shell (`mssh`, `msshd`, `mscp`) share:
//! the machine identity and trust store ([`ident`]), the wire pieces ([`proto`]: the
//! encrypted record channel, discovery), and the authenticated key agreement both run
//! over TCP ([`session`]). See docs/networking.md.

#![no_std]

extern crate alloc;

pub mod ident;
pub mod proto;
pub mod session;

/// Write an audit record through /dev/audit (the kernel records it as `user.<event>`).
pub fn audit(event: &str, detail: &str) {
    use rt::sys;
    if let Ok(fd) = sys::open("/dev/audit", sys::O_WRONLY | sys::O_CLOEXEC) {
        let _ = sys::write_all(fd, alloc::format!("{event} {detail}\n").as_bytes());
        let _ = sys::close(fd);
    }
}

/// An address as text (IPv4 dotted, IPv6 RFC 5952).
pub fn ip_str(ip: impl Into<rt::net::Ip>) -> alloc::string::String {
    alloc::format!("{}", ip.into())
}
