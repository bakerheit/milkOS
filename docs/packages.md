# Packages: `milk`

Files: `user/milk` (the tool), `scripts/mkpkg.py` (format, host side),
`scripts/milk-repo.sh` (build + serve the toy repo), `packages/` (fortune, snake, moo2048),
`scripts/pkg-test.sh` (end-to-end test), `rootfs/etc/milk/repos`. Kernel bits: `/dev/audit`
(docs/security.md) and the persistent `/Applications` bind (`milkfs::setup_applications`).

## Format (`.milkpkg`)

A ustar tar, built deterministically by `scripts/mkpkg.py` (mtime 0, root:root, sorted):

1. `manifest`: `key value` lines:
   ```text
   format 1
   name fortune
   version 1.0.0
   description Milk-themed quotes, one at a time
   exec bin/fortune            # optional: what /bin/<basename> links to
   dock Fortune #EBB650        # optional: dock label and icon colour, for the desktop
   depends snake moo2048>=1.0  # optional: packages installed first (milk), with minimum versions
   files bin/fortune share/fortune/quotes.txt
   payload-sha256 <hex>
   ```
2. the payload files, at their install paths (relative to `/Applications/<name>/`).

`payload-sha256` is SHA-256 over, for each path in `files` order: the path, a 0 byte, the
size as u64 little endian, the bytes. Names: 1-24 of `[A-Za-z0-9_-]`; payload paths may not
be absolute or contain `..` (milk refuses such archives).

**Repository**: a directory served over HTTP with the `.milkpkg` files and `index.txt`:

```text
# milk repo index v1
fortune 1.0.0 <sha256 of the .milkpkg> 102400 fortune-1.0.0.milkpkg Milk-themed quotes, ...
```

`mkpkg.py build SPEC`, `index DIR`, `verify PKG`, `show PKG` (see the script's help; a spec
is `packages/<name>/package.txt`).

## Signatures

Every repository file milk trusts is signed with **Ed25519** (RFC 8032), implemented from
scratch twice: `user/rt/src/ed25519.rs` (+ `sha512.rs`) in the guest, pure python in
`scripts/mkpkg.py` (only SHA-512 from hashlib). Both pass the RFC 8032 section 7.1
vectors (TEST 1, 2, 3, SHA(abc): key derivation, signing, verifying, and a flipped byte
rejected = 16/16): `milk selftest`, `mkpkg.py selftest`.

* `FILE.sig` next to every `FILE` (`index.txt.sig`, `<pkg>.milkpkg.sig`): one line,
  `ed25519 <key name> <128 hex chars>`, over the file's bytes.
