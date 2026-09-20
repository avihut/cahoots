#!/usr/bin/env bash
# post-merge tripwire (daft.yml), after the daft repo's merge:landed-check:
# the tree that landed is the tree the pre-merge rings tested. A local
# `daft merge` moves main before CI ever sees it, so this is the one check
# that the rings' verdict covers what is on main now.
set -euo pipefail

# git exports these to hooks; the cwd (the target worktree) is authoritative.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

sources="${DAFT_MERGE_SOURCE_SHAS:-}"
if [ -z "$sources" ]; then
    echo "landed-check: DAFT_MERGE_SOURCE_SHAS is empty (this only makes sense from daft's post-merge hook)" >&2
    exit 1
fi
if [ "${DAFT_MERGE_RESULT:-success}" != "success" ]; then
    echo "landed-check: merge result is '${DAFT_MERGE_RESULT}' — nothing landed to check"
    exit 0
fi

# A gated merge has a single source, so the first entry is the whole story.
source_sha=$(printf '%s\n' "$sources" | head -n 1)
landed=$(git rev-parse --verify "HEAD^{tree}")
gated=$(git rev-parse --verify "${source_sha}^{tree}")

if [ "$landed" != "$gated" ]; then
    echo "the landed tree is NOT the tree the rings gated:" >&2
    echo "  gated   $source_sha -> $gated" >&2
    echo "  landed  $(git rev-parse --short HEAD) -> $landed" >&2
    echo "A squash of a branch that already contains the target's tip lands that" >&2
    echo "branch's tree verbatim, so the target moved underneath the merge or the" >&2
    echo "source-up-to-date ring was skipped. Run: mise run gate" >&2
    exit 1
fi
echo "landed-check: the landed tree is the gated tree ($(git rev-parse --short HEAD))"
