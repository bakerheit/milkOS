# milkOS on the Google Pixel 2

The port to the Pixel 2 ("walleye": Qualcomm MSM8998, the Snapdragon 835), **display only**:

* **Stage 1** (ran on the real phone): a boot image that `fastboot boot` runs and that draws on the screen: a colour test
  pattern, the boot log, a counter.
* **Stage 2** (this document's new part, **not yet run on the phone**): the whole kernel and the desktop. The interrupt
  controller (GICv3) and the timer are brought up, then the scheduler, the file system with the initramfs, user space, the
  shell, and the desktop on the phone's 1080x1920 glass with a menu bar clock that shows seconds, so a frozen phone is
  recognisable. All RAM is used, above 4 GiB too. Optionally (`msm.smp=1`) the other seven CPUs are started through PSCI.
  **There is no input** (no touch, no USB), no storage, no sound, no network, no GPU, no cellular: the desktop is shown, and
  nothing on it can be operated.

Nothing is flashed, nothing is written to any storage, the phone's own system is untouched, and a restart brings Android
back. This document says what is known about the phone, what the images do, how to run them and read the result, what was
tested and, honestly, what was not.

Files: `kernel/src/msm/` (the Pixel 2 code: `stage1.rs` the stages and their idle loops, `fb.rs` the bootloader's
framebuffer, `irq.rs` the interrupt controller and timer bring-up, `present.rs` the desktop on the glass, `mem.rs`,
`gicv3.rs` the read-only look at the GIC, `wdt.rs`), `kernel/src/gic3.rs` (the GICv3 driver), `kernel/src/gic.rs` and
`timer.rs` (shared with the other boards), `kernel/src/platform.rs` (the board table: `MSM8998`, and `MSM8998_VIRT` for
the qemu test), `kernel/src/boot.S` (the relocation), `rpikit/src/{msm,bootimg,fdtwrite,fbpresent}.rs` and the
reserved-region readers in `rpikit/src/dtb.rs` (the host-tested pure parts), `scripts/android-bootimg.py`,
`scripts/dtb_build.py`, `scripts/android-build.sh`, `scripts/android-try.sh`, `scripts/pixel2-tools-test.sh`,
`scripts/pixel2-qemu-test.sh`, `scripts/pixel2_check.py`.

## What the real run showed (stage 1, photographed)

Every line below was read off a photo of the real phone, or off what `fastboot` printed.

* **`fastboot boot` works only from a FRESH bootloader session**: the first `fastboot boot` after `adb reboot bootloader` or
  `fastboot reboot bootloader`. Any later one in the same session fails with `Failed to load/authenticate boot image: Load Error`,
  whatever the image. (The first try of the "minimal tree" image failed that way: it was a second boot in the session, so it was
  never really tested. It is built unchanged and still needs a fresh-session try.)
* **A plain, uncompressed `Image` with the stock tree is accepted ("Booting OKAY") but NOT started**: the screen goes black and the
  bootloader keeps answering (`fastboot getvar product` still works). **A gzip-compressed kernel with the stock tree
  appended IS started** by ABL: it printed the stage 1 log and ran 9 minutes (`alive 539 s`). So the default image is the gzip
  one; the plain one is only a variant for tests.
* **How to tell that the image was started**: after `Booting OKAY`, `fastboot getvar product` no longer answers and the USB
  device vanishes (the bootloader is gone). If the bootloader still answers, nothing of ours ran. `scripts/android-try.sh` does the
  whole sequence and says which it was.
* The log: RAM `0x80000000..0x100000000` in the first range (1711 MiB after 14 reserved regions of 336 MiB are taken out; banks
  `0x80000000..0x85800000`, `0x88f00000..0x8ab00000`, `0x94400000..0x94500000`, `0x99c00000..0xa1100000`,
  `0xa3a00000..0xaffff000` (197 MiB), `0xb0000000..0x100000000` (1280 MiB)); the bootloader's patched tree is at `0x84a00000`
  (485283 bytes; it differs from the stock file); the kernel image at `0x80080000`, entered at EL1, MIDR `0x51af8014`; GICv3
  (distributor `0x17a00000`, redistributors `0x17b00000`, architecture revision 3, 640 lines, 16 ID bits, A3V 0,
  SecurityExtn 1, `GICD_CTLR` 0x0), eight redistributors with affinities `0x0..0x3` and `0x100..0x103`; the counter runs at 19.2 MHz;
  the screen is 1080x1920 `bgrx`, row 4320 bytes (the first guess was right); **the real splash region in the bootloader's tree
  is `0x9d400000` + `0x2400000`** (the stock file said `0x9d600000`: always read it from the tree); `ramoops_region` `0xa1100000` +
  `0x200000`, `alt_ramoops_region` `0xa3800000` + `0x200000`, `ramoops_meta_region` `0xaffff000` + `0x1000`; the SoC watchdog at
  `0x17817000` has `EN` 0 (nobody enabled it, so it does not reset the phone). **The screen shows the pixels we write: no display
  controller flush is needed.**
* The tree said "2 ranges" of RAM but the allocator found only 524288 frames (2 GiB): stage 1's board table limited RAM to the
  first 4 GiB of the physical address space (`ram_limit`), which cut off the second range. Stage 2 has no such limit (see "RAM above
  4 GiB").

## Status

| What | State |
|---|---|
| Boot image builder (`android-bootimg.py`): header version 0, kernel with an appended device tree, tiny ramdisk, command line, `--unpack`, gzip | **tested**: header, id and sections of the phone's own boot image reproduced byte for byte (checked locally against a copy of the stock image, which is never in this repository; the fixtures in the tests are made up); the gzip layout is the one the bootloader **started** on the real phone |
| `scripts/android-build.sh`: the default image is gzip, the plain one a variant, every variant gzip, a README with the fresh-session rule and the "was it started" test | **tested** (`pixel2-tools-test`, on a made-up Image) |
| `scripts/android-try.sh IMAGE`: fresh session, `fastboot boot`, wait, "jumped" or "still answers" | **tested against stand-ins** for `adb` and `fastboot` (jump, no jump, a second boot refused, no phone, not a boot image); **never run on the phone** (nothing here touches it) |
| Stage 1: entry at EL1, relocation, RAM from the tree, colour pattern, console, counter, crash screens | **ran on the real phone** (the gzip image) and runs on qemu `virt` |
| Stage 2: GICv3, timer (tried before relied on), scheduler, file system, user space, shell | **runs** on qemu `virt` with a GICv3 (`pixel2-qemu-test`, the same code linked for virt), including a tree shaped like the phone's and a tree with a wrong timer interrupt; **not run on the phone** |
| Stage 2: the desktop on the glass (doubled, or turned, or another size), the menu bar clock with seconds | **runs** on qemu (screendumps checked: pixel doubling, the clock changing, the turned picture), the phone's glass is the same code writing the same format; **not run on the phone** |
| RAM above 4 GiB (the phone's second 2 GiB) | **runs** on qemu with 6 GiB (frames above 4 GiB written and read back); whether the phone's second range is where the tree says is **untested** |
| `msm.smp=1`: the other seven CPUs through PSCI `CPU_ON` | **runs** on qemu with four CPUs; **untested on the phone**; off by default |

## What is known about the phone

All from the real device and Google's factory image (the notes of the survey are not in the repository):

* `walleye`, MSM8998 v2.1, `qcom,msm-id = <0x124 0x20001>`, 3.7 GB RAM from `0x8000_0000`, four little cores (MPIDR `0x0` to `0x3`) and four big (`0x100` to `0x103`), GICv3
  (`arm,gic-v3`, distributor `0x17a0_0000` size `0x1_0000`, redistributors `0x17b0_0000` size `0x10_0000`, stride `0x2_0000`), the ARM generic timer, PSCI 1.0 over `smc`.
  Qualcomm's hypervisor holds EL2, so the kernel starts at EL1.
* Android 10, bootloader unlocked (`fastboot getvar unlocked: yes`), `max-download-size` 512 MiB, the bootloader is UEFI based (ABL).
* The screen is 1080x1920, an AMOLED, density 420. The bootloader's "continuous splash" framebuffer is the reserved region `splash_region` (36 MiB). The stock file puts it at `0x9d60_0000`; **the tree the bootloader really hands over says `0x9d40_0000`**: always read it from the tree (the kernel does).
* Reserved regions with a fixed address (the secure world, the modem, the DSPs: **never to be mapped or touched**): `0x8580_0000` (0x370_0000, the largest, "removed_regions"),
  `0x8ab0_0000` (0x70_0000), `0x8b20_0000` (0x1a0_0000), `0x8cc0_0000` (0x700_0000, the modem), `0x93c0_0000` (0x50_0000), `0x9410_0000` (0x20_0000), `0x9430_0000` (0xf0_0000), `0x9520_0000` (0x10_0000).
  So the free RAM below the first one is `0x8000_0000..0x8580_0000`, 88 MiB, which is where the image lives. The bootloader adds the pstore regions (`ramoops_region`, `alt_ramoops_region`,
  `ramoops_meta_region@affff000`) and the overlay adds more (`easel_mem@95c00000`...) to the tree it hands over; the kernel reads all of them at run time.
* The stock boot image: header version 0, page size 4096, `kernel_addr 0x8000`, `ramdisk_addr 0x100_0000`, `second_addr 0xf0_0000`, `tags_addr 0x100`, the kernel an LZ4-compressed arm64 Image followed by **one** device tree
  (appended, at the very end of the kernel section), 306 bytes of command line (`androidboot.hardware=walleye androidboot.console=ttyMSM0 ... androidboot.dtbo_idx=3 buildvariant=user`).
* The bootloader picks the appended tree by `qcom,msm-id` and applies an overlay from the `dtbo` partition. That partition holds 13 entries; the stock command line says `androidboot.dtbo_idx=3`, which is
  `id 0x1bc rev 0x3` (entries 0 to 5 are `id 0x1bc` with `rev` 0, 1, 2, 3, 4, 0x80, the Pixel 2's own board; entries 6 to 12 are other boards, `id` `0x190`, `0x490`, `0x590`, `0x690`, `0x790`, `0x890`, `0xa90`;
  `custom` is 0 for all of them). `python3 -I scripts/dtb_build.py dtbo-list dtbo.img` prints the table of a partition image you extracted.
* What a normal (unprivileged `shell`) user can read afterwards: `/sys/fs/pstore` is **permission denied** (checked on the phone); `/proc/last_kmsg` and `/data/vendor/...` were not checked (no adb here);
  `adb shell 'ls -l /proc/last_kmsg /sys/fs/pstore /data/vendor 2>&1'` says. Android 10 has no `last_kmsg`. So **the screen, photographed, is the only channel in stage 1**.

## What the images do

Everything in the order a person looking at the phone sees it (`kernel/src/msm/stage1.rs`, then `kernel/src/main.rs`'s `boot_rest`):

**Stage 1** (the same on the real phone and on qemu):

1. **The tree is read** (x0, no memory or device touched) and the framebuffer found: `msm.fb=` on the command line, else the tree's `splash_region`, else (only on the phone) Google's address. Four large **colour blocks** are drawn into it
   straight away, before there is a memory allocator: red top left, green top right, blue bottom left, white bottom right, each with a black border and a black plate with its name in white letters, a white 1-pixel frame on the very edge of the screen
   (a cropped side shows as a missing side), a 3-pixel black diagonal from the top left corner to the bottom right and a white one the other way (a wrong row length shears them), on a gray background. Held for **5 seconds**
   (`msm.hold=`). The pixels are written with the guessed order and row length; the writes are cached, so every written range is cleaned to memory (`dc civac`) for the display controller, which reads RAM.
2. **RAM** comes from the tree: `/memory` minus every fixed reserved region and `/memreserve/` (`rpikit::msm::carve`, whole pages), mapped as cacheable RAM; the framebuffer stays in the map but is kept from the allocator.
   Then the boot log goes on the screen as a text console (a RAM copy of the screen, sent to the real one a row range at a time in the right pixel order): the kernel's own `mem:` lines, then the banner, the exception level and MIDR/MPIDR, where the image was entered and
   linked, the tree's model, compatible, `qcom,msm-id` and command line, the RAM it listed and the reserved regions taken out, the CPUs in the tree (how many, MPIDRs, the PSCI conduit), `CNTFRQ`, the screen (size, order, row length, where and why there),
   the ramoops regions, the GIC (distributor and redistributor addresses, `PIDR2` revision, interrupt lines, `GICD_CTLR`, one line per redistributor with its affinity and whether it is this CPU's), the watchdog registers, the options in effect and every option that was refused. A green band runs along the top edge.
3. With `msm.stage=1` (the `-stage1` image) the boot ends here in **a calm idle loop**: the last line of the screen counts `alive N s` once a second, and a white marker moves along the green band. A frozen phone is recognisable. The kernel polls the
   counter: **no interrupt is enabled**, the GIC is only read. (This is also where stage 2 stops, with its reason on the screen, when its first step fails.)

**Stage 2** (the default; the rest of `kmain` that the other boards run, in `boot_rest`):

4. `stage 2: interrupts and the timer, ...`, then the desktop's surface (`present: desktop 540x960 -> turn 0 clockwise -> 540x960 -> stretch x2.000 y2.000 -> glass 1080x1920 bgrx stride 4320`: see "The desktop on the glass").
5. **The interrupt controller and the timer** (`kernel/src/msm/irq.rs`, "Interrupts and the timer" below): `irq:` lines say which interrupts the tree names for the timers, the `[gic] v3 up` line, what each timer tried did, and
   `irq: scheduler tick on the virtual timer, INTID 19`. If this step fails the log says `stage 2 STOPPED: <reason>` and the image keeps counting, as stage 1 does.
6. The rest of the kernel, as on any board, minus what the phone has none of: the quick memory self-test (`selftest: ... high memory ok` when RAM is above 4 GiB), the file system (the initramfs, about 17 MB, unpacked into a tmpfs), the scheduler, `[smp] one CPU`
   (or `msm.smp=1`), the audit log, the network stack with only a loopback (`no NIC`), no sound device, no USB (`[usb] none yet on the Pixel 2`), no virtio (`[virtio] no devices`). **No device the SoC owns is probed by address**: the clock
   (the PL031 RTC of qemu), fw_cfg, the UART and the PCIe host that the other boards look for are skipped on the phone, because those addresses are Snapdragon registers.
7. **The log holds** for `msm.logsecs=` seconds (5) with the counter on its last line, *before* user space starts (a user program may clear the console it shares with the kernel's log).
8. User space starts (init, the services, the shell); `desktop: starting at N s`; the desktop takes the screen: a menu bar with the clock **with seconds** (a menu bar redrawn once a second: the proof that the machine has not frozen), the Welcome window, the dock.

One CPU runs unless `msm.smp=1`. No UART. No storage is touched. **The kernel never powers the phone off or resets it** (Shut down and Restart in the desktop log that they are not done on the phone and return). The only secure-world calls are PSCI's
(`PSCI_VERSION` with `msm.psci=1`, `CPU_ON` with `msm.smp=1`). A crash (a panic or any exception) puts the end of the log, in big letters, on the screen (`milkOS stopped. Photograph this screen.`), and the CPU waits; this works with the desktop up too.

**Watch for the watchdog.** The Snapdragon has an application processor watchdog that Linux pets from the moment its driver loads (bark 11 s, bite 14 s in Google's tree). **On the real phone it was found switched off** (`EN` 0): the 9 minute run of stage 1 was never reset.
`wdt: ... EN 0x1` in the log would mean otherwise; `msm.wdt=pet` (the `-wdt` image) pets it once a second, one register write.

## Running the experiment

```
scripts/android-build.sh OUTDIR --stock-dtb PATH/TO/walleye-stock-base.dtb      # or without --stock-dtb: the small tree
```

writes into `OUTDIR` the kernel (`Image`, 29.9 MB in a release build: the whole kernel, 17.7 MB of user programs and initramfs, the art), the boot images (gzip-compressed kernels, see below) and a `README.txt` with these commands. The phone: unlocked bootloader,
`adb` and `fastboot` from the platform tools. **Nothing in this repository talks to the phone**: you run the commands.

```
scripts/android-try.sh OUTDIR/milkos-pixel2.img
```

does the whole sequence and says what happened (use `--wait SECS` for another wait than 20 s, `--no-reboot` when the phone is already in a fresh bootloader session, `--serial S` for more than one device). By hand, it is:

```
adb reboot bootloader                    # or: fastboot reboot bootloader, or: power off, hold power + volume down
fastboot boot OUTDIR/milkos-pixel2.img   # loads into RAM and runs; writes nothing
fastboot getvar product                  # 20 seconds later
```

**The rules the real phone taught:**

* `fastboot boot` works only for the **first boot of a bootloader session**. A second one (in the same session, with any image, after a success or a failure) is refused with `Failed to load/authenticate boot image: Load Error`. Every try starts with a reboot into the bootloader.
* **Was the image started?** After `Booting OKAY`: if `fastboot getvar product` **still answers** (and `fastboot devices` still lists the phone), the bootloader accepted the image and did **not** start it (the plain image does this: black screen, a bootloader that goes on answering).
  If it **no longer answers** and the USB device is gone, the bootloader jumped: what is on the screen is milkOS or its crash screen. `android-try.sh` prints `RESULT: ... JUMPED` (exit 0) or `... STILL ANSWERS` (exit 3); a refused image is exit 2, no phone exit 4.
* **To get back to Android: hold the power button for 10 to 15 seconds** (the phone restarts). If it is still in the bootloader, `fastboot reboot` does the same.

What you should see, in this order:

1. The bootloader's own screens for an unlocked phone (a warning, a few seconds): not ours.
2. The **colour pattern**, 5 seconds: top left red, top right green, bottom left blue, bottom right white, each labelled with its colour name in white on black, black borders, a white line round the edge of the screen,
   a black diagonal from the top left to the bottom right, a white one the other way (confirmed right on the real phone).
3. Dark text (the boot log) with the green band at the top, and now much more of it than in stage 1: `mem:`, `milkOS`, `cpu:`, `image:`, `dtb:`, `ram:`, `cpus:`, `timer:`, `screen:`, `ramoops:`, `gic:` (read only, before it is touched), `wdt:`, `options:`, then
   `stage 2:`, `present:`, `display:`, `irq:` and `[gic] v3 up`, `[timer]`, `selftest:`, `fs:`, `[sched] up`, `[smp] one CPU`, `[net]`, `[audio]`, then `stage 2: the log stays up for 5 s` with `alive 0 s ... alive 4 s` on the last line and the white marker moving along the band.
4. A burst of user space lines (`[login]`, `[proc]`, `milkOS init`, the services) and, at once, **the desktop**: the menu bar with its clock showing seconds (`Thu 00:00:31`: the date is wrong, the phone has no clock the kernel can read; the seconds are the point), the "Welcome to milkOS" window and the dock.
   It is 540 x 960 of the desktop's own pixels shown doubled on the 1080 x 1920 glass (see "The desktop on the glass"). **The clock must keep going.** Nothing can be operated.

**What to photograph**: the pattern (2), the log while the counter runs (3: two photographs, a few seconds apart, if the screen is mostly full of text, `-log30` holds it for 30 s), the desktop twice a few seconds apart (the clock's seconds must differ), and whatever is on
the screen if something else happens (a `stage 2 STOPPED` text or a `milkOS stopped` crash screen). Say what `fastboot` printed and what `android-try.sh` said. If the screen shows the bootloader's picture and the bootloader still answers: the image was not started.

The same kernel with other command lines (the images `build` writes; or `--add-cmdline` to `scripts/android-bootimg.py`). **All are gzip images** except `-plain`:

| Image | Command line added | For |
|---|---|---|
| `milkos-pixel2.img` | | the first try (the stock tree if `--stock-dtb` was given, else the small tree) |
| `milkos-pixel2-plain.img` | | the same kernel uncompressed: the bootloader accepts it and does not start it (tests only) |
| `milkos-pixel2-minimal.img` | | (with `--stock-dtb`) the small tree instead of the stock one; never really tried (see "What the real run showed") |
| `-stage1` | `msm.stage=1` | stop after the log and count, no interrupts, no desktop (what stage 1 did, with the 30 MB kernel behind it): tells "the bigger image is started" apart from "stage 2 works"; a sensible try before the default image |
| `-log30` | `msm.logsecs=30` | the boot log stays up for 30 seconds before user space |
| `-smp` | `msm.smp=1` | also start the other seven CPUs through PSCI (try it only after the plain one showed the desktop) |
| `-turn` | `msm.rotate=90` | the desktop landscape, turned onto the portrait glass: turn the phone (see "The desktop on the glass") |
| `-desk720` | `msm.desk=720x1280` | a 720x1280 desktop stretched 1.5 times instead of 540x960 doubled: more of every window fits (the Welcome window's cards are cut with "..." at 540), the text is a little softer |
| `-phystimer` | `msm.timer=phys` | the physical timer for the tick instead of the virtual one (the default image tries the virtual one first and says what it did) |
| `-pattern` | `msm.hold=inf` | the pattern stays up, no text: to look at the colours |
| `-rgbx`, `-xrgb`, `-xbgr` | `msm.fmt=...` | the other 32-bit pixel orders (the real phone is `bgrx`: the default) |
| `-kick` | `msm.kick=ctl0 msm.hold=3` | refresh the panel by hand (see "If the screen does not change"; the real phone needed no refresh) |
| `-mdp` | `msm.mdp=dump msm.hold=3` | read display controller registers into the log |
| `-wdt` | `msm.wdt=pet` | keep the watchdog quiet (it was found off) |

### Options (kernel command line, `msm.<name>=<value>`; the other boards' `milk.<name>=<value>` options work too, for what the phone has)

| Option | Does |
|---|---|
| `msm.stage=1` / `2` (default) | stop after the stage 1 log and count / go on to the whole kernel and the desktop |
| `msm.logsecs=N` | seconds the boot log stays up with the counter before user space starts (default 5) |
| `msm.desk=WxH` | the desktop's size in its own pixels (default: half the glass, 540x960; 320 to 4096); it is turned and stretched onto the glass |
| `msm.rotate=0` (default) / `90` / `180` / `270` | turn the desktop clockwise on the glass (90: landscape; the phone is turned to read it) |
| `msm.timer=auto` (default) / `virt` / `phys` | which timer ticks. `auto` tries the virtual timer, then the non-secure physical one, and keeps the first whose interrupt the GIC raises |
| `msm.smp=1` | start the other CPUs through PSCI `CPU_ON` (`smc`); default one CPU |
| `msm.fmt=bgrx` (default), `rgbx`, `xrgb`, `xbgr`, `rgb888`, `bgr888`, `rgb565` | the framebuffer's pixel layout, bytes in memory order (`bgrx` = the little-endian word `0x00RRGGBB`, DRM XRGB8888 / ARGB8888); the extra byte is written `0xff` |
| `msm.stride=N` | bytes per row (default width times bytes per pixel, 4320) |
| `msm.size=WxH` | the picture's size (default 1080x1920) |
| `msm.fb=ADDR[:SIZE]` | the framebuffer's physical address (and the size of the region), instead of the tree's |
| `msm.hold=N` / `inf` | seconds the pattern stays up (default 5); `inf`: for good, no console |
| `msm.scale=1..4` | magnification of the console font (default 2: 67 columns) |
| `msm.kick=ctlN` | write 1 to CTL N's `CTL_START` ten times a second (the one register write the image can do to the display controller) |
| `msm.mdp=dump` | read (never write) the control path and interface registers, log them |
| `msm.wdt=pet` | pet the watchdog (one register write) once a second |
| `msm.psci=1` | ask PSCI for its version (one `smc`) |
| `msm.crashtest=panic` / `abort` / `early` / `pre` | stop on purpose, to look at the crash screen: a panic, a data abort, a panic before the console exists, a panic before memory is set up (the log is then drawn glyph by glyph straight into the framebuffer) |

A value that is wrong is logged (`option: msm.fmt: not one of ...`) and the default is used.

## Interrupts and the timer

The GICv3 driver (`kernel/src/gic3.rs`; the handler table, the counters and the numbering stay in `gic.rs`, shared with the GICv2 boards) follows what Linux's `irq-gic-v3` does for a non-secure kernel under a secure monitor, because that is what this GIC is known
to accept, and **touches only what it needs** (the distributor's SPIs up to INTID 287, this CPU's redistributor, this CPU's interface); every poll is bounded (100 ms; the redistributor's wake-up 200 ms) and every step that can fail says which one did:

1. `ICC_SRE_EL1.SRE = 1` (read back: the hypervisor must allow the system register interface);
2. distributor: `GICD_CTLR` to 0 (wait for `RWP`), SPIs configured (Group 1, level triggered, the default priority 0xa0, disabled, not pending, not active), then `ARE_NS | EnableGrp1` (read back: affinity routing must be on) and every SPI routed to the boot CPU;
3. this CPU's redistributor, found by its affinity in `GICR_TYPER` (frames walked from the tree's second `reg` range: the phone has eight), `ProcessorSleep` cleared and `ChildrenAsleep` waited out, its SGIs and PPIs Group 1, disabled, default priority;
4. the CPU interface: priority mask 0xf0, end of interrupt both drops the priority and deactivates (`EOImode` 0), Group 1 on.

The first run on the phone is the first time any of this is read or written on the real SoC (stage 1 only read the identification registers). If a step disagrees the log says (`[gic] GICv3 setup failed: ...`) and the screen shows `stage 2 STOPPED: ...` and goes on counting.

**The timer is tried before it is relied on** (`kernel/src/msm/irq.rs`, `timer::select_msm`). The tree's `arm,armv8-timer` node names four interrupts; in Google's tree they are PPIs 1, 2, 3, 0, that is INTIDs **17 (secure physical), 18 (non-secure physical), 19 (virtual), 16 (hypervisor)** (not the 29, 30, 27, 26 an ARM
reference design has). The kernel is an EL1 kernel under Qualcomm's hypervisor, which makes the virtual timer the natural tick (Linux does the same). Instead of trusting that, with the CPU's interrupts masked it arms the virtual comparator 1 ms ahead and watches the GIC: the timer's
own `ISTATUS`, and which private interrupts became pending (`GICR_ISPENDR0`). The log has one line per timer tried: `irq: virtual timer, INTID 19: condition met, private interrupts pending in the GIC 0x00080000` is the expected one (bit 19). A timer that fires but whose INTID the GIC does not show is skipped (then the physical timer is tried, `CNTP_*`
with the comparator converted for `CNTVOFF`); a timer whose interrupt shows up under exactly one other number is used under that number (`INTID 19 ... pending 0x08000000` then `irq: scheduler tick on the virtual timer, INTID 27`). If neither timer produces an interrupt the
step fails with `no timer interrupt reaches this CPU`. Possible surprises: a hypervisor that traps the physical timer registers (an exception dump with ESR class 0 or 0x18 right after the first `irq:` line), a timer that fires but is Group 0 or secure (condition met, nothing pending).

## The desktop on the glass

The desktop (the window system, unchanged) draws a **logical** picture in cached RAM; `kernel/src/msm/present.rs` turns it by `msm.rotate=`, stretches it to the glass's raster with the area-weighted converter the Raspberry Pi port uses (`rpikit::fbpresent`, the same tests), and
packs the changed rows into the bootloader's framebuffer in its byte order, cleaning the cache lines for the display controller. What was chosen and why:

* **Portrait, not turned, at half the glass: 540 x 960 of the desktop's own pixels, each shown as a 2 x 2 block.** The window system runs at every size the Pi port tried (640x480 to 1920x1080) and a portrait one works (checked on qemu: the menu bar, the dock, the Welcome window all fit; the window is narrow, its cards' text is cut with
  "..."). The phone's glass is 440 pixels per inch: a 16 pixel high character is a millimetre tall at 1:1, unreadable, and 2:1 makes it about 2 mm, the size of a phone's text. An exact 2x is a pixel doubling (no blur), and the converter keeps flat colours exact.
* **No rotation by default**, because the glass is portrait and the owner would have to turn the phone, and the desktop made for landscape then wastes less. `msm.rotate=90` (the `-turn` image) is the alternative: a 960 x 540 landscape desktop, turned clockwise onto the glass (its menu bar is on the glass's right edge),
  which reads upright with the phone on its left side (turn it anticlockwise). `msm.desk=720x1280` shows the desktop larger (a stretch of 1.5, slightly soft).
* The boot log stays portrait, at the glass's own size, whatever the desktop does. A crash screen draws straight on the glass and is not turned.
* Settings' "resolution" is refused (the size is fixed at boot). The menu bar clock shows seconds on this board only.

## RAM above 4 GiB

The tree lists two ranges (`/memory`) and the phone has about 3.7 GiB: 2 GiB at `0x8000_0000` and the rest above 4 GiB (probably `0x1_0000_0000..0x1_8000_0000`, to be read in the log). Stage 1's board table cut RAM at 4 GiB (`ram_limit`), so only the first range was used; stage 2's has no limit. The allocator covers
one span from the first bank to the last (the Pi port's way), the page tables map every bank that exists and nothing between, the gaps (the reserved regions) are marked used. The log shows `mem: RAM ...` and the banks, and `selftest: high memory ok (RAM to 0x...; 3 frames at 0x1... written, read back, freed)`:
**that line proves the second range works** (the self-test writes patterns into frames above 4 GiB and reads them back; if the range is not where the tree says, it stops with an exception dump at that point).

## More CPUs

`msm.smp=1` starts CPUs 1 to 7 through PSCI `CPU_ON` (function `0xc4000003`, conduit `smc` from the tree; target `MPIDR` from the tree's `/cpus`, the entry point the secondary stub in `smp.S`, the context a block with the page-table registers). Each starts at EL1 with the MMU off, switches it on through the
boot identity map of the second GiB, finds its own redistributor, brings up its interface and its timer, and joins the scheduler. Each is waited for one second and reported (`[smp] cpu 3 up`, `[smp] 8 of 8 CPUs online`, or `did not come up`). **Off by default** until it has run on the phone: the first try of stage 2 is one CPU.
The four big cores and four little ones have different affinities (`0x100..` is Aff1 = 1); SGIs (the reschedule IPI) are addressed by affinity.

## What does not exist yet

Touch and any other input (the first thing missing from a phone), USB, storage (UFS, the SD slot), sound, networking (WiFi, Bluetooth), the GPU, the camera, cellular, the battery and charging, the real-time clock (the desktop's clock counts from a fixed date: only the seconds mean something),
power off and restart (the desktop's buttons log that they do nothing), the panel's own brightness, a UART. The entropy pool is seeded from timer jitter only (the log says so with a warning).

## How the bootloader loads it

* **Format (verified on the real phone).** A boot image header version 0 whose kernel section is the arm64 `Image` (its 64-byte header: `b _real_start`, `text_offset 0x80000`, `image_size`, flags 2, `ARM\x64`) **gzip-compressed, followed by the device tree** (the layout of
  `Image.gz-dtb`; the stock one is LZ4). **ABL starts that.** A plain uncompressed Image with the tree after it (at `image_size`, the bytes between zero) is accepted ("Booting OKAY") **but not started**: the phone's bootloader goes on answering `fastboot`.
  So `scripts/android-build.sh` writes gzip images (level 9 through Python's `gzip`, no timestamp, reproducible) and keeps the plain one only as `milkos-pixel2-plain.img`. (Why ABL ignores a plain Image is not known: the guess that it wants the compressed kernel's magic and decompresses it itself is only a guess.)
* **Size.** `fastboot boot` takes images up to `max-download-size` (512 MiB) and ABL accepted a 33 MB image: the phone's own boot image, whose LZ4 kernel is 14.8 MB and **decompresses to 36.1 MB** (its Image header says 40.2 MB with `.bss`; measured with a small LZ4 decoder on the copy of the stock image, nothing of it is in
  the repository), plus a 10 MB ramdisk. Stage 2's kernel is the whole system, 17.7 MB of it the initramfs (the user programs, fonts, music, icons): **29.9 MB uncompressed (`image_size` 30.1 MB), 12.5 MB as one gzip stream, 12.9 MB for the whole boot image**, so smaller than the stock kernel in both forms; ABL's real limits
  are not documented, but a kernel bigger than ours is known to be loaded and started. The decompressed kernel at `0x8008_0000` runs past the ramdisk's address (`0x8100_0000`, a 512 byte empty archive) and the second-stage address: as the stock one does, which is 36 MB. The tree is at `0x84a0_0000`, the first free bank is 88 MiB: 30 MB fits with room to spare, and the kernel
  panics with a clear message if its bank is too small (a reserved region right behind it).
* **Where it is put.** `kernel_addr 0x8000` plus the start of RAM gives `0x8000_8000`; the arm64 convention is `0x8008_0000` (`text_offset`). **ABL used `0x8008_0000`.** The image is linked for `0x8008_0000` (kernel/build.rs) and `boot.S` handles all of
  them: entered anywhere else, it works out where the tree and the image end, **copies the tree and a small copy routine above everything (MMU off, caches cleaned first), moves the image** (forwards or backwards, so overlapping places are fine) to `0x8008_0000`,
  invalidates the instruction cache and carries on from the same place in the copy. The log says where it was entered (`image: ... entered at 0x80008000` and `boot.S moved it`). Qemu tests four such entries (above and below, near enough to overlap or not).
  The copy needs free RAM above the image: the first 88 MiB are free, the image is 30 MB. (On the real phone the image was entered right where it is linked, `0x8008_0000`, and nothing moved.)
* **At entry** (verified) the MMU and caches are off, the kernel is at EL1 (Qualcomm's hypervisor holds EL2), x0 is the patched tree (at `0x84a0_0000`, 485283 bytes, not the stock file) and only the core with MPIDR affinity level 0 equal to 0 runs.
* **The tree.** The bootloader needs an appended tree with the right `qcom,msm-id`; it applies the dtbo overlay and patches `/memory`, `/chosen/bootargs` and the reserved regions it knows. `--stock-dtb` copies the phone's own tree unchanged
  (the safe first try; it is yours, never in the repository). The small tree (`dtb_build.py minimal`) has the root properties, `/memory`, `/chosen`, eight CPUs and PSCI, the eight fixed reserved regions and the splash region, the GIC, the timer and the display controller's `reg`;
  the overlay probably expects more (it refers to nodes of the base tree by label) and ABL may drop it or refuse the tree: that is part of what the first runs find out.

## If the screen does not change

The colour pattern is written into the bootloader's framebuffer in RAM. Three things can make it not show:

1. **The pixel order or row length is wrong**: the picture is there but wrong (swapped colours, a sheared diagonal). Try `-rgbx`, `-xrgb`, `-xbgr`, `msm.stride=`, `msm.size=`; the log says what was assumed.
2. **The panel needs a refresh** (a command-mode panel only shows a frame when the display controller is told to send one; the bootloader's own picture stays, ours never appears). The `-kick` image writes 1 to `CTL_START` of control path 0 (`0xc90_0000` plus
   `qcom,mdss-ctl-off[0]` = `0x2000`, register `0x1c`, from the tree's `qcom,mdss_mdp` node), ten times a second, after logging `TOP` and `FLUSH`. **It writes a register of the display controller**, so read this first: the registers are those of the legacy MDSS driver
   (`CTL_TOP 0x14`, `CTL_FLUSH 0x18`, `CTL_START 0x1c`), written from memory of that driver, not from a manual; which control path drives the panel is a guess (the `-mdp` image reads the four interfaces' timing engine and frame counter, and each control path's `TOP` and `FLUSH`: a counter
   that moves between its two readings means a video-mode panel that needs no kick, a `TOP` that names an interface says which path is live).
   It also reads the source pipes the bootloader set up (`SRC0_ADDR`, `SRC_SIZE`, `OUT_SIZE`, `SRC_YSTRIDE0`, `SRC_FORMAT`, `SRC_UNPACK_PATTERN`, the register layout of Linux's mdp5 driver) and logs, for each pipe that has an address, **where it scans out, how long its rows are and
   in what byte order**: the real answer to the pixel order and row length questions, with a suggested `msm.fmt=` and `msm.stride=`. (The component codes are 0 = G, 1 = B, 2 = R, 3 = A and element 0 is taken to be the first byte in memory; compare with what the pattern shows.) The sensible order is `-mdp` first (read only), then `-kick` with the control path it points at (`msm.kick=ctlN`).
3. **The display is off** when the kernel starts (the bootloader switched it off at the hand-over; the splash only continues if it said so in the tree, node `qcom,mdss_fb_primary`, `qcom,cont-splash-memory`). Then there is nothing to draw on until a display driver brings up the DSI link and the panel from scratch
   (the DSI PHY and PLL, the controller, the panel's own init sequence, which is vendor specific and not known here). That is the next big piece of work and needs the owner's input on how far to go; until then **the only record is whatever else survives**: the RAM log region
   (`ramoops`, below) and the phone's own behaviour (a reset after 15 to 30 s is the watchdog, see above).

**ramoops.** The tree has the regions `ramoops_region`, `alt_ramoops_region` and `ramoops_meta_region` (Linux's pstore, which the bootloader fills in); the log names them (`ramoops: ramoops_region 0x... +0x...`). A later stage can write the log there in pstore's format so that
Android shows it as `/sys/fs/pstore/console-ramoops-0` after a reboot, which needs root to read (the owner has none right now) and a warm reset that leaves RAM alone. Nothing writes to those regions now, and the RAM list leaves them out.

## Reading the result

| You see | Means |
|---|---|
| nothing new: the bootloader's screen stays | the image was not started (the phone would normally return to the bootloader or restart), or it started and the display shows its old buffer: try `-mdp`, `-kick`; send the bootloader's text if it printed any |
| black screen | the display went off at the hand-over, or the image never ran; the phone restarting by itself after 15 to 30 s (the watchdog) suggests it ran |
| the pattern, colours right, upright | the guess (1080x1920, 4 bytes, `B G R x`, rows packed) is right: the whole display side of stage 1 is confirmed |
| the pattern with red and blue swapped | `-rgbx` |
| the pattern in odd colours | one of `-xrgb`, `-xbgr`, or a 16- or 24-bit layout (`msm.fmt=rgb565`, `rgb888`...) |
| diagonals bent or in steps, the picture sheared, doubled or cut | the row length (or the size): `msm.stride=`, `msm.size=`; the white frame and the diagonals show how far off it is |
| the pattern then the log, `alive` counting, and it stays so | stage 1 works (`-stage1` does only this); with the default image: the log shows `stage 2 STOPPED:` and a reason (see below), or `msm.stage=1` is set |
| the pattern, the log up to a `[gic]` or `irq:` line, then a frozen screen (the counter row not counting) | a hang in the interrupt controller or the timer set-up: the last line says which step; photograph it; `-stage1` still works |
| the log, `alive` counting for 5 s, then user space lines, then the desktop with a ticking clock | **stage 2 works** |
| the desktop appears but the clock's seconds do not change | the tick or the scheduler stopped after the desktop's first frame (the compositor draws once a second): `irq:` lines say which timer was used; try `-phystimer` |
| `stage 2 STOPPED: no timer interrupt reaches this CPU` | neither timer's interrupt reached the GIC: the two `irq:` timer lines say what each did (`condition NOT met`: the timer did not run, access trapped or masked; `condition met` with nothing pending: the interrupt is not wired to this redistributor, or not Group 1 / secure) |
| `stage 2 STOPPED: ICC_SRE_EL1.SRE will not turn on` | the hypervisor does not give EL1 the GICv3 system registers: stage 2 cannot use interrupts; photograph and report |
| `stage 2 STOPPED:` anything else with `GIC` in it | the register the text names disagreed with the sequence in "Interrupts and the timer" |
| an exception dump (`milkOS stopped`) during stage 2 | a fault: the first lines say where (ELR) and what (ESR); `selftest: high memory` just before it means the second RAM range is not where the tree says |
| the desktop is landscape and sideways | that is `-turn`: turn the phone anticlockwise |
| the pattern, then the phone restarts | a fault in the log, or the watchdog (found off on the real phone): try `-wdt`, `-stage1` |
| "milkOS stopped" or an exception dump | a crash: photograph it, the first lines say where (ELR) and what (ESR) |

## Tests

* `scripts/pixel2-tools-test.sh` (29 tests, no VM, no phone): the boot image header and sections against the layout of the real header, the id, the tree at `image_size`, gzip, refusals; the tree writer and reader (random trees round trip, the Rust writer's golden bytes, the small tree's contents, the dtbo table, the qemu test tree with its GIC and timer);
  **`android-build.sh` on a made-up Image** (the default image is gzip with the stock tree after the stream, the plain one a variant, every variant gzip with its option, the README's rules); **`android-try.sh` against stand-ins for `adb` and `fastboot`** (a phone that jumps, one that stays in the bootloader, a second boot in a session
  refused with its explanation, no phone, a file that is not a boot image).
  With `MILK_PIXEL2_STOCK` naming a folder with `boot.img` and `dtbo.img` extracted from your own phone, the stock header is also repacked to the identical bytes and its id reproduced.
* `rpikit-unit` (`scripts/rpikit-test.sh`): the same header and tree layouts in Rust (`bootimg`, `fdtwrite`), the options (the new ones too), pixel orders and row packing, the pattern, RAM carving, the reserved region and `reg` readers with the right cell sizes, GICv3 register decoding, the timer's interrupt specifiers, `GICD_IROUTER` and
  `ICC_SGI1R_EL1` values for the phone's eight affinities, finding a CPU's redistributor, the default desktop size; the Pi's present transform tests cover the conversion.
* `scripts/pixel2-qemu-test.sh` (about 5 to 8 minutes: it builds five whole kernels and boots a dozen machines, several at once): the kernel on qemu `virt` with a GICv3 and ramfb, see its header. Stage 1 (`msm.stage=1`): the pattern and the console in screendumps, the counter and the band moving, three crash screens, four relocations, a tree shaped like the phone's with sentinel words in reserved regions and the
  framebuffer's memory read back (also as rgb565 with another row length), and the paths that must degrade safely. Stage 2: the default boot to the desktop on 6 GiB (RAM above 4 GiB written and read back, the GICv3 and the virtual timer chosen, the shell, the desktop doubled onto the glass: every 2x2 block one colour, the clock changing between two dumps), the turned desktop, a 720x1280 desktop,
  four CPUs through PSCI, the physical timer, a tree with a wrong timer interrupt (found by trying), the phone-shaped tree all the way to the desktop with the sentinel words untouched and the desktop in the splash region's memory, and a GICv2 machine where stage 2 stops with its reason and the counter keeps going.

## Not tested, ranked by risk

For the stage 2 images (the stage 1 questions were answered by the real run: the gzip image starts, the screen shows RAM writes, `bgrx` with 4320 bytes a row, the watchdog is off).

1. **The GICv3 on the real SoC** (`kernel/src/gic3.rs`): the system register interface may be refused (`ICC_SRE_EL1`), the redistributor may not wake, a non-secure write to `IGROUPR` may not do what it does on qemu, the distributor's `ARE_NS` may not stick. The sequence is Linux's and every step is checked and bounded, but this kernel has never written those registers there.
   If it fails, stage 2 says which step on the screen and stays in stage 1.
2. **That the timer's interrupt reaches the CPU**: the tree's INTIDs (17 to 19 in Google's file, which the bootloader's patched tree may change), Group 1 versus secure, the hypervisor's view of the virtual and physical timer. The kernel tries the timer and adapts to what the GIC shows (see above), but if no timer works there is no scheduler.
3. **RAM above 4 GiB** is where the tree says (`selftest: high memory ok` says; if not, an exception dump right there).
4. **The whole kernel touches far more memory and more of the system than stage 1 did.** A protected region that the tree does not list as reserved would now be mapped and used (Linux would map it too); the audited list of what is *not* touched on the phone (the PL031 clock, fw_cfg, the UART, the PCIe host, virtio) is in `platform.rs` (`PHONE`). A bus error from an
   unlisted device would show as an exception dump (or, if the secure world takes it, a reset).
5. **`wfi` and the idle loop under Qualcomm's hypervisor** (the idle thread waits for interrupts; stage 1 never executed `wfi`), and **cache maintenance by the desktop's presents** (a full frame is 8 MB of `dc civac`): both ordinary, neither tried there.
6. **`msm.smp=1`**: PSCI `CPU_ON` for CPUs that the firmware may have parked in a state that does not accept it, the secondary entry's use of the boot identity map in the second GiB, per-CPU redistributors. Off by default.
7. The entropy is jitter only (the log warns). Fine for a display-only machine.
8. The register offsets of the `msm.kick`, `msm.mdp` and `msm.wdt` options (from the legacy MDSS and `msm_watchdog` drivers, from memory); all three are off unless asked, and the real run showed no kick is needed.

## What comes after

The log of the first stage 2 run decides the order. Likely: input (the touch controller, then USB), a pstore writer for the log (`ramoops` regions the bootloader leaves), the real display path (so the desktop can power the panel itself: brightness, off/on), storage (UFS) and the other CPUs on by default once `msm.smp=1` has been seen to work.
