#!/usr/bin/env bash
# commit-msg hook body: the subject is a conventional commit (cog, with
# `release` declared in cog.toml), and no commit made by hand is a release:
# the release commit, and the version it moves, are the release workflow's
# alone (RELEASING.md).
#
#   scripts/commit-msg.sh <path to the message file>
set -euo pipefail

message_file="${1:?usage: commit-msg.sh <message file>}"
subject=$(head -n 1 "$message_file")

# fixup!/squash!/amend! subjects are rebased away before they land, and git
# writes merge subjects itself.
cog verify --ignore-merge-commits --ignore-fixup-commits "$subject"

if [[ "$subject" =~ ^release(\(|!|:) ]]; then
    echo "commit-msg: release commits are made by the release workflow (RELEASING.md), never by hand" >&2
    exit 1
fi

# Cargo.toml's [package] version is the ONE version source, and only the
# release workflow's commit moves it. Read the INDEX: that is what this commit
# records. Each file is read whole before awk sees it (#68: no reader leaves
# its writer mid-write).
scratch=$(mktemp)
trap 'rm -f "$scratch"' EXIT
version_of() { # <git object> → its [package] version, or nothing
    git show "$1" >"$scratch" 2>/dev/null || return 0
    awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
         on && /^version *= *"/ && !found { gsub(/"/, "", $3); print $3; found = 1 }' "$scratch"
}
staged=$(version_of ":Cargo.toml")
committed=$(version_of "HEAD:Cargo.toml")

if [ -n "$committed" ] && [ "$staged" != "$committed" ]; then
    echo "commit-msg: this commit moves Cargo.toml's version $committed → $staged — the version moves only in the release workflow's commit" >&2
    exit 1
fi
