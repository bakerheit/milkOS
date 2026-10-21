# Drivers in user space

milkOS is a monolithic kernel. Its drivers (virtio blk, net, input, sound, rng, 9p) live in
`kernel/src/virtio/`, and a bug in any of them is a kernel bug. This document describes a
way to run a driver as an ordinary, supervised **process** instead, microkernel style, for
devices where that makes sense. A crashing driver is then restarted by milkd and the
kernel keeps running. Two drivers use it: **rngd** (virtio-rng) and **soundd**
(virtio-sound). The in-kernel drivers stay as the fallback.

Files: `kernel/src/udrv.rs` (the interface), `user/udrv` (the userland side: register
access, DMA, a split virtqueue), `user/rngd`, `user/soundd`, the units in
`/etc/milk/services/`, `scripts/driver-crash-test.sh`.

## Threat model

The user driver is a **root process with the `driver` capability**, run by milkd. We
protect the kernel against a **buggy** driver: one that crashes, hangs, scribbles over its
own memory, programs nonsense into its device, or never re-arms its interrupt. We do
**not** protect it against a **malicious** one. A root process with the `driver`
capability is trusted the way kernel code is trusted, for one reason: there is no IOMMU on
qemu `virt`, so a device does DMA to any physical address it is given (see "DMA" below).
What the interface buys is that ordinary driver bugs stop being kernel crashes.

