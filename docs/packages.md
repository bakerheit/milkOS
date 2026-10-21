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
milk install NAME      download; check the file's sha256 against the index; unpack; check the
                       payload sha256 against the manifest; swap into /Applications/NAME; link
milk remove NAME       unlink /bin, delete /Applications/NAME
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
discard (no clone left), try moo2048 + keep, run it from `/bin`, `audit --event user.pkg`,
`audit --event milkfs`, `audit verify` (RAM and disk OK). After shutdown `mkmilkfs.py`
checks the image (moo2048 there, fortune and snake not, no clone), then a second boot of the
same image runs moo2048 from the relinked `/bin`, and `audit verify` is OK across boots.

## Not done

Dependencies, upgrades beyond reinstall, signatures (the index's sha256 is only as
trustworthy as the repo it came from), running a tried app *as if* the clone were `/` (needs
per-process roots), the desktop reading `dock` entries.
