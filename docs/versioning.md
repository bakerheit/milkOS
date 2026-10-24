# Versions

milkOS has one version number, in the `VERSION` file at the top of the repository. Everything
else derives from it or is checked against it (`scripts/version-check.sh`). Releases are made
with `scripts/cut-release.sh` (docs/releasing.md).

## The scheme

[SemVer](https://semver.org) while the project is before 1.0: `0.MINOR.PATCH`.

| part | when it goes up | examples |
|---|---|---|
| MINOR | a batch of features, or any change that breaks something that worked before (a changed format, a removed command, a renumbered system call) | a new app, a new file system feature, a new `system` subcommand |
| PATCH | fixes only: no new feature, nothing breaks | a crash fix, a wrong word in a message, a slow path made fast |

A pre-release adds `-rc.N`, `-beta.N` or `-alpha.N` (`0.3.0-rc.1`). It sorts before its release
(`0.3.0-rc.1` < `0.3.0`), is published as a pre-release, and its tag and bundle carry the same
text. 1.0.0 is for when the formats below are ones we mean to keep; from then on MAJOR is for
breaking changes, MINOR for compatible features and PATCH for fixes.

The version is `MAJOR.MINOR.PATCH[-pre]`: no build metadata, no leading zeros. The git tag is
the version with a `v` in front (`v0.2.0`), annotated, made by `scripts/cut-release.sh`, never
moved and never reused.

## Where it shows

| where | what you see | how |
|---|---|---|
| `VERSION` | `0.2.0` | the source |
| Cargo packages | `0.2.0` | `[workspace.package] version` in `Cargo.toml` and `user/Cargo.toml` (packages inherit it with `version.workspace = true`); `user/filekit`, `user/fontkit` and `tls/probe` spell it out; the `Cargo.lock` files follow (including the example packages' in `packages/`, which lists the milkOS crates it builds on) |
| About, Settings, the boot message, `uname`, `/proc/version`, the Terminal banner, `system info`, `system version`, `/System/Version.txt`, the welcome text | `0.2.0` in a release; `0.2.0-dev (abc1234)` in any other build | `kernel/build.rs` sets `MILKOS_VERSION`; `@VERSION@` in `rootfs` text files is replaced with it |
| the audit log's first record | `kernel=milkOS-0.2.0 build=abc1234` | the kernel's package version, and `MILKOS_BUILD` |
| the release bundle | `milkOS-0.2.0.zip`, `SHA256SUMS` | `scripts/release.sh` names it from `VERSION` |

**Build id.** A build from a checkout is a dev build: `0.2.0-dev (abc1234)`, the short git hash,
and `abc1234+dirty` when the tree has changes (decided when the build script runs; it runs
again whenever the commit or anything under `kernel/src` changes). Without git (a source
tarball) the hash is `unknown`, and the build still works. A release build is made with
`MILKOS_RELEASE=1` (`scripts/release.sh` sets it) and shows the plain `0.2.0`. A release is
built from a clean checkout of the tag; `scripts/release.sh` checks that the kernel it built
says exactly that.

## Formats and interfaces with their own versions

These are separate from the milkOS version: a disk or a file written by one milkOS has to be
read by the next (or refused with a clear message), whatever the milkOS number says. Where a
number exists it is listed with the file that defines it. Changing one is a MINOR (it breaks
something that worked) and needs everything in the last column.

| format | version | defined in | a bump needs |
|---|---|---|---|
| milkfs on-disk format (superblock field at byte 8) | 3 (newest read; 0 is the oldest; a commit writes 1, a volume with reflinks 2, an encrypted one 3) | `milkfs/src/format.rs` (`VERSION`, `BASE_VERSION`, `REFLINK_VERSION`, `CRYPT_VERSION`, `MIN_VERSION`); the host tool `scripts/mkmilkfs.py` has its own copy (`VERSION` is what `mkfs` writes there) | older kernels must refuse the volume (the version stops them) and the new kernel must still read every older one, or `milkfs` must convert it (`convert.rs`, tested by `scripts/upgradetest.sh`); the same change in `scripts/mkmilkfs.py`; a section in docs/filesystem.md; a changelog line under **Storage** |
| milkfs send stream (`MILKSTRM`) | 1 | `milkfs/src/stream.rs` (`STREAM_VERSION`); `scripts/mkmilkfs.py` reads version 1 | receivers must refuse a newer stream by name; keep reading the older ones; docs/filesystem.md; changelog |
| milkfs encrypted send stream (`MILKSTRX`) | 1 | `milkfs/src/crypt.rs` (`SEALED_VERSION`) | same as the send stream; also `scripts/mkmilkfs.py` and `user/disks` which recognise the magic |
| package (`.milkpkg`) manifest | `format 1` | writer `scripts/mkpkg.py`, reader `user/apps/src/main.rs` (`Manifest::parse`) | `apps` must refuse unknown formats (it does) and keep installing format 1 packages; docs/packages.md; a changelog line under **Packages** |
| package repository index | `# milk repo index v1` | `scripts/mkpkg.py` (`index`) | a reader for the old one in `apps`; docs/packages.md; changelog |
| package signatures | `ed25519 <key> <hex>` in `FILE.sig` | `user/apps/src/main.rs` (the algorithm name is checked), `scripts/mkpkg.py` | a new algorithm name, never a changed meaning of `ed25519`; old signatures stay valid; docs/packages.md, docs/security.md; changelog |
| search index file (a cache) | 1 | `kernel/src/fs/search/mod.rs` (`FORMAT`) | nothing to migrate: another version is dropped and the index is rebuilt; a changelog line only if the rebuild is noticeable |
| structured pipes (`milk-table`) | 1 | `user/rt/src/table.rs` (`MAGIC`); the desktop reads it in `kernel/src/wm/apps/milkdrop.rs` | both ends change together; docs/shell.md; changelog |
| Milk Drop discovery datagrams | 1 | `user/drop/src/proto.rs` (`MAGIC`) | peers on the old one must keep working or be told; docs/networking.md; changelog |
| Milk Drop transfer protocol | 2 | `user/drop/src/proto.rs` (`XMAGIC`) | an old peer gets a clear refusal (version 1 already does); docs/networking.md; changelog |

The `format-constants` block below is read by `scripts/version-check.sh`: every line is a file
and the exact source line that holds the version there. If you change one of those lines, the
check fails until this block (and so, by the table above, the docs and changelog) is touched
too. That is the whole mechanism: a reminder, not a proof that the migration is right.

<!-- format-constants
milkfs/src/format.rs | pub const MAGIC: &[u8; 8] = b"MILKFS01";
milkfs/src/format.rs | pub const VERSION: u32 = 3;
milkfs/src/format.rs | pub const BASE_VERSION: u32 = 1;
milkfs/src/format.rs | pub const REFLINK_VERSION: u32 = 2;
milkfs/src/format.rs | pub const CRYPT_VERSION: u32 = 3;
milkfs/src/format.rs | pub const MIN_VERSION: u32 = 0;
scripts/mkmilkfs.py | VERSION = 1
scripts/mkmilkfs.py | MAX_VERSION = 3
scripts/mkmilkfs.py | CRYPT_VERSION = 3
scripts/mkmilkfs.py | MIN_VERSION = 0
milkfs/src/stream.rs | pub const STREAM_MAGIC: &[u8; 8] = b"MILKSTRM";
milkfs/src/stream.rs | pub const STREAM_VERSION: u32 = 1;
milkfs/src/crypt.rs | pub const SEALED_MAGIC: &[u8; 8] = b"MILKSTRX";
milkfs/src/crypt.rs | pub const SEALED_VERSION: u32 = 1;
scripts/mkpkg.py | lines = ["format 1"]
user/apps/src/main.rs | if get("format").as_deref() != Some("1") {
scripts/mkpkg.py | lines = ["# milk repo index v1"]
user/apps/src/main.rs | if alg != "ed25519" {
kernel/src/fs/search/mod.rs | const FORMAT: u32 = 1;
kernel/src/fs/search/mod.rs | const MAGIC: &[u8; 4] = b"MKSX";
user/rt/src/table.rs | pub const MAGIC: &str = "\x1emilk-table 1";
user/drop/src/proto.rs | pub const MAGIC: &str = "MILKDROP/1";
user/drop/src/proto.rs | pub const XMAGIC: &str = "MILKDROP/2";
-->

## Things that have no version number

These are real interfaces, but the code has no number for them. Change them with the same
care, and say so in the changelog.

| what | how it is kept compatible | defined in |
|---|---|---|
| system call ABI | Linux aarch64 numbering for the Linux calls (never renumbered); milkOS's own calls sit at `0x1000` and up and are only added to. Removing or renumbering one is a breaking change: a MINOR, with a changelog line, and `user/rt` changed in the same commit | `kernel/src/proc/syscall.rs` (`mod nr`), `user/rt/src/sys.rs` |
| settings file `~/.config/milk/settings` | plain `key = value` lines; a key it doesn't know is ignored and a missing one takes its default, so adding a key needs nothing. Renaming a key or changing what a value means needs the old spelling still read | `kernel/src/wm/prefs.rs` |
| first-boot migration of older disks | there is no marker file: `layout::migrate` is idempotent and recognises an old volume by its old folders (`home/`, `Applications/`, `etc/milk/`), moves them by renames, and audits `fs.layout.migrate`. The built-in app list `.milkos/builtin-apps` on the volume is how a new milkOS removes apps an older one shipped. A new layout change adds another step to `migrate` (never edits an old one) and a test that boots the old shape | `kernel/src/fs/layout.rs`, docs/filesystem.md "Older disks" |
| audit log records | `<seq> <time_ns> <pid> <exe> <event> <detail> <prev_hash> <hash>` lines chained by SHA-256; event names and `key=value` details are only added to. The first record says which kernel wrote the log | `kernel/src/audit/mod.rs`, docs/security.md |

## Adding a new versioned format

Give it a magic and a version number in one named constant; make the reader refuse a newer
version with a message that says so; keep reading every older version it ever wrote; add its
constants to the `format-constants` block and a row to the table; write the changelog line.
