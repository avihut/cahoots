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

# `with_stdin <text> <command…>` — for the pre-push check, which reads refs.
with_stdin() {
    local input=$1
    shift
    printf '%s' "$input" | "$@"
}

# ── no-warnings.sh ──────────────────────────────────────────────────────────
passes "$scripts/no-warnings.sh" /bin/sh -c 'echo clean'
fails "$scripts/no-warnings.sh" /bin/sh -c 'exit 7'
fails "$scripts/no-warnings.sh" /bin/sh -c 'echo "warning: package diagnostic" >&2'
fails "$scripts/no-warnings.sh" /bin/sh -c 'echo "ld: warning: linker diagnostic"'

# ── real-state.sh ───────────────────────────────────────────────────────────
fake_home="$tmp/fake-home"
mkdir -p "$fake_home/.claude/skills/cahoots" "$fake_home/.codex"
printf 'installed by a person\n' >"$fake_home/.claude/skills/cahoots/SKILL.md"
passes env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard /bin/sh -c 'echo harmless'
# The guarded command's own failure passes through, tripwire or not.
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard /bin/sh -c 'exit 3'
# A new file, a changed file, a removed file, a new agent definition: all trip it.
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "mkdir -p '$fake_home/.local/state/cahoots' && echo x >'$fake_home/.local/state/cahoots/install-manifest.json'"
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "echo edited >>'$fake_home/.claude/skills/cahoots/SKILL.md'"
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "mkdir -p '$fake_home/.codex/agents' && echo x >'$fake_home/.codex/agents/cahoots-delegate.toml'"
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "rm '$fake_home/.claude/skills/cahoots/SKILL.md'"
# Somebody else's files in an agent home are none of its business.
passes env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "mkdir -p '$fake_home/.claude/skills/other' && echo x >'$fake_home/.claude/skills/other/SKILL.md'"
fails "$scripts/real-state.sh" guard

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
printf '# generated\nversion = 4\n\n[[package]]\nname = "fixture"\nversion = "0.1.0"\n\n[[package]]\nname = "serde"\nversion = "1.0.0"\n' >Cargo.lock
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
release=$(git rev-parse HEAD)

# ── release-check.sh (pre-push stdin: local ref, sha, remote ref, sha) ──────
zero=0000000000000000000000000000000000000000
branch_push="refs/heads/main $release refs/heads/main $base
"
fails with_stdin "$branch_push" "$scripts/release-check.sh" # no tag yet
git tag v0.2.0 "$release"                                   # lightweight
fails with_stdin "$branch_push" "$scripts/release-check.sh"
fails with_stdin "refs/tags/v0.2.0 $release refs/tags/v0.2.0 $zero
" "$scripts/release-check.sh"
git tag -d v0.2.0 >/dev/null
git tag -a v0.2.0 -m 'notes' "$base" # annotated, wrong commit
fails with_stdin "$branch_push" "$scripts/release-check.sh"
git tag -d v0.2.0 >/dev/null
git tag -a v0.2.0 -m 'notes' "$release"
passes with_stdin "$branch_push" "$scripts/release-check.sh"
passes with_stdin "${branch_push}refs/tags/v0.2.0 $(git rev-parse v0.2.0) refs/tags/v0.2.0 $zero
" "$scripts/release-check.sh"
passes with_stdin "(delete) $zero refs/heads/gone $base
" "$scripts/release-check.sh"
passes with_stdin "" "$scripts/release-check.sh"
git tag -a v0.9.0 -m 'notes' "$release" # a tag whose version the tree doesn't hold
fails with_stdin "refs/tags/v0.9.0 $(git rev-parse v0.9.0) refs/tags/v0.9.0 $zero
" "$scripts/release-check.sh"
git tag -d v0.9.0 >/dev/null

# ── release-reminder.sh ─────────────────────────────────────────────────────
passes "$scripts/release-reminder.sh" # HEAD is the tagged release
git commit -q --allow-empty -m 'docs: not a release-worthy change'
passes "$scripts/release-reminder.sh"
git commit -q --allow-empty -m 'fix(gate): something users would notice'
fails "$scripts/release-reminder.sh"
git checkout -q -b topic
passes "$scripts/release-reminder.sh" # releases are cut from main only

