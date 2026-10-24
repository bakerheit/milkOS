//! User-space drivers (docs/drivers.md): a root process with the `driver` capability can
//! claim a virtio device that has a hand-off-capable kernel driver (rng, sound). It then
//! drives the device itself through `MILK_DRIVER` (0x1010): register access bounded to the
//! device's 0x200-byte virtio-mmio window, DMA buffers it gets the physical addresses of
//! (and only those), and its interrupt as a readable/pollable fd. The kernel masks the
//! interrupt line each time it fires; the driver re-arms it.
//!
//! A buggy driver can't take the kernel down: queue addresses are checked against its DMA
//! buffers when it sets QueueReady, every in-use descriptor is checked when it rings
//! QueueNotify (no indirect descriptors), and when it dies (or execs, or closes the fd) the
//! kernel resets the device before freeing anything and hands it back to the kernel
//! driver. Not a defence against a malicious driver: it can rewrite a descriptor after the
//! check and, with no IOMMU, the device would DMA wherever it says.
//!
//! Locks: `DEVS` is never held while taking a process's `inner` lock (the DMA mapping is
//! done after it is dropped) and never from IRQ context; the interrupt handler only uses
//! the atomics in `IRQ_OF` / `PENDING`.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use crate::exceptions::TrapFrame;
use crate::fs::errno::{Errno, Result as FsResult};
use crate::fs::node::{Inode, InodeType, Stat, alloc_dev};
use crate::mmu::{PAGE_SIZE, PhysAddr, phys_to_virt};
use crate::proc::errno::*;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;
use crate::virtio::mmio::{self, Transport};
use crate::{gic, kprintln, pmm};

/// Which kind of device (and which `opt/milkos/drivers` key).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Class {
    Rng,
    Sound,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Rng => "rng",
            Class::Sound => "sound",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Policy {
    /// Only the kernel driver; claims are refused.
    Kernel,
    /// A user driver may claim the device; the kernel driver runs when none does.
    User,
}

/// What the kernel driver of a hand-off-capable device provides.
#[derive(Clone, Copy)]
pub struct Hooks {
    /// Let go of the device: stop it, reset it, unregister the interrupt, free memory.
    pub detach: fn(),
    /// Take the device (again) from a fresh transport.
    pub attach: fn(Transport) -> core::result::Result<(), mmio::Error>,
    /// Before a dead driver's device is reset: make the device give back what it holds,
    /// through the driver's own queues (which the kernel tracked). For devices whose
    /// reset doesn't (qemu's virtio-sound keeps a stream's buffers and completes them
    /// later, onto whatever queue is live; see `virtio::sound::quiesce`).
    pub quiesce: Option<fn(&Transport, &[QueueRegs])>,
}

/// Where a user driver put a virtqueue (as it wrote the registers), for [`Hooks::quiesce`].
#[derive(Clone, Copy, Default)]
pub struct QueueRegs {
    pub num: u32,
    pub desc: u64,
    pub avail: u64,
    pub used: u64,
}

const WINDOW: usize = 0x200;
const MAX_QUEUES: usize = 8;
/// DMA memory per claim.
const DMA_MAX_PAGES: usize = 64;

// virtio-mmio registers the kernel watches.
const QUEUE_SEL: usize = 0x030;
const QUEUE_NUM: usize = 0x038;
const QUEUE_READY: usize = 0x044;
const QUEUE_NOTIFY: usize = 0x050;
const STATUS: usize = 0x070;
const QUEUE_DESC_LOW: usize = 0x080;
const QUEUE_DESC_HIGH: usize = 0x084;
const QUEUE_DRIVER_LOW: usize = 0x090;
const QUEUE_DRIVER_HIGH: usize = 0x094;
const QUEUE_DEVICE_LOW: usize = 0x0a0;
const QUEUE_DEVICE_HIGH: usize = 0x0a4;
const DESC_F_INDIRECT: u16 = 4;

#[derive(Clone, Copy, Default)]
struct QState {
    num: u32,
    desc: u64,
    drv: u64,
    dev: u64,
}

struct Claim {
    pid: u32,
    exe: String,
    /// Bumped per claim; a device fd from an earlier claim is dead.
    generation: u64,
    dma: Vec<(PhysAddr, usize)>,
    qsel: usize,
    q: [QState; MAX_QUEUES],
    refused: u64,
}

