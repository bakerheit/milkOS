# milkOS on the Raspberry Pi 4

milkOS runs on a Raspberry Pi 4 Model B (the BCM2711: four Cortex-A72 cores, 1 to 8 GB of RAM).
The same source tree builds two kernel images: the default one for qemu `virt`, and one for the Pi
(`cargo build --features raspi`, `scripts/pi-build.sh`). This document says what exists, how to run it
under qemu's `-M raspi4b`, how to put it on a card, how it debugs itself on a board with nothing but
an HDMI screen, and, honestly, which parts have only ever run on an emulator or not at all.

Files: `kernel/src/platform.rs` (the board table), `kernel/src/el.S` (EL2 to EL1), `kernel/src/rpi/*`
(the Pi's drivers), `kernel/src/{pci,dma,timeout,crashscreen}.rs`, `kernel/src/usb/*`,
`kernel/src/net/nic.rs`, the host-tested crate `rpikit/`, `scripts/pi-*.sh`, `scripts/{usb,highmem}-test.sh`.

![milkOS on qemu raspi4b](screenshots/raspberry-pi-desktop.png)

## Status

| What | State |
|---|---|
| Boot at EL2, drop to EL1, MMU on, higher-half kernel, 4 CPUs through the firmware's spin table | **runs** on qemu raspi4b (TCG); the EL2 and spin-table paths are exactly what the real firmware does |
| PL011 console, GIC-400, generic timer, scheduler, userspace, the shell, the whole desktop | **runs** on qemu raspi4b |
| Firmware mailbox, framebuffer (format, pitch and address from the firmware's answer) | **runs** on qemu raspi4b; the answer's shape is unit-tested against hostile values |
| Boot log and crash screen on HDMI, ACT LED, sign-of-life band | **runs** on qemu (screendumps are checked by the test); the LED and real HDMI scan-out are untested |
| SD card (SDHCI, polled PIO), as `/dev/mmcblk0`, milkfs on it | **runs** on qemu (its EMMC controller); **untested** on EMMC2 of a real board |
| RAM from the device tree, several banks and holes | **runs** on qemu (two banks and a hole, the GPU's); 4 GB / 8 GB layouts are unit-tested and the allocator above 4 GiB is tested on qemu virt |
| Hardware random generator (RNG200) | **untested** (qemu does not model it); falls back to the jitter-seeded CSPRNG |
| PCIe root complex + xHCI USB (the VL805: the four USB ports) | the xHCI driver and the HID keyboard/mouse **run** on qemu virt (`usb-test`); the PCIe bring-up of the BCM2711 is **untested** |
| Ethernet (GENET + BCM54213PE PHY) | **untested**: qemu has no GENET; written from the register layout, logs every step |
| Wi-Fi (CYW43455 over SDIO), Bluetooth, the touchscreen, audio, the GPU | not started |

"Untested" means it compiles, is gated on the device tree saying the hardware exists (qemu's does not,
so those paths are skipped there) and logs what it reads; the first boot on a real board is its test.

## Running it under qemu

```
scripts/pi-run.sh               # a window (cocoa) and the serial console on this terminal
scripts/pi-run.sh headless      # serial only
scripts/pi-qemu-test.sh         # the test: five boots, see below (about 2 minutes)
```

`-M raspi4b` runs on TCG (`-accel hvf` does not work for the Raspberry Pi machines), so boots take a
few seconds more than on `virt`. qemu 10.2.1 models: 4 Cortex-A72, the GIC-400 (GICv2) at
`0xff841000`, the ARM generic timer, PL011 UART0 (serial 0) and the mini UART (serial 1), the
mailbox with the property interface and a framebuffer, two SDHCI controllers (`0xfe300000` EMMC and
`0xfe340000` EMMC2; `-drive if=sd` lands on the first), a DWC2 USB OTG controller, GPIO, DMA, the
system timer. It does **not** model: PCIe (so no VL805, no USB ports), GENET (no Ethernet), the RNG, the
legacy `bcm2835-rng`... and its RAM is exactly 2 GiB (`-m` other than `2G` is refused: "Invalid RAM
size, should be 2 GiB"), of which the device tree it hands over shows 960 MiB (`0..0x3c000000`; the
GPU's 64 MiB, with the framebuffer at `0x3c100000`, is above that).

qemu gives the kernel **no device tree of its own**: `x0 = 0x100` points at nothing. The tests build
a small one (`scripts/pi_dtb.py`: the stock tree's shape: root `compatible` `raspberrypi,4-model-b`,
`/chosen`, `/memory`, four cpus with `enable-method = "spin-table"` and the `cpu-release-addr`s 0xd8,
0xe0, 0xe8, 0xf0) and pass it with `-dtb`; qemu then rewrites `/memory` (and removes any extra memory
nodes: it always says one bank of 960 MiB) and sets `/chosen/bootargs` from `-append`. To give the
kernel a tree qemu does not touch, `pi-qemu-test` loads it as a raw file where the boot stub's `x0`
points (`-device loader,file=...,addr=0x100,force-raw=on`; the stub's own code starts at 0x300, so the
tree must be under 512 bytes: no cpus). That is how the two-bank layout is tested.

### Boot options

The Pi has no qemu `fw_cfg`. What the tests and the launcher pass as `opt/milkos/<name>` files travels
on the kernel command line there (`/chosen/bootargs`: `cmdline.txt` on the card, `-append` under qemu):
`milk.<name>=<value>` (a value with spaces in double quotes). `fwcfg::find_file` / `read_file` serve
them, so every existing option works unchanged:

| option | does |
|---|---|
| `milk.selftest=full` (or a comma list) | the slow boot self-tests (kernel/src/selftest.rs) |
| `milk.drivers="rng=kernel sound=kernel"` | which driver runs each device (docs/drivers.md) |
| `milk.net="internet=on"` | the launcher's network policy (docs/networking.md) |
| `milk.fbscale=N` | magnify the boot log and crash screen font N times (default 1; 2 from 1600 px wide) |
| `milk.crashtest=early` / `late` | panic on purpose right after the display is up / once the desktop has been up 8 s (tests the crash screens) |

## Putting it on a card

The kernel is one file the firmware loads. These are **your** steps; nothing in the repository writes to
a card or a disk.

1. `scripts/pi-bootdir.sh /tmp/milkos-boot` builds the release kernel and puts `milkos8.img`,
   `config.milkos.txt` and `cmdline.milkos.txt` in that folder (it refuses a folder on a mounted volume).
2. On the card's boot partition (the FAT one with `start4.elf`, `fixup4.dat`, `config.txt`): **keep a copy
   of `config.txt`**, copy `milkos8.img` next to the firmware's files, and make `config.txt` say
   what `config.milkos.txt` says: `arm_64bit=1`, `kernel=milkos8.img`, `hdmi_force_hotplug=1`, `max_framebuffers=1`
   and **no `dtoverlay=vc4-kms-v3d`** (milkOS draws on the firmware's framebuffer, not on a KMS display).
   `cmdline.txt` can stay as it is (the kernel only looks at `milk.` words) or be replaced.
3. Put the card in a Pi 4 with an HDMI screen. To go back: restore the old `config.txt`.

The firmware (start4.elf) loads `milkos8.img` at 0x80000, hands over a device tree in `x0` and starts all four cores at EL2 (the
others wait in the spin table). The card also holds the root file system's persistent part: milkOS finds
milkfs volumes on any block device, so a card with a `Milk`-labelled milkfs partition mounts it as
`/Volumes/Milk` (home directories, packages, the audit log); without one the system runs from RAM
(`/` is a tmpfs filled from the image) and says "no Milk disk". Making a milkfs partition on the card
is not automated here (it needs the card's partitioning to be touched).

## What happens at boot

* `boot.S` starts at the load address, at EL2 or EL1 (`el.S`: at EL2 it sets `HCR_EL2.RW`, lets EL1 use the
  counter, copies MIDR/MPIDR, clears the trap registers and `eret`s to EL1h; at EL1 it is two instructions).
  Only core 0 runs it; it cleans and invalidates the data cache over the image, zeroes .bss, fills
  the boot tables (four 1 GiB entries chosen by the board: on virt MMIO then RAM, on the Pi RAM then
  three gaps and MMIO at `0xc0000000`: the UART must work before the real tables exist) and
  enables the MMU.
* `mem::init` reads the RAM banks from the tree (`rpikit::dtb::usable_banks`: sorted, page-aligned,
  overlaps merged, only the bank with the kernel and the ones above it). One allocator spans from the
  first bank to the end of the last; the gaps between banks (the GPU's memory, the peripheral window) are
  reserved, and only real banks are mapped, so nothing touches a gap.
* `platform::check_dtb` logs the board and warns if the tree's `compatible` is not this build's (a `virt`
  image on a Pi, or the other way round: the images are linked for different load addresses and cannot be
  mixed).
* The GIC-400 (GICv2: the same driver as qemu virt, different bases), the timer (virtual timer, PPI 11;
  if `CNTFRQ_EL0` is 0 the 54 MHz crystal is assumed), then the Pi's drivers: `rpi::init` (board info from
  the mailbox, the RNG, the SD card, the Ethernet) and so on to the shell and the desktop, as on virt.
* Secondary cores: for each `/cpus/cpu@N` the kernel reads `enable-method`; `spin-table` means
  "write the entry point at `cpu-release-addr` (clean it to the point of coherency, `sev`)" and the core
  arrives at `secondary_spin_entry` at EL2 with the MMU off and no arguments (it finds its boot block by its
  number in `spin_blocks`); `psci` means a firmware call through the conduit the tree names (qemu virt). The
  stock Pi 4 tree has no `psci` node.

## Memory map (Raspberry Pi 4, low-peripheral mode)

| Physical | What |
|---|---|
| `0x0000_0000..` | RAM. The firmware's spin table is at 0xd8..0xf8; the kernel loads at `0x80000`; the first bank ends at `0x3b400000` on a 4 GB board (the GPU's memory above it) |
| `0x4000_0000..0xfc00_0000` | more RAM (boards with more than 1 GB) |
| `0x1_0000_0000..0x2_0000_0000` | the last 4 GB of an 8 GB board |
| `0x3c10_0000` (qemu) | the framebuffer (qemu); on a board the firmware puts it in the GPU's memory |
| `0xfd20_8000` | RNG200 |
| `0xfd50_0000` | PCIe root complex |
| `0xfd58_0000` | GENET Ethernet |
| `0xfe00_b880` | VideoCore mailbox |
| `0xfe20_0000` | GPIO (ACT LED = GPIO 42) |
| `0xfe20_1000` | PL011 UART0 |
| `0xfe21_5000` | AUX: the mini UART at `0xfe21_5040` |
| `0xfe30_0000` / `0xfe34_0000` | EMMC (Wi-Fi's) / EMMC2 (the SD slot) |
| `0xfe98_0000` | DWC2 USB OTG (not a USB port on a 4B) |
| `0xff80_0000` | ARM local peripherals |
| `0xff84_1000` / `0xff84_2000` | GIC-400 distributor / CPU interface |
| `0x6_0000_0000` (CPU) = PCI `0xc000_0000` | the PCIe outbound window: BARs of the VL805 go here (1 GiB in the stock tree) |

Kernel virtual addresses are `0xffff_0000_0000_0000 + physical`, as on virt (docs/memory.md): the board's
device ranges (`0xfc000000..0x100000000`) are mapped as Device memory, each RAM bank as Normal
memory, the kernel image with W^X as before.

### RAM above 4 GB and DMA

* The device tree of a 4 GB board lists two banks (about 948 MiB, then `0x40000000` to `0xfc000000`), an
  8 GB board a third from `0x1_0000_0000` (its size cell is 32 bits, so the 4 GiB come as two entries that
  touch and merge). `mem.rs` uses all of it (`MAX_BANKS` is 8); the frame allocator, the page tables
  and the linear map take 64-bit physical addresses throughout (checked: no `as u32` on a physical
  address).
* **What can be tested where.** qemu `raspi4b` has exactly 2 GiB (the machine refuses any other `-m`), and
  rewrites the memory nodes of a tree it is given, so a real 4 GB or 8 GB layout cannot run there. What
  is tested instead: (1) host tests of the bank logic with 4 GB and 8 GB trees
  (`rpikit/src/tests.rs`: `banks_pi4_gb4`, `banks_pi4_gb8_uses_memory_above_4gib`, the hole arithmetic,
  hostile trees); (2) the two-bank layout with a hole *running* on raspi4b (`pi-qemu-test` boot 3: banks
  `0..0x3b400000` and `0x40000000..0x80000000`, the framebuffer in the hole, 1972 MiB usable); (3) memory above
  4 GiB *running*: qemu virt with `-m 6G` (`scripts/highmem-test.sh`: frames allocated above 4 GiB, written
  through the linear map and read back, freed; and `-m 8G` by hand, the RAM line says `0x40000000..0x240000000`).
* **DMA limits.** Everything on the BCM2711 that does DMA sits behind the same constraint: the PCIe
  controller's inbound window is set up to cover PCI addresses 0 to 4 GiB but the stock tree's
  `dma-ranges` give 3 GiB (`0..0xc0000000`), GENET and EMMC2 are in the "scb" domain with a `0xfc000000`
  limit, and the VL805 can address only 32 bits. So `platform::CURRENT.dma_limit` is `0xc000_0000` on the
  Pi (no limit on virt) and every DMA buffer comes from `pmm::alloc_dma_zeroed_as` / `dma::Buffer`, which
  allocate below it; the mailbox message page comes from the first GiB (the VideoCore sees RAM through a 1 GiB
  alias at `0xc0000000`). RAM above the limit is for the CPU only (user pages, the page cache, the heap).
  `memtest` checks both allocators.
* The allocator's metadata for 8 GiB is about 2.3 MiB (a bit per frame plus a tag byte), placed after the kernel.

## Debugging with nothing but an HDMI screen

There is no serial cable in this setup, so the kernel makes the screen the console:

1. **A sign of life the moment the framebuffer exists:** a green band (five rows) with a white line under it
   across the top, drawn straight into the buffer before anything else can fail. If the screen shows it, the
   firmware, the mailbox, the mapping of the framebuffer and the EL2/MMU boot all worked.
2. **The boot log** (`kprintln!`) is mirrored on the screen from that point on: first what was printed
   before the framebuffer existed (replayed from the kernel log), then everything, scrolling, in the 8x16
   console font (`milk.fbscale=2` doubles it; 1600+ wide screens do by default). No splash. The desktop
   takes the screen when it starts.
   ![boot log](screenshots/raspberry-pi-boot-log.png)
3. **A crash shows its text.** A kernel panic or exception paints the end of the kernel log (which holds the
   panic message with its source line, or the exception dump: ESR, FAR, ELR, all registers) as a full-screen
   text console, then "milkOS stopped.". If the crash comes before the desktop, the boot log is still on
   screen and the dump is appended to it: the screen is not cleared. The compositor is stopped so it does not paint over it.
   ![crash screen](screenshots/raspberry-pi-crash-screen.png)
4. **The green ACT LED** is on while the kernel runs and flashes at about 5 Hz after a crash, so "on" and
   "flashing" tell apart even if nothing reached the screen.
5. `milk.crashtest=early|late` triggers a panic on purpose so you can see the screens once.

**An option for later, not built:** keeping the log on the card. The boot partition is FAT, which the kernel
can read (`fs/fat.rs`) and could write; a ring of the last log lines flushed to `milkos.log` on it (at a
crash, and once a second while booting) would survive a reboot, so a board that dies before the screen works
could still be diagnosed from another computer. It needs the SD driver to be trusted on real hardware first
(it is the thing it would depend on), which is why it waits. A second cheap option: the same text as beeps or
more LED patterns for failures before the framebuffer.

## The drivers

**Mailbox** (`rpi/mailbox.rs`, messages in `rpikit::mailbox`): one transaction at a time under a lock, a page of
RAM in the first GiB as the buffer (`0xc0000000 | phys` as the bus address, as Linux does), cache lines cleaned
before and dropped after, status bits as in the BCM2835 datasheet (the read mailbox's bit 30 is *empty*, the
write mailbox's bit 31 is *full*). Used for board revision, firmware revision, the MAC address, clock rates
(the SD controller's base clock, tag `GET_CLOCK_RATE`), power domains, the framebuffer, and the VL805 reset notify.

**Framebuffer** (`rpi/fb.rs`): `SET_PHYSICAL_SIZE`, `SET_VIRTUAL_SIZE`, `SET_DEPTH 32`, `SET_PIXEL_ORDER 0`
(BGR: a little-endian `0x00RRGGBB` word is red-green-blue, what the desktop draws), alpha ignored, `ALLOCATE_BUFFER`
(16-byte alignment), `GET_PITCH`. Size, depth, address and pitch are the firmware's answer, sanity-checked
(`rpikit::mailbox::framebuffer_reply`: 32 bits per pixel, a pitch of at least a row, a buffer of at least pitch x height,
an address). A pitch that is not width x 4 is absorbed by growing the width (the rest of the kernel treats stride = width); RGB order
is refused (no screen rather than wrong colours). The buffer is in GPU memory on a board: mapped as
non-cacheable Normal memory (unaligned stores stay legal; Device memory would fault on a memcpy). Under qemu it lies
inside the RAM we manage and its frames are reserved.

**SD card** (`rpi/sdhci.rs`, encodings in `rpikit::sd`): a standard SDHCI 3.0 host with 32-bit register access only (the BCM2711's
EMMC2 needs that: 8 and 16 bit registers go through read-modify-write of the aligned word, the write-1-to-clear
interrupt status through whole-word writes). Programmed I/O, polled, every wait bounded: reset, 3.3 V, 400 kHz
identification (the divider from the clock the firmware reports), CMD0/CMD8/ACMD41/CMD2/CMD3/CMD9 (capacity from the CSD,
version 1 and 2)/CMD7/ACMD6 (4 bit), 25 MHz, `CMD17/18/24/25` with auto CMD12. Registers as `/dev/mmcblk0`; the card
detect bit is not trusted on EMMC2 (the Pi does not wire it). Not yet: DMA (SDMA/ADMA2: the buffers are already below the DMA
limit), high speed mode (50 MHz), UHS, CMD23, voltage switching, SDIO. `scripts/pi-qemu-test.sh` formats an image with milkfs,
mounts it from the emulated card, writes a file, takes a snapshot, reboots and finds the file.

**RNG** (`rpi/rng.rs`): the iProc RNG200 at `0xfd208000` (control, soft resets, FIFO count/data), started and read through
bounded waits, mixed into the CSPRNG's pool at seed time like virtio-rng. If the tree has no such node it is skipped, and
the "jitter only" warning appears.

**LED** (`rpi/led.rs`): GPIO 42 as an output, on at the start of `kmain`.

**PCIe + USB** (`rpi/pcie.rs`, `pci.rs`, `usb/`, `rpikit::{pci,usb}`). The four USB-A ports of a Pi 4 are root ports of a VIA VL805
xHCI controller on the BCM2711's PCIe x1 link. Bring-up (`rpi/pcie.rs`, **untested**): reset the bridge, SerDes out of
IDDQ, inbound window (PCI 0 = CPU 0, 4 GiB BAR2), outbound window from the tree's `ranges` (stock: PCI
`0xc0000000` = CPU `0x6_0000_0000`, 1 GiB), release PERST#, wait for the link, set the class code of the root port, then
`pci::enumerate` places the BARs and bridge windows (`rpikit::pci::Window`/`bridge_mem_window`, host-tested), enables memory
decoding and bus mastering, and the firmware is told about the VL805 (`NOTIFY_XHCI_RESET`, the call Linux makes). Config space of
the root port is the controller's register file; everything behind it goes through the index/data pair (`EXT_CFG_INDEX` 0x9000,
`EXT_CFG_DATA` 0x8000). The **xHCI driver** (`usb/xhci.rs`) is complete for what it does and runs on qemu virt's `qemu-xhci`:
reset, context base array, scratchpad, command ring, one event ring, ports (reset, speed), Enable Slot / Address Device /
Evaluate Context / Configure Endpoint, control transfers, interrupt IN endpoints, hot unplug, all polled from a thread every
4 ms, with 32- or 64-byte contexts, every wait bounded and the DMA memory synced with explicit cache maintenance (the BCM2711's PCIe
DMA is not cache-coherent). **HID boot protocol** (`usb/hid.rs`): keyboards (modifiers, six keys, rollover, software auto-repeat
after 500 ms every 33 ms) and mice (3 buttons, motion, wheel) feed `input::report_*`. `scripts/usb-test.sh` types on a `usb-kbd` and moves a
`usb-mouse` through qemu and reads `/proc/input`. Not done: hubs (a keyboard behind a hub; the VL805's ports are root ports, so
the ports on the board work without), interrupt-driven operation (MSI/INTx), report-protocol HID (media keys, a mouse's extra
buttons), LEDs, mass storage, isochronous.

**Ethernet** (`rpi/genet.rs`, **untested**, descriptor words and autonegotiation resolution in `rpikit::genet`): UMAC reset and
setup, RGMII with the PHY adding the receive delay (`rgmii-rxid`, so the PHY's RX skew bit is set and the MAC's ID delay is
left on), one 64-entry receive ring and one transmit ring (the default ring 16) of 2 KiB buffers below the DMA limit, the descriptors
in the controller's own memory, MDIO to the BCM54213PE (PHY found at the tree's address, id logged), advertise everything,
wait for autonegotiation, program speed/duplex into the MAC, interrupt on received frames (SPI 157) plus a 10 ms
poll as a safety net. It plugs into the stack as a `net::nic::Nic` next to virtio-net. Promiscuous mode is on for now.
Not done: the address filter, interrupt-driven transmit completion, checksum offload, flow control tuning, EEE, jumbo frames.

## What qemu can and cannot test (summary)

| Can | Cannot |
|---|---|
| EL2 entry, the EL1 drop, spin-table SMP, the GIC-400, the timer | the real firmware's quirks (cache state at entry, GIC security configuration, `CNTFRQ_EL0`) |
| the mailbox, the framebuffer, the console font, the crash screens | HDMI scan-out, the LED, the real VideoCore's answers |
| SDHCI against qemu's controller, milkfs on it | EMMC2's quirks, a real card's timing, high speed |
| device tree parsing, banks and holes, 64-bit physical addresses (on virt) | a 4 GB / 8 GB board's actual tree |
| xHCI, HID, PCI enumeration (on virt) | the BCM2711 PCIe bring-up, the VL805 itself, USB 3 |
| the net stack (on virt) | GENET, the PHY, the RGMII timing |

## Known gaps

* No Wi-Fi (the CYW43455 needs an SDIO driver and its firmware blob: `brcmfmac43455-sdio.bin` and the `.txt` NVRAM
  from the Pi OS image, which must not be committed here: load them from the card at run time), no Bluetooth, no
  touchscreen (the official 7" panel is I2C plus the DSI display through the firmware), no audio (the `bcm2835` PWM/I2S
  or HDMI audio), no GPU acceleration (the VideoCore needs a mailbox/firmware protocol milkOS does not speak).
* The desktop at 1024x768 on a Pi's CPU has not been measured; if it is slow the options are the release build (the default
  for `pi-bootdir.sh`) and a smaller compositor workload.
* Power button, poweroff, reboot: the PSCI-less Pi needs the watchdog (`PM_RSTC`) or a mailbox call; not wired.
* Thermal throttling is not watched (mailbox `GET_TEMPERATURE` exists).

## Stage 2: what to do with a board, in order

1. **Boot with the screen only.** Build with `scripts/pi-bootdir.sh`, copy, power on. Expected: the green band within a second
   (the firmware has to load a 30 MB file first), then the boot log scrolling, then the splash-less desktop. If the band appears
   and the log stops, the crash screen or the last line says where; the LED flashes after a crash. If nothing appears at all:
   check the LED (on = the kernel ran), `config.txt` (arm_64bit, kernel name, no KMS overlay), the card's other files.
2. **SD card.** Look for `[sdhci] 0xfe340000: card ready`. If EMMC2 does not answer, the log says at which command; the base clock
   is logged by the mailbox (`clock_rate`). A card without a milkfs partition: the desktop runs from RAM.
3. **USB.** Look for the `[pcie]` lines (link up status word), `[pci]` (the VL805: vendor 1106, device 3483, class 0c0330), `[usb] xHCI`, then
   `port N: ...: keyboard` / `mouse`. If the link is down the first suspects are the PERST# timing, the SerDes IDDQ bit and the window
   registers (all in `rpi/pcie.rs`, each written from the Broadcom core's documented sequence); if the VL805 enumerates but the xHCI
   does not start, the `NOTIFY_XHCI_RESET` call and the DMA window are next.
4. **Ethernet.** `[genet]` lines: the version register, the PHY id (BCM54213PE answers `0x600d84a?`), link up with speed and duplex,
   then `[net] dhcp`. If the PHY answers but no frames arrive: RGMII delays (`ID_MODE_DIS`, the PHY's skew bit), the descriptor ring
   registers, promiscuous mode.
5. **Then** the interrupt-driven USB and the SD DMA, the address filter, performance work, and Wi-Fi.

What needs the owner: a **serial cable is not needed** for the order above (the screen carries the log), but if any step fails in
a way the screen cannot show (a crash before the framebuffer exists), a USB serial adapter on GPIO 14/15 with `enable_uart=1` (the
config file above already has it) gives the same log at 115200 8N1 from the very first line, and a photo of the screen when the
LED flashes is the next best thing.
