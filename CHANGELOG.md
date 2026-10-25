# Changelog

## Unreleased

## 0.2.0 (2026-10-24)

**Raspberry Pi 4** (docs/raspberry-pi.md)
- milkOS boots on a Raspberry Pi 4: the same sources build a second kernel image (`--features raspi`,
  `scripts/pi-build.sh`) linked for the firmware's load address, with the board in `kernel/src/platform.rs`
  (UART, GIC-400, device ranges, boot tables). It starts at EL2 and drops to EL1, starts the other cores
  through the firmware's spin table (or PSCI where the device tree says so), takes RAM from the device tree
  (several banks, holes, memory above 4 GiB), and reads boot options from the kernel command line
  (`milk.<name>=<value>`) where qemu virt has fw_cfg. It runs to the shell and the full desktop on qemu's
  `-M raspi4b` (`scripts/pi-run.sh`, `scripts/pi-qemu-test.sh`).
- Drivers written from scratch for the BCM2711: the VideoCore mailbox and the framebuffer it allocates
  (format, pitch and address from the firmware's answer), the SD host controller (a block device, milkfs on
  it), the hardware random generator, the ACT LED, the PCIe root complex, and the GENET Ethernet MAC with its
  PHY (the last three, and EMMC2, untested until a real board: qemu does not model them).
- A screen-only debugging story for a board with no serial cable: a sign-of-life band the moment the
  framebuffer exists, the boot log mirrored on screen, a full-screen crash screen with the panic or exception dump
  (boot log not cleared), the LED on while running and flashing after a crash.
- USB keyboards and mice: PCI enumeration (`kernel/src/pci.rs`), an xHCI 1.1 driver, and the HID boot protocol
  (`kernel/src/usb/`), tested on qemu virt's `qemu-xhci` with `usb-kbd` and `usb-mouse` (`scripts/usb-test.sh`);
  `/proc/input` counts what the input layer received. Not yet: hubs, interrupts.
- The network stack takes any NIC through `net::nic::Nic` (virtio-net or GENET); the boot RAM allocator and
  page tables take 64-bit physical addresses and RAM banks; DMA buffers come from below the board's DMA limit
  (`pmm::alloc_dma_zeroed_as`, `dma::Buffer`). New host-tested crate `rpikit` (device tree reader, command line,
  mailbox messages, SD and SDHCI encoding, PCI math, xHCI TRBs and contexts, USB descriptors, HID reports,
  GENET descriptors); new tests `pi-qemu-test`, `usb-test`, `highmem-test`, `rpikit-unit`.

**Browser**
- The Browser is now a sandboxed user-space app (`/Apps/browser`) with tabs, back/forward,
  bookmarks, find in page, zoom, forms (GET and POST), cookies (no third-party), downloads,
  a padlock with certificate details, and plain-words error pages. It draws pages with a new
  engine (`user/webengine`): the HTML5 parsing algorithm, CSS (selectors level 3, the cascade,
  media queries, `var()`, `calc()`), and layout for blocks, text, floats, tables, flexbox and
  a subset of grid. Page text uses the system fonts with Unicode, and JPEG, GIF and WebP
  pictures (animated ones play in the front tab). Pages follow the Settings font and theme.
- JavaScript: an ES2020 engine written from scratch (`user/jsengine`: bytecode VM, garbage
  collector, Promises and async/await, classes, generators, RegExp, typed arrays, with budgets
  for memory, steps and time). Pages get a live DOM API, events, timers, storage and a console
  panel (Cmd-Alt-J). A runaway script gets a "This page is slow" bar; a toolbar switch turns
  JavaScript off. Not yet: real BigInt (literals parse and throw).