impl Claim {
    fn in_dma(&self, pa: u64, len: u64) -> bool {
        let end = match pa.checked_add(len) {
            Some(e) => e,
            None => return false,
        };
        self.dma.iter().any(|&(base, pages)| {
            let (b, e) = (base as u64, base as u64 + (pages * PAGE_SIZE) as u64);
            pa >= b && end <= e
        })
    }

    fn dma_pages(&self) -> usize {
        self.dma.iter().map(|d| d.1).sum()
    }
}

struct Dev {
    slot: usize,
    irq: u32,
    id: u32,
    class: Class,
    hooks: Hooks,
    kernel_attached: bool,
    claim: Option<Claim>,
    claims: u64,
    releases: u64,
    last_release: String,
}

static DEVS: SpinLock<Vec<Dev>> = SpinLock::new(Vec::new());
static POLICY_RNG: AtomicBool = AtomicBool::new(true); // true = User
static POLICY_SOUND: AtomicBool = AtomicBool::new(true);
static GENERATION: AtomicU64 = AtomicU64::new(1);
/// Per virtio-mmio slot: the interrupt a user driver owns (0 = none), and how many times
/// it fired since the driver last read its fd.
static IRQ_OF: [AtomicU32; mmio::MMIO_SLOTS] = [const { AtomicU32::new(0) }; mmio::MMIO_SLOTS];
static PENDING: [AtomicU64; mmio::MMIO_SLOTS] = [const { AtomicU64::new(0) }; mmio::MMIO_SLOTS];
static IRQ_WQ: WaitQueue = WaitQueue::new();
/// DMA buffers of claims whose fd was closed while the owner was still running: still
/// mapped in its address space, freed when that space is gone ([`space_gone`]).
static LEFTOVER: SpinLock<Vec<(u32, PhysAddr, usize)>> = SpinLock::new(Vec::new());
/// Released DMA buffers waiting out [`QUARANTINE_NS`] before they are freed: a device may
/// still complete requests it had taken before the reset (qemu's virtio-sound reset
/// doesn't flush a stream's queued buffers; it finishes them later and writes their
/// status into these pages). (free after, pa, pages)
static QUARANTINE: SpinLock<Vec<(u64, PhysAddr, usize)>> = SpinLock::new(Vec::new());
const QUARANTINE_NS: u64 = 2_000_000_000;

fn quarantine(pa: PhysAddr, pages: usize) {
    let at = crate::timer::now_ns() + QUARANTINE_NS;
    QUARANTINE.lock().push((at, pa, pages));
}

/// Free quarantined buffers whose time is up. Called from every entry point (the rngd and
/// soundd syscalls come often enough).
fn sweep() {
    let now = crate::timer::now_ns();
    let due: Vec<(PhysAddr, usize)> = {
        let mut q = QUARANTINE.lock();
        let (due, keep): (Vec<_>, Vec<_>) = q.drain(..).partition(|e| e.0 <= now);
        *q = keep;
        due.into_iter().map(|e| (e.1, e.2)).collect()
    };
    for (pa, n) in due {
        pmm::free_contiguous(pa, n);
    }
}

pub fn policy(c: Class) -> Policy {
    let user = match c {
        Class::Rng => POLICY_RNG.load(Ordering::Relaxed),
        Class::Sound => POLICY_SOUND.load(Ordering::Relaxed),
    };
    if user { Policy::User } else { Policy::Kernel }
}

/// Read the boot option `opt/milkos/drivers` (qemu `-fw_cfg name=opt/milkos/drivers,
/// string=rng=kernel sound=user`; `MILK_DRIVERS` in scripts/run.sh). Before `virtio::init`.
pub fn init_policy() {
    let mut text = [0u8; 128];
    let n = crate::fwcfg::find_file("opt/milkos/drivers")
        .filter(|f| (f.size as usize) <= text.len())
        .filter(|f| crate::fwcfg::read_file(*f, &mut text[..f.size as usize]))
        .map_or(0, |f| f.size as usize);
    let s = core::str::from_utf8(&text[..n]).unwrap_or("");
    for tok in s.split_whitespace() {
        let (k, v) = tok.split_once('=').unwrap_or((tok, ""));
        let user = match v {
            "user" => true,
            "kernel" => false,
            _ => continue,
        };
        match k {
            "rng" => POLICY_RNG.store(user, Ordering::Relaxed),
            "sound" => POLICY_SOUND.store(user, Ordering::Relaxed),
            _ => {}
        }
    }
    if n > 0 {
        kprintln!("[udrv] boot option drivers: {}", s.trim());
    }
}

