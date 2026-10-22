# Packages: `apps`

Files: `user/apps` (the `apps` command; it replaced `milk`), `scripts/mkpkg.py` (format, host side),
`scripts/milk-repo.sh` (build + serve the toy repo), `packages/` (fortune, snake, moo2048),
`scripts/pkg-test.sh` (end-to-end test), `rootfs/etc/milk/repos`. Kernel bits: `/dev/audit`
(docs/security.md) and the persistent `/Apps` bind (`milkfs::setup_applications`).

## Format (`.milkpkg`)

A ustar tar, built deterministically by `scripts/mkpkg.py` (mtime 0, root:root, sorted):

1. `manifest`: `key value` lines:
   ```text
   format 1
   name fortune
   version 1.0.0
   description Milk-themed quotes, one at a time
   exec fortune                # optional: the program; /Commands/<basename> links to it
   dock Fortune #EBB650        # optional: dock label and icon colour, for the desktop
   depends snake moo2048>=1.0  # optional: packages installed first (by `apps`), with minimum versions
   files fortune quotes.txt
   payload-sha256 <hex>
   ```
2. the payload files, at their install paths (relative to `/Apps/<name>/`).

`payload-sha256` is SHA-256 over, for each path in `files` order: the path, a 0 byte, the
size as u64 little endian, the bytes. Names: 1-24 of `[A-Za-z0-9_-]`; payload paths may not
be absolute or contain `..` (`apps` refuses such archives).

**Repository**: a directory served over HTTP with the `.milkpkg` files and `index.txt`:

```text
# milk repo index v1
fortune 1.0.0 <sha256 of the .milkpkg> 102400 fortune-1.0.0.milkpkg Milk-themed quotes, ...
```

`mkpkg.py build SPEC`, `index DIR`, `verify PKG`, `show PKG` (see the script's help; a spec
is `packages/<name>/package.txt`).

## Signatures

Every repository file `apps` trusts is signed with **Ed25519** (RFC 8032), implemented from
scratch twice: `user/rt/src/ed25519.rs` (+ `sha512.rs`) in the guest, pure python in
`scripts/mkpkg.py` (only SHA-512 from hashlib). Both pass the RFC 8032 section 7.1
vectors (TEST 1, 2, 3, SHA(abc): key derivation, signing, verifying, and a flipped byte
rejected = 16/16): `apps selftest`, `mkpkg.py selftest`.

* `FILE.sig` next to every `FILE` (`index.txt.sig`, `<pkg>.milkpkg.sig`): one line,
  `ed25519 <key name> <128 hex chars>`, over the file's bytes.
