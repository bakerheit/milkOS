#!/usr/bin/env bash
# Cut a release: bump the version everywhere, close the changelog section, commit, tag.
#
#   scripts/cut-release.sh X.Y.Z [--dry-run]        (X.Y.Z may be 0.3.0-rc.1 and the like)
#
# It refuses unless: X.Y.Z is SemVer and newer than VERSION; the tree is clean; you are on main
# (or on a detached HEAD at origin/main, as in the stage worktree) and local main equals
# origin/main; the tag doesn't exist; CHANGELOG.md has something under "## Unreleased".
# It then changes VERSION, every Cargo version and the three Cargo.lock files (offline),
# renames "## Unreleased" to "## X.Y.Z (DATE)" with a fresh empty "## Unreleased" above it,
# runs scripts/version-check.sh, commits "Release vX.Y.Z" and makes the annotated tag vX.Y.Z.
# It never pushes anything; it prints the next steps (docs/releasing.md). --dry-run prints
# what it would change and changes nothing.
#
# Environment: MILK_CUT_NO_FETCH=1 skips the `git fetch` of origin (offline); MILK_RELEASE_DATE
# sets the changelog date (default: today).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/lib-version.sh"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$HERE/.." || exit 2

NEW=""; DRY=0
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    -h|--help) sed -n '2,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "cut-release: unknown option $a (try --help)" >&2; exit 2 ;;
    *) [ -z "$NEW" ] && NEW="$a" || { echo "cut-release: one version only" >&2; exit 2; } ;;
  esac
done
refuse() { echo "cut-release: refusing: $*" >&2; exit 1; }
[ -n "$NEW" ] || { echo "usage: scripts/cut-release.sh X.Y.Z [--dry-run]" >&2; exit 2; }
NEW="${NEW#v}"
TAG="v$NEW"

# ---- checks ----------------------------------------------------------------------------------
milk_semver_valid "$NEW" || refuse "'$NEW' is not a version like 0.2.0 or 0.3.0-rc.1 (SemVer, no leading zeros, no +build)"
OLD="$(milk_version_file)"
milk_semver_valid "$OLD" || refuse "VERSION ($OLD) is not SemVer; fix it first"
[ "$(milk_semver_cmp "$NEW" "$OLD")" = 1 ] || refuse "$NEW is not newer than the current version $OLD"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || refuse "this is not a git checkout"
[ -z "$(git status --porcelain)" ] || refuse "the tree has changes (git status); commit or stash them first"

if [ "${MILK_CUT_NO_FETCH:-}" != 1 ]; then
  git fetch --quiet origin main 2>/dev/null || echo "cut-release: note: couldn't fetch origin; comparing with the origin/main this clone already has" >&2
fi
git rev-parse --verify -q origin/main >/dev/null || refuse "there is no origin/main to compare with"
HEAD_SHA="$(git rev-parse HEAD)"
ORIGIN_SHA="$(git rev-parse origin/main)"
BRANCH="$(git symbolic-ref -q --short HEAD || true)"
if [ -n "$BRANCH" ]; then
  [ "$BRANCH" = main ] || refuse "you are on branch '$BRANCH'; releases are cut from main"
  PUSH_REF="main"
else
  [ "$HEAD_SHA" = "$ORIGIN_SHA" ] || refuse "HEAD is detached at ${HEAD_SHA:0:9}, which is not origin/main (${ORIGIN_SHA:0:9}); a release is cut from the commit origin/main has"
  PUSH_REF="HEAD:main"
fi
if git rev-parse --verify -q refs/heads/main >/dev/null; then
  [ "$(git rev-parse refs/heads/main)" = "$ORIGIN_SHA" ] || refuse "local main ($(git rev-parse --short refs/heads/main)) is not origin/main ($(git rev-parse --short origin/main)); pull or push first"
fi
[ "$HEAD_SHA" = "$ORIGIN_SHA" ] || refuse "HEAD (${HEAD_SHA:0:9}) is not origin/main (${ORIGIN_SHA:0:9}); a release is cut from the commit origin/main has"
git rev-parse --verify -q "refs/tags/$TAG" >/dev/null && refuse "the tag $TAG already exists (tags are never moved or reused)"
if [ "${MILK_CUT_NO_FETCH:-}" != 1 ] && git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
  refuse "the tag $TAG already exists on origin (tags are never moved or reused)"
