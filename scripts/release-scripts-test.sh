#!/usr/bin/env bash
# Tests of the release scripts, with no VM and no network, in scratch clones of this repository
# under /tmp (each with its own bare "origin", so nothing here can push to the real one):
#   * version-check.sh: passes on the tree, and catches each kind of mismatch (a Cargo package
#     version, the workspace versions, a lock file, VERSION alone, the bundle name, the audit
#     string, a spelled-out number in a text file, the changelog, a changed format constant)
#   * cut-release.sh: a good cut (files, lock files, changelog, commit author and message,
#     annotated tag, nothing pushed, the next steps printed), every refusal (not SemVer, not
#     newer, dirty tree, wrong branch, detached HEAD elsewhere, local main != origin/main, tag
#     exists, empty Unreleased), --dry-run changing nothing, a pre-release followed by its release
#   * release-notes.sh: a section, a pre-release's, a missing one, an empty one
#   * publish-release.sh: with a stub `gh` first in PATH and a stub build (the real gh and the real
#     repository are never touched): the draft with the right arguments, a pre-release flag,
#     --dry-run, and every refusal
#   * install-hooks.sh: the pre-push hook installs, blocks a bad push, can be skipped and removed
#   * ci-local.sh's argument handling, and the workflows' rules (parse, pinned actions, permissions,
#     timeouts, standard runners, no pull_request_target) when ruby is there
# Prints "release-scripts-test: N checks passed" and PASS; a failure keeps the work folder.
set -uo pipefail
cd "$(dirname "$0")/.."
source scripts/grepq.sh
REPO="$(pwd)"
T="$(mktemp -d /tmp/milk-relscripts.XXXXXX)"; T="$(cd "$T" && pwd -P)"
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
FAIL=0; N=0
ok() { N=$((N + 1)); echo "  ok: $*"; }
bad() { N=$((N + 1)); FAIL=1; echo "  FAIL: $*"; }
# has TEXT PATTERN: TEXT contains the fixed string PATTERN
has() { case "$1" in *"$2"*) return 0 ;; *) return 1 ;; esac; }
# t DESCRIPTION CMD...: runs CMD, keeps its output in $OUT and its exit status in $RC
t() { local d="$1"; shift; OUT="$("$@" 2>&1)"; RC=$?; }
expect_ok() { local d="$1"; shift; OUT="$("$@" 2>&1)"; RC=$?; if [ $RC = 0 ]; then ok "$d"; else bad "$d (exit $RC): $(echo "$OUT" | tail -5)"; fi; }
# expect_refused DESCRIPTION WORDS CMD...: exit 1 and the output says WORDS
expect_refused() {
  local d="$1" w="$2"; shift 2
  OUT="$("$@" 2>&1)"; RC=$?
  if [ $RC = 1 ] && has "$OUT" "$w"; then ok "$d"; else bad "$d (exit $RC, wanted 1 and '$w'): $(echo "$OUT" | tail -3)"; fi
}

ID=(-c user.name=scratch -c user.email=270044729+bakerheit@users.noreply.github.com -c commit.gpgsign=false -c tag.gpgsign=false)

# mkscratch NAME: $T/NAME/work is a clone of this tree (committed state plus uncommitted changes,
# as one more commit) on branch main, with a bare origin next to it that already has main.
mkscratch() {
  local d="$T/$1"
  mkdir -p "$d"
  git clone -q --local "$REPO" "$d/work" 2>/dev/null || { echo "clone failed"; exit 1; }
  git init -q --bare -b main "$d/origin.git"
  (
    cd "$d/work" || exit 1
    git checkout -q -B main
    # the uncommitted state of the real tree, so the scripts under test are the ones being edited
    while IFS= read -r -d '' f; do mkdir -p "$(dirname "$f")"; cp -p "$REPO/$f" "$f"; done \
      < <(git -C "$REPO" ls-files -z -m -o --exclude-standard)
    while IFS= read -r -d '' f; do rm -f "$f"; done < <(git -C "$REPO" ls-files -z -d)
    git add -A
    git "${ID[@]}" commit -q -m "scratch baseline" --allow-empty
    git remote set-url origin "$d/origin.git"
    git push -q origin main
    git fetch -q origin
  ) || exit 1
}
reset_scratch() { # back to a clean main == origin/main, no extra branches or tags
  git checkout -q -f main 2>/dev/null; git reset -q --hard origin/main; git clean -qfd
  git branch --list | sed 's/^[* ] //' | grep -v '^main$' | while read -r b; do git branch -q -D "$b"; done
  git tag --list | while read -r x; do git tag -d "$x" >/dev/null; done
}