| a buggy driver | result |
|---|---|
| crashes, is killed (`kill -9`), exits, or execs | the kernel resets the device (Status = 0, which stops its DMA), then frees the driver's DMA buffers, unregisters its interrupt, and hands the device back to the in-kernel driver if the policy says so. milkd restarts the driver with backoff |
| touches a register outside its device | refused: register access goes through the kernel, bounded to the device's own 0x200-byte virtio-mmio window |
| reads or writes kernel memory, or another process's | impossible with the CPU: it can only map the DMA buffers it allocated |
| points a queue at memory it didn't allocate (QueueDesc/Driver/Device) | refused when it sets QueueReady (EPERM, audited `driver.dma_refused`) |
| puts a bad address in a descriptor | refused at QueueNotify: the kernel checks every in-use descriptor of that queue against the driver's DMA buffers first; the notify is dropped and audited |
| uses indirect descriptors | refused (the check doesn't follow them) |
| never acknowledges or re-arms its interrupt | that interrupt stays masked: no storm. The kernel masks the line when it fires and only the driver's explicit re-arm unmasks it |
| allocates DMA memory without end | capped per device (256 KiB) |
| hangs | the device stops working (no sound, no fresh entropy) and nothing else does. milkd doesn't detect hangs; `kill` it and milkd restarts it |
| feeds the kernel bad entropy (rngd) | mixed into the pool, never replaces it; the boot seed and jitter stay in. Worst case is the jitter-only fallback |

**What still trusts the driver (and the device):**

* **DMA after the check.** The descriptor check runs when the driver rings the doorbell.
  The driver can rewrite a descriptor in its (shared) DMA memory after that, and the
  device would then read or write anywhere in physical memory. A malicious driver can
  corrupt the kernel this way. Closing that hole needs an IOMMU (qemu has `virtio-iommu`;
  milkOS doesn't drive it yet). The check catches the realistic bug (a stale or garbage
  address); it is not a security boundary.
* **The device itself.** A device can DMA anywhere no matter what the driver does. On qemu
  the devices are qemu's own code, and we trust them as we trust the hypervisor.

## The interface: `MILK_DRIVER` (syscall 0x1010)

Who may use it: **euid 0 and the `driver` capability**. `driver` is a removable capability
like `spawn` or `net` (`sandbox ... driver=0`). It is on for root processes that weren't
sandboxed and off for anything sandboxed without it. milkd's driver units keep it and drop
everything else they can.

| op | arguments | does |
|---|---|---|
| 0 claim | virtio device id (4 rng, 25 sound) | takes the first such device. The policy must say `user` for it, and it must not be owned by another live process. The in-kernel driver lets go first (stream stopped, device reset). Returns a **device fd** (close-on-exec). Audited `driver.claim` |
| 1 read | fd, offset, width (1, 2, 4) | reads a register in the device's window (0x000-0x1ff; config space from 0x100) |
| 2 write | fd, offset, value, width | writes one. QueueSel, QueueNum and the queue address registers are tracked; QueueReady and QueueNotify are checked (above) |
| 3 dma | fd, bytes, `&pa` | allocates zeroed, physically contiguous pages, maps them read/write into the caller, and stores their physical address at `pa`. Returns the virtual address |
| 4 arm | fd | unmasks the interrupt |
| 5 info | fd, buf | `slot=N irq=N base=0x... id=N` (for logs) |

**Interrupts are the fd.** `read(fd, 8 bytes)` blocks until the device interrupts and
returns how many times it did since the last read (u64). `poll`/`epoll` report POLLIN
while that count is non-zero. When the interrupt fires, the kernel's handler **masks** the
line at the GIC, counts it and wakes the reader. The driver then reads InterruptStatus,
writes InterruptACK and calls `arm`. virtio-mmio interrupts are level-triggered, so the
order matters: ack before re-arm, or it fires again at once.

**Why registers go through the kernel, not a mapping.** The lead asked for the MMIO region
to be mapped into the driver. On qemu `virt` the virtio-mmio slots are 0x200 bytes apart,
so one 4 KiB page holds eight devices' registers, and a mapping could not stop rngd from
writing the disk controller. A user mapping is only safe for a region that has whole pages
to itself (a PCI BAR, say). There isn't one here, so every access is a syscall, bounded to
the window. That costs about a microsecond per access, which is nothing for these devices:
rngd does a handful per request, soundd about four per 10 ms period.

**DMA buffers.** `dma` memory is ordinary RAM from the kernel's frame allocator, owned by
the device claim and not by the process. It is mapped into the process like a desktop
window buffer, but never copied or shared by fork. The process sees exactly the
physical addresses of its own buffers and nothing else. virtio-mmio on qemu `virt` is
DMA-coherent, so a cacheable mapping is correct.

**Release** happens in three cases:

* the owner exits (in `finish_process`, after its address space is gone, so nothing maps
  the buffers any more);
* it execs (the same, for the old image);
* the last copy of the device fd is closed. The owner still maps the buffers then, so
  they are kept until its address space goes.

Steps:

1. Reset the device.
2. Unregister the user interrupt handler.
3. Put the DMA frames in **quarantine** for 2 s before they are freed (see "Devices that
   finish requests after a reset").
4. For sound, wait 50 ms.
5. Re-attach the in-kernel driver: the fallback while no user driver runs.

Audited `driver.release` with the reason.

**Devices that finish requests after a reset.** The crash test found that qemu's
virtio-sound reset does not flush the buffers a PCM stream holds. qemu completes them
later: as it plays them, or at the next PREPARE. It writes their status into the old
driver's memory and pushes their ids onto whatever queue is live by then. Without care
that does two things:

* It corrupted the next driver's queue. The in-kernel driver panicked on an integer
  underflow, and a restarted soundd ran out of descriptors.
* It scribbled on freed memory.

So:

* **Kernel detach** (soundd claims the device): the kernel driver sends PCM STOP and
  RELEASE first. RELEASE must complete every pending buffer (virtio 1.2, 5.14.6.6.5.1)
  while the pages are still the kernel's.
* **Releasing a dead driver**: it can't release anything, so the kernel waits 50 ms after
  the reset (qemu finishes what it was playing, at most 4 periods) and keeps the DMA
  frames quarantined for 2 s, past the kernel driver's PREPARE.
* **Both drivers** drain the tx used ring after PREPARE. They also ignore completions
  for heads they didn't queue (`strays` in `/proc/audio`), instead of trusting them.

This is the "the device is trusted" caveat from the threat model, met in practice.

## Policy: which driver runs

The boot option `opt/milkos/drivers` (a qemu fw_cfg string; `MILK_DRIVERS="rng=kernel
sound=user" scripts/run.sh`) sets each class to `user` or `kernel`:

* `kernel`: only the in-kernel driver. `claim` fails with EBUSY. The user driver's unit
  doesn't start (it is conditional on the policy).
* `user`: the in-kernel driver runs at boot (it seeds the CSPRNG and plays the boot
  chime) and whenever no user driver holds the device. The user driver takes over when it
  claims and gives the device back when it dies.

Defaults: **rng = user** and **sound = user**. soundd passes audio-test and music-test as
the default driver and survives the crash test, so it is solid enough to be the default.
`/proc/drivers` shows the policy and current owner of each device. The units test it with
`if-contains = /proc/drivers rng policy=user`.

## rngd

A user virtio-rng driver: one request queue and a 64-byte device-writable buffer. Every
30 s, and once at start, it reads 64 bytes from the device and writes them to
**`/dev/entropy`**. That is a new device node, write-only, 0200 root. The kernel mixes
what arrives there into the CSPRNG pool and counts it as hardware entropy
(`/proc/rng` shows `source=rngd+jitter`). The
kernel's reseed every 60 s or 1 MiB then uses what rngd fed. Without rngd the kernel's own
driver comes back, and without any device the kernel reseeds from timer jitter alone, as
before.

## soundd

The mixer stays in the kernel. It is shared by every writer of `/dev/audio` and the
desktop's system sounds, and its timing is simple there. Moving it out would put every
app's audio behind IPC to a daemon, so it stays. What moves is the **device side**:
virtio-sound setup, the PCM stream, the tx queue and the interrupt.

The kernel mixer writes 10 ms periods into a **sink**. The sink is either the in-kernel
virtio-sound driver, or **`/dev/pcm`** (0600 root) when soundd holds the device:

* `read(1920 bytes)` returns the next mixed period (S16_LE 48 kHz stereo), blocking until
  there is one; POLLIN when one is ready. At most 8 periods are out at a time (80 ms).
* `write(u32 n)` reports n periods played. That paces the mixer and keeps
  `SNDCTL_DSP_SYNC` and `/proc/audio` right.

soundd starts the PCM stream once 4 periods are queued (or nothing more is coming) and
stops it 200 ms after the mixer runs dry, like the kernel driver. When soundd dies:

1. Periods it had taken are lost (at most 80 ms of sound).
2. The kernel re-attaches its own driver and carries on with the next period, plus any
   periods soundd hadn't taken yet.
3. When milkd has restarted soundd, it claims the device back, so playback moves to it.

Writers never see an error. In the crash test the 440 Hz tone shows no measurable gap
(20 ms windows). Periods lost in a hand-off are counted as played, so `SNDCTL_DSP_SYNC`
doesn't wait for them.

## Supervision

Both are milkd services (`/etc/milk/services/rngd.service`, `soundd.service`):

* `user = root` and `restart = always`;
* a sandbox with only the one device node they need (`fs=/dev/entropy:rw` /
  `fs=/dev/pcm:rw`), `net=none spawn=0 clipboard=0 windows=0`, and the `driver`
  capability kept;
* output into the journal as `rngd` / `soundd`;
* conditional on the policy (`if-contains = /proc/drivers rng policy=user`).

milkd's backoff starts at 100 ms. `svc kill UNIT [SIG]` signals a unit without stopping
it, so the restart policy applies.

`scripts/driver-crash-test.sh` (in check-all):

* **Boot 1, default policy.** A 5 s 440 Hz tone plays, and 1.5 s in both drivers get
  SIGKILL. Checks:
  * the kernel never panics;
  * milkd restarts each, and it is driving its device again about 100 ms after the kill
    (journal, monotonic ms; the limit is 1 s);
  * `/proc/drivers` shows claims=2 releases=1;
  * the tone plays to its end with the longest gap < 300 ms (qemu's wav recording);
  * soundd is the sink at the end, and the CSPRNG's source is `rngd+jitter` again.
* **Boot 2, `rng=kernel sound=kernel`.** Both policies are kernel, nobody claims, the
  units are skipped, and the kernel drivers play and seed.

`utest driver` covers the interface itself:

* root and the capability are required (a `driver=0` sandbox or uid nobody gets EPERM);
* a second claim gets EBUSY;
* the window bounds hold;
* a queue pointed at kernel memory is refused at QueueReady;
* a planted bad descriptor is refused at QueueNotify (never on the available ring, so
  the device can't see it);
* a real request completes through the interrupt fd;
* a forked child gets SIGSEGV on the DMA address;
* close gives the device back, and the refusals are audited.
