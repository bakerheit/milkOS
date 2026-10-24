# Releasing

How a milkOS release is made. Version numbers and what counts as a MINOR or a PATCH are in
docs/versioning.md. The short version: **the scripts do the work on your own Mac, GitHub Actions
is an optional second pair of eyes**, and a Release is always created as a draft that a person
publishes.

## The local path (the main one)

Everything here runs on the Mac, with no GitHub Actions. `gh` (the GitHub CLI, logged in) is
needed only for the last step.

| # | who | what | command |
|---|---|---|---|
| 1 | a person | decide the version: MINOR for a batch of features, PATCH for fixes, `-rc.N` to try a release first. Read the **Unreleased** section of `CHANGELOG.md`: it is the release notes, so it has to read well | |
| 2 | script | bump and tag, on a clean `main` that equals `origin/main` | `scripts/cut-release.sh 0.2.0` (`--dry-run` first to see it) |
| 3 | a person | the gate, on the release commit | `scripts/check-all.sh` and `scripts/public-check.sh` |
| 4 | a person | push the commit, then the tag (the order matters: the tag must point into `main`) | `git push origin main`, then `git push origin v0.2.0` |
| 5 | script | build the release bundle from the tag and create the **draft** Release with the zip, `SHA256SUMS` and the notes | `scripts/publish-release.sh v0.2.0` (`--dry-run` shows every step) |
| 6 | a person | open the draft on GitHub, read the notes, download the zip, check it against `SHA256SUMS`, try it with `releasetest`, press **Publish** | |

What `cut-release.sh` does: checks the version is SemVer and newer than `VERSION`, the tree is
clean, you are on `main` (or on a detached HEAD at `origin/main`, as in the `stage` worktree),
local `main` equals `origin/main`, the tag doesn't exist, and `## Unreleased` isn't empty. Then it
sets `VERSION`, every Cargo version and the `Cargo.lock` files (offline), renames
`## Unreleased` to `## 0.2.0 (DATE)` with a fresh empty `## Unreleased` above it, runs
`scripts/version-check.sh`, commits `Release v0.2.0` and makes the annotated tag. **It pushes
nothing**; it prints the commands for steps 3 to 5. To back out before step 4:
`git tag -d v0.2.0 && git reset --hard origin/main`.

What `publish-release.sh` does: checks the tag is `v$(cat VERSION)`, exists on `origin`, points
into `origin/main`, that this checkout is clean and at the tag, that the notes exist and no Release for
the tag exists yet; builds with `MILKOS_RELEASE=1 scripts/release.sh` (a release kernel that says
plain `0.2.0`, the disk images, `dist/milkOS-0.2.0.zip` and `dist/SHA256SUMS`); and runs
`gh release create v0.2.0 --draft --verify-tag` with the notes (`scripts/release-notes.sh`) and
both files. A tag with `-alpha`, `-beta` or `-rc` in it is marked a pre-release. It must be run
from a checkout of the tag (`git checkout v0.2.0`) on the Mac that builds the bundle.

Optional: `scripts/install-hooks.sh` installs a `pre-push` hook that runs the quick checks
(`scripts/ci-local.sh --quick`: version-check and the public scan) before any push; skip it for
one push with `MILK_SKIP_HOOKS=1`. The gate in step 3 stays the main guard.

## Fixing a bad release

* **A draft that is wrong** (bad notes, wrong file): delete the draft on GitHub. If the tag
  itself is fine, run `scripts/publish-release.sh` again.
* **A published release, or a tag pointing at the wrong thing:** never move, delete-and-recreate
  or reuse a tag. People may already have it. Fix the problem on `main`, and cut the next version
  (a PATCH: `0.2.1`), with a changelog line saying what was wrong. If the bad release is harmful,
  say so at the top of its notes on GitHub and mark it as not the latest.
* **`cut-release.sh` made the commit but you haven't pushed:** back out as above and cut again.
* **The push of the commit was done but the gate then fails:** fix on `main` in a normal commit,
  delete the *unpushed* local tag (`git tag -d v0.2.0`), and cut `0.2.1` instead of retagging.
  (A tag that was never pushed is the only one that can be dropped.)

## GitHub Actions (optional)

Two workflows in `.github/workflows/` are thin wrappers around the scripts above. This repository
is public, so GitHub's standard hosted runners are free for it, and the workflows use only the
standard labels (`ubuntu-latest`, `macos-latest`), with a timeout on every job. They use only
GitHub's own actions (`actions/checkout`, `actions/upload-artifact`) pinned by full commit SHA, no
secrets, never `pull_request_target`, and each job asks for the least permission it needs
(`contents: write` only in the job that creates the draft). **Turn them off** in the repository's
Settings > Actions, or delete the two files: nothing in the local path depends on them.

* `ci.yml`, on pushes to `main` and on pull requests: `scripts/ci-local.sh --ci` on macOS, and the
  version and public checks on Linux. The status badge in the README is this one.
* `release.yml`, on a pushed tag `vX.Y.Z...`: `scripts/publish-release.sh`, the same as step 5
  (so a tag push makes the draft without you doing step 5), and on "Run workflow" by hand: builds
  the bundle and keeps it as a workflow artifact for 14 days, creating no Release. If you use the
  local step 5, don't also let the workflow do it: the second one finds the Release already there
  and stops.

**What Actions can and can't check.** A hosted runner has no hardware acceleration for a VM, so
nothing that boots milkOS runs there: not `check-all.sh`, not `releasetest.sh`. What it checks is
what doesn't need a VM: that the kernel and all of `user/` build for `aarch64-unknown-none-softfloat`
with the pinned toolchain from a clean checkout, clippy for the kernel and the shared crates,
the host unit tests of the smoke set, `version-check`, the release scripts' own tests, and the
parts of `public-check` that don't need the names list (emails, home paths, key material,
ticket ids; the names rule is skipped there and says so, because the list is kept outside the
repository). Formatting is reported but doesn't fail (the tree isn't `rustfmt`-clean today;
`MILK_CI_FMT=strict scripts/ci-local.sh` makes it fail). So a green check there means "builds and
unit-tests from scratch on someone else's machine", not "works": the local gate is still the one
that says that.