# ── merge-commits.sh ────────────────────────────────────────────────────────
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

# ── release.sh ──────────────────────────────────────────────────────────────
# The fixture's main holds a fix since v0.2.0.
passes "$scripts/release.sh" # on topic: releases are cut from main, so: nothing
passes test "$(git log -1 --format=%s)" = 'written with --no-verify'
git checkout -q main
printf 'stray\n' >stray.txt
fails "$scripts/release.sh" # nothing gets built on a dirty tree
rm stray.txt

passes "$scripts/release.sh" --dry-run
cp "$out" "$tmp/dry-run.log" # `passes` truncates $out before its own command reads it
passes grep -q '0.2.0 → 0.2.1' "$tmp/dry-run.log"
passes test "$(git log -1 --format=%s)" = 'fix(gate): something users would notice'
passes test -z "$(git status --porcelain)" # a dry run writes nothing

passes "$scripts/release.sh"
passes test "$(git log -1 --format=%s)" = 'release: v0.2.1'
passes test "$(git cat-file -t v0.2.1)" = tag
passes test -n "$(git tag --points-at HEAD --list v0.2.1)"
# The version moved in BOTH files, and only for this package.
passes grep -q '^version = "0.2.1"$' Cargo.toml
passes sh -c "grep -A1 '^name = \"fixture\"$' Cargo.lock | grep -q '^version = \"0.2.1\"$'"
passes sh -c "grep -A1 '^name = \"serde\"$' Cargo.lock | grep -q '^version = \"1.0.0\"$'"
passes grep -q '^## v0.2.1 — ' CHANGELOG.md
passes grep -q 'fix(gate): something users would notice' CHANGELOG.md
# And the commit it made passes the checks a push would put it through.
passes with_stdin "refs/heads/main $(git rev-parse HEAD) refs/heads/main $release
" "$scripts/release-check.sh"
landed=$(git rev-parse HEAD)

# Idempotent: a second run releases nothing and writes no commit.
passes "$scripts/release.sh"
passes test "$(git rev-parse HEAD)" = "$landed"

# A tip that is ALREADY a release commit only gets its missing tag.
git tag -d v0.2.1 >/dev/null
passes "$scripts/release.sh"
passes test "$(git cat-file -t v0.2.1)" = tag
passes test "$(git rev-parse HEAD)" = "$landed"

# The notes fragment is the annotation, and the release commit spends it.
mkdir -p .release-notes
# Written UNDER the template comment, as a real fragment is: the comment is
# scaffolding and must not reach the annotation.
printf '<!-- What shipped, in prose.\n     Second comment line. -->\n\nA short title\n\nProse the fragment carried.\n' >.release-notes/next.md
git add -A
git commit -qm 'feat(gate): something worth a minor'
passes "$scripts/release.sh"
passes test "$(git log -1 --format=%s)" = 'release: v0.3.0'
passes sh -c "git tag -l --format='%(contents)' v0.3.0 | grep -q 'Prose the fragment carried'"
passes test "$(git tag -l --format='%(contents:subject)' v0.3.0)" = 'A short title'
passes sh -c "! git tag -l --format='%(contents)' v0.3.0 | grep -q -e '<!--' -e 'comment line'"
passes sh -c "! grep -q 'Prose the fragment carried' .release-notes/next.md"
# The changelog grows at the top and keeps what was there.
passes sh -c "grep -n '^## v' CHANGELOG.md | head -1 | grep -q 'v0.3.0'"
passes grep -q '^## v0.2.1 — ' CHANGELOG.md
passes test "$(grep -c '^# Changelog$' CHANGELOG.md)" = 1

# A breaking change before 1.0 is a minor bump, never an automatic 1.0.0.
git commit -q --allow-empty -m 'feat(cli)!: a verb changed its meaning'
passes "$scripts/release.sh"
passes test "$(git log -1 --format=%s)" = 'release: v0.4.0'

cd "$root"
echo "test-hooks: $checks checks passed"
