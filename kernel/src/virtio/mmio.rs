//! virtio-mmio transport, modern (version 2) register layout only (virtio 1.x spec, 4.2.2).
//!
//! qemu `virt` has 32 transport slots at 0x0a00_0000 + 0x200 * n, slot n raising SPI 16 + n
//! (checked against the DTB qemu 10.2 generates: `virtio_mmio@a000000`, `interrupts = <0 16 1>`,
//! up to `virtio_mmio@a003e00`, `<0 47 1>`). Empty slots read device ID 0. qemu fills slots
//! from the top down, so the first `-device virtio-*-device` lands in slot 31.
//!
//! qemu defaults virtio-mmio to the legacy (version 1) interface; run it with
//! `-global virtio-mmio.force-legacy=false` to get version 2. [`Transport::probe`] rejects
//! legacy devices with a message saying exactly that.

// Public API surface for later drivers (block, net, gpu); not all of it is used yet.
#![allow(dead_code)]

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};

use super::queue::Virtqueue;
use crate::gic;

pub const MMIO_BASE: usize = 0x0a00_0000;
pub const MMIO_STRIDE: usize = 0x200;
pub const MMIO_SLOTS: usize = 32;
/// "SPI n" of slot 0; slot `i` is SPI `MMIO_SPI_BASE + i`.
pub const MMIO_SPI_BASE: u32 = 16;

const MAGIC: u32 = 0x7472_6976; // "virt"

// Register offsets (virtio 1.x, 4.2.2).
const MAGIC_VALUE: usize = 0x000;
const VERSION: usize = 0x004;
const DEVICE_ID: usize = 0x008;
const VENDOR_ID: usize = 0x00c;
const DEVICE_FEATURES: usize = 0x010;
const DEVICE_FEATURES_SEL: usize = 0x014;
const DRIVER_FEATURES: usize = 0x020;
const DRIVER_FEATURES_SEL: usize = 0x024;
const QUEUE_SEL: usize = 0x030;
const QUEUE_NUM_MAX: usize = 0x034;
const QUEUE_NUM: usize = 0x038;
const QUEUE_READY: usize = 0x044;
const QUEUE_NOTIFY: usize = 0x050;
const INTERRUPT_STATUS: usize = 0x060;
const INTERRUPT_ACK: usize = 0x064;
const STATUS: usize = 0x070;
const QUEUE_DESC_LOW: usize = 0x080;
const QUEUE_DESC_HIGH: usize = 0x084;
const QUEUE_DRIVER_LOW: usize = 0x090;
const QUEUE_DRIVER_HIGH: usize = 0x094;
const QUEUE_DEVICE_LOW: usize = 0x0a0;
const QUEUE_DEVICE_HIGH: usize = 0x0a4;
const CONFIG_GENERATION: usize = 0x0fc;
const CONFIG: usize = 0x100;

// Device status bits (2.1).
pub const STATUS_ACKNOWLEDGE: u32 = 1;
pub const STATUS_DRIVER: u32 = 2;
pub const STATUS_DRIVER_OK: u32 = 4;
pub const STATUS_FEATURES_OK: u32 = 8;
pub const STATUS_NEEDS_RESET: u32 = 64;
pub const STATUS_FAILED: u32 = 128;

/// Feature bit 32: the device speaks virtio 1.x (required for the modern interface).
pub const F_VERSION_1: u64 = 1 << 32;

/// InterruptStatus bits.
pub const INT_USED_RING: u32 = 1;
pub const INT_CONFIG_CHANGE: u32 = 2;

/// Device IDs (5).
pub mod device_id {
    pub const NET: u32 = 1;
    pub const BLOCK: u32 = 2;
    pub const CONSOLE: u32 = 3;
    pub const RNG: u32 = 4;
    pub const GPU: u32 = 16;
    pub const P9: u32 = 9;
    pub const INPUT: u32 = 18;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Slot is empty (device ID 0) or has no virtio magic.
    NoDevice,
    /// The transport is the legacy (version 1) interface.
    Legacy,
    /// The device lacks a feature we require.
    MissingFeatures(u64),
    /// The device refused our feature set (FEATURES_OK did not stick).
    FeaturesRejected,
    /// The queue does not exist, is already live, or is too small.
    BadQueue,
}

