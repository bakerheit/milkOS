#!/usr/bin/env bash
# One version number, everywhere (docs/versioning.md). <repo>/VERSION is the source; this fails
# if anything that carries the version disagrees with it:
#   * VERSION is a SemVer number (0.2.0, or 0.3.0-rc.1)
#   * every Cargo package version, the two [workspace.package] versions and the milkOS entries
#     in the Cargo.lock files (the kernel's version is what the audit log records at boot:
#     `kernel=milkOS-<version>`; the source must not spell a number out)
#   * the release bundle name (scripts/release.sh names it from VERSION), the text files that
#     get @VERSION@ filled in (System/Version.txt, motd, the bundle's README and launcher) and
#     that none of them spells the current number out
#   * the newest released heading in CHANGELOG.md is VERSION, and an "Unreleased" one exists
#   * the format and interface constants in docs/versioning.md match the code (so changing one
#     without touching that table fails)
# Usage: scripts/version-check.sh [DIR]     (DIR: a checkout other than this one; the tests use it)
# Prints "version-check: ok (...)" or every problem and "version-check: FAIL".
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/lib-version.sh"
ROOT="${1:-$HERE/..}"
cd "$ROOT" || exit 2
FAIL=0
bad() { echo "version-check: $*" >&2; FAIL=1; }

[ -f VERSION ] || { echo "version-check: FAIL, no VERSION file" >&2; exit 1; }
V="$(milk_version_file)"
milk_semver_valid "$V" || { bad "VERSION is '$V', which is not SemVer (0.2.0 or 0.3.0-rc.1)"; echo "version-check: FAIL" >&2; exit 1; }

# ---- Cargo -------------------------------------------------------------------------------
names=" "; npk=0; nws=0
while IFS= read -r f; do
  [ -z "$f" ] && continue
  while read -r kind a b; do
    case "$kind" in
      workspace)
        nws=$((nws + 1))
        [ "$a" = "$V" ] || bad "$f: [workspace.package] version is $a, VERSION is $V" ;;
      package)
        npk=$((npk + 1)); names="$names$a "
        case "$b" in
          inherited) ;;
          "$V") ;;
          none) bad "$f: package $a has no version" ;;
          *) bad "$f: package $a is $b, VERSION is $V" ;;
        esac ;;
    esac
  done < <(milk_manifest_info "$f")
done < <(milk_cargo_files manifest)
[ "$npk" -gt 0 ] || bad "found no Cargo.toml files (is this a checkout?)"
nlock=0
while IFS= read -r f; do
  [ -z "$f" ] && continue
  while read -r n v; do
    case "$names" in *" $n "*) ;; *) continue ;; esac
    nlock=$((nlock + 1))
    [ "$v" = "$V" ] || bad "$f: $n is $v, VERSION is $V (cargo update --workspace --offline, in that folder)"
  done < <(milk_lock_info "$f")
done < <(milk_cargo_files lock)

# The audit log's version string comes from the kernel's package version: no literal number.
if grep -q 'kernel=milkOS-' kernel/src/audit/mod.rs && grep -q 'env!("CARGO_PKG_VERSION")' kernel/src/audit/mod.rs \
   && ! grep -Eq 'kernel=milkOS-[0-9]' kernel/src/audit/mod.rs; then :; else
  bad "kernel/src/audit/mod.rs: the boot record must be built from env!(\"CARGO_PKG_VERSION\"), not a spelled-out number"
fi
# The strings people see come from build.rs (MILKOS_VERSION), not from a number in the source.
grep -q 'MILKOS_VERSION' kernel/build.rs || bad "kernel/build.rs no longer sets MILKOS_VERSION"

# ---- the bundle and the text files ---------------------------------------------------------
grep -Eq '^V=.*< VERSION' scripts/release.sh && grep -qF 'NAME="milkOS-$V"' scripts/release.sh \
  || bad "scripts/release.sh must set V from VERSION and name the bundle NAME=\"milkOS-\$V\""
for f in rootfs/System/Version.txt rootfs/System/Settings/motd scripts/release/README.txt scripts/release/milkOS.command; do
  [ -f "$f" ] || { bad "$f is missing"; continue; }
  grep -q '@VERSION@' "$f" || bad "$f has no @VERSION@ (the build fills it in)"
  if grep -Fq "$V" "$f"; then bad "$f spells out $V; use @VERSION@"; fi
done

# ---- the changelog -----------------------------------------------------------------------
if [ -f CHANGELOG.md ]; then
  grep -q '^## Unreleased$' CHANGELOG.md || bad "CHANGELOG.md has no '## Unreleased' heading"
  top="$(grep -m1 -E '^## [0-9]+\.[0-9]+\.[0-9]+' CHANGELOG.md | sed -E 's/^## ([^ ]+).*/\1/')"
  [ "$top" = "$V" ] || bad "the newest released heading in CHANGELOG.md is '${top:-none}', VERSION is $V (scripts/cut-release.sh does both)"
else
  bad "CHANGELOG.md is missing"
fi

# ---- format and interface constants (docs/versioning.md) ------------------------------------
doc=docs/versioning.md
nconst=0
if [ -f "$doc" ]; then
  # The block between "<!-- format-constants" and "-->": `path | the source line`. The line must
  # still be in that file, as written (a trailing comment may differ).
  while IFS= read -r line; do
    [ -z "$line" ] && continue
    path="${line%% | *}"; want="${line#* | }"
    nconst=$((nconst + 1))
    if [ ! -f "$path" ]; then bad "$doc lists $path, which does not exist"; continue; fi
    if ! WANT="$want" awk '
        BEGIN { want = ENVIRON["WANT"] }
        { l = $0; sub(/^[ \t]+/, "", l) }
        index(l, want) == 1 { rest = substr(l, length(want) + 1); if (rest == "" || rest ~ /^[ \t]*(#|\/\/|$)/) found = 1 }
        END { exit !found }' "$path"; then
      bad "$path no longer has \`$want\`: a format or interface version changed. Update the table in $doc (and the changelog, and write the migration it asks for)"
    fi
  done < <(sed -n '/^<!-- format-constants/,/^-->/p' "$doc" | sed '1d;$d')
  [ "$nconst" -gt 0 ] || bad "$doc has no <!-- format-constants --> block"
else
  bad "$doc is missing"
fi

if [ "$FAIL" = 0 ]; then
  echo "version-check: ok (version $V; $npk packages, $nws workspace versions, $nlock lock entries, $nconst format constants)"
  exit 0
fi
echo "version-check: FAIL" >&2
exit 1
