//! The network interface behind a link: a virtio-net device (qemu virt) or the Raspberry Pi 4's
//! GENET Ethernet MAC (`rpi/genet.rs`). The stack calls these methods and does not care which.
//! An enum, not a trait object: `send_with` takes a closure by value.

use crate::virtio::net::VirtioNet;

/// Interface counters, the columns of `/proc/net/dev` we can fill.
#[derive(Clone, Copy, Default, Debug)]
pub struct DevStats {
    pub rx_bytes: u64,
    pub rx_packets: u64,
    pub rx_errors: u64,
    pub rx_dropped: u64,
    pub tx_bytes: u64,
    pub tx_packets: u64,
    pub tx_errors: u64,
    pub tx_dropped: u64,
}

/// A received frame, still sitting in its rx buffer. Hand it back with
/// [`VirtioNet::rx_done`] once processed; the slice stays valid until then.
pub struct RxBuf {
    pub(crate) head: u16,
    ptr: *const u8,
    len: usize,
}

impl RxBuf {
    /// A received frame of `len` bytes at `ptr`, in the device's receive slot `head`.
    pub fn new(head: u16, ptr: *const u8, len: usize) -> RxBuf {
        RxBuf { head, ptr, len }
    }

    /// The Ethernet frame (virtio header stripped).
    ///
    /// The returned slice is not tied to a borrow of the device, so the stack can keep
    /// using (and transmitting through) the device while it parses the frame.
    pub fn frame<'a>(&self) -> &'a [u8] {
        // SAFETY: the buffer belongs to a descriptor the device has returned and that is not
        // re-offered until `rx_done(self)`, which consumes `self`; the stack does not keep
        // the slice past that call (it processes each frame to completion first).
        unsafe { core::slice::from_raw_parts(self.ptr, self.len) }
    }
}

pub enum Nic {
    Virtio(VirtioNet),
    #[cfg(feature = "raspi")]
    Genet(crate::rpi::genet::Genet),
}

impl Nic {
    pub fn mac(&self) -> [u8; 6] {
        match self {
            Nic::Virtio(d) => d.mac(),
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.mac(),
        }
    }

    pub fn link_up(&self) -> bool {
        match self {
            Nic::Virtio(d) => d.link_up(),
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.link_up(),
        }
    }

    pub fn rx_pop(&mut self) -> Option<RxBuf> {
        match self {
            Nic::Virtio(d) => d.rx_pop(),
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.rx_pop(),
        }
    }

    pub fn rx_done(&mut self, b: RxBuf) {
        match self {
            Nic::Virtio(d) => d.rx_done(b),
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.rx_done(b),
        }
    }

    pub fn rx_kick(&mut self) {
        match self {
            Nic::Virtio(d) => d.rx_kick(),
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.rx_kick(),
        }
    }

    pub fn tx_reclaim(&mut self) {
        match self {
            Nic::Virtio(d) => d.tx_reclaim(),
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.tx_reclaim(),
        }
    }

    pub fn send_with(&mut self, fill: impl FnOnce(&mut [u8]) -> usize) -> bool {
        match self {
            Nic::Virtio(d) => d.send_with(fill),
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.send_with(fill),
        }
    }

    pub fn stats(&self) -> DevStats {
        match self {
            Nic::Virtio(d) => d.stats,
            #[cfg(feature = "raspi")]
            Nic::Genet(d) => d.stats,
        }
    }
}

/// Set once any NIC is attached.
static PRESENT: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

pub fn set_present() {
    PRESENT.store(true, core::sync::atomic::Ordering::Release);
}

pub fn present() -> bool {
    PRESENT.load(core::sync::atomic::Ordering::Acquire)
}