- Memory (docs/browser.md "Memory"): the Browser no longer gives every page, frame and worker the
  same few megabytes. Heap limits follow what the machine has free (`/proc/meminfo`, new
  `MemAvailable` and `/proc/mempressure`): a floor always stays for the desktop and the other apps,
  the room above it is shared out among the realms of all tabs (a page in front gets most of it), and
  a page can hold hundreds of MB on a roomy machine where it was stopped at 64 MB (a frame at 16, a
  worker at 32); as memory gets short the limits shrink, caches are dropped, garbage is collected,
  background tabs are discarded (they come back from their address) and the heaviest page is
  offered "This page uses a lot of memory. Stop it?"; a page that runs out is stopped alone, with
  a console line and a bar, and gives its heap back at once. What used to be a refusal is streamed:
  a page shows its top while the rest arrives and is cut at a memory-derived window with "This
  page is too big to show completely. Load more?", `fetch` bodies are real streams with
  backpressure (the page acknowledges what it read, the server waits), downloads are written to
  disk as they come (no 64 MB limit), and the sizes of scripts, stylesheets, pictures and whole-body
  reads follow the memory too. The engine's heap limit can now move while a script runs
  (`jsengine::Realm::set_heap_limit`, `set_memory_hook`; `webengine::ScriptHost::heap_allowance`);
  milkweb streams a body through a hook (`Hooks::stream` / `body`) and decodes chunked, gzip and
  deflate bodies as they arrive.
- Big scripts: a script is parsed and compiled one top-level statement at a time, functions are
  compiled on their first call, and the code a page keeps counts against its heap budget, so
  bundles of several megabytes (a 3 MB bundle of functions peaks at about 10 to 30 MB instead of
  over 200 MB; an 8 MB one that was refused now loads) load without running the Browser out of
  memory, and a page that used to fail with "the code is too large to fit in memory" for a big
  script now runs it. A single function or statement bigger than the compile allowance is still
  refused, and so is code that does not fit the heap.
- `MessageChannel` and `MessagePort` (structured copies, ports handed on to frames and
  windows, ordered, without starving timers), `BroadcastChannel`, a real `MutationObserver`,
  `IntersectionObserver` and `ResizeObserver`, `Proxy`, `Intl` (number, date, list, plural and
  relative-time formatting, collation, segmenter; en-US data and a few locales), `DOMParser`,
  `XMLSerializer`, `Range`, `Selection`, `TreeWalker`, `NodeIterator`, custom element
  registry (definitions only), `crypto.getRandomValues` / `randomUUID` and, on https pages,
  `crypto.subtle.digest` and HMAC, `requestIdleCallback`, `scheduler`, `navigator.locks`,
  `performance.mark` / `measure` / observers, `matchMedia`, `CSS.supports`, geometry classes,
  `Element.animate` (animations finish at once), `WeakRef` (strong), more events and
  `navigator` / `screen` members, a fuller `getComputedStyle` (with `getPropertyValue`), and
  `structuredClone` errors that are real `DOMException`s. See docs/browser.md for what is and
  is not there.
- `fetch` and `XMLHttpRequest` with the web's rules: same-origin, CORS with preflight,
  credentials modes, forbidden headers, mixed-content blocking, and no requests from a public
  page to private addresses (`user/browserkit` netpolicy). Requests run off the page's thread,
  and a Network list in the console panel shows them. Synchronous XHR is refused.
- ES modules: `<script type=module>` (deferred, run once per URL, `nomodule` skipped),
  `<link rel=modulepreload>`, import maps, dynamic `import()`, `import.meta.url`, JSON modules
  and top-level `await`. Modules are fetched with CORS and a JavaScript MIME check, with caps on
  module count and depth per page. The engine handles `import`/`export` in all forms with live
  bindings and cycles, plus private class members (`#x`), and default-parameter TDZ.