/// A kernel driver attached `slot` and can hand it to a user driver.
pub fn register(slot: usize, irq: u32, id: u32, class: Class, hooks: Hooks) {
    DEVS.lock().push(Dev {
        slot,
        irq,
        id,
        class,
        hooks,
        kernel_attached: true,
        claim: None,
        claims: 0,
        releases: 0,
        last_release: String::new(),
    });
}

/// Is a user driver driving this class right now?
pub fn user_owned(c: Class) -> bool {
    DEVS.lock()
        .iter()
        .any(|d| d.class == c && d.claim.is_some())
}

fn user_irq(irq: u32, _frame: &mut TrapFrame) {
    // Mask first: the line is level-triggered and stays asserted until the driver acks.
    let _ = gic::disable(irq);
    for (slot, owner) in IRQ_OF.iter().enumerate() {
        if owner.load(Ordering::Acquire) == irq {
            PENDING[slot].fetch_add(1, Ordering::AcqRel);
        }
    }
    IRQ_WQ.wake_all();
    crate::poll::notify();
}

fn transport(slot: usize) -> Option<Transport> {
    Transport::probe(slot).ok()
}

/// Reset the device and wait (briefly) for it to read back 0: that stops its DMA.
fn reset(slot: usize) {
    if let Some(t) = transport(slot) {
        t.write_raw(STATUS, 4, 0);
        let end = crate::timer::now_ns() + 100_000_000;
        while t.read_raw(STATUS, 4) != 0 && crate::timer::now_ns() < end {
            core::hint::spin_loop();
        }
    }
}

// ---- claim and release ---------------------------------------------------------------------

fn claim(id: u32) -> KResult<usize> {
    let pid = crate::sched::current_pid();
    let exe = crate::proc::get(pid)
        .map(|p| p.inner.lock().exe.clone())
        .unwrap_or_default();
    if !crate::proc::current_cred().is_root() || !crate::proc::current_caps().driver {
        crate::audit!(
            "driver.deny",
            "pid={pid} exe={exe} id={id} reason=needs-root-and-driver-cap"
        );
        return Err(EPERM);
    }
    let (slot, irq, class, detach) = {
        let mut devs = DEVS.lock();
        let d = devs.iter_mut().find(|d| d.id == id).ok_or(Errno::ENODEV)?;
        if policy(d.class) == Policy::Kernel {
            crate::audit!(
                "driver.deny",
                "pid={pid} exe={exe} id={id} reason=policy-kernel"
            );
            return Err(Errno::EBUSY);
        }
        // One driver at a time (an exited owner's claim is ended by `space_gone`).
        if d.claim.is_some() {
            return Err(Errno::EBUSY);
        }
        let generation = GENERATION.fetch_add(1, Ordering::Relaxed);
        d.claim = Some(Claim {
            pid,
            exe: exe.clone(),
            generation,
            dma: Vec::new(),
            qsel: 0,
            q: [QState::default(); MAX_QUEUES],
            refused: 0,
        });
        d.claims += 1;
        let detach = d.kernel_attached.then_some(d.hooks.detach);
        d.kernel_attached = false;
        (d.slot, d.irq, d.class, detach)
    };
    if let Some(f) = detach {
        f();
    }
    reset(slot);
    let _ = gic::disable(irq);
    let _ = gic::unregister(irq);
    PENDING[slot].store(0, Ordering::Release);
    IRQ_OF[slot].store(irq, Ordering::Release);
    if gic::register(irq, user_irq).is_err() {
        kprintln!("[udrv] {}: IRQ {irq} still taken", class.name());
    }
    let generation = DEVS
        .lock()
        .iter()
        .find(|d| d.slot == slot)
        .and_then(|d| d.claim.as_ref().map(|c| c.generation))
        .unwrap_or(0);
    let node = Arc::new(DevFd {
        slot,
        generation,
        ino: generation,
        dev: alloc_dev(),
    });
    let of = crate::fs::OpenFile::new(node, crate::fs::abi::O_RDWR);
    let fd = crate::proc::current()
        .inner
        .lock()
        .fds
        .alloc(Arc::new(of), true)?;
    crate::audit!(
        "driver.claim",
        "pid={pid} exe={exe} device={} slot={slot} irq={irq}",
        class.name()
    );
    kprintln!(
        "[udrv] {} (slot {slot}, IRQ {irq}) claimed by pid {pid} ({exe})",
        class.name()
    );
    if class == Class::Sound {
        crate::audio::sink_changed();
    }
    Ok(fd as usize)
}