* Trusted keys: `/System/Settings/milk/keys/<name>.pub`, `ed25519 <64 hex chars> <name>`; `apps keys`.
* `apps update` checks the index signature before using the index; `install`/`try`/`upgrade`
  check the package signature (then the index's sha256, then the payload sha256).
* Unsigned, badly signed, or signed by an untrusted key: **refused**, with a
  `user.pkg.verify_fail` audit record (`what`, `repo`, `reason`, `insecure=false`).
  `apps --insecure ...` accepts it anyway, with a warning and the same record
  (`insecure=true`).
* Keys: `mkpkg.py keygen NAME DIR` (private `NAME.secret`, mode 600; public `NAME.pub`),
  `mkpkg.py sign SECRET FILE`, `mkpkg.py index DIR --key SECRET` (signs the index and all
  packages), `mkpkg.py verify-sig PUBFILE FILE`.
* **The toy repo's key is TEST ONLY**: `rootfs/etc/milk/keys/milk-test.pub`, private half
  committed at `scripts/milk-test-key.secret` (with a banner saying so). Anyone can sign
  with it; remove it on a system that installs real software.
* Not constant-time (fine for verifying public data; the signer is a host script).

## Versions and dependencies

Versions compare semver-ish (`user/apps/src/version.rs`, 10 checks in `apps selftest`):
numeric parts as numbers (`1.2.10 > 1.2.9`, `1.2 == 1.2.0`), others as text, and a
pre-release (`1.0.0-rc.1`) before its release. The index may list several versions of a
package; `apps` takes the newest unless told `name=VER`.

`depends a b>=1.2` in a manifest: `apps` downloads and verifies the package, then installs
every dependency that is missing or too old (depth first, so dependencies always land
before their dependents; cycles are refused), then unpacks the package. Packages pulled in
as dependencies carry a `.auto` marker (`apps list` shows "(auto)"); installing one
explicitly clears it; `apps remove --unused` removes auto packages no installed package depends
on (repeatedly, so chains go too). Upgrades keep the marker.

## Where packages live

`/Apps/<name>/` (manifest, `.origin`, payload: the toy packages put their program at the
top, `/Apps/fortune/fortune`). The built-in apps (Store, Calendar, Clock, ...) sit in the
same folder, without an `.origin`: `apps` never lists, replaces or removes those (`apps
remove store` says it is a built-in app), and refuses a package with a built-in app's name.
With a milkfs volume, `/Apps` is a bind mount of `/Volumes/Milk/Apps`, so packages persist
and are part of every snapshot and clone; without one it's tmpfs (built-in apps only).
docs/filesystem.md, "The tree", has how the built-in apps get onto the disk. `/Commands` is in RAM, so the links
`/Commands/<x> -> /Apps/<name>/<exec>` are recreated at boot by init (`apps relink --quiet`).
`apps` never replaces a `/Commands` entry that isn't one of its links. The index cache is
`/Apps/.milk/index.txt`.

## Commands

```text
apps update            fetch index.txt from every URL in /System/Settings/milk/repos
apps search [WORD]     name/description match; marks installed ones
apps info NAME         index entry + installed manifest (files, exec, dock)
apps install NAME[=VER]  the newest version (or exactly VER); download; check the signature,
                       the file's sha256 against the index, the payload sha256 against the
                       manifest; dependencies first; swap into /Apps/NAME; link
apps remove [--force] NAME  unlink /Commands, delete /Apps/NAME; refused while another
                       installed package depends on it (unless --force)
apps remove --unused        remove packages installed only as dependencies that nothing needs now
apps upgrade [NAME] [--yes]  newer versions of installed packages (and any new dependencies)
                       into ONE clone, try-upgrade, as a transaction: any failure discards the
                       clone, so live never sees half an upgrade. Then `apps keep upgrade` or
                       `apps discard upgrade`; --yes (-y) keeps it at once
apps list              installed packages
apps try NAME          snapshot milk-try-NAME, clone it as try-NAME (/Volumes/Milk-try-NAME),
                       install into the clone only
apps run [--no-sandbox] NAME [ARGS]  run the tried app with its clone as / (chroot)
                       in a sandbox (below); also finds NAME inside the try-upgrade clone
apps keep NAME         drop `apps run`'s scratch dirs, promote the clone (it becomes the live
                       volume), link /Commands; the state
                       before is snapshot before-try-NAME (rollback to undo)
apps discard NAME      discard the clone and its snapshot; the live system never changed
apps relink [--quiet]  recreate /Commands links
apps repos             the repository URLs (/System/Settings/milk/repos)
apps keys              the trusted signing keys
apps selftest          SHA-256, SHA-512, Ed25519 and version-order test vectors
apps help              every subcommand, one line each (`apps` alone: list)
```

Repositories (`/System/Settings/milk/repos`, one URL per line): `http://IP[:PORT]/path/` with an IP
address or `localhost` only, never a name (user programs have no resolver, and the repo is a
server on the Mac's 127.0.0.1, seen as 10.0.2.2). The default line is
`http://10.0.2.2:8765/`, what `scripts/milk-repo.sh serve` uses; run.sh's netdev is
restricted, so only boots with an open user netdev (pkg-test.sh) can reach it.

SHA-256 is `rt::sha256`, the same code `audit verify` uses. Every install / remove / try /
keep / discard writes a `user.pkg.*` audit record through `/dev/audit`, and the kernel logs
the snapshot / clone / promote / discard itself (`milkfs.*`, attributed to `/Commands/apps`):

```text
29 ... /Commands/apps  user.pkg.install name=fortune version=1.0.0 sha256=ff3f... from=http://10.0.2.2:PORT/fortune-1.0.0.milkpkg
39 ... /Commands/apps  milkfs.clone     snap=milk-try-snake name=try-snake result=ok
41 ... /Commands/apps  user.pkg.try     name=snake ... clone=try-snake at=/Volumes/Milk-try-snake
61 ... /Commands/apps  milkfs.clone.promote name=try-moo2048 kept_as=before-try-moo2048 result=ok
62 ... /Commands/apps  user.pkg.keep    name=moo2048 ... undo=rollback:before-try-moo2048
```

## Milk Store (`/Apps/store/store`)

A desktop window (user/gui, MILK_WIN syscalls) over `/Commands/apps`. Use its dock entry
(below), or type `open store` in the Terminal.

* The list comes from `MILK_TABLE=1 apps search` (the structured output, parsed with
  `rt::table`), plus the index cache (versions, size), installed manifests (`depends`, and
  the `dock` colour as the tile colour; packages that aren't installed get an accent picked
  by a hash of the name) and `/System/Kernel/fs/milkfs/clones` (`try-<name>` means "trying").
* Rows: tile, name, description, and one badge: `installed` (matcha), `<ver> new` (honey,
  an upgrade), `trying` (blue), or the version. The card shows the details and the
  buttons Install, Try, Keep, Discard, Remove, Upgrade, each enabled only when it makes
  sense (Keep/Discard only while trying, Upgrade only with a newer version). The top bar
  has Update (`apps update`) and Upgrade all (`apps upgrade --yes`).
* Every action runs `/Commands/apps` (fork, a pipe on stdout+stderr, the read end
  non-blocking); the log box shows its output live, word-wrapped, with a moving progress
  strip, then a `done` / `failed` chip from the exit status, and the list is reloaded. So
  signatures, clones, `--yes` and audit records are exactly the command line's. Upgrade from
  the Store keeps automatically (`--yes`); Try leaves the clone for Keep or Discard.
* First run with no index cache runs `apps update` on its own.
* Run (enabled while trying) is `apps run NAME`: the tried app with its clone as `/`,
  sandboxed; a window app like moo2048 opens its window, a terminal app's output lands in
  the log box. The Store waits for it (other buttons are off until it exits).
* Keys: Up/Down (or the wheel) select, `i` install, `t` try, `o`/Enter run, `k` keep, `d` discard,
  `r` remove, `u` upgrade, `g` update, `a` upgrade all, `q` quit.
* Theme: SPEC tokens. The wm doesn't publish its theme in `/dev/wm`'s status; the Store
  looks for a `theme dark` line there first, then `theme = chocolate` (the wm's own prefs line) in
  `/Users/milk/.config/milk/settings`, and then uses the wm's "Chocolate milk" values.
* Dock entry: `rootfs/Apps/store/manifest` is a milk-format manifest with
  `exec store` (the program next to it, `/Apps/store/store`) and `dock Store #E58C97`, for
  the desktop's manifest dock reading.

## The toy packages (`packages/`)

A sibling cargo workspace (same target and linker script as `user/`, uses `user/rt` and
`user/gui`), so these never land in the initramfs. `scripts/milk-repo.sh build [DIR]`
(default `/tmp/milk-repo`) builds and packages them; `serve [PORT] [DIR]` runs
`python3 -m http.server --bind 127.0.0.1` on them, stopped by its PID.

* `fortune`: milk-themed quotes (`-a` all, `-n N`); reads `quotes.txt` next to the program,
  so it works from a clone path too.
* `snake`: raw-mode terminal snake (TCSETS, ANSI escapes, ppoll clock): arrows/WASD, p, q.
  `snake --demo N` runs an autopilot with no input.
* `moo2048`: 2048 in a desktop window (gui), tiles named drop, sip, glass, ... COW.
  `moo2048 --selftest` checks the slide/merge rules and plays a random game, no window.

## Test (`scripts/pkg-test.sh`)

Fresh private milkfs + scratch images in a mktemp dir, repo served on a free 127.0.0.1
port, boot with an open user netdev, then in msh: update, repos, search, info, install fortune
(both hashes checked, /Commands link), run it twice, list, remove (gone from `/Apps`; `apps remove store` refused: a built-in app), try
snake, run it from the clone, check the live `/Apps` and `/Commands` don't have it,
discard (no clone left), try moo2048 + keep, run it from `/Commands`, `apps selftest`, then the
host tampers with the served repo: a flipped byte in `fortune.milkpkg` (install refused: BAD
signature), `snake.milkpkg.sig` deleted (refused: unsigned; `--insecure` installs it with a
warning), a flipped byte in `index.txt` (update refused), each with a
`user.pkg.verify_fail` record; then the full repo comes back (5 packages, fortune 1.1.0
and `games`, a meta-package that depends on snake and moo2048): install `fortune=1.0.0`,
`search` offers the upgrade, `upgrade fortune` (1.1.0 runs from the clone, live stays 1.0.0),
`discard upgrade`, `upgrade --yes` (live is 1.1.0), `install games` (snake first, as a
dependency), `remove snake` refused, `remove games`, `remove --unused` (snake goes); then
`audit --event user.pkg`,
`audit --event milkfs`, `audit verify` (RAM and disk OK). After shutdown `mkmilkfs.py`
checks the image (moo2048, fortune 1.1.0 and the built-in apps there, snake and games not, no clone), then a second boot of the
same image runs moo2048 from the relinked `/Commands`, and `audit verify` is OK across boots.

## Running a tried app with its clone as `/` (per-process root)

`apps try NAME` installs into a clone volume mounted at `/Volumes/Milk-try-NAME`. To run
the app *as if that clone were the system root*, the kernel now has per-process roots
(`chroot(2)`, syscall 51) and a `/System/Developer/chroot` helper:

```sh
/System/Developer/chroot /Volumes/Milk-try-NAME /Commands/<app> [args...]
```

`chroot DIR [CMD]` sets the calling process's root to `DIR`, `chdir`s to the new `/`, then
`execve`s `CMD` (default `/Commands/msh`). The child and its descendants see `DIR` as `/`;
absolute paths resolve under it and `..` at the root stays at the root (no escape through
path resolution). The parent and the rest of the system are unaffected — it's a
per-process view, not a global mount change, so several tried apps can run at once with
different roots. The cwd is left as-is across `chroot` (as on Linux), which is why the
helper does the `chdir("/")`.

`apps run [--no-sandbox] NAME [ARGS...]` does this (user/apps, `cmd_run_try`):

1. Finds the clone: `try-NAME`, or `try-upgrade` if NAME is in it; reads the app's `exec`
   from the clone's manifest; makes a scratch dir `/tmp/run-NAME` inside the clone.
2. Writes `user.pkg.run_try name= version= clone= root= exec= sandbox=` to the audit log.
3. `chroot(<clone>)`, `chdir("/")`: `/Apps` is now the clone's, nothing else
   resolves.
4. Unless `--no-sandbox`, `sandbox("fs=/Apps:r fs=<clone>/Apps:r
   fs=/tmp/run-NAME:rw fs=<clone>/tmp/run-NAME:rw net=local clipboard=0")` (the kernel logs
   `sandbox.enter`): the clone's `/Apps` read-only and nothing else of it (its
   `/Users/milk` and `/System/Settings`, including the copy of `/System/Settings/milk/drop`'s secret key, stay
   unreadable), the scratch dir writable (each named both ways, because grants match the
   jailed absolute path or the real path for a cwd-relative one), loopback/link-local network only, no clipboard;
   audio, windows and spawn stay on, so window apps and sounds work.
5. execve `/Apps/NAME/<exec>` with `PATH=/Apps/NAME`,
   `HOME=TMPDIR=/tmp/run-NAME`, and TERM / MILK_* passed through. `apps` *becomes* the app:
   same pid, same terminal, its exit status.

`apps keep` removes `tmp/run-*` from the clone before promoting it, so scratch never lands
on the live volume. Inside the jail there is no `/dev`, `/proc`, `/System/Kernel` or `/Commands` (the clone
is the user volume, which has none): static apps that use stdio, tty ioctls and the window
syscalls work (fortune, snake, moo2048); an app that needs device nodes would need bind
mounts inside the jail, which aren't wired. pkg-test checks it: snake's demo through
`apps run`, `apps run moo2048 --selftest` plus its scratch dir, `keep` leaving no `tmp`, and
`apps run fortune -n 15` in the upgrade clone printing the 1.1.0-only quote while live is
still 1.0.0 (so the jail really is the clone), plus the `user.pkg.run_try` and
`sandbox.enter` records.

Caveat: `getdents64` on the chroot root still reports the real parent's inode number for
`..` (path resolution is clamped, directory listing isn't); nothing leaks a usable path.

## Not done

Version ranges beyond `>=`, conflicts/provides, key rotation or revocation lists,
device nodes inside the `apps run` jail, and a full mount namespace (chroot is a
per-process root, not per-process mounts).