echo "== scripts parse and are executable"
for s in scripts/lib-version.sh scripts/version-check.sh scripts/cut-release.sh scripts/release-notes.sh \
         scripts/publish-release.sh scripts/release.sh scripts/ci-local.sh scripts/install-hooks.sh scripts/release-scripts-test.sh; do
  if bash -n "$s" 2>/dev/null; then ok "$s: bash -n"; else bad "$s: bash -n"; fi
done
if command -v shellcheck >/dev/null 2>&1; then
  if shellcheck -S warning scripts/cut-release.sh scripts/release-notes.sh scripts/publish-release.sh scripts/version-check.sh scripts/ci-local.sh scripts/install-hooks.sh >/dev/null 2>&1; then ok "shellcheck (warnings and up)"; else bad "shellcheck: $(shellcheck -S warning scripts/cut-release.sh scripts/release-notes.sh scripts/publish-release.sh scripts/version-check.sh scripts/ci-local.sh scripts/install-hooks.sh 2>&1 | head -8)"; fi
else
  echo "  (shellcheck is not installed: skipped)"
fi

echo "== SemVer helpers"
source scripts/lib-version.sh
for v in 0.2.0 1.10.3 0.3.0-rc.1 0.3.0-alpha 1.0.0-x.7.z-9; do milk_semver_valid "$v" && ok "valid: $v" || bad "should be valid: $v"; done
for v in 1.0 01.2.3 1.2.3.4 v1.2.3 1.2.3+build 1.2.3- 1.2.3-01 1.2.3-rc..1 abc ""; do milk_semver_valid "$v" && bad "should be invalid: '$v'" || ok "invalid: '$v'"; done
cmp_is() { [ "$(milk_semver_cmp "$1" "$2")" = "$3" ] && ok "$1 vs $2 is $3" || bad "$1 vs $2 should be $3, got $(milk_semver_cmp "$1" "$2")"; }
cmp_is 0.2.0 0.1.0 1; cmp_is 0.1.0 0.2.0 -1; cmp_is 0.2.0 0.2.0 0; cmp_is 0.10.0 0.9.0 1; cmp_is 0.2.0-rc.1 0.2.0 -1
cmp_is 0.2.0 0.2.0-rc.1 1; cmp_is 0.2.0-rc.2 0.2.0-rc.10 -1; cmp_is 0.2.0-alpha 0.2.0-beta -1; cmp_is 0.2.0-rc.1 0.2.0-rc 1
cmp_is 0.2.0-1 0.2.0-a -1; cmp_is 0.2.1 0.2.0-rc.9 1

mkscratch a
cd "$T/a/work" || exit 1

