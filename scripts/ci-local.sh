#!/usr/bin/env bash
# The portable checks, the same ones the GitHub workflow (.github/workflows/ci.yml) runs. The
# workflow only checks the repository out and calls this script, so everything it does can be
# done on this Mac without GitHub. It is a second opinion: the real gate is scripts/check-all.sh
# (it boots VMs, which hosted runners can't do without hardware acceleration).
#
#   scripts/ci-local.sh                 all steps
#   scripts/ci-local.sh --quick         only version-check and public-check (what the pre-push hook runs)
#   scripts/ci-local.sh --only a,b      only these steps
#   scripts/ci-local.sh --list          the steps
#   scripts/ci-local.sh --ci            for GitHub Actions: public-check doesn't look at commit history
#                                       (the merge commits GitHub makes are not ours)
#
# Steps (every one runs even if an earlier one fails; the last line says PASS or FAIL):
#   fmt             cargo fmt --check. ADVISORY today: the tree has ~1000 unformatted spots, so it
#                   prints how many files differ and passes. MILK_CI_FMT=strict makes it fail.
#   clippy          cargo clippy for the kernel and the host-testable crates (root workspace,
#                   aarch64-unknown-none-softfloat, -D warnings), and for the host unit tests.
#                   The user/ workspace is checked too, but its warnings are only listed (there are
#                   some today); a compile error fails.
#   hosttests       the host unit tests of the smoke set (the "*-unit" tests of check-all.sh), no VM.
#   build           cargo build of the whole OS (kernel + user/), for aarch64-unknown-none-softfloat.
#   version-check   scripts/version-check.sh
#   release-scripts scripts/release-scripts-test.sh (cut/notes/publish/hook scripts, in a scratch clone)
#   public-check    scripts/public-check.sh: emails, home paths, key material, ticket ids. The names
#                   list lives outside the repository (.git/info/public-check-names); without it
#                   that one rule is SKIPPED, and the output says so.
# Needs rustup with the pinned nightly (rust-toolchain.toml), python3, git, bash; macOS or Linux
# for the checks, macOS for the host tests' target (aarch64-apple-darwin on the Mac; the runner's
# own host triple elsewhere).
set -uo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
cd "$HERE/.." || exit 2
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
STEPS="fmt clippy hosttests build version-check release-scripts public-check"
ONLY=""; CI=0
while [ $# -gt 0 ]; do
  case "$1" in
    --quick) ONLY="version-check public-check" ;;
    --only) ONLY="$(echo "${2:?--only needs step names}" | tr ',' ' ')"; shift ;;
    --list) for s in $STEPS; do echo "$s"; done; exit 0 ;;
    --ci) CI=1 ;;
    -h|--help) sed -n '2,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "ci-local: unknown option $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done
for s in $ONLY; do case " $STEPS " in *" $s "*) ;; *) echo "ci-local: no step '$s' (try --list)" >&2; exit 2 ;; esac; done

HOST="${MILK_HOST_TARGET:-$(rustc -vV 2>/dev/null | sed -n 's/^host: //p')}"
CT="$HERE/../target/ci-local"   # cargo's work dir for the checks that must not touch the kernel build's
mkdir -p "$CT"

step_fmt() {
  local n=0 out
  out="$(cargo fmt --all --check 2>&1)"; local rc=$?
  n="$(printf '%s\n' "$out" | grep -c '^Diff in' || true)"
  if [ $rc = 0 ]; then echo "fmt: clean"; return 0; fi
  if [ "${MILK_CI_FMT:-}" = strict ]; then printf '%s\n' "$out" | head -40; echo "fmt: $n place(s) differ"; return 1; fi
  echo "fmt: ADVISORY, $n place(s) in $(printf '%s\n' "$out" | grep '^Diff in' | sed 's/:[0-9]*:$//' | sort -u | wc -l | tr -d ' ') file(s) differ from rustfmt (MILK_CI_FMT=strict fails on this)"
  return 0
}

step_clippy() {
  local rc=0
  echo "-- clippy: kernel and shared crates (root workspace), -D warnings"
  cargo clippy --workspace --target-dir "$CT/root" -- -D warnings || rc=1
  echo "-- clippy: host unit tests of the shared crates ($HOST)"
  local out
  out="$(cargo clippy -p milkfs -p milkcrypto -p tls -p milkweb --tests --target "$HOST" --target-dir "$CT/host" 2>&1)"; local r=$?
  if [ $r != 0 ]; then printf '%s\n' "$out" | tail -30; rc=1; else
    echo "   $(printf '%s\n' "$out" | grep -c '^warning' || true) warning line(s), listed only"; fi
  echo "-- clippy: user space (user/ workspace); warnings are listed, errors fail"
  out="$(cd user && cargo clippy --workspace --target-dir "$CT/user" 2>&1)"; r=$?
  if [ $r != 0 ]; then printf '%s\n' "$out" | tail -40; rc=1; else
    echo "   $(printf '%s\n' "$out" | grep -c '^warning' || true) warning line(s), not failing"; fi
  return $rc
}

step_hosttests() {
  local rc=0 total=0 n s line
  # The smoke set's host unit tests: the "*-unit" lines of check-all.sh --list (so this list
  # can't drift from the gate's).
  while read -r s; do
    [ -z "$s" ] && continue
    line="$("$s" 2>&1 | tail -1)"
    case "$line" in
      *": "*" passed") n="${line##*: }"; n="${n% passed}"; total=$((total + n)); echo "$line" ;;
      *) echo "FAIL: $s -> $line"; rc=1 ;;
    esac
  done < <("$HERE/check-all.sh" --list --smoke | awk '$1 ~ /-unit$/ {print $5}')
  echo "hosttests: $total tests passed in total"
  return $rc
}

step_build() { cargo build; }
step_version_check() { "$HERE/version-check.sh"; }
step_release_scripts() { "$HERE/release-scripts-test.sh"; }
step_public_check() {
  if [ "$CI" = 1 ]; then "$HERE/public-check.sh" --no-history; else "$HERE/public-check.sh"; fi
}

fails=""; passes=""
for s in ${ONLY:-$STEPS}; do
  echo "== ci-local: $s"
  fn="step_$(echo "$s" | tr - _)"
  t0=$SECONDS
  if $fn; then passes="$passes $s"; echo "== ci-local: $s ok ($((SECONDS - t0)) s)"
  else fails="$fails $s"; echo "== ci-local: $s FAILED ($((SECONDS - t0)) s)"; fi
done
echo
if [ -z "$fails" ]; then echo "ci-local: PASS ($(echo $passes | wc -w | tr -d ' ') steps:$passes)"; exit 0; fi
echo "ci-local: FAIL (failed:$fails)"
exit 1
