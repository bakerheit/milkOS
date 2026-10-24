#!/usr/bin/env bash
# Build the release bundle and create the DRAFT GitHub Release for a tag. Run it by hand on the
# Mac after scripts/cut-release.sh, the gate and `git push` of the commit and the tag
# (docs/releasing.md); the release workflow runs the same script, so there is one path.
#
#   scripts/publish-release.sh vX.Y.Z [--dry-run]
#
# It refuses unless: the tag is vX.Y.Z with X.Y.Z equal to VERSION; the tag exists, is on
# origin/main and on origin; this checkout is clean and at the tag; CHANGELOG.md has a section
# for it (scripts/release-notes.sh); `gh` is there and logged in; no Release for the tag exists.
# Then: MILKOS_RELEASE=1 scripts/release.sh (the bundle zip and SHA256SUMS in dist/), and
# `gh release create TAG --draft --verify-tag` with the notes and both files attached; a tag
# with -alpha, -beta or -rc in it is marked a pre-release. The Release stays a draft: a human
# reads it on GitHub and presses Publish. Nothing else is touched; no tag is created or moved.
#
# --dry-run runs the checks that only look at this checkout and the notes extraction, and
# prints every other step instead of doing it (no build, no network, no gh).
# Environment: GH_REPO (gh's own: which repository), MILK_CUT_NO_FETCH=1 (skip `git fetch`),
# MILK_PUBLISH_BUILD (the command that builds dist/; default scripts/release.sh) and MILK_GH (the
# gh command; default gh) are test hooks.
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
source "$HERE/lib-version.sh"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
cd "$HERE/.." || exit 2

TAG=""; DRY=0
for a in "$@"; do
  case "$a" in
    --dry-run) DRY=1 ;;
    -h|--help) sed -n '2,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    -*) echo "publish-release: unknown option $a (try --help)" >&2; exit 2 ;;
    *) [ -z "$TAG" ] && TAG="$a" || { echo "publish-release: one tag only" >&2; exit 2; } ;;
  esac
done
[ -n "$TAG" ] || { echo "usage: scripts/publish-release.sh vX.Y.Z [--dry-run]" >&2; exit 2; }
refuse() {
  echo "publish-release: refusing: $*" >&2
  [ -n "${GITHUB_ACTIONS:-}" ] && echo "::error title=release::$*"
  exit 1
}
step() { if [ "$DRY" = 1 ]; then echo "  would: $*"; else echo "== $*"; fi; }

# ---- checks that only look at this checkout ------------------------------------------------------
case "$TAG" in v*) ;; *) refuse "'$TAG' is not a tag like v0.2.0" ;; esac
V="${TAG#v}"
milk_semver_valid "$V" || refuse "'$TAG' is not vMAJOR.MINOR.PATCH[-pre]"
FILEV="$(milk_version_file)"
[ "$V" = "$FILEV" ] || refuse "the tag says $V but VERSION says $FILEV (a release is built from the tagged commit)"
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || refuse "this is not a git checkout"
git rev-parse --verify -q "refs/tags/$TAG^{commit}" >/dev/null || refuse "no tag $TAG here (git fetch --tags, or run scripts/cut-release.sh)"
TAG_SHA="$(git rev-parse "refs/tags/$TAG^{commit}")"
[ "$(git rev-parse HEAD)" = "$TAG_SHA" ] || refuse "this checkout is at $(git rev-parse --short HEAD), not at $TAG (${TAG_SHA:0:9}); git checkout $TAG first"
[ -z "$(git status --porcelain)" ] || refuse "the tree has changes; a release is built from a clean checkout of the tag"
if [ "${MILK_CUT_NO_FETCH:-}" != 1 ] && [ "$DRY" = 0 ]; then
  git fetch --quiet origin main 2>/dev/null || echo "publish-release: note: couldn't fetch origin; using the origin/main this clone already has" >&2
