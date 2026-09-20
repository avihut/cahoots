#!/usr/bin/env bash
# Tests for the hook scripts themselves: every pass path AND every refusal,
# in a throwaway repository. A gate nobody has seen fail is a gate nobody
# knows works. Touches nothing of this repository's index or refs, no
# network, no credentials, no signing.
set -euo pipefail

scripts=$(cd "$(dirname "$0")" && pwd)
root=$(dirname "$scripts")
mkdir -p "$root/.cache"
tmp=$(mktemp -d "$root/.cache/test-hooks.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
out="$tmp/output.log"
checks=0

# git may have exported these when this runs inside a hook.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

passes() {
    if ! "$@" >"$out" 2>&1; then
        cat "$out" >&2
        echo "test-hooks: expected SUCCESS: $*" >&2
        exit 1
    fi
    checks=$((checks + 1))
}
fails() {
    if "$@" >"$out" 2>&1; then
        cat "$out" >&2
        echo "test-hooks: expected a REFUSAL: $*" >&2
        exit 1
    fi
    checks=$((checks + 1))
}

# ── no-warnings.sh ──────────────────────────────────────────────────────────
passes "$scripts/no-warnings.sh" /bin/sh -c 'echo clean'
fails "$scripts/no-warnings.sh" /bin/sh -c 'exit 7'
fails "$scripts/no-warnings.sh" /bin/sh -c 'echo "warning: package diagnostic" >&2'
fails "$scripts/no-warnings.sh" /bin/sh -c 'echo "ld: warning: linker diagnostic"'

# ── a fixture shaped like this repository ───────────────────────────────────
repo="$tmp/repo"
mkdir -p "$repo" "$tmp/no-hooks"
cd "$repo"
git init -q -b main .
git config user.name "Hook tests"
git config user.email "hooks@example.invalid"
git config commit.gpgsign false
git config tag.gpgsign false
git config core.hooksPath "$tmp/no-hooks"

set_version() {
    printf '[package]\nname = "fixture"\nversion = "%s"\n\n[dependencies]\nserde = "1"\n' "$1" >Cargo.toml
}
mkdir -p scripts src/install .github/workflows .github/rulesets
cp "$root/cog.toml" .
# The REAL workflows and ruleset: rule 8 is about these files agreeing, so the
# fixture that proves the rule passes is the set that ships.
cp "$root"/.github/workflows/*.yml .github/workflows/
cp "$root/.github/rulesets/main-pr-gate.json" .github/rulesets/
set_version 0.1.0
printf '[tasks.tool]\nrun = "scripts/tool.sh"\n' >mise.toml
printf '#!/bin/sh\n' >scripts/tool.sh
printf 'fn main() {}\n' >src/main.rs
printf 'use std::process::Command;\npub fn spawn() { let _ = Command::new("claude"); }\n' >src/spawn.rs
printf 'pub const SKILLS: &str = ".agents/skills";\n' >src/install/paths.rs
printf 'pub fn home_override() -> Option<String> { std::env::var("CAHOOTS_STATE_DIR").ok() }\n' >src/env.rs
git add -A
git commit -qm 'chore: fixture'
base=$(git rev-parse HEAD)

# ── guard.sh ────────────────────────────────────────────────────────────────
passes "$scripts/guard.sh"
passes "$scripts/guard.sh" --staged

# Each rule trips on its own, and the tree is restored after.
trips() { # trips <file> <content to append>
    printf '%s\n' "$2" >>"$1"
    git add -A
    fails "$scripts/guard.sh"
    fails "$scripts/guard.sh" --staged
    git reset -q --hard "$base"
    git clean -qfd
}
# Assembled at run time so this file never holds a token-shaped string.
trips tests-fixture.json "\"token\": \"sk-ant-$(printf 'oat01')-AbCdEfGhIjKlMnOp\""
trips tests-fixture.json "\"token\": \"$(printf 'gh')o_$(printf 'A%.0s' {1..36})\""
trips src/main.rs 'use std::net::TcpStream;'
trips src/main.rs 'use std::os::unix::net::UnixStream;'
trips src/main.rs 'const CREDS: &str = "oauth_creds.json";'
trips src/main.rs 'fn rogue() { let _ = std::process::Command::new("codex"); }'
trips src/spawn.rs 'pub fn shell() { let _ = Command::new("sh"); }'
trips src/spawn.rs 'pub fn shell() { let _ = Command::new("/bin/bash"); }'
trips src/main.rs 'const HOME: &str = ".codex/agents";'
trips src/main.rs 'fn home() -> String { std::env::var("HOME").unwrap() }'
trips Cargo.toml 'reqwest = "0.12"'
trips Cargo.toml '[dependencies.tokio]'
trips .github/workflows/ci.yml '      - uses: actions/checkout@v7'
trips .github/rulesets/main-pr-gate.json '{ "context": "build", "integration_id": 15368 }'
printf '#!/bin/sh\n' >scripts/orphan.sh
git add -A
fails "$scripts/guard.sh" --staged
git reset -q --hard "$base"
# A renamed job is a required check nobody reports.
sed -i.bak 's/^    name: gate$/    name: Gate/' .github/workflows/ci.yml && rm .github/workflows/ci.yml.bak
fails "$scripts/guard.sh"
git reset -q --hard "$base"

# --staged reads the INDEX: a violation staged and then fixed only in the
# working tree is still what the commit would record.
printf 'use std::net::TcpStream;\n' >>src/main.rs
git add -A
git show "$base:src/main.rs" >src/main.rs
passes "$scripts/guard.sh"
fails "$scripts/guard.sh" --staged
git reset -q --hard "$base"

# ── commit-msg.sh ───────────────────────────────────────────────────────────
msg="$tmp/message with spaces.txt"
subject() { printf '%s\n' "$1" >"$msg"; }
subject 'feat(gate): a reserve per role' && passes "$scripts/commit-msg.sh" "$msg"
subject 'fixup! feat: adjust the gate' && passes "$scripts/commit-msg.sh" "$msg"
subject 'a subject with no type' && fails "$scripts/commit-msg.sh" "$msg"
subject 'gate: an area is a scope, not a type' && fails "$scripts/commit-msg.sh" "$msg"
subject 'release: v0.1.0' && passes "$scripts/commit-msg.sh" "$msg"
subject 'release: v0.2.0' && fails "$scripts/commit-msg.sh" "$msg"
subject 'release: the big one' && fails "$scripts/commit-msg.sh" "$msg"
# A commit that moves the version must BE the release commit for it.
set_version 0.2.0
git add -A
subject 'fix: a bump smuggled into a fix' && fails "$scripts/commit-msg.sh" "$msg"
subject 'release: v0.2.0' && passes "$scripts/commit-msg.sh" "$msg"
git commit -qm 'release: v0.2.0'

# ── merge-commits.sh ────────────────────────────────────────────────────────
git checkout -q -b topic
fails env -u DAFT_MERGE_TARGET_BRANCH "$scripts/merge-commits.sh"
git commit -q --allow-empty -m 'test: a conventional incoming commit'
passes env DAFT_MERGE_TARGET_BRANCH=main "$scripts/merge-commits.sh"
git commit -q --allow-empty -m 'written with --no-verify'
fails env DAFT_MERGE_TARGET_BRANCH=main "$scripts/merge-commits.sh"

# ── landed-check.sh ─────────────────────────────────────────────────────────
head=$(git rev-parse HEAD)
passes env DAFT_MERGE_RESULT=success DAFT_MERGE_SOURCE_SHAS="$head" "$scripts/landed-check.sh"
# An inherited GIT_DIR must not retarget it.
passes env DAFT_MERGE_RESULT=success DAFT_MERGE_SOURCE_SHAS="$head" GIT_DIR="$root/.git" "$scripts/landed-check.sh"
fails env DAFT_MERGE_RESULT=success DAFT_MERGE_SOURCE_SHAS="$base" "$scripts/landed-check.sh"
passes env DAFT_MERGE_RESULT=conflict DAFT_MERGE_SOURCE_SHAS="$base" "$scripts/landed-check.sh"
fails env -u DAFT_MERGE_SOURCE_SHAS "$scripts/landed-check.sh"

cd "$root"
echo "test-hooks: $checks checks passed"
