# milkOS on the Google Pixel 2

Stage 1 of the port to the Pixel 2 ("walleye": Qualcomm MSM8998, the Snapdragon 835): **a boot image that
`fastboot boot` can run and that draws on the screen.** Nothing is flashed, nothing is written to any storage, the
phone's own system is untouched, and a restart brings Android back. This document says what is known about the phone,
what the image does, how to run it and read the result, what was tested and, honestly, what was not: nothing here has
run on the real phone yet.

Files: `kernel/src/msm/` (the Pixel 2 code), `kernel/src/platform.rs` (the board table: `MSM8998`, and `MSM8998_VIRT`
for the qemu test), `kernel/src/boot.S` (the relocation), `rpikit/src/{msm,bootimg,fdtwrite}.rs` and the reserved
region readers in `rpikit/src/dtb.rs` (the host-tested pure parts), `scripts/android-bootimg.py`,
`scripts/dtb_build.py`, `scripts/android-build.sh`, `scripts/pixel2-tools-test.sh`, `scripts/pixel2-qemu-test.sh`,
`scripts/pixel2_check.py`.

## Status

| What | State |
|---|---|
| Boot image builder (`android-bootimg.py`): header version 0, kernel with an appended device tree, tiny ramdisk, command line, `--unpack` | **tested**: header, id (SHA-1 of the contents) and sections of the phone's own boot image are reproduced byte for byte (checked locally against a copy of the stock image, which is never in this repository; the fixtures in the tests are made up) |
| Device trees (`dtb_build.py`): a small writer and reader, the small Pixel 2 tree, the stock tree copied unchanged, the dtbo table listing | **tested** (golden bytes shared with the Rust writer, round trips, hostile input); whether the *bootloader* takes the small tree is **untested** |
| `msm8998` platform: entry at EL1 with the MMU off and a tree in x0, relocation to the linked address, RAM from the tree minus its reserved regions, polled timer, GICv3 identification (read only) | **runs** on qemu `virt` (`msm-virt-test`, the same code linked for virt), including a tree shaped like the phone's; the SoC's own registers are **untested** |
| The colour pattern, the console, the counter, the crash screens, every pixel order | **runs** on qemu (screendumps and memory read back are checked); the phone's pixel order, row length and whether a write to that RAM reaches the glass are **untested** |
| Boot on the phone | **not run** |

## What is known about the phone

All from the real device and Google's factory image (the notes of the survey are not in the repository):

* `walleye`, MSM8998 v2.1, `qcom,msm-id = <0x124 0x20001>`, 3.7 GB RAM from `0x8000_0000`, four little cores (MPIDR `0x0` to `0x3`) and four big (`0x100` to `0x103`), GICv3
  (`arm,gic-v3`, distributor `0x17a0_0000` size `0x1_0000`, redistributors `0x17b0_0000` size `0x10_0000`, stride `0x2_0000`), the ARM generic timer, PSCI 1.0 over `smc`.
  Qualcomm's hypervisor holds EL2, so the kernel starts at EL1.
* Android 10, bootloader unlocked (`fastboot getvar unlocked: yes`), `max-download-size` 512 MiB, the bootloader is UEFI based (ABL).
* The screen is 1080x1920, an AMOLED, density 420. The bootloader's "continuous splash" framebuffer is the reserved region `splash_region@9d600000` (36 MiB, `0x9d60_0000`).
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

## What the image does

Everything stage 1 does, in the order a person looking at the phone sees it (`kernel/src/msm/stage1.rs`):

1. **The tree is read** (x0, no memory or device touched) and the framebuffer found: `msm.fb=` on the command line, else the tree's `splash_region`, else (only on the phone) Google's address. Four large **colour blocks** are drawn into it
   straight away, before there is a memory allocator: red top left, green top right, blue bottom left, white bottom right, each with a black border and a black plate with its name in white letters, a white 1-pixel frame on the very edge of the screen
   (a cropped side shows as a missing side), a 3-pixel black diagonal from the top left corner to the bottom right and a white one the other way (a wrong row length shears them), on a gray background. Held for **5 seconds**
   (`msm.hold=`). The pixels are written with the guessed order and row length; the writes are cached, so every written range is cleaned to memory (`dc civac`) for the display controller, which reads RAM.
