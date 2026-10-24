#!/usr/bin/env bash
# Print one version's section of CHANGELOG.md, for the text of a GitHub Release.
#   scripts/release-notes.sh v0.2.0        (the v is optional)
# The section is what is under "## 0.2.0 (DATE)", up to the next "## " heading, without the
# heading itself and without blank lines at either end. Fails (nothing on stdout) if there is
# no such section or it is empty.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
V="${1:-}"
case "$V" in -h|--help|"") echo "usage: scripts/release-notes.sh vX.Y.Z" >&2; [ -n "$V" ] && exit 0; exit 2 ;; esac
V="${V#v}"
case "$V" in [0-9]*) ;; *) echo "release-notes: '$1' is not a version like v0.2.0" >&2; exit 1 ;; esac
[ -f CHANGELOG.md ] || { echo "release-notes: no CHANGELOG.md" >&2; exit 1; }
OUT="$(awk -v v="$V" '
  /^## / { if (on) exit; h = $2; if (h == v) { on = 1; next } }
  on { print }' CHANGELOG.md | awk '
  { l[NR] = $0 }
  END { s = 1; e = NR
        while (s <= e && l[s] ~ /^[ \t]*$/) s++
        while (e >= s && l[e] ~ /^[ \t]*$/) e--
        for (i = s; i <= e; i++) print l[i] }')"
if [ -z "$OUT" ]; then
  if grep -Eq "^## $(printf '%s' "$V" | sed 's/[.+]/\\&/g') " CHANGELOG.md; then
    echo "release-notes: the CHANGELOG.md section for $V is empty" >&2
  else
    echo "release-notes: CHANGELOG.md has no section '## $V (DATE)'" >&2
  fi
  exit 1
fi
printf '%s\n' "$OUT"
