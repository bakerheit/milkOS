#!/usr/bin/env bash
# Install (or remove) an optional git pre-push hook that runs the quick part of the portable
# checks before anything leaves this machine: scripts/ci-local.sh --quick, which is
# version-check and the public scan (emails, home paths, keys, ticket ids, names). It takes a
# few seconds. The real gate is still scripts/check-all.sh; this only catches the cheap mistakes.
#
#   scripts/install-hooks.sh              install .git/hooks/pre-push
#   scripts/install-hooks.sh --uninstall  remove it (only if this script made it)
#   scripts/install-hooks.sh --force      replace a pre-push hook that something else made
#
# Skip it for one push with MILK_SKIP_HOOKS=1 git push ...   (a hook can also be skipped with
# git push --no-verify, but the repository's rules say never to; use the variable.)
# The hooks live in the repository's shared hooks folder, so every worktree gets it.
set -uo pipefail
cd "$(dirname "$0")/.." || exit 2
MARK="# milkOS pre-push hook (scripts/install-hooks.sh)"
HOOK="$(git rev-parse --git-path hooks/pre-push)" || { echo "install-hooks: not a git checkout" >&2; exit 1; }
case "${1:-}" in
  -h|--help) sed -n '2,/^set -uo/p' "$0" | sed '$d' | sed 's/^# \{0,1\}//'; exit 0 ;;
  --uninstall)
    if [ -e "$HOOK" ] && grep -qF "$MARK" "$HOOK"; then rm -f "$HOOK"; echo "install-hooks: removed $HOOK"
    elif [ -e "$HOOK" ]; then echo "install-hooks: $HOOK was not made by this script; left alone" >&2; exit 1
    else echo "install-hooks: no pre-push hook"; fi
    exit 0 ;;
  ""|--force) ;;
  *) echo "install-hooks: unknown option $1 (try --help)" >&2; exit 2 ;;
esac
if [ -e "$HOOK" ] && ! grep -qF "$MARK" "$HOOK" && [ "${1:-}" != --force ]; then
  echo "install-hooks: $HOOK already exists and was not made by this script; --force replaces it" >&2
  exit 1
fi
mkdir -p "$(dirname "$HOOK")"
cat > "$HOOK" <<'HOOKEOF'
#!/usr/bin/env bash
# milkOS pre-push hook (scripts/install-hooks.sh)
[ "${MILK_SKIP_HOOKS:-}" = 1 ] && { echo "pre-push: skipped (MILK_SKIP_HOOKS=1)"; exit 0; }
top="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0
[ -x "$top/scripts/ci-local.sh" ] || exit 0
echo "pre-push: scripts/ci-local.sh --quick (MILK_SKIP_HOOKS=1 skips it)"
"$top/scripts/ci-local.sh" --quick || { echo "pre-push: the quick checks failed; nothing was pushed" >&2; exit 1; }
HOOKEOF
chmod 755 "$HOOK"
echo "install-hooks: installed $HOOK (runs scripts/ci-local.sh --quick before each push)"