/// End the claim on `slot` (if it is still claim `generation`, or any claim when None).
/// `space_alive`: the owner still maps the DMA buffers, so they wait in LEFTOVER.
fn release(slot: usize, generation: Option<u64>, reason: &str, space_alive: bool) {
    let (claim, irq, class, attach, quiesce) = {
        let mut devs = DEVS.lock();
        let Some(d) = devs.iter_mut().find(|d| d.slot == slot) else {
            return;
        };
        match (&d.claim, generation) {
            (None, _) => return,
            (Some(c), Some(g)) if c.generation != g => return,
            _ => {}
        }
        let c = d.claim.take().unwrap();
        d.releases += 1;
        d.last_release = alloc::format!("pid {} ({}): {reason}", c.pid, c.exe);
        // The kernel driver is the fallback whenever no user driver holds the device.
        (c, d.irq, d.class, Some(d.hooks.attach), d.hooks.quiesce)
    };
    // 1. Stop the device (and its DMA): first make it give back what it holds, while the
    // driver's queues still exist (only for queues inside its own DMA buffers).
    if let (Some(q), Some(t)) = (quiesce, transport(slot)) {
        let regs: Vec<QueueRegs> = claim
            .q
            .iter()
            .map(|q| {
                let n = q.num as u64;
                let ok = n > 0
                    && claim.in_dma(q.desc, 16 * n)
                    && claim.in_dma(q.drv, 6 + 2 * n)
                    && claim.in_dma(q.dev, 6 + 8 * n);
                if ok {
                    QueueRegs {
                        num: q.num,
                        desc: q.desc,
                        avail: q.drv,
                        used: q.dev,
                    }
                } else {
                    QueueRegs::default()
                }
            })
            .collect();
        q(&t, &regs);
    }
    reset(slot);
    // 2. Its interrupt is no longer the driver's.
    let _ = gic::disable(irq);
    IRQ_OF[slot].store(0, Ordering::Release);
    let _ = gic::unregister(irq);
    PENDING[slot].store(0, Ordering::Release);
    IRQ_WQ.wake_all();
    crate::poll::notify();
    // 3. Its memory: once nothing maps it (the owner's space is gone) and the device can't
    // be finishing old requests into it any more (quarantine).
    let pages: usize = claim.dma_pages();
    if space_alive {
        LEFTOVER
            .lock()
            .extend(claim.dma.iter().map(|&(pa, n)| (claim.pid, pa, n)));
    } else {
        for &(pa, n) in &claim.dma {
            quarantine(pa, n);
        }
    }
    crate::audit!(
        "driver.release",
        "pid={} exe={} device={} reason={reason} dma_pages={pages} refused={}",
        claim.pid,
        claim.exe,
        class.name(),
        claim.refused
    );
    kprintln!(
        "[udrv] {} released by pid {} ({reason}); kernel driver {}",
        class.name(),
        claim.pid,
        if attach.is_some() {
            "takes over"
        } else {
            "stays off"
        }
    );
    // 4. The kernel driver again (the fallback while no user driver runs).
    if let Some(attach) = attach
        && let Some(t) = transport(slot)
    {
        match attach(t) {
            Ok(()) => {
                if let Some(d) = DEVS.lock().iter_mut().find(|d| d.slot == slot) {
                    d.kernel_attached = true;
                }
            }
            Err(e) => kprintln!(
                "[udrv] {}: kernel driver failed to re-attach: {e:?}",
                class.name()
            ),
        }
    }
    if class == Class::Sound {
        crate::audio::sink_changed();
    }
}