fi
grep -q '^## Unreleased$' CHANGELOG.md || refuse "CHANGELOG.md has no '## Unreleased' heading"
if [ -z "$(awk '/^## Unreleased$/ {on = 1; next} on && /^## / {exit} on && NF {print; exit}' CHANGELOG.md)" ]; then
  refuse "the Unreleased section of CHANGELOG.md is empty"
fi
"$HERE/version-check.sh" >/dev/null 2>&1 || { "$HERE/version-check.sh" >&2; refuse "version-check fails before the bump; fix that first"; }

DATE="${MILK_RELEASE_DATE:-$(date +%Y-%m-%d)}"
MANIFESTS="$(milk_cargo_files manifest)"
LOCK_DIRS=""
while IFS= read -r l; do [ -n "$l" ] && LOCK_DIRS="$LOCK_DIRS $(dirname "$l")"; done < <(milk_cargo_files lock)

# ---- dry run -----------------------------------------------------------------------------------
if [ "$DRY" = 1 ]; then
  echo "cut-release: dry run, nothing is changed. Releasing $OLD -> $NEW from ${HEAD_SHA:0:9} (origin/main)."
  echo "  VERSION: $OLD -> $NEW"
  while IFS= read -r f; do
    if grep -Eq "^version = \"$OLD\"" "$f"; then echo "  $f: version $OLD -> $NEW"; fi
  done <<< "$MANIFESTS"
  for d in $LOCK_DIRS; do echo "  $d/Cargo.lock: cargo update --workspace --offline"; done
  echo "  CHANGELOG.md: '## Unreleased' becomes '## $NEW ($DATE)', with a new empty '## Unreleased' above it"
  echo "  commit 'Release $TAG' (as bakerheit, noreply address), annotated tag $TAG"
  echo "  then it would print: gate, git push origin $PUSH_REF, git push origin $TAG, publish"
  exit 0
fi

# ---- do it -------------------------------------------------------------------------------------
undo() { echo "cut-release: $1; putting the files back" >&2; git checkout -q -- . ; exit 1; }
printf '%s\n' "$NEW" > VERSION
while IFS= read -r f; do
  [ -z "$f" ] && continue
  if grep -Eq "^version = \"$OLD\"" "$f"; then
    milk_set_manifest_version "$f" "$OLD" "$NEW" || undo "couldn't change the version in $f"
  fi
done <<< "$MANIFESTS"
for d in $LOCK_DIRS; do
  (cd "$d" && cargo update --workspace --offline --quiet) || undo "cargo update --workspace --offline failed in $d"
done
tmp="$(mktemp "${TMPDIR:-/tmp}/milk-changelog.XXXXXX")" || undo "no temp file"
awk -v new="$NEW" -v date="$DATE" '
  !done && /^## Unreleased$/ { print "## Unreleased"; print ""; print "## " new " (" date ")"; done = 1; next }
  { print }' CHANGELOG.md > "$tmp" && cat "$tmp" > CHANGELOG.md
rm -f "$tmp"
"$HERE/version-check.sh" >&2 || undo "version-check fails after the bump"

ID=(-c user.name=bakerheit -c user.email=270044729+bakerheit@users.noreply.github.com -c commit.gpgsign=false -c tag.gpgsign=false)
git add -A || undo "git add failed"
git "${ID[@]}" commit -q -m "Release $TAG" -m "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>" || undo "git commit failed"
git "${ID[@]}" tag -a "$TAG" -m "milkOS $NEW" || { echo "cut-release: the commit is made but the tag failed; fix and run: git tag -a $TAG -m 'milkOS $NEW'" >&2; exit 1; }

cat <<NEXT
cut-release: committed "Release $TAG" ($(git rev-parse --short HEAD)) and tagged $TAG. Nothing is pushed.
Next, in this order (docs/releasing.md):
  1. the gate:         scripts/check-all.sh && scripts/public-check.sh
  2. push the commit:  git push origin $PUSH_REF
  3. push the tag:     git push origin $TAG
  4. the draft Release: scripts/publish-release.sh $TAG     (or let the release workflow make it)
  5. read the draft on GitHub, then publish it there.
To back out before step 2: git tag -d $TAG && git reset --hard origin/main
NEXT