2. **RAM** comes from the tree: `/memory` minus every fixed reserved region and `/memreserve/` (`rpikit::msm::carve`, whole pages), mapped as cacheable RAM; the framebuffer stays in the map but is kept from the allocator.
   Then the boot log goes on the screen as a text console (a RAM copy of the screen, sent to the real one a row range at a time in the right pixel order): the kernel's own `mem:` lines, then the banner, the exception level and MIDR/MPIDR, where the image was entered and
   linked, the tree's model, compatible, `qcom,msm-id` and command line, the RAM it listed and the reserved regions taken out, the CPUs in the tree (how many, MPIDRs, the PSCI conduit), `CNTFRQ`, the screen (size, order, row length, where and why there),
   the ramoops regions, the GIC (distributor and redistributor addresses, `PIDR2` revision, interrupt lines, `GICD_CTLR`, one line per redistributor with its affinity and whether it is this CPU's), the watchdog registers, the options in effect and every option that was refused. A green band runs along the top edge.
3. **A calm idle loop**: the last line of the screen counts `alive N s` once a second, and a white marker moves along the green band. A frozen phone is recognisable. The kernel polls the counter: **no interrupt is enabled**, the GIC is only read.

One CPU runs, the others stay where the firmware left them (no PSCI `CPU_ON`). No UART. No storage is touched. The kernel never powers the phone off or resets it. The only secure-world call is the optional `msm.psci=1`
(`PSCI_VERSION`). A crash (a panic or any exception) puts the end of the log, in big letters, on the screen (`milkOS stopped. Photograph this screen.`), and the CPU waits.

**Watch for the watchdog.** The Snapdragon has an application processor watchdog that Linux pets from the moment its driver loads (bark 11 s, bite 14 s in Google's tree). If the bootloader left it running, the phone **resets by
itself some 15 to 30 seconds after the image started**, whatever the kernel is doing: it is not a crash. The counter's last value says how long it ran; the boot log has the watchdog's registers (`wdt: ... EN 0x1` means it was running);
`msm.wdt=pet` (the `-wdt` image) pets it once a second, one register write, to keep the phone up for photographs.

## Running the experiment

```
scripts/android-build.sh OUTDIR --stock-dtb PATH/TO/walleye-stock-base.dtb      # or without --stock-dtb: the small tree
```

writes into `OUTDIR` the kernel (`Image`), the boot images and a `README.txt` with these commands. The phone: unlocked bootloader, fastboot from the platform tools.

```
adb reboot bootloader                    # or: power off, hold power + volume down
fastboot boot OUTDIR/milkos-pixel2.img   # loads into RAM and runs; writes nothing
```

What you should see, in this order:

1. The bootloader's own screens for an unlocked phone (a warning, a few seconds): not ours.
2. The **colour pattern**, 5 seconds: top left red, top right green, bottom left blue, bottom right white, each labelled with its colour name in white on black, black borders, a white line round the edge of the screen,
   a black diagonal from the top left to the bottom right, a white one the other way. Tell me: do the labels match the colours (red and blue swapped means the pixel order is wrong), is it upright and complete, are the diagonals straight, is anything shifted or sheared?
3. Dark text (the boot log) with the green band at the top. About a screen of it: `mem:`, `milkOS`, `cpu:`, `image:`, `dtb:`, `ram:`, `cpus:`, `timer:`, `screen:`, `ramoops:`, `gic:`, `wdt:`, `options:`.
4. `alive 0 s`, `alive 1 s`... on the last line and the marker moving along the band. If it stops, the phone has frozen. If the phone restarts by itself, see "Watch for the watchdog".

Photograph 2 and 4, and the log. To go back: hold the power button for 10 to 15 seconds (the phone restarts into Android) or, if it is still in the bootloader, `fastboot reboot`.

The same kernel with other command lines (the images `build` writes; or `--add-cmdline` to `scripts/android-bootimg.py`):

| Image | Command line added | For |
|---|---|---|
| `milkos-pixel2.img` | | the first try |
| `milkos-pixel2-minimal.img` | | (with `--stock-dtb`) the small tree instead of the stock one |
| `-gz` | | the kernel gzip-compressed like `Image.gz-dtb`, if the bootloader refuses a plain Image |
| `-pattern` | `msm.hold=inf` | the pattern stays up, no text: to look at the colours |
| `-rgbx`, `-xrgb`, `-xbgr` | `msm.fmt=...` | the other 32-bit pixel orders |
| `-kick` | `msm.kick=ctl0 msm.hold=3` | refresh the panel by hand (see "If the screen does not change") |
| `-mdp` | `msm.mdp=dump msm.hold=3` | read display controller registers into the log |
| `-wdt` | `msm.wdt=pet` | keep the watchdog quiet |

### Options (kernel command line, `msm.<name>=<value>`)

| Option | Does |
|---|---|
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
| `msm.crashtest=panic` / `abort` / `early` | stop on purpose, to look at the crash screen: a panic, a data abort, a panic before the console exists |

A value that is wrong is logged (`option: msm.fmt: not one of ...`) and the default is used.

## How the bootloader loads it (assumptions)

* **Format.** A boot image header version 0 whose kernel section is the flat arm64 `Image` (its 64-byte header: `b _real_start`, `text_offset 0x80000`, `image_size`, flags 2, `ARM\x64`) followed by the device tree. The Image is *not*
  compressed (the stock one is LZ4); `-gz` is the fallback with gzip, the layout of `Image.gz-dtb` that the bootloader has handled for years. The `image_size` field counts the memory the image needs after the file (`.bss` and the boot stack: 0xc0000 now),
  so the tree is placed **at that offset**, the bytes between are zero: it is found whether the bootloader scans for the tree's magic (the stock tree sits at an odd offset at the end of the section, so it scans or uses the end) or goes by the header.
* **Where it is put.** `kernel_addr 0x8000` plus the start of RAM gives `0x8000_8000`; the arm64 convention is `0x8008_0000` (`text_offset`). Which one ABL uses (or neither) is not known. The image is linked for `0x8008_0000` (kernel/build.rs) and `boot.S` handles all of
  them: entered anywhere else, it works out where the tree and the image end, **copies the tree and a small copy routine above everything (MMU off, caches cleaned first), moves the image** (forwards or backwards, so overlapping places are fine) to `0x8008_0000`,
  invalidates the instruction cache and carries on from the same place in the copy. The log says where it was entered (`image: ... entered at 0x80008000` and `boot.S moved it`). Qemu tests four such entries (above and below, near enough to overlap or not).
  The copy needs free RAM above the image: the first 88 MiB are free, the image is under 1 MiB.
* **At entry** the MMU and caches are off and x0 is the patched tree (ARM boot protocol). Only the core with MPIDR affinity level 0 equal to 0 runs. The kernel does not need EL2.
* **The tree.** The bootloader needs an appended tree with the right `qcom,msm-id`; it applies the dtbo overlay and patches `/memory`, `/chosen/bootargs` and the reserved regions it knows. `--stock-dtb` copies the phone's own tree unchanged
  (the safe first try; it is yours, never in the repository). The small tree (`dtb_build.py minimal`) has the root properties, `/memory`, `/chosen`, eight CPUs and PSCI, the eight fixed reserved regions and the splash region, the GIC, the timer and the display controller's `reg`;
  the overlay probably expects more (it refers to nodes of the base tree by label) and ABL may drop it or refuse the tree: that is part of what the first runs find out.

## If the screen does not change

The colour pattern is written into the bootloader's framebuffer in RAM. Three things can make it not show:

1. **The pixel order or row length is wrong**: the picture is there but wrong (swapped colours, a sheared diagonal). Try `-rgbx`, `-xrgb`, `-xbgr`, `msm.stride=`, `msm.size=`; the log says what was assumed.
2. **The panel needs a refresh** (a command-mode panel only shows a frame when the display controller is told to send one; the bootloader's own picture stays, ours never appears). The `-kick` image writes 1 to `CTL_START` of control path 0 (`0xc90_0000` plus
   `qcom,mdss-ctl-off[0]` = `0x2000`, register `0x1c`, from the tree's `qcom,mdss_mdp` node), ten times a second, after logging `TOP` and `FLUSH`. **It writes a register of the display controller**, so read this first: the registers are those of the legacy MDSS driver
   (`CTL_TOP 0x14`, `CTL_FLUSH 0x18`, `CTL_START 0x1c`), written from memory of that driver, not from a manual; which control path drives the panel is a guess (the `-mdp` image reads the four interfaces' timing engine and frame counter, and each control path's `TOP` and `FLUSH`: a counter
   that moves between its two readings means a video-mode panel that needs no kick, a `TOP` that names an interface says which path is live). The sensible order is `-mdp` first (read only), then `-kick` with the control path it points at (`msm.kick=ctlN`).
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
| the pattern then the log, `alive` counting | stage 1 works; the log says what the tree contained (this is the information stage 2 needs) |
| the pattern, then the phone restarts | the watchdog, or a fault in the log: try `-wdt` |
| "milkOS stopped" or an exception dump | a crash: photograph it, the first lines say where (ELR) and what (ESR) |

## Tests

* `scripts/pixel2-tools-test.sh` (16 tests, no VM): the boot image header and sections against the layout of the real header, the id, the tree at `image_size`, gzip, refusals; the tree writer and reader (random trees round trip, the Rust writer's golden bytes, the small tree's contents, the dtbo table).
  With `MILK_PIXEL2_STOCK` naming a folder with `boot.img` and `dtbo.img` extracted from your own phone, the stock header is also repacked to the identical bytes and its id reproduced.
* `rpikit-unit` (`scripts/rpikit-test.sh`): the same header and tree layouts in Rust (`bootimg`, `fdtwrite`), the options, pixel orders and row packing, the pattern, RAM carving, the reserved region and `reg` readers with the right cell sizes, GICv3 register decoding.
* `scripts/pixel2-qemu-test.sh` (about 30 s): the kernel on qemu `virt` with a GICv3 and ramfb, see its header: the pattern and the console in screendumps, the counter and the band moving, three crash screens, four relocations, a tree shaped like the phone's with sentinel words in reserved regions and the
  framebuffer's memory read back (also as rgb565 with another row length), and the paths that must degrade safely.

## Not tested, ranked by risk

1. **That the bootloader runs a plain, uncompressed Image with an appended tree** (`-gz` is the second try), and that it accepts the small tree and the overlay (the stock tree is the first try).
2. **That a write to the splash region shows on the panel** (command-mode panels need a kick; the display may be off at the hand-over): the whole picture depends on it.
3. **The pixel order and row length** (the first guess is the likely one; the variants and options exist because it is only a guess).
4. **The hardware watchdog** may restart the phone before anyone has looked.
5. The load address (handled by the relocation, tested on qemu with the MMU on its way, but never with the bootloader's own cache state), and the cache state at entry in general.
6. The reserved region list is the tree's: a protected region the tree does not list would be mapped (as Linux would map it too); the first run's `ram:` and `mem:` lines show what was used.
7. The GIC identification reads (`GICD_PIDR2`, `GICR_TYPER`...): standard registers Linux reads, but never read from this kernel on this SoC.
8. The register offsets of the `msm.kick`, `msm.mdp` and `msm.wdt` options (from the legacy MDSS and `msm_watchdog` drivers, from memory); all three are off unless asked.

## What comes after

The log of the first run decides the order. Likely: a stable picture with the right order; interrupts (GICv3 initialisation for the timer, then the scheduler); the other CPUs through PSCI; a pstore writer for the log; the watchdog; then the real display path and storage (UFS), none of which is started.
