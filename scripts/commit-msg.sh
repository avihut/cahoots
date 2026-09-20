#!/usr/bin/env bash
# commit-msg hook body: the subject is a conventional commit (cog, with
# `release` declared in cog.toml), and a release commit and the version it
# names agree.
#
#   scripts/commit-msg.sh <path to the message file>
set -euo pipefail

message_file="${1:?usage: commit-msg.sh <message file>}"
subject=$(head -n 1 "$message_file")

# fixup!/squash!/amend! subjects are rebased away before they land, and git
# writes merge subjects itself.
cog verify --ignore-merge-commits --ignore-fixup-commits "$subject"

# Cargo.toml's [package] version is the ONE version source, and the bump
# lives in the release commit. Read the INDEX: that is what this commit
# records.
version_of() {
    awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
         on && /^version *= *"/ { gsub(/"/, "", $3); print $3; exit }'
}
staged=$(git show ":Cargo.toml" | version_of)
committed=$(git show "HEAD:Cargo.toml" 2>/dev/null | version_of || true)

if [[ "$subject" =~ ^release: ]]; then
    if [[ ! "$subject" =~ ^release:\ v[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
        echo "commit-msg: a release subject is exactly 'release: vX.Y.Z' — got '$subject'" >&2
        exit 1
    fi
    if [ "$subject" != "release: v$staged" ]; then
        echo "commit-msg: '$subject' but Cargo.toml's version is $staged" >&2
        exit 1
    fi
elif [ -n "$committed" ] && [ "$staged" != "$committed" ]; then
    echo "commit-msg: this commit moves Cargo.toml's version $committed → $staged," >&2
    echo "  so its subject must be 'release: v$staged' (docs/WORKFLOW.md: releases)" >&2
    exit 1
fi