/// `pid`'s address space is gone (exit, or exec replaced it): end its claims and free the
/// DMA buffers it kept mapped. Thread context, no locks held.
pub fn space_gone(pid: u32) {
    let mine: Vec<usize> = DEVS
        .lock()
        .iter()
        .filter(|d| d.claim.as_ref().is_some_and(|c| c.pid == pid))
        .map(|d| d.slot)
        .collect();
    for slot in mine {
        release(slot, None, "owner exited or exec'd", false);
    }
    let freed: Vec<(PhysAddr, usize)> = {
        let mut l = LEFTOVER.lock();
        let (theirs, rest): (Vec<_>, Vec<_>) = l.drain(..).partition(|e| e.0 == pid);
        *l = rest;
        theirs.into_iter().map(|e| (e.1, e.2)).collect()
    };
    for (pa, n) in freed {
        quarantine(pa, n);
    }
    sweep();
}

// ---- the device fd -------------------------------------------------------------------------

struct DevFd {
    slot: usize,
    generation: u64,
    ino: u64,
    dev: u64,
}

impl DevFd {
    fn live(&self) -> bool {
        DEVS.lock().iter().any(|d| {
            d.slot == self.slot
                && d.claim
                    .as_ref()
                    .is_some_and(|c| c.generation == self.generation)
        })
    }

    /// Run `f` on this fd's claim (Errno::ENODEV once it has been released).
    fn with<R>(&self, f: impl FnOnce(&mut Dev) -> KResult<R>) -> KResult<R> {
        let mut devs = DEVS.lock();
        let d = devs
            .iter_mut()
            .find(|d| {
                d.slot == self.slot
                    && d.claim
                        .as_ref()
                        .is_some_and(|c| c.generation == self.generation)
            })
            .ok_or(Errno::ENODEV)?;
        f(d)
    }
}

impl Drop for DevFd {
    fn drop(&mut self) {
        // The last copy of the fd is gone. The owner may still be running (it closed it),
        // so its DMA mappings stay until its address space goes.
        release(self.slot, Some(self.generation), "device fd closed", true);
    }
}

impl Inode for DevFd {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        Stat::new(self.dev, self.ino, InodeType::CharDev, 0o600)
    }
    /// Wait for the interrupt: 8 bytes, how many times it fired since the last read.
    fn read_at(&self, _off: u64, buf: &mut [u8]) -> FsResult<usize> {
        if buf.len() < 8 {
            return Err(Errno::EINVAL);
        }
        loop {
            if !self.live() {
                return Err(Errno::ENODEV);
            }
            let n = PENDING[self.slot].swap(0, Ordering::AcqRel);
            if n > 0 {
                buf[..8].copy_from_slice(&n.to_le_bytes());
                return Ok(8);
            }
            if crate::proc::signal::interrupted() {
                return Err(Errno::EINTR);
            }
            IRQ_WQ.wait_until_timeout(
                || {
                    PENDING[self.slot].load(Ordering::Acquire) > 0
                        || crate::proc::signal::interrupted()
                },
                100_000_000,
            );
        }
    }
    fn poll(&self) -> u32 {
        if !self.live() {
            return crate::poll::POLLERR | crate::poll::POLLHUP;
        }
        if PENDING[self.slot].load(Ordering::Acquire) > 0 {
            crate::poll::POLLIN | crate::poll::POLLRDNORM
        } else {
            0
        }
    }
}

// ---- register access -----------------------------------------------------------------------

fn refuse(d: &mut Dev, what: &str) -> Errno {
    if let Some(c) = d.claim.as_mut() {
        c.refused += 1;
        crate::audit!(
            "driver.dma_refused",
            "pid={} exe={} device={} {what}",
            c.pid,
            c.exe,
            d.class.name()
        );
        if c.refused <= 3 {
            kprintln!(
                "[udrv] {}: refused {what} from pid {}",
                d.class.name(),
                c.pid
            );
        }
    }
    EPERM
}