echo "== version-check.sh"
expect_ok "passes on the tree" scripts/version-check.sh
V0="$(cat VERSION)"
vc_catches() { # description WORDS (the tree is edited already)
  OUT="$(scripts/version-check.sh 2>&1)"; RC=$?
  if [ $RC = 1 ] && has "$OUT" "$2"; then ok "catches: $1"; else bad "should catch $1 (exit $RC, wanted '$2'): $(echo "$OUT" | tail -3)"; fi
  git checkout -q -- . ; git clean -qfd
}
sed -i.bak 's/^version.workspace = true$/version = "0.9.9"/' kernel/Cargo.toml && rm -f kernel/Cargo.toml.bak
vc_catches "a Cargo package with its own, different version" "kernel/Cargo.toml: package milkos-kernel is 0.9.9"
sed -i.bak 's/^version = "'"$V0"'"$/version = "0.9.9"/' Cargo.toml && rm -f Cargo.toml.bak
vc_catches "the root [workspace.package] version" "Cargo.toml: [workspace.package] version is 0.9.9"
sed -i.bak 's/^version = "'"$V0"'"$/version = "0.9.9"/' user/Cargo.toml && rm -f user/Cargo.toml.bak
vc_catches "the user [workspace.package] version" "user/Cargo.toml: [workspace.package] version is 0.9.9"
sed -i.bak 's/^version = "'"$V0"'"$/version = "0.9.9"/' user/filekit/Cargo.toml && rm -f user/filekit/Cargo.toml.bak
vc_catches "a spelled-out version (filekit)" "user/filekit/Cargo.toml: package filekit is 0.9.9"
# a lock file entry for the kernel
awk '/^name = "milkos-kernel"$/ {print; getline; print "version = \"0.9.9\""; next} {print}' Cargo.lock > Cargo.lock.new && mv Cargo.lock.new Cargo.lock
vc_catches "a stale Cargo.lock" "Cargo.lock: milkos-kernel is 0.9.9"
printf '0.1.1\n' > VERSION
vc_catches "VERSION changed alone" "VERSION is 0.1.1"
printf 'banana\n' > VERSION
OUT="$(scripts/version-check.sh 2>&1)"; RC=$?; { [ $RC = 1 ] && has "$OUT" "not SemVer"; } && ok "catches: VERSION that is not SemVer" || bad "VERSION=banana: $OUT"; git checkout -q -- .
sed -i.bak 's/^NAME="milkOS-\$V"/NAME="milk-$V"/' scripts/release.sh && rm -f scripts/release.sh.bak
vc_catches "the bundle name not coming from VERSION" "scripts/release.sh must set V from VERSION"
sed -i.bak 's/env!("CARGO_PKG_VERSION")/"0.1.0"/' kernel/src/audit/mod.rs && rm -f kernel/src/audit/mod.rs.bak
vc_catches "the audit log's version string spelled out" "kernel/src/audit/mod.rs"
sed -i.bak "s/@VERSION@/$V0/" rootfs/System/Settings/motd && rm -f rootfs/System/Settings/motd.bak
vc_catches "a version number spelled out in the motd" "rootfs/System/Settings/motd"
sed -i.bak 's/^## '"$V0"' /## 0.0.9 /' CHANGELOG.md && rm -f CHANGELOG.md.bak
vc_catches "the changelog's newest release not being VERSION" "newest released heading"
sed -i.bak 's/^pub const VERSION: u32 = 3;/pub const VERSION: u32 = 4;/' milkfs/src/format.rs && rm -f milkfs/src/format.rs.bak
vc_catches "a milkfs format version changed without the docs table" "milkfs/src/format.rs no longer has"
sed -i.bak 's/^const FORMAT: u32 = 1;/const FORMAT: u32 = 2;/' kernel/src/fs/search/mod.rs && rm -f kernel/src/fs/search/mod.rs.bak
vc_catches "the search index format changed" "kernel/src/fs/search/mod.rs no longer has"
sed -i.bak 's/lines = \["format 1"\]/lines = ["format 2"]/' scripts/mkpkg.py && rm -f scripts/mkpkg.py.bak
vc_catches "the package format changed" "scripts/mkpkg.py no longer has"
# the same change, with the docs line updated too, is accepted
sed -i.bak 's/^pub const VERSION: u32 = 3;/pub const VERSION: u32 = 4;/' milkfs/src/format.rs && rm -f milkfs/src/format.rs.bak
sed -i.bak 's/^milkfs\/src\/format.rs | pub const VERSION: u32 = 3;/milkfs\/src\/format.rs | pub const VERSION: u32 = 4;/' docs/versioning.md && rm -f docs/versioning.md.bak
OUT="$(scripts/version-check.sh 2>&1)"; RC=$?; [ $RC = 0 ] && ok "a format change with the docs block updated passes" || bad "docs block updated: $OUT"
git checkout -q -- .
expect_ok "clean again" scripts/version-check.sh

echo "== release-notes.sh"
OUT="$(scripts/release-notes.sh v$V0 2>&1)"; RC=$?
{ [ $RC = 0 ] && has "$OUT" "The first release" && ! has "$OUT" "## "; } && ok "prints the $V0 section without its heading" || bad "notes for $V0: $OUT"
[ "$(scripts/release-notes.sh "$V0" 2>&1)" = "$OUT" ] && ok "the leading v is optional" || bad "notes without v"
case "$OUT" in [[:space:]]*|*[[:space:]]) bad "notes have blank space at an end" ;; *) ok "no blank lines at either end" ;; esac
expect_refused "a missing section fails" "no section" scripts/release-notes.sh v9.9.9
mkdir -p "$T/notes/scripts" && cp scripts/release-notes.sh "$T/notes/scripts/"
printf '# Changelog\n\n## Unreleased\n\n- x\n\n## 0.5.0 (2030-01-01)\n\n\n## 0.4.0 (2029-01-01)\n\n- four\n- also four\n\n\n## 0.3.0-rc.1 (2028-01-01)\n- rc\n' > "$T/notes/CHANGELOG.md"
expect_refused "an empty section fails" "is empty" "$T/notes/scripts/release-notes.sh" v0.5.0
OUT="$("$T/notes/scripts/release-notes.sh" v0.4.0 2>&1)"; [ "$OUT" = "$(printf -- '- four\n- also four')" ] && ok "stops at the next heading, trims blank lines" || bad "0.4.0 notes: $OUT"
OUT="$("$T/notes/scripts/release-notes.sh" v0.3.0-rc.1 2>&1)"; [ "$OUT" = "- rc" ] && ok "a pre-release's section" || bad "rc notes: $OUT"
expect_refused "Unreleased is not a version" "is not a version" "$T/notes/scripts/release-notes.sh" Unreleased