* Trusted keys: `/etc/milk/keys/<name>.pub`, `ed25519 <64 hex chars> <name>`; `milk keys`.
* `milk update` checks the index signature before using the index; `install`/`try`/`upgrade`
  check the package signature (then the index's sha256, then the payload sha256).
* Unsigned, badly signed, or signed by an untrusted key: **refused**, with a
  `user.pkg.verify_fail` audit record (`what`, `repo`, `reason`, `insecure=false`).
  `milk --insecure ...` accepts it anyway, with a warning and the same record
  (`insecure=true`).
* Keys: `mkpkg.py keygen NAME DIR` (private `NAME.secret`, mode 600; public `NAME.pub`),
  `mkpkg.py sign SECRET FILE`, `mkpkg.py index DIR --key SECRET` (signs the index and all
  packages), `mkpkg.py verify-sig PUBFILE FILE`.
* **The toy repo's key is TEST ONLY**: `rootfs/etc/milk/keys/milk-test.pub`, private half
  committed at `scripts/milk-test-key.secret` (with a banner saying so). Anyone can sign
  with it; remove it on a system that installs real software.
* Not constant-time (fine for verifying public data; the signer is a host script).

## Versions and dependencies

Versions compare semver-ish (`user/milk/src/version.rs`, 10 checks in `milk selftest`):
numeric parts as numbers (`1.2.10 > 1.2.9`, `1.2 == 1.2.0`), others as text, and a
pre-release (`1.0.0-rc.1`) before its release. The index may list several versions of a
package; milk takes the newest unless told `name=VER`.

`depends a b>=1.2` in a manifest: milk downloads and verifies the package, then installs
every dependency that is missing or too old (depth first, so dependencies always land
before their dependents; cycles are refused), then unpacks the package. Packages pulled in
as dependencies carry a `.auto` marker (`milk list` shows "(auto)"); installing one
explicitly clears it; `milk autoremove` removes auto packages no installed package depends
on (repeatedly, so chains go too). Upgrades keep the marker.

## Where packages live

`/Applications/<name>/` (manifest, `.origin`, payload). With a milkfs volume,
`/Applications` is a bind mount of `/Volumes/Milk/Applications`, so packages persist and are
part of every snapshot and clone; without one it's tmpfs. `/bin` is in RAM, so the links
`/bin/<x> -> /Applications/<name>/<exec>` are recreated at boot by init (`milk relink -q`).
milk never replaces a `/bin` entry that isn't one of its links. The index cache is
`/Applications/.milk/index.txt`.

## Commands

```text
milk update            fetch index.txt from every URL in /etc/milk/repos
milk search [WORD]     name/description match; marks installed ones
milk info NAME         index entry + installed manifest (files, exec, dock)
milk install NAME[=VER]  the newest version (or exactly VER); download; check the signature,
                       the file's sha256 against the index, the payload sha256 against the
                       manifest; dependencies first; swap into /Applications/NAME; link
milk remove [--force] NAME  unlink /bin, delete /Applications/NAME; refused while another
                       installed package depends on it (unless --force)
milk autoremove        remove packages installed only as dependencies that nothing needs now
milk upgrade [NAME] [-y]  newer versions of installed packages (and any new dependencies)
                       into ONE clone, try-upgrade, as a transaction: any failure discards the
                       clone, so live never sees half an upgrade. Then `milk keep upgrade` or
                       `milk discard upgrade`; -y keeps it at once
milk list              installed packages
milk try NAME          snapshot milk-try-NAME, clone it as try-NAME (/Volumes/Milk-try-NAME),
                       install into the clone only; prints the path to run it from
milk keep NAME         promote the clone (it becomes the live volume), link /bin; the state
                       before is snapshot before-try-NAME (rollback to undo)
milk discard NAME      discard the clone and its snapshot; the live system never changed
milk relink [-q]       recreate /bin links
```

Repositories (`/etc/milk/repos`, one URL per line): `http://IP[:PORT]/path/` with an IP
address or `localhost` only, never a name (user programs have no resolver, and the repo is a
server on the Mac's 127.0.0.1, seen as 10.0.2.2). The default line is
`http://10.0.2.2:8765/`, what `scripts/milk-repo.sh serve` uses; run.sh's netdev is
restricted, so only boots with an open user netdev (pkg-test.sh) can reach it.

SHA-256 is `rt::sha256`, the same code `audit verify` uses. Every install / remove / try /
keep / discard writes a `user.pkg.*` audit record through `/dev/audit`, and the kernel logs
the snapshot / clone / promote / discard itself (`milkfs.*`, attributed to `/bin/milk`):

```text
29 ... /bin/milk  user.pkg.install name=fortune version=1.0.0 sha256=ff3f... from=http://10.0.2.2:PORT/fortune-1.0.0.milkpkg
39 ... /bin/milk  milkfs.clone     snap=milk-try-snake name=try-snake result=ok
41 ... /bin/milk  user.pkg.try     name=snake ... clone=try-snake at=/Volumes/Milk-try-snake
61 ... /bin/milk  milkfs.clone.promote name=try-moo2048 kept_as=before-try-moo2048 result=ok
62 ... /bin/milk  user.pkg.keep    name=moo2048 ... undo=rollback:before-try-moo2048
```

## Milk Store (`/bin/store`)

A desktop window (user/gui, MILK_WIN syscalls) over `/bin/milk`. Type `store` in the
Terminal, or use its dock entry (below).

* The list comes from `MILK_TABLE=1 milk search` (the structured output, parsed with
  `rt::table`), plus the index cache (versions, size), installed manifests (`depends`, and
  the `dock` colour as the tile colour; packages that aren't installed get an accent picked
  by a hash of the name) and `/sys/fs/milkfs/clones` (`try-<name>` means "trying").
* Rows: tile, name, description, and one badge: `installed` (matcha), `<ver> new` (honey,
  an upgrade), `trying` (blue), or the version. The card shows the details and the
  buttons Install, Try, Keep, Discard, Remove, Upgrade, each enabled only when it makes
  sense (Keep/Discard only while trying, Upgrade only with a newer version). The top bar
  has Update (`milk update`) and Upgrade all (`milk upgrade -y`).
* Every action runs `/bin/milk` (fork, a pipe on stdout+stderr, the read end
  non-blocking); the log box shows its output live, word-wrapped, with a moving progress
  strip, then a `done` / `failed` chip from the exit status, and the list is reloaded. So
  signatures, clones, `-y` and audit records are exactly the command line's. Upgrade from
  the Store keeps automatically (`-y`); Try leaves the clone for Keep or Discard.
* First run with no index cache runs `milk update` on its own.
* Keys: Up/Down (or the wheel) select, `i` install, `t` try, `k` keep, `d` discard,
  `r` remove, `u` upgrade, `g` update, `a` upgrade all, `q` quit.
* Theme: SPEC tokens. The wm doesn't publish its theme in `/dev/wm`'s status; the Store
  looks for a `theme dark` line there first, then `dark = true` in
  `/home/milk/.config/milk/settings`, and then uses the wm's "Chocolate milk" values.
* Dock entry: `rootfs/System/Applications/store/manifest` is a milk-format manifest with
  `exec /bin/store` and `dock Store #E58C97`, for the desktop's manifest dock reading.

## The toy packages (`packages/`)

A sibling cargo workspace (same target and linker script as `user/`, uses `user/rt` and
`user/gui`), so these never land in the initramfs. `scripts/milk-repo.sh build [DIR]`
(default `/tmp/milk-repo`) builds and packages them; `serve [PORT] [DIR]` runs
`python3 -m http.server --bind 127.0.0.1` on them, stopped by its PID.

* `fortune`: milk-themed quotes (`-a` all, `-n N`); reads its quotes file next to its
  `bin/`, so it works from a clone path too.
* `snake`: raw-mode terminal snake (TCSETS, ANSI escapes, ppoll clock): arrows/WASD, p, q.
  `snake --demo N` runs an autopilot with no input.
* `moo2048`: 2048 in a desktop window (gui), tiles named drop, sip, glass, ... COW.
  `moo2048 --selftest` checks the slide/merge rules and plays a random game, no window.

## Test (`scripts/pkg-test.sh`)

Fresh private milkfs + scratch images in a mktemp dir, repo served on a free 127.0.0.1
port, boot with an open user netdev, then in msh: update, search, info, install fortune
(both hashes checked, /bin link), run it twice, list, remove (`/Applications` empty), try
snake, run it from the clone, check the live `/Applications` and `/bin` don't have it,
discard (no clone left), try moo2048 + keep, run it from `/bin`, `milk selftest`, then the
host tampers with the served repo: a flipped byte in `fortune.milkpkg` (install refused: BAD
signature), `snake.milkpkg.sig` deleted (refused: unsigned; `--insecure` installs it with a
warning), a flipped byte in `index.txt` (update refused), each with a
`user.pkg.verify_fail` record; then the full repo comes back (5 packages, fortune 1.1.0
and `games`, a meta-package that depends on snake and moo2048): install `fortune=1.0.0`,
`search` offers the upgrade, `upgrade fortune` (1.1.0 runs from the clone, live stays 1.0.0),
`discard upgrade`, `upgrade -y` (live is 1.1.0), `install games` (snake first, as a
dependency), `remove snake` refused, `remove games`, `autoremove` (snake goes); then
`audit --event user.pkg`,
`audit --event milkfs`, `audit verify` (RAM and disk OK). After shutdown `mkmilkfs.py`
checks the image (moo2048 there, fortune and snake not, no clone), then a second boot of the
same image runs moo2048 from the relinked `/bin`, and `audit verify` is OK across boots.

## Not done

Version ranges beyond `>=`, conflicts/provides, key rotation or revocation lists, running a tried app *as if* the clone were `/` (needs
per-process roots).
