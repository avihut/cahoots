#!/usr/bin/env bash
# post-merge reminder (daft.yml), and `mise run release-reminder` by hand:
# a shipped feature or fix is not done until it is released. Exits non-zero
# while main holds unreleased feat/fix commits, so daft shows it as a warning
# row; post-merge never rolls back.
set -euo pipefail

# git exports these to hooks; the cwd (the target worktree) is authoritative.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

branch=$(git symbolic-ref --short -q HEAD || true)
if [ "$branch" != "main" ]; then
    echo "release-reminder: '$branch' is not main — releases are cut from main"
    exit 0
fi

last=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
range=${last:+$last..}HEAD
unreleased=$(git log --format='%h %s' "$range" | grep -E '^[0-9a-f]+ (feat|fix)(\([^)]*\))?!?: ' || true)

if [ -z "$unreleased" ]; then
    echo "release-reminder: nothing unreleased on main since ${last:-the first commit}"
    exit 0
fi

{
    echo "main holds unreleased work since ${last:-the first commit}:"
    printf '%s\n' "$unreleased" | sed 's/^/  /'
    echo
    echo "To release it (RELEASING.md):"
    echo "  1. mise run release             # the bump, the 'release: vX.Y.Z' commit, the signed tag"
    echo "  2. git show vX.Y.Z              # read what is about to become permanent"
    echo "  3. git push origin main vX.Y.Z  # the tag starts the release workflow"
} >&2
exit 1
