#!/usr/bin/env bash
# post-merge reminder (daft.yml), and `mise run release-reminder` by hand:
# lists the feat/fix commits on main since the last release. Always exits 0:
# the release PR already holds them, and the release button on the desk is
# the reminder — a warning row after every merge would be noise.
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
# Read whole before grep sees it (#68: no reader leaves its writer mid-write).
log=$(mktemp)
trap 'rm -f "$log"' EXIT
git log --format='%h %s' "$range" >"$log"
unreleased=$(grep -E '^[0-9a-f]+ (feat|fix)(\([^)]*\))?!?: ' "$log" || true)

if [ -z "$unreleased" ]; then
    echo "release-reminder: nothing unreleased on main since ${last:-the first commit}"
    exit 0
fi

echo "main holds unreleased work since ${last:-the first commit}:"
printf '%s\n' "$unreleased" | sed 's/^/  /'
echo "The release PR holds them — merging it is the release (RELEASING.md)."
