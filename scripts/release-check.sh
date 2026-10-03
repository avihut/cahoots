#!/usr/bin/env bash
# pre-push hook body: nothing release-shaped is pushed by hand. A `v*` tag, the
# `release-pr` branch, a `release:` commit and a commit that moves
# Cargo.toml's version are all the release workflow's (RELEASING.md); the
# rulesets refuse them on GitHub, and this says so before the push does.
#
# Reads git's pre-push lines on stdin:
#   <local ref> <local sha> <remote ref> <remote sha>
# Local only — never asks the remote anything.
set -euo pipefail

zero=0000000000000000000000000000000000000000
manifest=Cargo.toml
failures=0

fail() {
    failures=$((failures + 1))
    printf '✗ %s\n' "$1" >&2
}

scratch_dir=$(mktemp -d)
trap 'rm -rf "$scratch_dir"' EXIT

# Every file is read whole before awk sees it (#68: no reader leaves its
# writer mid-write).
version_at() {
    git show "$1:$manifest" >"$scratch_dir/manifest" 2>/dev/null || return 0
    awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
         on && /^version *= *"/ && !found { gsub(/"/, "", $3); print $3; found = 1 }' \
        "$scratch_dir/manifest"
}

while read -r _local_ref local_sha remote_ref remote_sha; do
    [ "$local_sha" = "$zero" ] && continue # a delete pushes nothing to check

    case "$remote_ref" in
    refs/tags/v*)
        fail "${remote_ref#refs/tags/}: release tags are made by the release workflow"
        continue
        ;;
    refs/heads/release-pr)
        fail "release-pr is the release workflow's branch"
        continue
        ;;
    esac
    # A branch push is chosen by where it lands: the local side may be spelt
    # HEAD, a sha or any expression, and its commits are checked all the same.
    case "$remote_ref" in
    refs/heads/*) ;;
    *) continue ;;
    esac

    # The commits this push brings: not on the remote branch, and not on any
    # remote-tracking ref — so a branch rebased onto a main that holds a
    # release commit is not refused for it.
    if [ "$remote_sha" = "$zero" ]; then
        git rev-list "$local_sha" --not --remotes >"$scratch_dir/commits"
    else
        git rev-list "$local_sha" "^$remote_sha" --not --remotes >"$scratch_dir/commits"
    fi
    while IFS= read -r commit; do
        [ -n "$commit" ] || continue
        short=$(git rev-parse --short "$commit")
        subject=$(git log -1 --format=%s "$commit")
        if [[ "$subject" =~ ^release(\(|!|:) ]]; then
            fail "$short '$subject' is a release commit — only the release workflow makes one"
            continue
        fi
        git rev-parse -q --verify "$commit^1" >/dev/null || continue # a root commit moves nothing
        mine=$(version_at "$commit")
        parent=$(version_at "$commit^1")
        if [ "$mine" != "$parent" ]; then
            fail "$short moves Cargo.toml's version ${parent:-none} → ${mine:-none}"
        fi
    done <"$scratch_dir/commits"
done

if [ "$failures" -gt 0 ]; then
    echo "  the release workflow makes releases (RELEASING.md)" >&2
    exit 1
fi
echo "release-check: no release made by hand in this push"