echo "== cut-release.sh: refusals"
cd "$T/a/work" && reset_scratch
cut() { scripts/cut-release.sh "$@"; }
for v in 1.0 v 01.2.3 0.2.0+build abc 0.2.0-; do expect_refused "refuses '$v' (not SemVer)" "not a version" cut "$v"; done
expect_refused "refuses the current version" "not newer" cut "$V0"
expect_refused "refuses an older version" "not newer" cut 0.0.9
expect_refused "refuses a pre-release of the current version" "not newer" cut "$V0-rc.1"
t x scripts/cut-release.sh; [ $RC = 2 ] && ok "no argument is a usage error" || bad "no argument: $RC"
echo x >> README.md
expect_refused "refuses a changed file" "has changes" cut 0.2.0
git checkout -q -- .; echo x > stray.txt
expect_refused "refuses an untracked file" "has changes" cut 0.2.0
rm -f stray.txt
git checkout -q -b topic
expect_refused "refuses another branch" "releases are cut from main" cut 0.2.0
git checkout -q main; git branch -q -D topic
git checkout -q --detach; echo y > f.txt; git add f.txt; git "${ID[@]}" commit -q -m "detached work"
expect_refused "refuses a detached HEAD that is not origin/main" "not origin/main" cut 0.2.0
reset_scratch
echo z > g.txt; git add g.txt; git "${ID[@]}" commit -q -m "unpushed"
expect_refused "refuses when local main is ahead of origin/main" "not origin/main" cut 0.2.0
reset_scratch
git clone -q -b main "$T/a/origin.git" "$T/a/other" 2>/dev/null
( cd "$T/a/other" && echo w > w.txt && git add w.txt && git "${ID[@]}" commit -q -m "someone else" && git push -q origin HEAD:main )
expect_refused "refuses when origin/main has moved on (local main is behind)" "not origin/main" cut 0.2.0
git pull -q --ff-only origin main; reset_scratch
git tag v0.2.0
expect_refused "refuses when the tag exists" "already exists" cut 0.2.0
git tag -d v0.2.0 >/dev/null
cp CHANGELOG.md "$T/CHANGELOG.keep"
awk '/^## Unreleased$/ {print; print ""; skip = 1; next} skip && /^## / {skip = 0} !skip {print}' CHANGELOG.md > c.new && mv c.new CHANGELOG.md
git add -A; git "${ID[@]}" commit -q -m "empty unreleased"; git push -q origin main
expect_refused "refuses an empty Unreleased section" "is empty" cut 0.2.0
git reset -q --hard HEAD~1; git push -q -f origin main
git fetch -q origin
t x git status --porcelain; [ -z "$OUT" ] && ok "refusals left the tree clean" || bad "tree changed: $OUT"

echo "== cut-release.sh: --dry-run"
H0="$(git rev-parse HEAD)"
OUT="$(cut 0.2.0 --dry-run 2>&1)"; RC=$?
{ [ $RC = 0 ] && has "$OUT" "dry run" && has "$OUT" "VERSION: $V0 -> 0.2.0" && has "$OUT" "tls/probe/Cargo.toml" && has "$OUT" "user/Cargo.lock" && has "$OUT" "tag v0.2.0"; } && ok "prints what it would change" || bad "dry run output (exit $RC): $OUT"
[ -z "$(git status --porcelain)" ] && [ "$(git rev-parse HEAD)" = "$H0" ] && [ -z "$(git tag)" ] && [ "$(cat VERSION)" = "$V0" ] && ok "a dry run changes nothing (tree, HEAD, tags, VERSION)" || bad "dry run changed something"
echo x >> README.md
expect_refused "a dry run refuses what a real run refuses" "has changes" cut 0.2.0 --dry-run
git checkout -q -- .
git checkout -q --detach origin/main
OUT="$(cut 0.2.0 --dry-run 2>&1)"; RC=$?; { [ $RC = 0 ] && has "$OUT" "HEAD:main"; } && ok "a detached HEAD at origin/main is allowed (the stage worktree), and the push is HEAD:main" || bad "detached dry run: $OUT"
git checkout -q main

echo "== cut-release.sh: a good cut"
export MILK_RELEASE_DATE=2030-01-02
OUT="$(cut 0.2.0 2>&1)"; RC=$?
if [ $RC = 0 ]; then ok "cut 0.2.0"; else bad "cut 0.2.0 (exit $RC): $OUT"; fi
[ "$(cat VERSION)" = "0.2.0" ] && ok "VERSION is 0.2.0" || bad "VERSION: $(cat VERSION)"
OUT2="$(scripts/version-check.sh 2>&1)"; [ $? = 0 ] && has "$OUT2" "version 0.2.0" && ok "version-check passes on the result" || bad "version-check after the cut: $OUT2"
[ -z "$(git status --porcelain)" ] && ok "everything is committed" || bad "tree not clean after cut"
has "$(git cat-file -t v0.2.0)" tag && ok "v0.2.0 is an annotated tag" || bad "tag type"
[ "$(git rev-parse v0.2.0^{commit})" = "$(git rev-parse HEAD)" ] && ok "the tag is on the release commit" || bad "tag commit"
[ "$(git log -1 --format=%s)" = "Release v0.2.0" ] && ok "commit message is 'Release v0.2.0'" || bad "subject: $(git log -1 --format=%s)"
has "$(git log -1 --format=%b)" "Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>" && ok "the co-author trailer is there" || bad "trailer"
[ "$(git log -1 --format='%an <%ae>|%cn <%ce>')" = "bakerheit <270044729+bakerheit@users.noreply.github.com>|bakerheit <270044729+bakerheit@users.noreply.github.com>" ] && ok "author and committer are bakerheit's noreply address" || bad "identity: $(git log -1 --format='%an <%ae>|%cn <%ce>')"
[ "$(git tag -l --format='%(taggername) <%(taggeremail)>' v0.2.0)" = "bakerheit <<270044729+bakerheit@users.noreply.github.com>>" ] && ok "the tagger is the same" || bad "tagger: $(git tag -l --format='%(taggername) <%(taggeremail)>' v0.2.0)"
for f in Cargo.toml user/Cargo.toml tls/probe/Cargo.toml user/filekit/Cargo.toml user/fontkit/Cargo.toml; do
  [ "$(grep -c '^version = "0.2.0"$' "$f")" = 1 ] && ok "$f is 0.2.0" || bad "$f not bumped"