- Frames in the web engine: `<iframe>` elements are slots the Browser fills with documents. The
  engine lays each frame out in a viewport the size of its box, paints it into the page's
  display list (clipped to the box, with the frame's own scroll offset and its own `position:
  fixed` boxes), and hit-tests, scrolls (the wheel passes on to the parent when a frame is at
  its end), searches, copies text and orders the Tab key across frames. Frames nest up to 5
  deep and a page has at most 32; `window.name`, `postMessage` and same-origin reach between
  frames have hooks for the bindings (docs/frames.md).

- `<iframe>` in the Browser: frames are fetched as navigations (the page as `Referer`, cookies
  only along one site, every redirect hop checked, mixed content blocked, errors and refusals
  shown inside the frame), `srcdoc`, `about:blank` and `data:` documents, `X-Frame-Options` and
  CSP `frame-ancestors`, and the `sandbox` attribute (scripts, same-origin, forms, popups, top
  navigation, modals, downloads). Scripts get `parent`, `top`, `frames`, `frameElement`,
  `contentWindow` / `contentDocument` (same-origin frames share a DOM through proxies; other
  origins get only `postMessage`, `location` and the rest of the safe set, `SecurityError`
  otherwise) and `postMessage` with the structured clone. Links and forms take `target=`
  (`_self`, `_parent`, `_top`, `_blank`, a frame's name); Back and Forward step a joint history
  of the frames; clicks, the wheel, Tab, typing and find go to the right frame, and each frame
  scrolls on its own. At most 5 levels and 32 frames; a runaway frame is stopped alone.
- Web Workers: `new Worker(url, { type, name })` for dedicated classic and module workers, each
  on a thread of its own with its own realm, so heavy work no longer freezes the tab and
  libraries that start workers (Comlink, inline `blob:` workers, bundlers' worker loaders)
  run. `postMessage` both ways with the structured clone and a transfer list of `ArrayBuffer`s
  (moved, the sender's is detached) and `MessagePort`s (they keep working across the two
  threads), `terminate()`, `onmessage` / `onerror` / `onmessageerror`, `error` events with the
  file, line and column, and inside the worker `importScripts`, module workers with static and
  dynamic `import`, `close()`, `fetch` and `XMLHttpRequest` through the same network rules as
  the page's (with the worker's own origin), timers, `console` (tagged `[worker NAME]` in the
  panel and on the serial line), `crypto`, `URL`, `Blob`, `TextEncoder`, `Intl` and the rest of
  the platform that is not about a document (no `window`, no `document`). A worker script comes
  from the page's own origin, a `blob:` address the page made, or a `data:` address (an origin
  of its own); anything else is an `error` event and a console line. Limits: 16 per page and
  tab, 48 threads, 32 MB of heap each and 160 MB for a tab's workers together with its page,
  8 seconds for one task (`while (true) {}` stops that worker alone, with a console line, and
  the page keeps running), full inboxes drop messages, and navigating, closing the tab or
  removing the frame ends a page's workers. JavaScript off means no workers. Not done:
  `SharedWorker`, service workers, nested workers, `WebAssembly`. New `NetKind::Script`
  (scriptnet checks it like a module), `ScriptHost::fetch_blocking`, `rt::thread::try_spawn_sized`
  and `JoinHandle::is_finished`; `scripts/workers-test.sh` in the default set.
- `view-source:ADDRESS` (and "View Page Source" in the menu, Cmd-Alt-U) opens the page's source
  in a tab: the page is fetched again like any navigation, shown as plain numbered monospace
  text, and nothing in it runs. Lines wrap to the width of the window (a minified script of a
  megabyte on one line reads whole, with no sideways scrolling), the numbers are not part of
  the text, and a page the server refused (a 429, a 404) shows its body too. Only the first 4 MB
  and 50,000 lines are shown, and it says so.
- Errors say where they happened. An uncaught error or a rejected promise prints its stack
  with the source line at the top frame, cut around the column on a long line, and a caret
  under the column (`file:line:col`, then `672 | ...text...`), on the console panel and on the
  serial line (one line each, the first starting `[browser] console error:`). Frames name the
  functions a script never named by where they were stored (`a.b = function` is `a.b`,
  `var f = function`, an object's `key: function`), and an error and each frame point at the
  link of a chain that is meant (`a.b(c).d(e)`: the `d`), not at the start of the statement,
  so `x is not a function` says `a.b(...).d is not a function` and where. The prelude's file is
  `browser-api`, not `fetch-api`.
- A function of another same-origin frame keeps its properties: `frame.contentWindow.Object
  .getOwnPropertyNames`, `.Array.isArray`, `.Array.prototype` and the like are there (they were
  `undefined`, and a script that took its builtins from a fresh frame, reCAPTCHA's fingerprint
  among them, died with `V is not a function`). They are still not instances of this
  realm's classes, and `Object.getOwnPropertyNames(frameWindow)` lists only the enumerable names.
  An object or function of the page that was stored on a frame's window (`frames[0].o = obj`)
  and read back is now `obj` itself (it came back as `undefined`: the read-back gave up the
  reference the frame's proxy held).
- Script loading follows the HTML standard more closely: a parser-blocking script sees only the
  page before it (`document.body` is `null` in a `<head>` script, a later element is not found,
  `document.scripts` ends with the running script), and an external script that `document.write`
  adds blocks the parser in order, before the page's next script, instead of running whenever
  it arrives. `webengine/src/tests/loading.rs` checks the order of blocking, `defer`, `async`,
  `document.write` and inserted scripts for any arrival order, scripts that throw or do not
  compile, and the events.
- Settings > Browser picks the search engine for words typed in the address bar: DuckDuckGo
  (Lite, still the default), DuckDuckGo (HTML), Brave Search, Startpage, Bing or Google. The
  choice is saved with the other settings and the Browser uses it from the next search, without
  a restart; a new tab names the engine in use. Words are encoded safely (`+`, `&`, `#`, quotes
  and non-ASCII text) and cut at 2000 bytes.