fn reg_write(d: &mut Dev, off: usize, width: usize, v: u32) -> KResult<usize> {
    let slot = d.slot;
    let c = d.claim.as_mut().ok_or(Errno::ENODEV)?;
    let lo = |old: u64| (old & !0xffff_ffff) | v as u64;
    let hi = |old: u64| (old & 0xffff_ffff) | ((v as u64) << 32);
    let qs = c.qsel.min(MAX_QUEUES - 1);
    match off {
        QUEUE_SEL => {
            if v as usize >= MAX_QUEUES {
                return Err(EINVAL);
            }
            c.qsel = v as usize;
        }
        QUEUE_NUM => c.q[qs].num = v,
        QUEUE_DESC_LOW => c.q[qs].desc = lo(c.q[qs].desc),
        QUEUE_DESC_HIGH => c.q[qs].desc = hi(c.q[qs].desc),
        QUEUE_DRIVER_LOW => c.q[qs].drv = lo(c.q[qs].drv),
        QUEUE_DRIVER_HIGH => c.q[qs].drv = hi(c.q[qs].drv),
        QUEUE_DEVICE_LOW => c.q[qs].dev = lo(c.q[qs].dev),
        QUEUE_DEVICE_HIGH => c.q[qs].dev = hi(c.q[qs].dev),
        QUEUE_READY if v == 1 => {
            let q = c.q[qs];
            let n = q.num as u64;
            let ok = n > 0
                && c.in_dma(q.desc, 16 * n)
                && c.in_dma(q.drv, 6 + 2 * n)
                && c.in_dma(q.dev, 6 + 8 * n);
            if !ok {
                return Err(refuse(
                    d,
                    &alloc::format!("queue={qs} outside its DMA buffers"),
                ));
            }
        }
        QUEUE_NOTIFY => {
            let qi = v as usize;
            let q = *c.q.get(qi).ok_or(EINVAL)?;
            if q.num == 0 || !c.in_dma(q.desc, 16 * q.num as u64) {
                return Err(refuse(d, &alloc::format!("notify of unset queue {qi}")));
            }
            for i in 0..q.num as u64 {
                // SAFETY: the table is inside one of this claim's DMA buffers (checked
                // above), which are ordinary RAM the kernel can read.
                let p = phys_to_virt((q.desc + 16 * i) as PhysAddr) as *const u8;
                let (addr, len, flags) = unsafe {
                    (
                        core::ptr::read_volatile(p as *const u64),
                        core::ptr::read_volatile(p.add(8) as *const u32),
                        core::ptr::read_volatile(p.add(12) as *const u16),
                    )
                };
                if len == 0 {
                    continue;
                }
                if flags & DESC_F_INDIRECT != 0 || !c.in_dma(addr, len as u64) {
                    return Err(refuse(
                        d,
                        &alloc::format!("queue={qi} desc={i} addr={addr:#x} len={len}"),
                    ));
                }
            }
        }
        _ => {}
    }
    if let Some(t) = transport(slot) {
        mmio::mb();
        t.write_raw(off, width, v);
    }
    Ok(0)
}

fn check_reg(off: usize, width: usize) -> KResult<()> {
    let ok_width = matches!(width, 1 | 2 | 4) && off.is_multiple_of(width);
    // The registers below the config space are 32-bit only (virtio-mmio 4.2.2).
    if !ok_width || off + width > WINDOW || (off < 0x100 && width != 4) {
        return Err(EINVAL);
    }
    Ok(())
}

fn devfd(fd: usize) -> KResult<Arc<crate::fs::OpenFile>> {
    let f = crate::proc::fd_file(fd as i64)?;
    if crate::fs::node::downcast::<DevFd>(f.inode()).is_none() {
        return Err(EINVAL);
    }
    Ok(f)
}