done
for l in Cargo.lock user/Cargo.lock tls/probe/Cargo.lock packages/Cargo.lock; do
  has "$(git diff HEAD~1 -- "$l")" '+version = "0.2.0"' && ok "$l follows" || bad "$l not updated"
done
[ -z "$(git diff HEAD~1 --stat -- packages/fortune packages/snake packages/moo2048 packages/Cargo.toml)" ] && ok "the example packages keep their own versions" || bad "touched the example packages' manifests"
[ "$(sed -n 1,6p CHANGELOG.md)" = "$(printf '# Changelog\n\n## Unreleased\n\n## 0.2.0 (2030-01-02)\n')" ] && ok "changelog: an empty Unreleased above '## 0.2.0 (2030-01-02)'" || bad "changelog head: $(sed -n 1,7p CHANGELOG.md)"
NOTES="$(scripts/release-notes.sh v0.2.0)"; has "$NOTES" "The Browser is now a sandboxed" && ok "the notes for 0.2.0 are the old Unreleased text" || bad "notes after cut"
[ "$(git rev-parse origin/main)" = "$(git -C "$T/a/origin.git" rev-parse main)" ] && [ "$(git rev-parse origin/main)" = "$H0" ] && [ -z "$(git -C "$T/a/origin.git" tag)" ] && ok "nothing was pushed (origin's main unchanged, no tag there)" || bad "something reached origin"
has "$OUT" "git push origin main" && has "$OUT" "git push origin v0.2.0" && has "$OUT" "scripts/publish-release.sh v0.2.0" && has "$OUT" "scripts/check-all.sh" && ok "the next steps are printed" || bad "next steps: $OUT"
expect_refused "a second cut of the same version is refused" "not newer" cut 0.2.0

echo "== publish-release.sh (stub gh, stub build)"
mkdir -p "$T/bin"
cat > "$T/bin/gh" <<'GH'
#!/usr/bin/env bash
# a stand-in for the GitHub CLI: records its arguments and answers the few calls publish-release makes
echo "gh $*" >> "$GH_STUB_LOG"
case "$1 $2" in
  "auth status") exit 0 ;;
  "release view") [ "${GH_STUB_EXISTS:-}" = 1 ] && exit 0; exit 1 ;;
  "release create")
    shift 2
    while [ $# -gt 0 ]; do
      case "$1" in
        --notes-file) cp "$2" "$GH_STUB_NOTES"; shift ;;
        --title) shift ;;
        --draft|--verify-tag|--prerelease) ;;
        -*) ;;
        v[0-9]*) ;;
        *) [ -f "$1" ] || { echo "stub gh: no such file $1" >&2; exit 1; }; echo "asset $1" >> "$GH_STUB_LOG" ;;
      esac
      shift
    done
    exit 0 ;;