/// Full barrier for ordering ring memory against MMIO. `dsb sy` rather than `dmb`: the
/// QueueNotify store must not overtake the ring stores, and once `mem` lands the rings are
/// Normal memory while the registers stay Device memory.
#[inline]
pub fn mb() {
    unsafe { asm!("dsb sy", options(nostack, preserves_flags)) };
}

/// One virtio-mmio transport slot.
#[derive(Debug, Clone, Copy)]
pub struct Transport {
    base: usize,
    slot: usize,
    device_id: u32,
}

impl Transport {
    /// Look at slot `slot`. Ok only for a present, modern (version 2) device.
    pub fn probe(slot: usize) -> Result<Transport, Error> {
        if slot >= MMIO_SLOTS {
            return Err(Error::NoDevice);
        }
        let t = Transport {
            base: MMIO_BASE + slot * MMIO_STRIDE,
            slot,
            device_id: 0,
        };
        if t.read(MAGIC_VALUE) != MAGIC {
            return Err(Error::NoDevice);
        }
        let id = t.read(DEVICE_ID);
        if id == 0 {
            return Err(Error::NoDevice);
        }
        if t.read(VERSION) != 2 {
            return Err(Error::Legacy);
        }
        Ok(Transport { device_id: id, ..t })
    }

    pub fn slot(&self) -> usize {
        self.slot
    }
    pub fn base(&self) -> usize {
        self.base
    }
    pub fn device_id(&self) -> u32 {
        self.device_id
    }
    pub fn vendor_id(&self) -> u32 {
        self.read(VENDOR_ID)
    }
    /// GIC INTID of this slot's interrupt.
    pub fn irq(&self) -> u32 {
        gic::spi(MMIO_SPI_BASE + self.slot as u32)
    }

    #[inline]
    fn read(&self, off: usize) -> u32 {
        unsafe { read_volatile((self.base + off) as *const u32) }
    }
    #[inline]
    fn write(&self, off: usize, v: u32) {
        unsafe { write_volatile((self.base + off) as *mut u32, v) }
    }

    pub fn status(&self) -> u32 {
        self.read(STATUS)
    }
    pub fn set_status(&self, s: u32) {
        self.write(STATUS, s);
    }
    fn add_status(&self, bits: u32) {
        self.set_status(self.status() | bits);
    }

    /// Reset the device and wait for the reset to finish (status reads back 0).
    pub fn reset(&self) {
        self.set_status(0);
        while self.status() != 0 {
            core::hint::spin_loop();
        }
    }

    /// Mark the device as failed; it stays dead until the next reset.
    pub fn fail(&self) {
        self.add_status(STATUS_FAILED);
    }

    pub fn device_features(&self) -> u64 {
        self.write(DEVICE_FEATURES_SEL, 0);
        let lo = self.read(DEVICE_FEATURES) as u64;
        self.write(DEVICE_FEATURES_SEL, 1);
        let hi = self.read(DEVICE_FEATURES) as u64;
        hi << 32 | lo
    }

    fn set_driver_features(&self, f: u64) {
        self.write(DRIVER_FEATURES_SEL, 0);
        self.write(DRIVER_FEATURES, f as u32);
        self.write(DRIVER_FEATURES_SEL, 1);
        self.write(DRIVER_FEATURES, (f >> 32) as u32);
    }

