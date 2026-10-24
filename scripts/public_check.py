#!/usr/bin/env python3
"""public-check: this repository is public, so nothing personal may land in it. Scans the
working tree (tracked files plus new, not-ignored ones) and the commits that are not on
origin/main yet (messages, author and committer emails and names), and prints every hit as
FILE:LINE: [rule] text (or COMMIT: ... for history). Exit 1 if there is any.

Rules
  names    names from a local list, as whole words (see below)
  email    any e-mail address except noreply ones (users.noreply.github.com, noreply@anthropic.com)
  path     /Users/<anyone but the guest's own milk or root>, other people's /home/..., Windows
           user folders, tool scratch paths
  secret   private key blocks and API key / token patterns
  ticket   ticket ids and Notion links

The names list is not in the repository, in any form (no names, no hashes): it is read from
$MILK_PUBLIC_CHECK_NAMES, else from <git dir>/info/public-check-names. One entry per line, `#`
starts a comment. An entry is matched case-insensitively as a whole word; an entry starting
with `^` is matched only in its capitalised spelling and not where it could just be the first
word of a sentence (for names that are also ordinary words). Without the file (a fresh clone,
GitHub Actions) the names rule is skipped, loudly: a NOTE at the top and "names rule SKIPPED"
on the last line, never a silent pass; every other rule still runs and still fails.

  --no-history   check the files only, not the commits that origin/main doesn't have (CI:
                 scripts/ci-local.sh --ci; merge commits made by a hosting service are not ours)

Third-party text (licences, vendored data) is exempted by path in EXEMPT below; the explicit
TEST-ONLY key in scripts/milk-test-key.secret is allowed by the secret rule.
"""
import os
import re
import subprocess
import sys

ROOT = subprocess.run(["git", "rev-parse", "--show-toplevel"], capture_output=True, text=True, check=True).stdout.strip()

EXEMPT = {  # rule -> path prefixes that are skipped for it
    "email": ["LICENSE", "assets/fonts/"],
    "names": ["assets/fonts/", "LICENSE"],
    "secret": ["scripts/milk-test-key.secret"],
}
SKIP_FILES = {"scripts/public_check.py"}  # holds the name list
SKIP_DIRS = ("target/", ".git/", "dist/", "share/")

NOREPLY = re.compile(r"(@users\.noreply\.github\.com|^noreply@anthropic\.com)$", re.I)
EMAIL = re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+")
PATHS = re.compile(
    r"/Users/(?!milk\b|root\b|Shared\b)[A-Za-z0-9._-]+|/home/(?!milk\b|user\b|you\b|name\b|nobody\b|alice\b|bob\b)[a-z][a-z0-9_-]*"
    r"|[A-Za-z]:\\Users\\|/private/tmp/claude|/private/var/folders|~[a-z]+/\.claude"
)
SECRET = re.compile(
    r"-----BEGIN [A-Z ]*" + "PRIVATE" + r" KEY-----|ghp_[A-Za-z0-9]{20,}|github_pat_[A-Za-z0-9_]{20,}"
    r"|\bsk-[A-Za-z0-9_-]{20,}|AKIA[0-9A-Z]{16}|xox[baprs]-[A-Za-z0-9-]{10,}|AIza[0-9A-Za-z_-]{35}"
)
TICKET = re.compile(r"\b(?:INV|BH)-\d+\b|https?://(?:www\.)?notion\.(?:so|site|com)\b|notion://")

def names_file():
    p = os.environ.get("MILK_PUBLIC_CHECK_NAMES")
    if p:
        return p
    r = subprocess.run(["git", "rev-parse", "--git-path", "info/public-check-names"], cwd=ROOT,
                       capture_output=True, text=True)
    p = r.stdout.strip()
    return p if os.path.isabs(p) else os.path.join(ROOT, p)


def load_names():
    """[(compiled word regex, capitalised_only)] from the local list; [] if there is none."""
    path = names_file()
    try:
        lines = open(path, encoding="utf-8").read().splitlines()
    except OSError:
        return None
    out = []
    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        cap = line.startswith("^")
        word = re.escape(line.lstrip("^").strip())
        rx = re.compile(r"(?<![A-Za-z0-9_])" + word + r"(?![A-Za-z0-9_])", 0 if cap else re.I)
        out.append((rx, cap))
    return out