esac
echo "stub gh: unexpected call: $*" >&2; exit 1
GH
cat > "$T/bin/fake-release.sh" <<'FR'
#!/usr/bin/env bash
# a stand-in for scripts/release.sh: the zip and the checksums, and a record of MILKOS_RELEASE
V="$(tr -d '[:space:]' < VERSION)"
echo "MILKOS_RELEASE=${MILKOS_RELEASE:-unset}" >> "$GH_STUB_LOG"
mkdir -p dist && echo "zip of $V" > "dist/milkOS-$V.zip" && (cd dist && shasum -a 256 "milkOS-$V.zip" > SHA256SUMS)
FR
chmod +x "$T/bin/gh" "$T/bin/fake-release.sh"
export GH_STUB_LOG="$T/gh.log" GH_STUB_NOTES="$T/notes.out" GH_REPO="scratch/does-not-exist"
: > "$GH_STUB_LOG"
SAFEPATH="$T/bin:$PATH"
[ "$(PATH="$SAFEPATH" command -v gh)" = "$T/bin/gh" ] && ok "the stub gh is the gh the scripts will find" || bad "gh is not the stub"
pub() { PATH="$SAFEPATH" MILK_PUBLISH_BUILD="$T/bin/fake-release.sh" scripts/publish-release.sh "$@"; }
expect_refused "refuses before the release commit is pushed (not on origin/main)" "is not on origin/main" pub v0.2.0
git push -q origin main
expect_refused "refuses while the tag is not on origin" "is not on origin" pub v0.2.0
expect_refused "refuses a tag that is not VERSION" "VERSION says" pub v0.3.0
expect_refused "refuses a name that is not a tag" "not a tag" pub 0.2.0
TAGOBJ="$(git rev-parse v0.2.0)"; git tag -d v0.2.0 >/dev/null
expect_refused "refuses a tag that does not exist here" "no tag v0.2.0" pub v0.2.0
git update-ref refs/tags/v0.2.0 "$TAGOBJ"
git push -q origin v0.2.0
: > "$GH_STUB_LOG"
OUT="$(pub v0.2.0 --dry-run 2>&1)"; RC=$?
{ [ $RC = 0 ] && has "$OUT" "would: gh release create v0.2.0 --draft --verify-tag" && has "$OUT" "MILKOS_RELEASE=1"; } && ok "--dry-run prints the build and the gh call" || bad "dry run (exit $RC): $OUT"
[ ! -e dist ] && [ ! -s "$GH_STUB_LOG" ] && ok "--dry-run built nothing and called no gh" || bad "dry run did work: $(ls dist 2>&1) $(cat "$GH_STUB_LOG")"
echo q >> README.md
expect_refused "refuses a dirty tree" "has changes" pub v0.2.0
git checkout -q -- .
echo n > later.txt; git add later.txt; git "${ID[@]}" commit -q -m "later"
expect_refused "refuses when HEAD is not at the tag" "not at v0.2.0" pub v0.2.0
git reset -q --hard v0.2.0
: > "$GH_STUB_LOG"
GH_STUB_EXISTS=1 expect_refused "refuses when a Release for the tag already exists" "already exists" pub v0.2.0
OUT="$(MILK_GH=no-such-gh-here pub v0.2.0 2>&1)"; RC=$?; { [ $RC = 1 ] && has "$OUT" "is not installed"; } && ok "refuses without gh" || bad "without gh (exit $RC): $OUT"
OUT="$(pub v0.2.0 2>&1)"; RC=$?
[ $RC = 0 ] && ok "publish v0.2.0" || bad "publish v0.2.0 (exit $RC): $OUT"
LOG="$(cat "$GH_STUB_LOG")"
has "$LOG" "MILKOS_RELEASE=1" && ok "the build ran with MILKOS_RELEASE=1" || bad "no MILKOS_RELEASE=1: $LOG"
has "$LOG" "gh release create v0.2.0 --draft --verify-tag --title milkOS 0.2.0 --notes-file" && ok "gh release create TAG --draft --verify-tag with a title and notes" || bad "gh args: $LOG"
has "$LOG" "--prerelease" && bad "a final release was marked a pre-release" || ok "a final release is not a pre-release"
has "$LOG" "asset dist/milkOS-0.2.0.zip" && has "$LOG" "asset dist/SHA256SUMS" && ok "the zip and SHA256SUMS are attached" || bad "assets: $LOG"
[ "$(cat "$GH_STUB_NOTES")" = "$(scripts/release-notes.sh v0.2.0)" ] && ok "the notes are release-notes.sh's output" || bad "notes differ"
(cd dist && shasum -a 256 -c SHA256SUMS >/dev/null 2>&1) && ok "SHA256SUMS checks out" || bad "SHA256SUMS"
has "$OUT" "draft Release v0.2.0 created" && ok "says it is a draft" || bad "message: $OUT"
rm -rf dist