    /// Steps 1-6 of device initialisation (3.1.1): reset, ACKNOWLEDGE, DRIVER, negotiate
    /// features, FEATURES_OK. We accept `required | (offered & wanted)`; [`F_VERSION_1`] is
    /// always required. Returns the negotiated set. On error the device is marked FAILED.
    /// Next: set up queues with [`setup_queue`](Self::setup_queue), then
    /// [`driver_ok`](Self::driver_ok).
    pub fn begin_init(&self, required: u64, wanted: u64) -> Result<u64, Error> {
        self.reset();
        self.add_status(STATUS_ACKNOWLEDGE);
        self.add_status(STATUS_DRIVER);
        let offered = self.device_features();
        let required = required | F_VERSION_1;
        if offered & required != required {
            self.fail();
            return Err(Error::MissingFeatures(required & !offered));
        }
        let features = required | (offered & wanted);
        self.set_driver_features(features);
        self.add_status(STATUS_FEATURES_OK);
        if self.status() & STATUS_FEATURES_OK == 0 {
            self.fail();
            return Err(Error::FeaturesRejected);
        }
        Ok(features)
    }

    /// Largest queue size the device supports for queue `idx` (0 = queue does not exist).
    pub fn queue_num_max(&self, idx: u16) -> u16 {
        self.write(QUEUE_SEL, idx as u32);
        self.read(QUEUE_NUM_MAX).min(u16::MAX as u32) as u16
    }

    /// Tell the device where virtqueue `idx` lives and enable it. `vq.size()` must not
    /// exceed [`queue_num_max`](Self::queue_num_max).
    pub fn setup_queue(&self, idx: u16, vq: &Virtqueue) -> Result<(), Error> {
        self.write(QUEUE_SEL, idx as u32);
        if self.read(QUEUE_READY) != 0 {
            return Err(Error::BadQueue);
        }
        let max = self.read(QUEUE_NUM_MAX);
        if max == 0 || vq.size() as u32 > max {
            return Err(Error::BadQueue);
        }
        self.write(QUEUE_NUM, vq.size() as u32);
        let (desc, avail, used) = vq.addresses();
        self.write(QUEUE_DESC_LOW, desc as u32);
        self.write(QUEUE_DESC_HIGH, (desc >> 32) as u32);
        self.write(QUEUE_DRIVER_LOW, avail as u32);
        self.write(QUEUE_DRIVER_HIGH, (avail >> 32) as u32);
        self.write(QUEUE_DEVICE_LOW, used as u32);
        self.write(QUEUE_DEVICE_HIGH, (used >> 32) as u32);
        mb();
        self.write(QUEUE_READY, 1);
        Ok(())
    }

    /// Final init step: the driver is live. Buffers may be (and usually are) queued before.
    pub fn driver_ok(&self) {
        mb();
        self.add_status(STATUS_DRIVER_OK);
    }

    /// Kick queue `idx` (the device has new available buffers).
    pub fn notify(&self, idx: u16) {
        mb();
        self.write(QUEUE_NOTIFY, idx as u32);
    }

    /// Read and acknowledge the interrupt cause ([`INT_USED_RING`], [`INT_CONFIG_CHANGE`]).
    /// Ack before draining the used ring, so a buffer used meanwhile raises a fresh IRQ.
    pub fn ack_interrupt(&self) -> u32 {
        let s = self.read(INTERRUPT_STATUS);
        if s != 0 {
            self.write(INTERRUPT_ACK, s);
        }
        s
    }

    pub fn config_generation(&self) -> u32 {
        self.read(CONFIG_GENERATION)
    }

    /// Byte `off` of the device-specific config space.
    pub fn config_read8(&self, off: usize) -> u8 {
        unsafe { read_volatile((self.base + CONFIG + off) as *const u8) }
    }
    pub fn config_write8(&self, off: usize, v: u8) {
        unsafe { write_volatile((self.base + CONFIG + off) as *mut u8, v) }
    }
    /// Little-endian u32 at `off` (must be 4-aligned) of the config space.
    pub fn config_read32(&self, off: usize) -> u32 {
        debug_assert!(off % 4 == 0);
        unsafe { read_volatile((self.base + CONFIG + off) as *const u32) }
    }
}
