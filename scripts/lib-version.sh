#!/usr/bin/env bash
# Shared by version-check.sh, cut-release.sh, publish-release.sh and release-notes.sh (source it,
# don't run it): reading VERSION, SemVer checks and ordering, and the list of Cargo manifests
# and lock files that carry the version. Plain bash 3.2 + awk (macOS and Linux).

# milk_version_file [DIR]: the version in DIR/VERSION (default: here), trimmed.
milk_version_file() { tr -d '[:space:]' < "${1:-.}/VERSION"; }

# milk_semver_valid V: 0 if V is `MAJOR.MINOR.PATCH` or `MAJOR.MINOR.PATCH-prerelease` (SemVer 2.0:
# no leading zeros in numbers, dot-separated [0-9A-Za-z-] identifiers in the prerelease,
# no build metadata: cargo versions and tags carry none).
milk_semver_valid() {
  local v="$1" pre id
  [[ "$v" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$ ]] || return 1
  pre="${v#*-}"; [ "$pre" = "$v" ] && return 0
  local IFS=.
  for id in $pre; do
    case "$id" in *[!0-9]*) ;; 0) ;; 0*) return 1 ;; esac
  done
  return 0
}

# milk_semver_cmp A B: prints -1, 0 or 1 (SemVer precedence: a release is newer than its
# prereleases; prerelease identifiers compare one by one, numbers below words).
milk_semver_cmp() {
  local a="$1" b="$2" ca cb pa="" pb=""
  ca="${a%%-*}"; cb="${b%%-*}"
  [ "$a" != "$ca" ] && pa="${a#*-}"
  [ "$b" != "$cb" ] && pb="${b#*-}"
  local i x y
  for i in 1 2 3; do
    x="$(echo "$ca" | cut -d. -f$i)"; y="$(echo "$cb" | cut -d. -f$i)"
    if [ "$x" -lt "$y" ]; then echo -1; return; fi
    if [ "$x" -gt "$y" ]; then echo 1; return; fi
  done
  if [ -z "$pa" ] && [ -z "$pb" ]; then echo 0; return; fi
  if [ -z "$pa" ]; then echo 1; return; fi
  if [ -z "$pb" ]; then echo -1; return; fi
  local -a ra rb
  local IFS=.
  read -ra ra <<< "$pa"; read -ra rb <<< "$pb"
  local n=0 ia ib na nb
  while :; do
    ia="${ra[$n]-}"; ib="${rb[$n]-}"
    if [ $n -ge ${#ra[@]} ] && [ $n -ge ${#rb[@]} ]; then echo 0; return; fi
    if [ $n -ge ${#ra[@]} ]; then echo -1; return; fi
    if [ $n -ge ${#rb[@]} ]; then echo 1; return; fi
    if [ "$ia" != "$ib" ]; then
      na=0; nb=0
      case "$ia" in *[!0-9]*) ;; *) na=1 ;; esac
      case "$ib" in *[!0-9]*) ;; *) nb=1 ;; esac
      if [ $na = 1 ] && [ $nb = 1 ]; then
        if [ "$ia" -lt "$ib" ]; then echo -1; else echo 1; fi
      elif [ $na = 1 ]; then echo -1
      elif [ $nb = 1 ]; then echo 1
      elif [[ "$ia" < "$ib" ]]; then echo -1
      else echo 1; fi
      return
    fi
    n=$((n + 1))
  done
}

# milk_cargo_files KIND [DIR]: the tracked Cargo.toml (KIND=manifest) or Cargo.lock (KIND=lock)
# files that carry milkOS's own version, relative to DIR (default: here), one per line. The
# example packages in packages/ are separate products with their own versions: their manifests
# are left out, but packages/Cargo.lock is in (it lists the milkOS crates they build on).
milk_cargo_files() {
  local kind="$1" dir="${2:-.}" name
  case "$kind" in manifest) name=Cargo.toml ;; lock) name=Cargo.lock ;; *) return 1 ;; esac
  if git -C "$dir" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$dir" ls-files -- "$name" "*/$name"
  else
    (cd "$dir" && find . -name "$name" -not -path './target/*' -not -path './.git/*' | sed 's|^\./||')
  fi | { if [ "$kind" = manifest ]; then grep -v '^packages/'; else cat; fi; } | sort
}

# milk_manifest_info FILE: what the manifest says about versions, one fact per line:
#   workspace VERSION          a [workspace.package] version
#   package NAME VERSION       a [package] with its own version (VERSION is "inherited" for
#                              `version.workspace = true`)
milk_manifest_info() {
  awk '
    /^\[/ { sec = $0; sub(/[ \t]*(#.*)?$/, "", sec); next }
    function quoted(s,   a) { split(s, a, "\""); return a[2] }
    sec == "[package]" && /^name[ \t]*=/ { name = quoted($0) }
    sec == "[package]" && /^version\.workspace[ \t]*=[ \t]*true/ { ver = "inherited" }
    sec == "[package]" && /^version[ \t]*=[ \t]*"/ { ver = quoted($0) }
    sec == "[workspace.package]" && /^version[ \t]*=[ \t]*"/ { print "workspace " quoted($0) }
    END { if (name != "") print "package " name " " (ver == "" ? "none" : ver) }
  ' "$1"
}

# milk_lock_info FILE: "NAME VERSION" for every package in the lock file that has no source
# (that is: the ones in this repository, not downloaded).
milk_lock_info() {
  awk '
    function quoted(s,   a) { split(s, a, "\""); return a[2] }
    function flush() { if (name != "" && !src) print name " " ver; name = ""; ver = ""; src = 0 }
    /^\[\[package\]\]/ { flush(); next }
    /^name = / { name = quoted($0) }
    /^version = / { ver = quoted($0) }
    /^source = / { src = 1 }
    END { flush() }
  ' "$1"
}

# milk_set_manifest_version FILE OLD NEW: in FILE, change a `version = "OLD"` in [package] or
# [workspace.package] to NEW. Nothing else is touched (dependency versions are not in those
# sections). Prints the section it changed, if any.
milk_set_manifest_version() {
  local f="$1" old="$2" new="$3" tmp
  tmp="$(mktemp "${TMPDIR:-/tmp}/milk-manifest.XXXXXX")" || return 1
  awk -v old="$old" -v new="$new" '
    /^\[/ { sec = $0; sub(/[ \t]*(#.*)?$/, "", sec) }
    (sec == "[package]" || sec == "[workspace.package]") && $0 == "version = \"" old "\"" {
      print "version = \"" new "\""; changed = 1; next }
    { print }
    END { exit changed ? 0 : 3 }
  ' "$f" > "$tmp"
  local rc=$?
  if [ $rc = 0 ]; then cat "$tmp" > "$f"; fi
  rm -f "$tmp"
  return $rc
}