echo "== a pre-release, then its release"
mkscratch b; cd "$T/b/work" || exit 1
expect_ok "cut 0.2.0-rc.1" scripts/cut-release.sh 0.2.0-rc.1
has "$(git log -1 --format=%s)" "Release v0.2.0-rc.1" && ok "commit and tag for the rc" || bad "rc commit"
has "$(scripts/release-notes.sh v0.2.0-rc.1)" "The Browser is now" && ok "notes for the rc" || bad "rc notes"
git push -q origin main; git push -q origin v0.2.0-rc.1
: > "$GH_STUB_LOG"
expect_ok "publish the rc" pub v0.2.0-rc.1
has "$(cat "$GH_STUB_LOG")" "--draft --verify-tag" && has "$(cat "$GH_STUB_LOG")" "--prerelease" && ok "-rc is marked a pre-release" || bad "rc flags: $(cat "$GH_STUB_LOG")"
rm -rf dist
expect_refused "the same rc again is not newer" "not newer" scripts/cut-release.sh 0.2.0-rc.1
expect_refused "0.2.0 is refused while Unreleased is empty" "is empty" scripts/cut-release.sh 0.2.0
awk '/^## Unreleased$/ {print; print ""; print "- one more fix"; next} {print}' CHANGELOG.md > c.new && mv c.new CHANGELOG.md
git add -A; git "${ID[@]}" commit -q -m "a fix after the rc"; git push -q origin main
expect_ok "0.2.0 follows 0.2.0-rc.1" scripts/cut-release.sh 0.2.0
[ "$(scripts/release-notes.sh v0.2.0)" = "- one more fix" ] && ok "the final release's notes are only what came after the rc" || bad "final notes: $(scripts/release-notes.sh v0.2.0)"
expect_ok "version-check after both" scripts/version-check.sh

echo "== install-hooks.sh (in the scratch clone only)"
cd "$T/a/work" && reset_scratch
HOOKF="$(git rev-parse --git-path hooks/pre-push)"
case "$(cd "$T/a/work" && git rev-parse --absolute-git-dir)" in "$T"/*) ok "the scratch clone has its own .git" ;; *) bad "scratch clone shares a git dir"; exit 1 ;; esac
expect_ok "install" scripts/install-hooks.sh
[ -x "$HOOKF" ] && ok "the hook is there and executable" || bad "no hook"
echo ok > hook-ok.txt; git add hook-ok.txt; git "${ID[@]}" commit -q -m "a push the hook lets through"
t x git push origin main; [ $RC = 0 ] && has "$OUT" "scripts/ci-local.sh --quick" && ok "a good push goes through the hook" || bad "push through the hook (exit $RC): $OUT"
printf '9.9.9\n' > VERSION; git add VERSION; git "${ID[@]}" commit -q -m "break the version"
t x git push origin main; { [ $RC != 0 ] && has "$OUT" "VERSION is 9.9.9"; } && ok "a version mismatch blocks the push" || bad "hook did not block (exit $RC): $OUT"
t x env MILK_SKIP_HOOKS=1 git push origin main; [ $RC = 0 ] && ok "MILK_SKIP_HOOKS=1 skips it" || bad "skip: $OUT"
git reset -q --hard HEAD~1; git push -q -f origin main
t x scripts/install-hooks.sh; [ $RC = 0 ] && ok "installing twice is fine" || bad "reinstall"
rm -f "$HOOKF"; printf '#!/bin/sh\nexit 0\n' > "$HOOKF"; chmod +x "$HOOKF"
expect_refused "a hook something else made is not replaced" "already exists" scripts/install-hooks.sh
expect_refused "nor removed" "not made by this script" scripts/install-hooks.sh --uninstall
expect_ok "--force replaces it" scripts/install-hooks.sh --force
expect_ok "--uninstall" scripts/install-hooks.sh --uninstall
[ ! -e "$HOOKF" ] && ok "the hook is gone" || bad "hook still there"

echo "== ci-local.sh"
t x scripts/ci-local.sh --list; [ $RC = 0 ] && [ "$(echo "$OUT" | wc -l | tr -d ' ')" = 7 ] && has "$OUT" "hosttests" && ok "--list shows the 7 steps" || bad "--list: $OUT"
t x scripts/ci-local.sh --only nonsense; [ $RC = 2 ] && ok "an unknown step is a usage error" || bad "--only nonsense: $RC"
t x scripts/ci-local.sh --quick --ci; { [ $RC = 0 ] && has "$OUT" "ci-local: PASS" && has "$OUT" "version-check: ok" && has "$OUT" "public-check: ok"; } && ok "--quick --ci runs version-check and public-check" || bad "--quick: $OUT"
t x env MILK_PUBLIC_CHECK_NAMES=/nonexistent/names scripts/ci-local.sh --only public-check --ci
{ [ $RC = 0 ] && has "$OUT" "names rule is SKIPPED" && has "$OUT" "names rule SKIPPED"; } && ok "without the names list, public-check passes and says the names rule was skipped" || bad "no names list: $OUT"
printf 'contact: someone%sexample.org\n' @ > leak.txt
t x env MILK_PUBLIC_CHECK_NAMES=/nonexistent/names scripts/ci-local.sh --only public-check --ci
{ [ $RC = 1 ] && has "$OUT" "[email]"; } && ok "...and still fails on an email address" || bad "email not caught: $OUT"
rm -f leak.txt

cd "$REPO" || exit 1
echo "== the workflows (.github/workflows)"
WF=.github/workflows
for f in "$WF/ci.yml" "$WF/release.yml"; do
  [ -f "$f" ] || { bad "$f is missing"; continue; }
  c="$(cat "$f")"
  has "$c" "pull_request_target" && bad "$f uses pull_request_target" || ok "$f: no pull_request_target"
  if has "$c" "standard hosted runners"; then ok "$f: says it uses standard (free) runners"; else bad "$f: no note about standard runners"; fi
  # every `uses:` is a GitHub-owned action pinned by a full commit SHA with the version in a comment
  badu="$(awk '/^[ \t-]*uses:/ {
      u = $0; sub(/^[ \t-]*uses:[ \t]*/, "", u)
      n = split(u, p, /[@ \t]+/)
      okk = (p[1] == "actions/checkout" || p[1] == "actions/upload-artifact") && p[2] ~ /^[0-9a-f]+$/ && length(p[2]) == 40 && p[3] == "#" && p[4] ~ /^v[0-9]/
      if (!okk) print u }' "$f")"
  [ -z "$badu" ] && ok "$f: actions are GitHub-owned and pinned by full SHA with a version comment" || bad "$f: unpinned or third-party action: $badu"
  badr="$(awk '/^[ \t]*runs-on:/ { r = $2; if (r != "ubuntu-latest" && r != "macos-latest") print r }' "$f")"
  [ -z "$badr" ] && ok "$f: standard runner labels only" || bad "$f: runner label(s): $badr"
  jobs="$(awk '/^jobs:/ {j = 1; next} j && /^  [a-z][a-z0-9_-]*:[ \t]*$/ {n++} END {print n + 0}' "$f")"
  tmo="$(grep -c '^    timeout-minutes: [0-9]' "$f")"
  perm="$(grep -c '^    permissions:' "$f")"
  [ "$jobs" -gt 0 ] && [ "$jobs" = "$tmo" ] && ok "$f: timeout-minutes on all $jobs job(s)" || bad "$f: $jobs job(s), $tmo timeout(s)"
  [ "$jobs" = "$perm" ] && ok "$f: permissions on every job" || bad "$f: $jobs job(s), $perm permissions blocks"
  has "$c" "secrets." && bad "$f uses secrets" || ok "$f: no secrets"