/// `MILK_DRIVER(op, a, b, c, d)` (docs/drivers.md).
pub fn sys(op: usize, a: usize, b: usize, c: usize, d: usize) -> KResult<usize> {
    sweep();
    if op == 0 {
        return claim(a as u32);
    }
    let f = devfd(a)?;
    let df = crate::fs::node::downcast::<DevFd>(f.inode()).ok_or(EINVAL)?;
    match op {
        // read(fd, off, width)
        1 => {
            check_reg(b, c)?;
            df.with(|dev| {
                let t = transport(dev.slot).ok_or(Errno::ENODEV)?;
                Ok(t.read_raw(b, c) as usize)
            })
        }
        // write(fd, off, value, width)
        2 => {
            check_reg(b, d)?;
            df.with(|dev| reg_write(dev, b, d, c as u32))
        }
        // dma(fd, bytes, &pa) -> va
        3 => {
            let pages = b.div_ceil(PAGE_SIZE);
            if pages == 0 {
                return Err(EINVAL);
            }
            let pa = df.with(|dev| {
                let claim = dev.claim.as_mut().ok_or(Errno::ENODEV)?;
                if claim.pid != crate::sched::current_pid() {
                    return Err(EPERM);
                }
                if claim.dma_pages() + pages > DMA_MAX_PAGES {
                    return Err(ENOMEM);
                }
                let pa =
                    pmm::alloc_contiguous_zeroed_as(pmm::Use::Devices, pages, 1).ok_or(ENOMEM)?;
                claim.dma.push((pa, pages));
                Ok(pa)
            })?;
            // Mapped after DEVS is dropped (lock order: never DEVS -> process inner).
            let va = crate::proc::with_current_space(|s| s.map_dma(pa, pages * PAGE_SIZE))?;
            crate::proc::copy_out_current(c, &(pa as u64).to_le_bytes())?;
            Ok(va)
        }
        // arm(fd)
        4 => df.with(|dev| {
            let _ = gic::enable(dev.irq);
            Ok(0)
        }),
        // info(fd, buf, len)
        5 => {
            let s = df.with(|dev| {
                Ok(alloc::format!(
                    "slot={} irq={} base={:#x} id={}",
                    dev.slot,
                    dev.irq,
                    crate::platform::CURRENT.virtio.map_or(0, |v| v.base)
                        + dev.slot * mmio::MMIO_STRIDE,
                    dev.id
                ))
            })?;
            let n = s.len().min(c);
            crate::proc::copy_out_current(b, &s.as_bytes()[..n])?;
            Ok(n)
        }
        _ => Err(EINVAL),
    }
}

/// `/proc/drivers`: each hand-off-capable device, its policy and who drives it.
pub fn proc_text() -> String {
    let mut s = String::new();
    for d in DEVS.lock().iter() {
        let owner = match (&d.claim, d.kernel_attached) {
            (Some(c), _) => alloc::format!("pid {} ({})", c.pid, c.exe),
            (None, true) => String::from("kernel"),
            (None, false) => String::from("none"),
        };
        let _ = writeln!(
            s,
            "{} policy={} slot={} irq={} owner={owner} claims={} releases={}{}",
            d.class.name(),
            match policy(d.class) {
                Policy::User => "user",
                Policy::Kernel => "kernel",
            },
            d.slot,
            d.irq,
            d.claims,
            d.releases,
            if d.last_release.is_empty() {
                String::new()
            } else {
                alloc::format!(" last_release=\"{}\"", d.last_release)
            }
        );
    }
    s
}

/// `/dev/entropy` (write-only, 0200 root): bytes a user-space RNG driver read from its
/// device, mixed into the kernel CSPRNG and counted as hardware entropy.
struct EntropyDev {
    ino: u64,
    dev: u64,
}

impl Inode for EntropyDev {
    fn ino(&self) -> u64 {
        self.ino
    }
    fn dev(&self) -> u64 {
        self.dev
    }
    fn kind(&self) -> InodeType {
        InodeType::CharDev
    }
    fn stat(&self) -> Stat {
        Stat::new(self.dev, self.ino, InodeType::CharDev, 0o200)
    }
    fn write_at(&self, _off: u64, buf: &[u8]) -> FsResult<usize> {
        let n = buf.len().min(4096);
        crate::crypto::rng::add_device_entropy(&buf[..n]);
        Ok(n)
    }
    fn truncate(&self, _len: u64) -> FsResult<()> {
        Ok(())
    }
    fn poll(&self) -> u32 {
        crate::poll::POLLOUT
    }
}

/// `/dev/entropy` and `/proc/drivers`. After `fs::init`.
pub fn init_files() {
    let dev = crate::fs::devfs::ids().map_or(0, |ids| ids.dev);
    let node = Arc::new(EntropyDev {
        ino: 0x000e_0001,
        dev,
    });
    if let Err(e) = crate::fs::devfs::add_device("entropy", node) {
        kprintln!("[udrv] /dev/entropy: {e:?}");
    }
    if let Some(root) = crate::fs::procfs::root() {
        root.add_file("drivers", proc_text);
    }
}