**Tests**
- A full `scripts/check-all.sh` runs 8 tests at a time and takes about 3 minutes (it was 7 to
  8). Boot self-tests are opt-in, long tests are shorter or split, passing tests clean up
  their work folders, and `--if-needed` skips a tree that already passed.

**Desktop** (photos)
- Six CC0 photo wallpapers (Settings > Wallpaper > Photos), decoded when chosen.

**Network**
- https:// pages: a TLS 1.3 client written from scratch (`tls/`, shared by the kernel and
  user space; docs/tls.md), so the Browser and `network get` open secure sites. ChaCha20-
  Poly1305 and AES-128-GCM; X25519, P-256 and P-384; certificates always checked against the
  compiled-in Mozilla root store (121 roots, 126 KiB), with errors in plain words ("the
  certificate is for another site", "the certificate has expired", "the site doesn't speak
  TLS 1.3"). New in milkcrypto: AES-128 (bitsliced), GHASH/GCM, SHA-384, HKDF, RSA, P-256 and
  P-384. Tested against RFC 8448's trace and OpenSSL 3 (`scripts/tls-interop.sh`).
- TCP under loss no longer stalls for minutes: after a retransmission timeout, an ACK that
  echoes the timestamp of the original send is no longer taken as an RTT sample (Karn's rule
  for timestamps). Those samples counted the RTO wait as round trip, so the RTO grew to many
  seconds and each further loss waited a minute (the boot self-test's lossy rounds went
  silent on a loaded host). The net self-test runs every check under a watchdog that fails
  loudly with a state dump, and has a stress mode (`scripts/net-stress-test.sh`). A peer
  that SACKs our FIN can no longer spin the sender.

**Desktop**
- Real system fonts: UI text is anti-aliased Figtree (default), IBM Plex Sans, Fira Sans,
  Atkinson Hyperlegible Next or Ubuntu Sans, picked in Settings > Appearance > Font and
  switched live (saved as `font = <key>`). Baked at build time by `scripts/gen_sysfont.py`
  at 13, 26, 52 and 104 px (Regular, Medium, Bold): 451 KiB of glyphs, the kernel image
  grew 492 KiB. Terminal, Notes and the console stay on Unifont. Credits in Settings >
  About, licences in `/System/Fonts`.

**Files**
- `files`, one command for files and folders: `create` (a trailing slash makes a folder),
  `copy`, `move` (across disks too), `rename`, `delete`, `info`, `list`, `link`, `help`, with
  errors in plain English. It replaces `ls`, `cp`, `mv`, `rm`, `mkdir`, `touch` and `ln`,
  which are gone. A slash at the end of a path means a folder: on a file it is an error.
- `files owner` and `files access` replace `chown` and `chmod`, which are gone.
  `files owner notes.txt` says "milk (group: staff)"; `files owner PATH USER`, `USER:GROUP` or
  `:GROUP` changes it. `files access notes.txt` says who may use it in one sentence ("milk can
  read and change it, staff can read it, everyone else can't", with set-ID and sticky when
  they're on), and `files access PATH private | shared | group | read-only | open` sets it
  (600/700, 644/755, 660/770, 444/555, 666/777 for files/folders; a runnable file stays
  runnable), `runnable` / `not-runnable` turn running on and off, `--mode NNNN` sets an exact
  octal mode. `--all` on a folder changes everything in it without following links. The
  kernel's rules are the same: only an admin gives things away, an owner can change the group
  to one they're in. `files info` shows the sentence beside the mode; the `files list` table's
  `mode` column is now `access`.
- `go` and `where` instead of `cd` and `pwd`, which are gone: `go Documents`, `go ..`,
  `go /Apps`, `go` (home), `go back` (the folder before; a folder called back is
  `go ./back`); `where` prints the folder you're in, in pipes too, and takes no
  arguments. Errors in plain English ("Documents doesn't exist",
  "notes.txt is a file, not a folder", "you can't open ..."). In msh and the Terminal's
  built-in shell.
- `text`, one command for lines of text: `show`, `first [N]`, `last [N]`, `find`, `sort`,
  `unique`, `count`, `help`, reading what's piped in when there's no file. It replaces `cat`,
  `head`, `tail`, `grep`, `sort`, `uniq` and `wc`, which are gone; their flags are spelled out
  (`text find --ignore-case --invert --numbers --count --names`, `text sort --reverse
  --numbers --unique --field N`, `text unique --count --repeated`, `text count --lines
  --words --bytes`). `text count` and `text unique --count` give msh a table. `print`
  replaces `echo` (`--no-newline`, `--escapes`, and -n / -e still work), `pause` replaces
  `sleep` (`pause 5`, `5s`, `2m`, `500ms`). Errors in plain English.
- msh's `variables` built-in replaces `env` and the `export`, `set` and `unset` built-ins:
  `variables list` (a table too), `get NAME`, `set NAME=value` (exported), `remove NAME`,
  `run NAME=value COMMAND`. `NAME=value` and `NAME=value COMMAND` work as before.
- msh's table filter is now `filter` instead of `where`, same syntax and operators:
  `ps | filter rss > 100`, `files list | filter size > 1M`. `where` with arguments says
  "where takes no arguments; to filter a table use filter".
- A real Trash: `files delete` moves things to `~/.Trash` (or a disk's own `.Trash`),
  `files delete --now` skips it, `trash list`, `trash restore`, `trash empty`. The `trashd`
  service deletes things for good after 30 days, or what Settings > Trash says (or never).
  Files deletes to it too and shows it in its sidebar (Restore, Empty Trash); the dock has a
  Trash at its right end, empty or full.
- `renameat2` takes `RENAME_NOREPLACE`.
- Plain-English folders instead of `/bin`, `/etc`, `/home` and `/srv`: `/Apps` (apps you
  open, built-in and installed, one folder each), `/Commands` (what you type in Terminal;
  `PATH=/Commands`), `/Users/<name>` (homes, root's too), `~/Sites` (the web server's pages),
  and `/System` with `Services`, `Startup`, `Settings`, `Fonts`, `Sounds` and `Music`.
  A clean break: no links with the old names. `open <app>` finds apps by name; Files has
  Apps in its sidebar.
- The last Unix folders that only milkOS itself used moved under `/System`: the service
  sockets (`/run`) are in `/System/Services/Sockets`, the kernel controls (`/sys`) in
  `/System/Kernel` (`/System/Kernel/fs/milkfs/...`). `/dev`, `/proc` and `/tmp` stay for
  Linux and POSIX programs, hidden in Files and in `files list /` (`--all` shows them).
- Test and developer tools (`utest`, `fault`, `hello`, `fsstress`, `tcpbench`, `linux-hello`,
  `chroot`, `sandbox`) moved out of `/Commands` into `/System/Developer`, which is not on
  `PATH`.
- Existing disks move over once at boot (or when a login unlocks them): `home/<user>`,
  `Applications` and `etc/milk/{drop,net}` are renamed inside the volume, the home gets
  `~/Sites`, and old snapshots still open in Files and Versions.
- The toy packages put their program at the top of their folder (`/Apps/fortune/fortune`).

**Processes, services and accounts**
- Five commands with plain verbs replace fourteen, which are gone (no aliases):
  `processes` (`list`, `watch`, `stop [--now]`, `signal`, `signals`, `info`) replaces `ps`,
  `top` and `kill`; `services` (`list`, `status`, `start`, `stop`, `restart`, `signal`,
  `reload`, `logs [--follow] [--since 10m] [--last N] [--level L] [--this-boot]`) replaces
  `svc` and `log`; `system` (`info`, `memory`, `uptime`) replaces `uname`, `uptime` and
  `free`; `users` (`me`, `list`, `password`, `switch`) replaces `whoami`, `id`, `passwd` and
  `su`; `admin COMMAND` replaces `sudo`, and `admin ticket COMMAND` / `admin status`
  replace `auth`. Each has `help` in the format `files help` uses, errors in plain English
  ("there's no process called x", "only an administrator can stop services"), and a table
  for msh where it prints rows (`processes list | filter memory > 1M`, `users list`,
  `services list`, `system memory`). `processes stop NAME` refuses a name two processes
  share and lists their PIDs (`--all` for every one). `system memory` shows the new
  `/proc/meminfo` breakdown in Activity Monitor's groups. There is no `system restart` or
  `shutdown` yet: only the desktop's menu can.
- `/proc/mempressure` (level normal / low / critical against marks at 1/8 and 1/16 of RAM, the
  lowest free memory since boot, how often the marks were crossed, allocations that found no
  memory) and a `MemAvailable` line in `/proc/meminfo`: a program that can give memory back (the
  Browser) knows when to (docs/memory.md "Memory pressure").
- `users` and `admin` are set-user-ID root like `su`, `sudo` and `passwd` were, with the same
  checks; their other subcommands give root up first. `admin` now gives the root command a
  clean environment from the target account (PATH, HOME, USER, LOGNAME, TERM, plus
  `ADMIN_USER`) instead of the caller's, and wrong passwords for `users switch`,
  `users password` and `admin` are in the audit log (`user.password.fail`).
- Fixed: asking for a password on a terminal (`rt::users::read_password`, used by all of
  them) wrote 24 bytes past its termios buffer on the stack; the kernel's termios is 60
  bytes, the buffer was 36.
- The rules are in `user/syskit` (host tests: `scripts/syskit-test.sh`, smoke set), the
  commands in `user/systools`; `scripts/systools-test.sh` (default set) runs every
  subcommand in a VM and checks the old names are gone.

**File system**
- milkfs is its own crate (`<repo>/milkfs`, `no_std`, shared with the kernel like the crypto
  crate), with the kernel's VFS glue in `kernel/src/fs/milkfs`. Nothing changes on disk or in
  behaviour. It builds for the Mac too: `scripts/milkfs-test.sh` runs the file system there in
  a few seconds (memory disks, a fake clock, a seeded RNG, power cuts at many points, a drive
  that reorders unflushed writes), covering what the qemu milkfs tests check below the VFS;
  check-all runs it in the smoke set.

**Network, disks, apps and search**
- `network` replaces `ifconfig`, `netstat`, `ping`, `nc`, `netctl` and `fetch`, which are gone:
  `network status` (links, addresses, counters, the hostname and Milk Drop; `--keys` for
  the Settings > Network panel), `connections`, `counters`, `ping HOST`, `get URL` (`--save
  FILE`, never overwriting without `--replace`), `connect HOST PORT`, `listen PORT`,
  `name [NAME]` (the hostname; `--local` the .local name), `hide` and `show` (Milk Drop
  visibility), `help`. Options are spelled out (`--count 3`, `--ipv6`), short forms kept.
- `disks` replaces `df`, `snap` and `dedupe`, which are gone: `list` (free space), `devices`,
  `info`, `snapshots`, `snapshot NAME`, `delete`, `rollback`, `changes A [B]` (was diff),
  `auto`, `clones`, `clone`, `keep` (was promote), `discard`, `check`, `verify` (was scrub),
  `format` (was mkfs), `encrypt`, `decrypt`, `lock`, `unlock`, `passphrase` (was passwd),
  `send`, `receive`, `dedupe PATH`, `help`. Same checks, prompts and secrets handling; the
  stream temp files for `send` to stdout and `receive` from stdin are now made O_EXCL,
  mode 0600 (security review SR-25, user-space half).
- `apps` replaces the `milk` package manager, which is gone (the user, the folders and the
  package format keep the milk name): `list`, `search`, `info`, `install`, `remove`
  (`--unused` was autoremove), `update`, `upgrade` (`--yes`), `try`, `run` (was run-try),
  `keep`, `discard`, `relink`, `repos` (new), `keys`, `selftest`, `help`. Signatures,
  checksums and audit records are unchanged. The Store and init use it.
- `search` replaces `spot`, which is gone, with the same options spelled out: `--long`,
  `--limit N`, `--status`, `--wait`, `--wait-index`, `--rebuild`.
- `network status`, `connections`, `counters`, `disks list`, `devices`, `snapshots`,
  `clones`, `apps list`, `apps search` and `search` give msh tables. The parsing and wording
  are in `user/cmdkit` (host tests, smoke set); `scripts/net-disks-apps-test.sh` tests every
  subcommand in a VM.

**Internet**
- The internet is on for `scripts/run.sh` and the release `milkOS.command` (slirp without
  `restrict=on`; `MILK_OFFLINE=1` or `--offline` for the old restricted network), and every
  test stays offline: check-all puts a qemu wrapper first in each test's `PATH` that refuses
  an `internet=on` boot, and fails the run if one is tried. The kernel is told in fw_cfg
  `opt/milkos/net`; Settings > Network, `network status` and `/proc/net/internet` say
  "Internet: on" or "off (offline mode)" (docs/networking.md, "Internet").
- Real DNS when online: the DHCP server (slirp's 10.0.2.3), A and AAAA, CNAMEs, TCP for
  truncated answers, a TTL cache. Offline keeps the `.test`-only policy. User space asks the
  kernel (`MILK_RESOLVE`); `network lookup NAME` is new.
- `milkweb`, a shared no_std crate (kernel and user space, host-tested): URLs, an HTTP/1.1
  client over a small Transport trait (chunked, Content-Length, redirects with a loop guard
  and a limit of 10, gzip/deflate with imgdec's inflate, `User-Agent: milkOS/0.1`, time and
  size limits, an `https://` hook for TLS: until then "secure pages aren't supported yet"),
  DNS messages and the resolver's logic, the Browser's HTML subset.
- The Browser visits host names (offline: IP literals and localhost, as before), shows
  where redirects ended, says what went wrong (no internet, not found, can't connect, secure
  pages), searches DuckDuckGo's lite page for a bare word, and reads more of the web:
  entities, script/style dropped, div/span/table/blockquote/hr, `<img alt>`, UTF-8 and
  Latin-1. `network get` uses the same client (host names, redirects, gzip).
- Tests: `scripts/milkweb-test.sh` (host) and `scripts/web-test.sh` (a VM against a DNS and
  HTTP server on the Mac's 127.0.0.1; lookups, a redirect chain, a chunked gzip page, the
  Browser by host name).

**Memory**
- A real heap for every program (`user/heapkit`, the global allocator in `rt::heap`). The old
  one was a bump allocator that only took back the most recent block, so long-running
  processes grew without end (Activity Monitor, top and milkd most of all). Now: size
  classes for small blocks, coalescing free lists for the rest, realloc in place, and free
  memory goes back to the kernel (the break shrinks; free pages inside the heap are
  munmapped and taken back with `mmap(MAP_FIXED)` when reused). A lock makes it safe with
  `rt::thread`. In 3 minutes of a busy desktop Activity Monitor went from 2.6 to 18.5 MB
  before and stays at about 2.3 MB now; top went from 0.5 to 5.3 MB and stays at 0.3 MB.
  `scripts/heapkit-test.sh` (host, smoke set) and `scripts/leak-test.sh` (a desktop for a
  minute, default set) check it.

**Releases**
- A real version number: `VERSION` is the one source (SemVer, `0.MINOR.PATCH` until 1.0; the
  next release is 0.2.0). The Cargo package versions follow it (they were 0.0.0 and 0.0.1, so the
  audit log said `kernel=milkOS-0.0.0`; it now says the real version and the build), and
  `scripts/version-check.sh` fails if anything that carries the version disagrees, or if a
  format version in the code changes without `docs/versioning.md` (which lists every format and
  interface with its own version) being updated.
- A build from a checkout says `0.2.0-dev (abc1234)` (the commit, `+dirty` with changes,
  `unknown` without git) in About, Settings, the boot message, `uname`, `/proc/version` and
  `/System/Version.txt`; a release build says plain `0.2.0`. New `system version` prints it.
- `scripts/cut-release.sh X.Y.Z` bumps everything, closes the changelog section, commits and
  tags (it pushes nothing); `scripts/publish-release.sh vX.Y.Z` builds the bundle
  (`scripts/release.sh`, which now also writes `SHA256SUMS`) and creates a draft GitHub
  Release with the notes from `scripts/release-notes.sh`; `scripts/ci-local.sh` runs the
  portable checks (build, clippy, host tests, version and public checks);
  `scripts/install-hooks.sh` adds an optional pre-push hook. All of it works on the Mac with no
  GitHub Actions; the two workflows in `.github/workflows/` are optional wrappers around the
  scripts. `scripts/release-scripts-test.sh` (smoke set) tests the scripts in scratch clones.
  See `docs/releasing.md` and `docs/versioning.md`.

## 0.1.0 (2026-10-21)

The first release: everything below exists and is tested, on qemu `virt` (aarch64) with
HVF on an Apple Silicon Mac. Details are in `docs/` (one file per area).

**Release**
- `scripts/release.sh` builds `dist/milkOS-0.1.0/` and a zip: a release kernel, a ready
  milkfs disk, and `milkOS.command`, which runs on double-click (needs only QEMU).
  `scripts/releasetest.sh` runs the unzipped bundle from outside the repo.
- One version number, in `VERSION` (uname, `/proc/version`, motd, About, Settings).

**Kernel** (`docs/memory.md`, `docs/processes.md`, `docs/smp.md`)
- Boot, MMU with 4 KiB pages, frame allocator and heap, GICv2, generic timer, UART.
- SMP on 4 CPUs: PSCI bring-up, real spinlocks, IPIs, TLB shootdown, per-CPU stats.
- Preemptive scheduler with priorities, wait queues, sleeping mutexes.
- EL0 processes with their own address spaces (ASIDs), ELF loader, copy-on-write `fork`,
  threads and futexes, signals, job control, pipes, `chroot`; over 80 Linux-numbered
  syscalls.

**Security** (`docs/security.md`)
- Per-process capability sandbox (files, network, devices), enforced in the syscall layer;
  packaged apps run sandboxed and get files only through the Powerbox Open/Save panel.
- Tamper-evident audit log (SHA-256 hash chain) of execs, connections, mounts,
  snapshots, sandbox denials, scrub errors; `audit`, `audit verify`.

**Storage** (`docs/filesystem.md`)
- VFS with tmpfs root, initramfs, devfs, procfs, sysfs; virtio-blk; block cache; a page
  cache with readahead.
- milkfs, a copy-on-write filesystem with checksums on everything: snapshots, rollback,
  writable clones (try-before-install), Time Machine (automatic snapshots), `snap diff`,
  per-file version history, reflink copies (`cp --reflink`) and `dedupe`, `check
  --repair`, background scrub, `snap send` / `snap receive` (full and incremental,
  atomic) with Mac-side tools (`mkmilkfs.py`). Persistent space map (mount in under
  1 ms), multi-block extents and inline small files. Crash-tested: SIGKILL, power cuts,
  torn superblock writes.
- The home folder lives on milkfs; FAT32 with long file names; the Mac's `share/` folder
  as `/Volumes/Mac` (virtio-9p, read-only by default).

**Network and sound** (`docs/networking.md`, `docs/audio.md`)
- virtio-net and our own stack: Ethernet, ARP, IPv4, ICMP, UDP, DHCP, DNS, TCP; sockets;
  `ifconfig`, `ping`, `netstat`, `nc`, `fetch`, `httpd`. qemu's network is restricted.
- virtio-sound with a mixer, `/dev/audio`, `play`, a boot chime and system sounds.

**Desktop** (`docs/ui-concepts/`)
- Compositor with per-window buffers, glass, shadows, animations; menu bar, dock, Skim
  (launcher and command bar), Today, Spaces, notifications, control centre, light and dark
  themes, window snapping, lock screen, screenshots, live resolution switching.
- Apps: Files (with Time Machine and Browse All Versions), Terminal (tabs, colours),
  Notes, Browser, Paint, Music, Settings, About; user programs can open windows.

**Userland** (`docs/shell.md`, `docs/packages.md`)
- `msh`, a shell with pipelines, redirection, job control and structured pipes
  (`ps | where rss > 100 | sort-by name`); coreutils; `top`; `snap`, `versions`,
  `dedupe`.
- `milk`, a package manager with Ed25519-signed packages and dependencies; `milk try`
  installs into a throwaway clone, kept or discarded.

**Fixed before release**
- Three intermittent hangs/crashes, each root-caused and covered by `scripts/serial-soak.sh`: a
  lost PL011 RX interrupt (console stopped mid-line), a stale-waiter lost wakeup in `WaitQueue`,
  and a CPU taking interrupts before joining the scheduler during SMP bring-up (boot crash).

**Known limits**
- Kernel RAM is mapped RWX; processes have no user credentials yet, so file permission
  bits aren't checked (the capability sandbox is the access control).
- milkfs: one volume per disk, at most 30 snapshots + clones, sends only from the
  primary volume, scrub reports but doesn't repair.
- Needs QEMU on a Mac; Intel Macs only via slow, untested emulation.