done
c="$(cat "$WF/ci.yml" 2>/dev/null)"
has "$c" "contents: read" && ! has "$c" "contents: write" && ok "ci.yml: read-only" || bad "ci.yml permissions"
has "$c" "scripts/ci-local.sh" && ok "ci.yml calls scripts/ci-local.sh" || bad "ci.yml does not call ci-local.sh"
c="$(cat "$WF/release.yml" 2>/dev/null)"
n_write="$(grep -c 'contents: write' "$WF/release.yml")"
[ "$n_write" = 1 ] && ok "release.yml: contents: write in exactly one job" || bad "release.yml: contents: write x $n_write"
has "$c" "scripts/publish-release.sh" && ok "release.yml calls scripts/publish-release.sh" || bad "release.yml does not call publish-release.sh"
if command -v ruby >/dev/null 2>&1; then
  for f in "$WF/ci.yml" "$WF/release.yml"; do
    OUT="$(ruby -ryaml -e '
      y = YAML.safe_load(File.read(ARGV[0]), aliases: false)
      raise "no jobs" unless y["jobs"].is_a?(Hash) && !y["jobs"].empty?
      on = y["on"] || y[true]
      raise "no triggers" unless on
      y["jobs"].each do |name, j|
        raise "#{name}: no steps" unless j["steps"].is_a?(Array)
        j["steps"].each_with_index do |s, i|
          next unless s["run"]
          File.write(File.join(ARGV[1], "#{File.basename(ARGV[0])}.#{name}.#{i}.sh"), s["run"])
        end
      end
      puts "parsed"' "$f" "$T" 2>&1)"
    [ "$OUT" = parsed ] && ok "$f parses as YAML with jobs, triggers and steps" || bad "$f: $OUT"
  done
  nrun=0
  for s in "$T"/*.yml.*.sh; do
    [ -e "$s" ] || continue
    nrun=$((nrun + 1))
    # ${{ }} expressions are not shell; stand them in for a word
    sed -i.bak 's/\${{[^}]*}}/EXPR/g' "$s" && rm -f "$s.bak"
    if bash -n "$s" 2>/dev/null; then :; else bad "run block $(basename "$s") does not parse: $(bash -n "$s" 2>&1 | head -2)"; fi
  done
  ok "$nrun run blocks passed bash -n"
else
  echo "  (ruby is not installed: the YAML parse and the run-block syntax check are skipped)"
fi

cd /
echo
if [ $FAIL = 0 ]; then
  echo "release-scripts-test: $N checks passed"
  echo "release-scripts-test: PASS"
  [ "${MILK_KEEP:-}" = 1 ] || rm -rf "$T"
  exit 0
fi
echo "release-scripts-test: FAIL (work folder kept: $T)"
exit 1