NAMES = load_names()
# A capitalised-only entry at the start of a sentence is just a word, unless it is clearly used
# as a name: followed by a colon, comma, bracket or 's, or joined with another capitalised word.
SENTENCE_START = re.compile(r"(?:^|[.!?:]\s+|^[\s#>*\-\"'(/]+)$")
NAME_USE = re.compile(r"^(?:'s|’s|\s*[:,(]|\s+(?:and|or|&)\s+[A-Z])")


def name_hits(line):
    hits = []
    for rx, cap in NAMES or []:
        for m in rx.finditer(line):
            if cap and SENTENCE_START.search(line[: m.start()]) and not NAME_USE.search(line[m.end():]):
                continue
            hits.append(m.group(0))
            break
    return hits


def exempt(rule, path):
    return any(path == p or path.startswith(p) for p in EXEMPT.get(rule, []))


def scan_text(label, text, path, hits, in_history=False):
    """Run every rule over `text` (a file, or one commit message)."""
    for i, line in enumerate(text.splitlines(), 1):
        where = f"{label}:{i}" if not in_history else label
        def hit(rule, what):
            hits.append(f"{where}: [{rule}] {what.strip()[:140]}")
        if not exempt("names", path):
            for n in name_hits(line):
                hit("names", f"{n}: {line}")
        if not exempt("email", path):
            for m in EMAIL.finditer(line):
                if not NOREPLY.search(m.group(0)):
                    hit("email", m.group(0))
        if not exempt("path", path) and PATHS.search(line):
            hit("path", PATHS.search(line).group(0))
        if not exempt("secret", path):
            m = SECRET.search(line)
            if m:
                hit("secret", m.group(0)[:30] + "...")
        if TICKET.search(line):
            hit("ticket", TICKET.search(line).group(0))


def tree_files():
    out = subprocess.run(["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
                         cwd=ROOT, capture_output=True, check=True).stdout.decode()
    return [f for f in out.split("\0") if f]


def main():
    hits = []
    if NAMES is None:
        print("public-check: NOTE: the names rule is SKIPPED: no names list at "
              f"{names_file()} (it is kept outside the repository; see the header of scripts/public_check.py)")
        if os.environ.get("GITHUB_ACTIONS"):
            print("::notice title=public-check::the names rule was skipped (the names list is not in the repository); "
                  "emails, paths, keys and ticket ids were checked")
    for path in tree_files():
        if path in SKIP_FILES or path.startswith(SKIP_DIRS):
            continue
        try:
            data = open(f"{ROOT}/{path}", "rb").read()
        except (IsADirectoryError, FileNotFoundError, PermissionError):
            continue
        if b"\0" in data[:8192]:
            continue  # binary
        scan_text(path, data.decode("utf-8", "replace"), path, hits)

    # History: commits that origin/main doesn't have.
    if "--no-history" in sys.argv[1:]:
        return finish(hits, 0, "history not checked")
    base = subprocess.run(["git", "rev-parse", "--verify", "-q", "origin/main"], cwd=ROOT,
                          capture_output=True, text=True)
    rng = "origin/main..HEAD" if base.returncode == 0 else "HEAD"
    if base.returncode != 0:
        print("public-check: no origin/main here, checking every commit", file=sys.stderr)
    log = subprocess.run(["git", "log", rng, "--format=%H%x1f%an%x1f%ae%x1f%cn%x1f%ce%x1f%B%x1e"],
                         cwd=ROOT, capture_output=True, text=True, check=True).stdout
    ncommits = 0
    for rec in log.split("\x1e"):
        rec = rec.strip("\n")
        if not rec:
            continue
        ncommits += 1
        sha, an, ae, cn, ce, msg = rec.split("\x1f", 5)
        label = f"commit {sha[:9]}"
        for who, em in (("author", ae), ("committer", ce)):
            if not NOREPLY.search(em):
                hits.append(f"{label}: [email] {who} email {em}")
        for who, nm in (("author", an), ("committer", cn)):
            for n in name_hits(nm):
                hits.append(f"{label}: [names] {who} name {nm}")
        scan_text(label, msg, "", hits, in_history=True)

    return finish(hits, ncommits, f"{ncommits} unpushed commits")


def finish(hits, ncommits, what):
    for h in hits:
        print(h)
    nfiles = len(tree_files())
    skipped = ", names rule SKIPPED (no names list)" if NAMES is None else ""
    if hits:
        print(f"public-check: FAIL, {len(hits)} hit(s) ({nfiles} files, {what}{skipped})")
        return 1
    print(f"public-check: ok ({nfiles} files, {what}{skipped})")
    return 0


sys.exit(main())