fi
git rev-parse --verify -q origin/main >/dev/null || refuse "there is no origin/main to check the tag against"
git merge-base --is-ancestor "$TAG_SHA" origin/main || refuse "$TAG (${TAG_SHA:0:9}) is not on origin/main: push the release commit first (docs/releasing.md)"
NOTES="$(mktemp "${TMPDIR:-/tmp}/milk-notes.XXXXXX")" || exit 1
trap 'rm -f "$NOTES"' EXIT
"$HERE/release-notes.sh" "$TAG" > "$NOTES" || refuse "no usable release notes for $TAG in CHANGELOG.md"
echo "publish-release: $TAG is VERSION, is at HEAD, on origin/main; notes: $(wc -l < "$NOTES" | tr -d ' ') lines"

PRE=()
case "$V" in *-alpha*|*-beta*|*-rc*) PRE=(--prerelease) ;; esac
BUILD="${MILK_PUBLISH_BUILD:-$HERE/release.sh}"
GH="${MILK_GH:-gh}"
ZIP="dist/milkOS-$V.zip"

# ---- the rest looks at the network, or does the work -----------------------------------------------
if [ "$DRY" = 1 ]; then
  echo "publish-release: dry run; only the checks above ran."
  step "check that gh is installed and logged in (gh auth status)"
  step "check that $TAG is on origin (git ls-remote --tags origin) and that no Release for it exists (gh release view $TAG)"
  step "MILKOS_RELEASE=1 $BUILD   (builds $ZIP)"
  step "write dist/SHA256SUMS and check it"
  step "gh release create $TAG --draft --verify-tag --title \"milkOS $V\" --notes-file <the notes> ${PRE[*]-} $ZIP dist/SHA256SUMS"
  exit 0
fi

command -v "$GH" >/dev/null 2>&1 || refuse "gh (the GitHub CLI) is not installed"
"$GH" auth status >/dev/null 2>&1 || refuse "gh is not logged in (gh auth login)"
REMOTE_SHA="$(git ls-remote origin "refs/tags/$TAG^{}" 2>/dev/null | awk '{print $1}')"
[ -n "$REMOTE_SHA" ] || REMOTE_SHA="$(git ls-remote origin "refs/tags/$TAG" 2>/dev/null | awk '{print $1}')"
[ -n "$REMOTE_SHA" ] || refuse "$TAG is not on origin; push it first: git push origin $TAG"
[ "$REMOTE_SHA" = "$TAG_SHA" ] || refuse "origin's $TAG (${REMOTE_SHA:0:9}) is a different commit than yours (${TAG_SHA:0:9}); tags are never moved: stop and look"
if "$GH" release view "$TAG" >/dev/null 2>&1; then
  refuse "a Release for $TAG already exists (a bad draft: delete the draft on GitHub first; never reuse a tag, make the next version)"
fi

step "MILKOS_RELEASE=1 $BUILD"
rm -f dist/SHA256SUMS "$ZIP"
MILKOS_RELEASE=1 "$BUILD" || refuse "the release build failed"
[ -s "$ZIP" ] || refuse "the build made no $ZIP"
[ -s dist/SHA256SUMS ] || refuse "the build made no dist/SHA256SUMS"
(cd dist && shasum -a 256 -c SHA256SUMS >/dev/null 2>&1) || refuse "dist/SHA256SUMS does not match the zip"
grep -q "milkOS-$V.zip" dist/SHA256SUMS || refuse "dist/SHA256SUMS does not list milkOS-$V.zip"

step "gh release create $TAG --draft --verify-tag ${PRE[*]-}"
"$GH" release create "$TAG" --draft --verify-tag --title "milkOS $V" --notes-file "$NOTES" ${PRE[@]+"${PRE[@]}"} "$ZIP" dist/SHA256SUMS || refuse "gh release create failed (nothing is published; a half-made draft can be deleted on GitHub)"
echo "publish-release: draft Release $TAG created with $(basename "$ZIP") and SHA256SUMS. Read it on GitHub, then press Publish."
