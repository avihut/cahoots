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
# The fake processes the no-strays cases leave running, by pid, one a line.
fake_pids="$tmp/fake-pids"
# Kills each recorded fixture and checks that it is gone, however it was
# started (absolute path or relative): a leftover is a failure of the test.
reap_fakes() {
    local pid left=0
    [ -s "$fake_pids" ] || return 0
    while read -r pid; do
        # One that already ended may have had its pid reused: only a fixture
        # (named cahoots, absolute or `./cahoots`) is touched.
        ps -p "$pid" -o args= 2>/dev/null | grep -q 'cahoots' || continue
        kill "$pid" 2>/dev/null || true
        for _ in 1 2 3 4 5 6 7 8 9 10; do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.5
        done
        if kill -0 "$pid" 2>/dev/null; then
            echo "test-hooks: fixture process $pid is still running" >&2
            left=1
        fi
    done <"$fake_pids"
    : >"$fake_pids"
    return "$left"
}
trap 'reap_fakes; rm -rf "$tmp"' EXIT
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

# `exits <code> <command…>` — a refusal with a particular exit code.
exits() {
    local want=$1 got=0
    shift
    "$@" >"$out" 2>&1 || got=$?
    if [ "$got" != "$want" ]; then
        cat "$out" >&2
        echo "test-hooks: expected exit $want, got $got: $*" >&2
        exit 1
    fi
    checks=$((checks + 1))
}
# `stdout_is <expected> <command…>` — succeeds, and stdout is exactly the
# expected lines (nothing at all when empty). $out holds stderr, then stdout.
stdout_is() {
    local want=$1
    shift
    if ! "$@" >"$tmp/stdout" 2>"$out"; then
        cat "$out" "$tmp/stdout" >&2
        echo "test-hooks: expected SUCCESS: $*" >&2
        exit 1
    fi
    if [ -n "$want" ]; then
        printf '%s\n' "$want" >"$tmp/expected"
    else
        : >"$tmp/expected"
    fi
    if ! cmp -s "$tmp/expected" "$tmp/stdout"; then
        echo "test-hooks: expected stdout [$want], got [$(cat "$tmp/stdout")]: $*" >&2
        exit 1
    fi
    cat "$tmp/stdout" >>"$out"
    checks=$((checks + 1))
}
# `said <substring>` — the last command's output holds it.
said() {
    if ! grep -qF -- "$1" "$out"; then
        cat "$out" >&2
        echo "test-hooks: the output does not say: $1" >&2
        exit 1
    fi
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

# ── no-strays.sh ────────────────────────────────────────────────────────────
# A real program under a fake build directory: a copy of `sleep` (re-signed,
# where macOS needs it, since a copied system binary is otherwise killed).
physical_tmp=$(cd "$tmp" && pwd -P)
strays_target="$physical_tmp/strays-target"
fake="$strays_target/debug/cahoots"
mkdir -p "$strays_target/debug"
cp "$(command -v sleep)" "$fake"
if command -v codesign >/dev/null 2>&1; then
    codesign --force -s - "$fake" >/dev/null 2>&1
fi
no_strays() { env NO_STRAYS_TARGET="$strays_target" NO_STRAYS_GRACE=3 "$scripts/no-strays.sh" "$@"; }
# `$tmp/fake-run <seconds> [background|relative]`: the fake sleeps. `relative`
# is in the background too, and started as `./cahoots` from its directory.
cat >"$tmp/fake-run" <<'SCRIPT'
#!/usr/bin/env bash
dir="$(cd "$(dirname "$0")" && pwd -P)/strays-target/debug"
case "${2:-}" in
    background) ("$dir/cahoots" "$1" >/dev/null 2>&1 & echo $! >>"$(dirname "$0")/fake-pids") ;;
    relative) (cd "$dir" && ./cahoots "$1" >/dev/null 2>&1 & echo $! >>"$(dirname "$0")/fake-pids") ;;
    *) exec "$dir/cahoots" "$1" ;;
esac
SCRIPT
chmod +x "$tmp/fake-run"
stray_sleeps() { reap_fakes; }
passes no_strays /bin/sh -c 'echo clean'
passes no_strays "$tmp/fake-run" 0
exits 7 no_strays /bin/sh -c 'exit 7'
# Still winding down when the command returns, gone within the grace period.
passes no_strays "$tmp/fake-run" 1 background
# Left running for good: refused, and named.
fails no_strays "$tmp/fake-run" 30 background
said "$fake"
stray_sleeps
# Started by a relative path: the file it runs is what counts.
fails no_strays "$tmp/fake-run" 30 relative
said "$fake"
stray_sleeps
passes no_strays "$tmp/fake-run" 1 relative
# A relative build directory is the one under the directory this runs in, and
# a stray under it is refused like any other.
in_tmp() { (cd "$physical_tmp" && env CARGO_TARGET_DIR=strays-target NO_STRAYS_GRACE=3 "$scripts/no-strays.sh" "$@"); }
passes in_tmp /bin/sh -c 'echo clean'
fails in_tmp "$tmp/fake-run" 30 background
said "$fake"
stray_sleeps
fails in_tmp "$tmp/fake-run" 30 relative
said "$fake"
stray_sleeps
passes in_tmp "$tmp/fake-run" 1 relative
# One an earlier run left is not this command's.
"$tmp/fake-run" 31 background
passes no_strays /bin/sh -c 'echo clean'
stray_sleeps
# A process list that cannot be read is a refusal, never an empty list. The
# lookup stands in for lsof: it prints what `$tmp/lookup.<call>` holds, and
# fails when there is no such file.
cat >"$tmp/lookup" <<'SCRIPT'
#!/usr/bin/env bash
count="$(dirname "$0")/lookup.count"
n=$(($(cat "$count" 2>/dev/null || echo 0) + 1))
echo "$n" >"$count"
cat "$(dirname "$0")/lookup.$n" 2>/dev/null
SCRIPT
chmod +x "$tmp/lookup"
lookup_is() { # <call> <process lines…>: what the next runs' call number <call> shows
    local call=$1 line
    shift
    : >"$tmp/lookup.$call"
    for line in "$@"; do printf 'p%s\nn%s\n' "${line%% *}" "${line#* }" >>"$tmp/lookup.$call"; done
}
fresh_lookup() { rm -f "$tmp"/lookup.[0-9]* "$tmp/lookup.count"; }
with_lookup() { env NO_STRAYS_LOOKUP="$tmp/lookup" NO_STRAYS_TARGET="$strays_target" NO_STRAYS_GRACE=2 "$scripts/no-strays.sh" "$@"; }
ran="$tmp/ran"
# A successful, empty snapshot (no process under the build directory) passes.
fresh_lookup
lookup_is 1 "1 /sbin/init"
lookup_is 2 "1 /sbin/init"
passes with_lookup /bin/sh -c 'echo clean'
# Unreadable at the start: refused, and the command is not run.
fresh_lookup
rm -f "$ran"
fails with_lookup /bin/sh -c "touch '$ran'"
said "cannot be read"
[ ! -e "$ran" ] || { echo "test-hooks: no-strays ran the command with no process list" >&2; exit 1; }
# Unreadable at the end of the command.
fresh_lookup
lookup_is 1 "1 /sbin/init"
fails with_lookup /bin/sh -c 'echo clean'
said "cannot be read"
# Unreadable while waiting out a stray.
fresh_lookup
lookup_is 1 "1 /sbin/init"
lookup_is 2 "1 /sbin/init" "42 $fake"
fails with_lookup /bin/sh -c 'echo clean'
said "cannot be read"
# And a stray that does not go is named from the same source.
fresh_lookup
lookup_is 1 "1 /sbin/init"
for call in 2 3 4 5 6; do lookup_is "$call" "1 /sbin/init" "42 $fake"; done
fails with_lookup /bin/sh -c 'echo clean'
said "42 $fake"

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
# The eval suite is a person's too: a task appearing in it trips it.
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "mkdir -p '$fake_home/.local/share/cahoots/evals/tasks/x' && echo '{}' >'$fake_home/.local/share/cahoots/evals/tasks/x/task.json'"
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "echo edited >>'$fake_home/.claude/skills/cahoots/SKILL.md'"
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "mkdir -p '$fake_home/.codex/agents' && echo x >'$fake_home/.codex/agents/cahoots-delegate.toml'"
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "rm '$fake_home/.claude/skills/cahoots/SKILL.md'"
# A kind's subagent appearing, or one that was there changing, trips it too.
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "mkdir -p '$fake_home/.claude/agents' && echo x >'$fake_home/.claude/agents/cahoots-kind-rust-review.md'"
fails env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "echo edited >>'$fake_home/.claude/agents/cahoots-kind-rust-review.md'"
# Somebody else's files in an agent home are none of its business.
passes env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "mkdir -p '$fake_home/.claude/skills/other' && echo x >'$fake_home/.claude/skills/other/SKILL.md'"
passes env REAL_STATE_HOME="$fake_home" "$scripts/real-state.sh" guard \
    /bin/sh -c "echo x >'$fake_home/.claude/agents/mine.md'"
fails "$scripts/real-state.sh" guard

# A real cahoots running meanwhile (#75) excuses nothing: the guard still
# fails, and its words say so, to tell a rerun from a test that touched real
# state. A stand-in is a `sleep` whose argv[0] says otherwise, started beside
# the guard, not by the command it runs. Under REAL_STATE_HOME only one in
# that home counts — and every stand-in is inside this repository, which a
# real guard ignores, so a suite running alongside never reads one as real.
fake_state="$fake_home/.local/state/cahoots"
mkdir -p "$fake_state/runs/old" "$fake_home/.config/cahoots"
echo '{"event":"old"}' >"$fake_state/history.jsonl"
echo '{}' >"$fake_state/runs/old/run.json"
echo 'schema = 1' >"$fake_home/.config/cahoots/config.toml"
standin=""
outside=""
trap '[ -z "$standin" ] || kill "$standin" 2>/dev/null; rm -rf "$tmp" ${outside:+"$outside"}' EXIT
real_running() {
    (exec -a "$1" sleep 60) &
    standin=$!
    local tries=0
    until ps -o args= -p "$standin" | grep -qF -- "$1"; do
        tries=$((tries + 1))
        [ "$tries" -lt 100 ] || { echo "test-hooks: the stand-in $1 never showed in ps" >&2; exit 1; }
        sleep 0.05
    done
}
real_stopped() {
    kill "$standin"
    wait "$standin" 2>/dev/null || true
    standin=""
}
guard() {
    env REAL_STATE_HOME="$fake_home" REAL_STATE_OWN_ROOT="$fake_home/project" \
        "$scripts/real-state.sh" guard "$@"
}
new_run() {
    echo "mkdir -p '$fake_state/runs/$1' && echo '{}' >'$fake_state/runs/$1/run.json'"
}
appended="echo '{\"event\":\"new\"}' >>'$fake_state/history.jsonl'"
rerun="A real cahoots ran meanwhile; rerun when it is idle"
likely="a test likely touched real state"

# A real one running, and nothing changed: the guard passes.
real_running "$fake_home/.local/bin/cahoots"
passes guard /bin/sh -c 'echo harmless'
# What a run writes changed while it ran: still a failure, and it says rerun.
fails guard /bin/sh -c "$(new_run during) && $appended"
said "$rerun"
said "What a run writes:"
said "$fake_state/runs/during/run.json"
said "$fake_state/history.jsonl"
said "$fake_home/.local/bin/cahoots"
# What no run writes changed while it ran: it fails, names it, and says a test
# likely did it.
fails guard /bin/sh -c "echo 'schema = 2' >'$fake_home/.config/cahoots/config.toml'"
said "Paths normal runs do not write:"
said "$fake_home/.config/cahoots/config.toml"
said "But no run writes the second list: $likely"
real_stopped
# With none running, it says a test likely touched real state.
fails guard /bin/sh -c "$(new_run alone)"
said "No real cahoots was seen running meanwhile: $likely"
# A build is never a real cahoots: the supervisor a test left behind, or any
# binary of this repository's own, or one outside the watched home.
for build in "$fake_home/src/target/debug/cahoots" "$fake_home/project/feat/x/bin/cahoots" "$tmp/elsewhere/cahoots"; do
    real_running "$build"
    fails guard /bin/sh -c "$(new_run "${build//\//_}")"
    said "No real cahoots was seen running meanwhile"
    real_stopped
done

# The sampler dying mid-look, by a TERM (how it was once stopped, #87),
# neither breaks the guard nor lets a change through: nothing changed still
# passes, and a change still fails, in words. The helper runs directly under
# the guard, so its sibling there is the sampler: it kills it, records how
# many it saw die, and only then makes its change, if it was given one.
cat >"$tmp/kill-sampler" <<EOF
#!/bin/sh
dead=0
for pid in \$(ps -A -o pid=,ppid= | awk -v guard="\$PPID" -v me="\$\$" '\$2 == guard && \$1 != me { print \$1 }'); do
    kill -TERM "\$pid" 2>/dev/null || continue
    tries=0
    while ps -o stat= -p "\$pid" | grep -qv '^Z'; do
        tries=\$((tries + 1))
        [ "\$tries" -lt 50 ] || exit 1
        sleep 0.05
    done
    dead=\$((dead + 1))
done
echo "\$dead" >'$tmp/sampler-dead'
[ \$# -eq 0 ] || { mkdir -p "\$1" && echo '{}' >"\$1/run.json"; }
EOF
chmod +x "$tmp/kill-sampler"
sampler_died() {
    [ "$(cat "$tmp/sampler-dead")" = 1 ] || { echo "test-hooks: kill-sampler killed $(cat "$tmp/sampler-dead") samplers, not 1" >&2; exit 1; }
    rm "$tmp/sampler-dead"
}
real_running "$fake_home/.local/bin/cahoots"
passes guard "$tmp/kill-sampler"
sampler_died
fails guard "$tmp/kill-sampler" "$fake_state/runs/killed"
sampler_died
said "What a run writes:"
said "$fake_state/runs/killed/run.json"
real_stopped
# Quick commands, back to back: each one's sampler is gone before it ends.
for _ in 1 2 3 4 5 6 7 8 9 10; do
    passes guard /bin/sh -c true
done

# The installed cahoots, run by name, fails the suite — even when the test
# swallows its refusal. Merely finding it on PATH is no failure. (A harmless
# `cahoots` is next on PATH, so a guard that lost its own still never reaches
# the real one from here.)
mkdir -p "$tmp/harmless"
printf '#!/bin/sh\nexit 0\n' >"$tmp/harmless/cahoots"
chmod +x "$tmp/harmless/cahoots"
passes env PATH="$tmp/harmless:$PATH" REAL_STATE_HOME="$fake_home" REAL_STATE_OWN_ROOT="$fake_home/project" \
    "$scripts/real-state.sh" guard /bin/sh -c 'command -v cahoots'
fails env PATH="$tmp/harmless:$PATH" REAL_STATE_HOME="$fake_home" REAL_STATE_OWN_ROOT="$fake_home/project" \
    "$scripts/real-state.sh" guard /bin/sh -c 'cahoots run || true'
said "ran \`cahoots\` by name"

# Which repository's builds are never real is found from the script, whatever
# a hook exported: a GIT_DIR naming another repository, absolute or relative,
# changes nothing. So a stand-in inside this repository (where test-hooks
# runs) is never read as real, even with git's variables pointing elsewhere.
other_repo="$tmp/other-repo"
git init -q "$other_repo"
project=$(dirname "$(git -C "$root" rev-parse --path-format=absolute --git-common-dir)")
stdout_is "$project" env GIT_DIR="$other_repo/.git" GIT_WORK_TREE="$other_repo" \
    "$scripts/real-state.sh" project
stdout_is "$project" /bin/sh -c "cd '$other_repo' && GIT_DIR=.git GIT_INDEX_FILE=.git/index exec '$scripts/real-state.sh' project"
real_running "$fake_home/.local/bin/cahoots"
passes env GIT_DIR="$other_repo/.git" GIT_WORK_TREE="$other_repo" REAL_STATE_HOME="$fake_home" \
    "$scripts/real-state.sh" guard /bin/sh -c 'echo harmless'
fails env GIT_DIR="$other_repo/.git" GIT_WORK_TREE="$other_repo" REAL_STATE_HOME="$fake_home" \
    "$scripts/real-state.sh" guard /bin/sh -c "$(new_run hooked)"
said "No real cahoots was seen running meanwhile"
real_stopped
# Outside any repository there is no answer, and the guard refuses.
outside=$(mktemp -d "${TMPDIR:-/tmp}/real-state-outside.XXXXXX")
cp "$scripts/real-state.sh" "$outside/"
outside_env=(env GIT_CEILING_DIRECTORIES="$(dirname "$outside")" REAL_STATE_HOME="$fake_home")
fails "${outside_env[@]}" "$outside/real-state.sh" project
fails "${outside_env[@]}" "$outside/real-state.sh" guard /bin/sh -c true
said "cannot find the repository"

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
# The one environment builder: cleared, the caller's variables, then the
# prohibition on lazy fetching, last.
spawn_with() { # spawn_with <the builder's body lines…>
    {
        printf 'use std::process::Command;\n'
        printf 'const NO_LAZY_FETCH: (&str, &str) = ("GIT_NO_LAZY_FETCH", "1");\n'
        printf 'fn scrubbed(command: &mut Command, vars: Vec<(String, String)>) {\n'
        printf '    %s\n' "$@"
        printf '}\n'
        printf 'pub fn spawn() { let mut command = Command::new("claude"); scrubbed(&mut command, Vec::new()); }\n'
    } >src/spawn.rs
}
spawn_with 'command.env_clear();' 'command.envs(vars);' 'command.env(NO_LAZY_FETCH.0, NO_LAZY_FETCH.1);'
printf 'pub const SKILLS: &str = ".agents/skills";\n' >src/install/paths.rs
printf 'pub fn home_override() -> Option<String> { std::env::var("CAHOOTS_STATE_DIR").ok() }\n' >src/env.rs
# The layers as rule 11 draws them: the command layer prints and asks through
# the TUI, and the TUI touches the terminal.
mkdir -p src/cli src/tui
printf 'pub fn emit(line: &str) { println!("{line}"); }\npub fn ask() -> bool { crate::tui::open() }\n' >src/cli.rs
printf 'pub fn which() -> bool { crate::tui::open() }\n' >src/cli/questions.rs
printf 'use std::io::IsTerminal;\npub fn open() -> bool { std::io::stderr().is_terminal() }\n' >src/tui/mod.rs
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
trips src/gate.rs 'fn shout() { eprintln!("over the cap"); }'
trips src/gate.rs 'fn answer() -> std::io::Stdin { std::io::stdin() }'
trips src/gate.rs 'use std::io::IsTerminal;'
trips src/spawn.rs 'pub fn grouped(command: &mut Command) { command.env_clear(); }'
trips src/main.rs 'const LAZY: (&str, &str) = ("GIT_NO_LAZY_FETCH", "0");'
trips src/tui/mod.rs 'use crate::meter::MeterId;'
trips src/gate.rs 'use crate::tui::Rail;'
# Rule 12: a PR's code never meets a secret. Rule 13: the release app's key is
# named in release-flow.yml alone.
trips .github/workflows/ci.yml 'on: pull_request_target'
# shellcheck disable=SC2016 # a workflow expression, appended as text
trips .github/workflows/ci.yml '      K: ${{ secrets.WHEATLEY_BOT_PRIVATE_KEY }}'
trips .github/workflows/ci.yml '    environment: release'
printf '#!/bin/sh\n' >scripts/orphan.sh
git add -A
fails "$scripts/guard.sh" --staged
git reset -q --hard "$base"
# The prohibition dropped, or set before the caller's variables, which could
# then relax it; and the definition gone.
for body in "command.env_clear();|command.envs(vars);" \
    "command.env_clear();|command.env(NO_LAZY_FETCH.0, NO_LAZY_FETCH.1);|command.envs(vars);"; do
    IFS='|' read -r -a lines <<<"$body"
    spawn_with "${lines[@]}"
    git add -A
    fails "$scripts/guard.sh"
    fails "$scripts/guard.sh" --staged
    git reset -q --hard "$base"
done
sed -i.bak 's/("GIT_NO_LAZY_FETCH", "1")/("GIT_NO_LAZY_FETCH", "true")/' src/spawn.rs && rm src/spawn.rs.bak
fails "$scripts/guard.sh"
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
# pass:feat-subject
subject 'feat(gate): a reserve per role' && passes "$scripts/commit-msg.sh" "$msg"
subject 'fixup! feat: adjust the gate' && passes "$scripts/commit-msg.sh" "$msg"
# refuse:no-type, refuse:area-as-type
subject 'a subject with no type' && fails "$scripts/commit-msg.sh" "$msg"
subject 'gate: an area is a scope, not a type' && fails "$scripts/commit-msg.sh" "$msg"
# refuse:release-subject, refuse:release-scoped — the release workflow alone
# makes a release commit.
subject 'release: v0.1.0' && fails "$scripts/commit-msg.sh" "$msg"
said 'made by the release workflow'
subject 'release(x): y' && fails "$scripts/commit-msg.sh" "$msg"
# A commit that moves the version is refused, whatever its subject.
set_version 0.2.0
git add -A
# refuse:version-move-under-fix
subject 'fix: a bump smuggled into a fix' && fails "$scripts/commit-msg.sh" "$msg"
said "moves Cargo.toml's version"
# refuse:version-move-under-release-subject
subject 'release: v0.2.0' && fails "$scripts/commit-msg.sh" "$msg"
git reset -q --hard "$base"

# The fixture's first release, as the release workflow would leave it on main:
# the release commit and its annotated tag.
set_version 0.2.0
git add -A
git commit -qm 'release: v0.2.0'
release=$(git rev-parse HEAD)
git tag -a v0.2.0 -m 'notes' "$release"

# ── release-check.sh (pre-push stdin: local ref, sha, remote ref, sha) ──────
zero=0000000000000000000000000000000000000000
# pass:empty-stdin, pass:delete
passes with_stdin "" "$scripts/release-check.sh"
passes with_stdin "(delete) $zero refs/heads/gone $base
" "$scripts/release-check.sh"
# refuse:tag-push — a v* tag is the release workflow's.
fails with_stdin "refs/tags/v0.2.0 $(git rev-parse v0.2.0) refs/tags/v0.2.0 $zero
" "$scripts/release-check.sh"
said 'made by the release workflow'
# refuse:release-pr-push — whatever the local branch is called.
fails with_stdin "refs/heads/topic $release refs/heads/release-pr $zero
" "$scripts/release-check.sh"
said "release-pr is the release workflow's branch"
# refuse:release-commit-in-push
fails with_stdin "refs/heads/main $release refs/heads/main $base
" "$scripts/release-check.sh"
said 'is a release commit'
# refuse:release-commit-from-non-branch-ref
fails with_stdin "HEAD $release refs/heads/main $base
" "$scripts/release-check.sh"
said 'is a release commit'
fails with_stdin "$release $release refs/heads/topic $zero
" "$scripts/release-check.sh"
said 'is a release commit'
# Main, release commit included, is on the remote from here on.
git update-ref refs/remotes/origin/main "$release"
git checkout -q -b topic
git commit -q --allow-empty -m 'fix(gate): a plain fix'
# pass:branch-with-plain-commits
passes with_stdin "refs/heads/topic $(git rev-parse HEAD) refs/heads/topic $release
" "$scripts/release-check.sh"
# pass:release-commit-already-on-remote — a branch on top of a main that holds
# a release commit is not refused for it.
passes with_stdin "refs/heads/topic $(git rev-parse HEAD) refs/heads/topic $zero
" "$scripts/release-check.sh"
# refuse:version-move-in-push
set_version 0.9.0
git add -A
git commit -qm 'fix: a version smuggled into a fix'
fails with_stdin "refs/heads/topic $(git rev-parse HEAD) refs/heads/topic $zero
" "$scripts/release-check.sh"
said "moves Cargo.toml's version 0.2.0 → 0.9.0"
# refuse:version-move-from-non-branch-ref — git spells the local side as given
# (`git push origin HEAD:main`, a sha); where it lands is what counts.
fails with_stdin "HEAD $(git rev-parse HEAD) refs/heads/main $release
" "$scripts/release-check.sh"
said "moves Cargo.toml's version 0.2.0 → 0.9.0"
git checkout -q main
git branch -q -D topic

# ── release-reminder.sh ─────────────────────────────────────────────────────
# pass:nothing-unreleased (HEAD is the tagged release, then a docs change)
passes "$scripts/release-reminder.sh"
git commit -q --allow-empty -m 'docs: not a release-worthy change'
passes "$scripts/release-reminder.sh"
said 'nothing unreleased'
# pass:unreleased-listed — listed, and still a pass: the release PR holds it.
git commit -q --allow-empty -m 'fix(gate): something users would notice'
fix=$(git rev-parse HEAD)
passes "$scripts/release-reminder.sh"
said 'The release PR holds them'
said 'fix(gate): something users would notice'
# pass:off-main
git checkout -q -b topic
passes "$scripts/release-reminder.sh"

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
# The fixture's main holds a fix since v0.2.0. Every case below is one step
# of the release workflow, run as the workflow runs it.
release_sh="$scripts/release.sh"
bot='wheatley-the-moronic-ci-bot[bot]'
# A body of 100,000 bytes, none of whose lines is a conventional subject: what
# made a reader that stops early break its writer (#68).
awk 'BEGIN { for (i = 0; i < 2000; i++) printf "%049d\n", i }' >"$tmp/large-body"
section_of() { # <version>: its CHANGELOG.md section, blank lines trimmed at both ends
    awk -v heading="## v$1 — " '
        index($0, heading) == 1 { on = 1; next }
        on && /^## v/ { on = 0 }
        on { lines[++n] = $0 }
        END {
            first = 1; while (first <= n && lines[first] == "") first++
            last = n; while (last >= first && lines[last] == "") last--
            for (i = first; i <= last; i++) print lines[i]
        }' CHANGELOG.md
}
large_commit() { # <subject>: an empty commit whose body is the large one
    { printf '%s\n\n' "$1" && cat "$tmp/large-body"; } >"$tmp/large-message"
    git commit -q --allow-empty -F "$tmp/large-message"
}

# refuse:usage
exits 2 "$release_sh"
said 'usage:'
exits 2 "$release_sh" --dry-run
exits 2 "$release_sh" tag
# pass:bot
stdout_is "$bot" "$release_sh" bot

git checkout -q main
# pass:plan-fix
stdout_is 0.2.1 "$release_sh" plan
said '0.2.0 → 0.2.1'
passes test -z "$(git status --porcelain)" # planning writes nothing
# pass:plan-feat
git commit -q --allow-empty -m 'feat(gate): something worth a minor'
stdout_is 0.3.0 "$release_sh" plan
git reset -q --hard "$fix"
# pass:plan-breaking-pre1 — a breaking change before 1.0 is a minor bump,
# never an automatic 1.0.0.
git commit -q --allow-empty -m 'feat(cli)!: a verb changed its meaning'
stdout_is 0.3.0 "$release_sh" plan
said 'taking a minor bump'
git reset -q --hard "$fix"
# pass:plan-nothing
git checkout -q --detach "$release"
stdout_is '' "$release_sh" plan
said 'nothing releasable since v0.2.0'
# #68: pass:plan-large-log-fix, -feat, -breaking. The newest commit carries
# the subject; its body is large. Each fails on the script before this one.
large_commit 'fix(gate): a fix with a long story'
stdout_is 0.2.1 "$release_sh" plan
git checkout -q --detach "$release"
large_commit 'feat(gate): a feature with a long story'
stdout_is 0.3.0 "$release_sh" plan
git checkout -q --detach "$release"
large_commit 'fix(gate)!: a breaking fix with a long story'
stdout_is 0.3.0 "$release_sh" plan
git checkout -q main

# commit: refuse:commit-off-release-pr
fails "$release_sh" commit
said 'built on release-pr'
git checkout -q -B release-pr main
# refuse:commit-dirty
printf 'stray\n' >stray.txt
fails "$release_sh" commit
said 'not clean'
rm stray.txt

# pr-body: pass:pr-body-cap — 310 commits since the last tag list 300 and
# say how many more.
# body_fits: the body is within GitHub's limit with room to spare (bytes, which
# never undercount characters), its marker is the first line, and it lists
# its commits.
body_fits() {
    passes test "$(wc -c <"$tmp/body" | tr -d ' ')" -le 60000
    passes grep -q '^<!-- cahoots-release version=' "$tmp/body"
    passes grep -qxF -- '## Commits since v0.2.0' "$tmp/body"
}
fillers() { # <count> <subject text>: that many empty commits on HEAD
    local tree tip i=0
    tree=$(git rev-parse 'HEAD^{tree}')
    tip=$(git rev-parse HEAD)
    while [ "$i" -lt "$1" ]; do
        tip=$(git commit-tree -p "$tip" -m "docs: filler $i $2" "$tree")
        i=$((i + 1))
    done
    git reset -q --hard "$tip"
}
fillers 308 ''
passes "$release_sh" commit
passes "$release_sh" pr-body
cp "$out" "$tmp/body"
passes test "$(grep -cE '^- [0-9a-f]+ ' "$tmp/body")" = 300
passes grep -qxF -- '- … and 10 more' "$tmp/body"
passes grep -qF -- 'docs: filler 307' "$tmp/body" # short subjects stay whole
body_fits
# pass:pr-body-long-subjects — 300 subjects of ~250 bytes each would be 75,000
# bytes of list alone: each is shortened to fit, and keeps its sha. Em dashes,
# so a cut can land inside a character.
git checkout -q -B release-pr main
long=$(awk 'BEGIN { for (i = 0; i < 80; i++) printf "ab—" }')
fillers 308 "$long"
passes "$release_sh" commit
passes "$release_sh" pr-body
cp "$out" "$tmp/body"
body_fits
passes test "$(grep -cE '^- [0-9a-f]{7,} docs: filler [0-9]+ .*…$' "$tmp/body")" = 300
passes grep -qxF -- '- … and 10 more' "$tmp/body"
passes grep -qF -- "$(git rev-parse --short HEAD^)" "$tmp/body"
# No cut leaves half a character: the body is still valid UTF-8.
passes iconv -f UTF-8 -t UTF-8 "$tmp/body"
# pass:pr-body-long-changelog — a 70,000-byte fragment is the section, cut to
# fit with a line saying so, while the marker and the list stay whole.
git checkout -q -B release-pr main
mkdir -p .release-notes
{
    printf 'A long release\n\n'
    awk 'BEGIN { for (i = 0; i < 1600; i++) printf "Line %04d of prose — the notes go on and on.\n", i }'
} >.release-notes/next.md
git add -A
git commit -qm 'docs: long notes'
passes "$release_sh" commit
passes "$release_sh" pr-body
cp "$out" "$tmp/body"
body_fits
passes grep -qxF -- '(cut short — CHANGELOG.md at the PR head is whole)' "$tmp/body"
passes grep -qxF -- 'A long release' "$tmp/body"
passes grep -qxF -- "- $(git rev-parse --short "$fix") fix(gate): something users would notice" "$tmp/body"
passes test "$(wc -c <CHANGELOG.md | tr -d ' ')" -gt 70000 # the file itself is whole

# pass:commit-fix — on a release-pr built from main.
git checkout -q -B release-pr main
passes "$release_sh" commit
said 'committed release: v0.2.1'
built=$(git rev-parse HEAD)
passes test "$(git log -1 --format=%s)" = 'release: v0.2.1'
passes test -z "$(git status --porcelain)"
# The version moved in BOTH files, and only for this package.
passes grep -qx 'version = "0.2.1"' Cargo.toml
grep -A1 '^name = "fixture"$' Cargo.lock >"$tmp/lock-fixture"
grep -A1 '^name = "serde"$' Cargo.lock >"$tmp/lock-serde"
passes grep -qx 'version = "0.2.1"' "$tmp/lock-fixture"
passes grep -qx 'version = "1.0.0"' "$tmp/lock-serde"
passes grep -q '^## v0.2.1 — ' CHANGELOG.md
# The section IS the notes, title line included.
printf 'cahoots 0.2.1\n\n- fix(gate): something users would notice\n' >"$tmp/section-0.2.1"
section_of 0.2.1 >"$tmp/written"
passes cmp "$tmp/section-0.2.1" "$tmp/written"

# pass:pr-body-marker, pass:pr-body-commits
passes "$release_sh" pr-body
cp "$out" "$tmp/body"
passes test "$(sed -n 1p "$tmp/body")" = "<!-- cahoots-release version=0.2.1 base=$(git rev-parse main) since=v0.2.0 -->"
passes grep -qxF -- "- $(git rev-parse --short "$fix") fix(gate): something users would notice" "$tmp/body"
passes grep -qxF -- '## Changelog' "$tmp/body"
passes grep -qxF -- 'cahoots 0.2.1' "$tmp/body"
passes grep -qxF -- '## Commits since v0.2.0' "$tmp/body"
# refuse:pr-body-not-release
git checkout -q main
fails "$release_sh" pr-body
said 'HEAD is not a release commit'

# Merging the release PR: GitHub's squash, with its default subject.
git merge -q --squash release-pr >/dev/null
git commit -qm 'release: v0.2.1 (#7)'
squash=$(git rev-parse HEAD)

# pending-tag: pass:pending-tag-found
stdout_is "sha=$squash
version=0.2.1" "$release_sh" pending-tag
# pass:pending-tag-large-history
large_commit 'docs: a newer commit with a long story'
stdout_is "sha=$squash
version=0.2.1" "$release_sh" pending-tag
git reset -q --hard "$squash"
# refuse:plan-while-untagged, refuse:commit-while-untagged
fails "$release_sh" plan
said 'not tagged yet'
git checkout -q -B release-pr main
fails "$release_sh" commit
said 'not tagged yet'
git checkout -q main

# tag <sha>, with the merged PR as GitHub's API gives it.
good=(MERGED_PR_NUMBER=7 MERGED_PR_HEAD_REF=release-pr MERGED_PR_CROSS_REPO=false "MERGED_PR_AUTHOR=$bot")
# refuse:tag-no-pr
fails "$release_sh" tag "$squash"
said 'no merged pull request was given'
# refuse:tag-head-ref, refuse:tag-fork, refuse:tag-author, refuse:tag-number
fails env "${good[@]}" MERGED_PR_HEAD_REF=feature "$release_sh" tag "$squash"
said 'was not merged from release-pr'
fails env "${good[@]}" MERGED_PR_CROSS_REPO=true "$release_sh" tag "$squash"
said 'came from a fork'
fails env "${good[@]}" MERGED_PR_AUTHOR=avihut "$release_sh" tag "$squash"
said 'was not opened by the release bot'
fails env "${good[@]}" MERGED_PR_NUMBER=8 "$release_sh" tag "$squash"
said 'names #7 but was merged from #8'
# refuse:tag-not-release-subject
fails env "${good[@]}" "$release_sh" tag "$base"
said 'is not a release commit'
# refuse:tag-version-mismatch — a release subject that leaves the version alone.
git checkout -q --detach "$squash"
git commit -q --allow-empty -m 'release: v0.9.0 (#7)'
fails env "${good[@]}" "$release_sh" tag HEAD
said "but Cargo.toml's version there is"
# refuse:tag-no-move
git checkout -q --detach "$squash"
git commit -q --allow-empty -m 'release: v0.2.1 (#7)'
fails env "${good[@]}" "$release_sh" tag HEAD
said 'does not move the version'
git checkout -q main
# refuse:tag-not-ancestor — the release-pr commit itself never landed.
fails env "${good[@]}" "$release_sh" tag "$built"
said "is not on this branch's history"
# refuse:tag-no-section — a release commit with no CHANGELOG section.
git checkout -q --detach "$squash^"
set_version 0.2.1
git add -A
git commit -qm 'release: v0.2.1 (#7)'
no_section=$(git rev-parse HEAD)
fails env "${good[@]}" "$release_sh" tag HEAD
said 'CHANGELOG.md has no section for v0.2.1'
git checkout -q main
# pass:tag-unsigned-under-gpgsign — a configuration that signs every tag, and a
# signer that always fails: only --no-sign gets a tag made.
git config tag.gpgsign true
git config gpg.program false
passes env "${good[@]}" "$release_sh" tag "$squash"
said "tagged v0.2.1 on $(git rev-parse --short "$squash")"
git config --unset tag.gpgsign
git config --unset gpg.program
passes test "$(git cat-file -t v0.2.1)" = tag
passes test "$(git rev-parse 'v0.2.1^{commit}')" = "$squash"
git cat-file tag v0.2.1 >"$tmp/tag-object"
passes test "$(grep -c 'BEGIN PGP' "$tmp/tag-object")" = 0
passes test "$(git tag -l --format='%(contents:subject)' v0.2.1)" = 'cahoots 0.2.1'
# The annotation is the tag object's message: everything after its header.
awk 'message { print } !message && $0 == "" { message = 1 }' "$tmp/tag-object" >"$tmp/annotation"
passes cmp "$tmp/section-0.2.1" "$tmp/annotation"
# pass:tag-idempotent
passes env "${good[@]}" "$release_sh" tag "$squash"
said 'already tagged'
# refuse:tag-exists-elsewhere
git checkout -q --detach "$no_section"
fails env "${good[@]}" "$release_sh" tag HEAD
said 'already exists on'
git checkout -q main
# pass:pending-tag-after-tag, pass:plan-after-tag
stdout_is '' "$release_sh" pending-tag
said 'v0.2.1 is tagged'
stdout_is '' "$release_sh" plan
said 'nothing releasable since v0.2.1'
# pass:commit-nothing
git checkout -q -B release-pr main
passes "$release_sh" commit
passes test "$(git rev-parse HEAD)" = "$squash"
git checkout -q main

# pass:commit-fragment — the notes fragment is the section, and the release
# commit spends it. Written UNDER the template comment, as a real fragment is:
# the comment is scaffolding and must not reach the notes.
mkdir -p .release-notes
printf '<!-- What shipped, in prose.\n     Second comment line. -->\n\nA short title\n\nProse the fragment carried.\n' >.release-notes/next.md
git add -A
git commit -qm 'feat(gate): something worth a minor'
git checkout -q -B release-pr main
passes "$release_sh" commit
passes test "$(git log -1 --format=%s)" = 'release: v0.3.0'
section_of 0.3.0 >"$tmp/written"
passes test "$(sed -n 1p "$tmp/written")" = 'A short title'
passes grep -qxF 'Prose the fragment carried.' "$tmp/written"
passes test "$(grep -c -e '<!--' -e 'comment line' "$tmp/written")" = 0
passes grep -qF '<!--' .release-notes/next.md
passes test "$(grep -c 'Prose the fragment carried' .release-notes/next.md)" = 0
# pass:changelog-grows-at-top — and keeps what was there.
passes test "$(grep -m1 '^## v' CHANGELOG.md | cut -c1-9)" = '## v0.3.0'
passes grep -q '^## v0.2.1 — ' CHANGELOG.md
passes test "$(grep -c '^# Changelog$' CHANGELOG.md)" = 1
release_pr=$(git rev-parse HEAD)
git checkout -q main

# ── pr-title.sh ─────────────────────────────────────────────────────────────
# Each case is GitHub's merge ref for a PR: the branch merged into main, with
# HEAD detached on the merge commit.
pr_merge() { # <commit>
    git checkout -q --detach main
    git merge -q --no-ff --no-edit "$1"
}
repo_name=avihut/cahoots
pr_title() { # <title> <head ref> <head repo> <author>
    env PR_TITLE="$1" PR_HEAD_REF="$2" PR_HEAD_REPO="$3" PR_BASE_REPO="$repo_name" PR_AUTHOR="$4" \
        "$scripts/pr-title.sh"
}
git checkout -q -b feat/x main
printf 'fn main() { println!("x"); }\n' >src/main.rs
git commit -qam 'feat(cli): x'
feature=$(git rev-parse HEAD)
git checkout -q main

pr_merge "$feature"
# pass:conventional, refuse:no-type
passes pr_title 'feat(cli): a feature' feat/x "$repo_name" avihut
said 'pr-title: ok'
fails pr_title 'a title with no type' feat/x "$repo_name" avihut
# refuse:release-title-other-branch
fails pr_title 'release: v0.3.0' feat/x "$repo_name" avihut
said "a release title comes only from the release workflow's release-pr"
fails pr_title 'release(cli): v0.3.0' feat/x "$repo_name" avihut
# refuse:release-bang-title — `release!:` is a valid conventional subject, and
# a release title all the same.
fails pr_title 'release!: v0.3.0' feat/x "$repo_name" avihut
said "a release title comes only from the release workflow's release-pr"
# refuse:release-no-move — the release-pr name on a diff that moves nothing.
fails pr_title 'release: v0.2.1' release-pr "$repo_name" "$bot"
said 'does not move the version'

pr_merge "$release_pr"
# pass:release-pr — the release workflow's own commit.
passes pr_title 'release: v0.3.0' release-pr "$repo_name" "$bot"
said 'pr-title: ok'
# refuse:release-title-fork — a fork's branch of that name; a deleted fork too.
fails pr_title 'release: v0.3.0' release-pr someone/cahoots "$bot"
said "a release title comes only from the release workflow's release-pr"
fails pr_title 'release: v0.3.0' release-pr '' "$bot"
# refuse:release-pr-wrong-author
fails pr_title 'release: v0.3.0' release-pr "$repo_name" avihut
said 'the release PR is opened by the release bot'
# refuse:release-pr-non-release-title
fails pr_title 'feat(cli): something else' release-pr "$repo_name" "$bot"
said 'release-pr carries only the release'
# refuse:release-title-malformed
fails pr_title 'release: v0.3.0 now' release-pr "$repo_name" "$bot"
said "a release title is exactly 'release: vX.Y.Z'"
# refuse:release-version-mismatch
fails pr_title 'release: v0.9.0' release-pr "$repo_name" "$bot"
said "but Cargo.toml's version is 0.3.0"

# refuse:release-extra-file
git checkout -q --detach "$release_pr"
printf 'fn main() { println!("smuggled"); }\n' >src/main.rs
git commit -qam 'release: v0.3.0'
pr_merge "$(git rev-parse HEAD)"
fails pr_title 'release: v0.3.0' release-pr "$repo_name" "$bot"
said 'not src/main.rs'
# refuse:release-cargo-toml-other-line
git checkout -q --detach "$release_pr"
printf 'tokio = "1"\n' >>Cargo.toml
git commit -qam 'release: v0.3.0'
pr_merge "$(git rev-parse HEAD)"
fails pr_title 'release: v0.3.0' release-pr "$repo_name" "$bot"
said 'not Cargo.toml'
# refuse:non-release-moves-version
git checkout -q -b fix/x main
set_version 0.9.0
git commit -qam 'fix: a version smuggled into a fix'
pr_merge fix/x
fails pr_title 'fix: a quiet fix' fix/x "$repo_name" avihut
said "moves Cargo.toml's version 0.2.1 → 0.9.0"
# refuse:usage-missing-env
exits 2 env -u PR_AUTHOR PR_TITLE='fix: x' PR_HEAD_REF=fix/x PR_HEAD_REPO="$repo_name" PR_BASE_REPO="$repo_name" \
    "$scripts/pr-title.sh"
said 'usage:'
git checkout -q main

# ── release-rulesets-audit.sh ───────────────────────────────────────────────
# The pure checks, on the shipped records and on records changed one way each.
audit="$scripts/release-rulesets-audit.sh"
tags_record="$root/.github/rulesets/release-tags-by-workflow.json"
pr_record="$root/.github/rulesets/release-pr-by-workflow.json"
variant() { # <record> <jq filter> → a changed copy, its path on stdout
    local file
    file="$tmp/ruleset-$checks-$RANDOM.json"
    jq "$2" "$1" >"$file"
    printf '%s\n' "$file"
}
# pass:records — what ships reserves both refs for the app alone.
passes "$audit" coverage tag "$tags_record"
passes "$audit" exclusive tag "$tags_record"
passes "$audit" coverage release-pr "$pr_record"
passes "$audit" exclusive release-pr "$pr_record"
# refuse:usage
exits 2 "$audit" coverage tag
exits 2 "$audit" covers tag "$tags_record"
exits 2 "$audit" coverage main "$tags_record"
# refuse:exclusion-cancels — whole, in part, or by ~ALL; one elsewhere is fine.
fails "$audit" coverage tag "$(variant "$tags_record" '.conditions.ref_name.exclude = ["refs/tags/v*"]')"
said 'its exclusions cancel part of refs/tags/v'
fails "$audit" coverage tag "$(variant "$tags_record" '.conditions.ref_name.exclude = ["refs/tags/v9*"]')"
fails "$audit" coverage tag "$(variant "$tags_record" '.conditions.ref_name.exclude = ["refs/tags/*"]')"
fails "$audit" coverage tag "$(variant "$tags_record" '.conditions.ref_name.exclude = ["~ALL"]')"
passes "$audit" coverage tag "$(variant "$tags_record" '.conditions.ref_name.exclude = ["refs/tags/nightly-*"]')"
fails "$audit" coverage release-pr "$(variant "$pr_record" '.conditions.ref_name.exclude = ["refs/heads/release-pr"]')"
said 'its exclusions cancel part of refs/heads/release-pr'
fails "$audit" coverage release-pr "$(variant "$pr_record" '.conditions.ref_name.exclude = ["refs/heads/release*"]')"
passes "$audit" coverage release-pr "$(variant "$pr_record" '.conditions.ref_name.exclude = ["~DEFAULT_BRANCH"]')"
# refuse:inclusion-too-narrow, and a wider one is fine.
fails "$audit" coverage tag "$(variant "$tags_record" '.conditions.ref_name.include = ["refs/tags/v1*"]')"
said 'it does not include all of refs/tags/v'
passes "$audit" coverage tag "$(variant "$tags_record" '.conditions.ref_name.include = ["~ALL"]')"
# refuse:not-active, refuse:wrong-target, refuse:missing-rule
fails "$audit" coverage tag "$(variant "$tags_record" '.enforcement = "evaluate"')"
said 'it is not active'
fails "$audit" coverage release-pr "$tags_record"
said 'it targets tag, not branch'
fails "$audit" coverage release-pr "$(variant "$pr_record" '.rules |= map(select(.type != "update"))')"
said 'it lacks the rules update'
fails "$audit" coverage tag "$root/.github/rulesets/release-tags.json"
said 'it lacks the rules creation'
# refuse:extra-bypass-actor — an admin beside the app passes coverage, which
# can't see it, and fails exclusivity.
admin='.bypass_actors += [{"actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always"}]'
passes "$audit" coverage tag "$(variant "$tags_record" "$admin")"
fails "$audit" exclusive tag "$(variant "$tags_record" "$admin")"
said 'its bypass list is not exactly the release app'
fails "$audit" exclusive release-pr "$(variant "$pr_record" "$admin")"
fails "$audit" exclusive tag "$(variant "$tags_record" '.bypass_actors[0].actor_id = 1')"
fails "$audit" exclusive tag "$(variant "$tags_record" '.bypass_actors[0].bypass_mode = "pull_request"')"
fails "$audit" exclusive tag "$(variant "$tags_record" '.bypass_actors = []')"
# refuse:bypass-list-unseen — what a token without ruleset write access gets:
# coverage holds, exclusivity is unknown, so it fails.
passes "$audit" coverage tag "$(variant "$tags_record" 'del(.bypass_actors)')"
fails "$audit" exclusive tag "$(variant "$tags_record" 'del(.bypass_actors)')"
said 'its bypass list is not visible to these credentials'
# Extra fields GitHub adds to a bypass entry change nothing.
passes "$audit" exclusive tag "$(variant "$tags_record" '.bypass_actors[0].node_id = "x"')"
# refuse:empty-response, refuse:blank-response — zero JSON values is no
# evidence, in either mode; nor is anything but exactly one object.
: >"$tmp/empty.json"
printf ' \n\t\n' >"$tmp/blank.json"
printf '[]\n' >"$tmp/array.json"
printf 'not json\n' >"$tmp/not-json.json"
cat "$tags_record" "$tags_record" >"$tmp/two.json"
for mode in coverage exclusive; do
    for kind in tag release-pr; do
        fails "$audit" "$mode" "$kind" "$tmp/empty.json"
        said 'not exactly one ruleset object (0 JSON values)'
        fails "$audit" "$mode" "$kind" "$tmp/blank.json"
        said 'not exactly one ruleset object (0 JSON values)'
    done
    fails "$audit" "$mode" tag "$tmp/array.json"
    fails "$audit" "$mode" tag "$tmp/two.json"
    fails "$audit" "$mode" tag "$tmp/not-json.json"
done

# The complete audit, with gh replaced by canned responses: the list names
# both rulesets, and each detail response is a file in $FAKE_GH.
mkdir -p "$tmp/fake-gh-bin" "$tmp/fake-gh"
cat >"$tmp/fake-gh-bin/gh" <<'GH'
#!/bin/sh
[ "$1" = api ] || exit 2
case $2 in
repos/avihut/cahoots/rulesets\?*) cat "$FAKE_GH/list.json" ;;
repos/avihut/cahoots/rulesets/*) cat "$FAKE_GH/${2##*/}.json" ;;
*) echo "fake gh: no response for $2" >&2; exit 1 ;;
esac
GH
chmod +x "$tmp/fake-gh-bin/gh"
jq -n --slurpfile t "$tags_record" --slurpfile p "$pr_record" \
    '[{id: 11, name: $t[0].name}, {id: 12, name: $p[0].name}, {id: 13, name: "main: integrity"}]' >"$tmp/fake-gh/list.json"
live_audit() { env PATH="$tmp/fake-gh-bin:$PATH" FAKE_GH="$tmp/fake-gh" "$audit"; }
# pass:audit-live — the records as GitHub would return them.
cp "$tags_record" "$tmp/fake-gh/11.json"
cp "$pr_record" "$tmp/fake-gh/12.json"
passes live_audit
said 'both release rulesets hold'
# refuse:audit-empty-details, refuse:audit-blank-details
: >"$tmp/fake-gh/11.json"
: >"$tmp/fake-gh/12.json"
fails live_audit
said 'not exactly one ruleset object'
printf '\n  \n' >"$tmp/fake-gh/11.json"
cp "$pr_record" "$tmp/fake-gh/12.json"
fails live_audit
said 'not exactly one ruleset object'
# refuse:audit-extra-actor, refuse:audit-unseen-bypass, refuse:audit-missing
cp "$tags_record" "$tmp/fake-gh/11.json"
jq "$admin" "$pr_record" >"$tmp/fake-gh/12.json"
fails live_audit
said 'its bypass list is not exactly the release app'
jq 'del(.bypass_actors)' "$tags_record" >"$tmp/fake-gh/11.json"
cp "$pr_record" "$tmp/fake-gh/12.json"
fails live_audit
said 'not visible to these credentials'
cp "$tags_record" "$tmp/fake-gh/11.json"
jq 'map(select(.id != 12))' "$tmp/fake-gh/list.json" >"$tmp/fake-gh/short.json"
mv "$tmp/fake-gh/short.json" "$tmp/fake-gh/list.json"
fails live_audit
said "has no single ruleset named"
: >"$tmp/fake-gh/list.json"
fails live_audit

# ── formula.sh ──────────────────────────────────────────────────────────────
# A formula written the way dist 0.30 writes one (platform branches, the alias
# table, the install method), filled in from this repository's own Cargo.toml,
# which the check reads too; no dist, no network.
cargo_field() { sed -n "s/^$1 = \"\(.*\)\"\$/\1/p" "$root/Cargo.toml" | head -1; }
formula="$tmp/cahoots.rb"
release_url="$(cargo_field repository)/releases/download/v$(cargo_field version)"
sum=$(printf '%064d' 0)
cat >"$formula" <<RUBY
class Cahoots < Formula
  desc "$(cargo_field description)"
  homepage "$(cargo_field homepage)"
  version "$(cargo_field version)"
  if OS.mac?
    if Hardware::CPU.arm?
      url "$release_url/cahoots-aarch64-apple-darwin.tar.xz"
      sha256 "$sum"
    end
    if Hardware::CPU.intel?
      url "$release_url/cahoots-x86_64-apple-darwin.tar.xz"
      sha256 "$sum"
    end
  end
  if OS.linux?
    if Hardware::CPU.arm?
      url "$release_url/cahoots-aarch64-unknown-linux-gnu.tar.xz"
      sha256 "$sum"
    end
    if Hardware::CPU.intel?
      url "$release_url/cahoots-x86_64-unknown-linux-gnu.tar.xz"
      sha256 "$sum"
    end
  end
  license any_of: ["MIT", "Apache-2.0"]

  BINARY_ALIASES = {
    "aarch64-apple-darwin": {},
    "aarch64-unknown-linux-gnu": {},
    "x86_64-apple-darwin": {},
    "x86_64-unknown-linux-gnu": {}
  }

  def target_triple
    cpu = Hardware::CPU.arm? ? "aarch64" : "x86_64"
    os = OS.mac? ? "apple-darwin" : "unknown-linux-gnu"

    "#{cpu}-#{os}"
  end

  def install_binary_aliases!
    BINARY_ALIASES[target_triple.to_sym].each do |source, dests|
      dests.each do |dest|
        bin.install_symlink bin/source.to_s => dest
      end
    end
  end

  def install
    if OS.mac? && Hardware::CPU.arm?
      bin.install "cahoots"
    end
    if OS.mac? && Hardware::CPU.intel?
      bin.install "cahoots"
    end
    if OS.linux? && Hardware::CPU.arm?
      bin.install "cahoots"
    end
    if OS.linux? && Hardware::CPU.intel?
      bin.install "cahoots"
    end

    install_binary_aliases!

    # Homebrew will automatically install these, so we don't need to do that
    doc_files = Dir["README.*", "readme.*", "LICENSE", "LICENSE.*", "CHANGELOG.*"]
    leftover_contents = Dir["*"] - doc_files

    pkgshare.install(*leftover_contents) unless leftover_contents.empty?
  end
end
RUBY
passes "$scripts/formula.sh" --check "$formula"
fails "$scripts/formula.sh" --check "$tmp/no-such-formula.rb"
fails "$scripts/formula.sh" --check
# Each thing the check reads, taken out of (or changed in) an otherwise good
# formula. Real files, not process substitution: the check reads one many times.
broken="$tmp/broken.rb"
refuses_formula() { # <what the refusal says> <command that rewrites a file…>
    local says=$1
    shift
    "$@" "$formula" >"$broken"
    fails "$scripts/formula.sh" --check "$broken"
    grep -q -- "$says" "$out" || {
        cat "$out" >&2
        echo "test-hooks: formula.sh refused, but not over: $says" >&2
        exit 1
    }
}
refuses_formula 'desc' sed 's/^  desc .*/  desc "something else"/'
refuses_formula 'Apache-2.0' sed 's/"MIT", "Apache-2.0"/"MIT"/'
# The archive branches: one gone, a checksum gone, the two CPUs swapped (here
# and in the install branches alike), an extra archive.
refuses_formula 'archive for each platform' awk '/x86_64-apple-darwin.tar.xz/{getline;next}1'
refuses_formula 'archive for each platform' awk '/^      sha256 /&&!done{done=1;next}1'
refuses_formula 'archive for each platform' sed '1,/^  license /{s/CPU\.arm?/CPU.@@?/;s/CPU\.intel?/CPU.arm?/;s/CPU\.@@?/CPU.intel?/;}'
refuses_formula 'binary installed on each platform' sed '/^  def install$/,/install_binary_aliases!/{s/CPU\.arm?/CPU.@@?/;s/CPU\.intel?/CPU.arm?/;s/CPU\.@@?/CPU.intel?/;}'
refuses_formula 'archive for each platform' sed '/^  license /i\
  if OS.linux?\
    url "https://example.com/extra.tar.xz"\
  end'
# The binary: the wrong name, one install more (inside the method and out of
# it), a platform left without one, an alias for another name.
refuses_formula 'binary installed on each platform' awk '/bin.install "cahoots"/&&!done{done=1;sub(/cahoots/,"other")}1'
refuses_formula 'binary installed on each platform' sed '/^    install_binary_aliases!$/i\
    bin.install "wrong"'
refuses_formula 'exactly four bin.install' sed '/^    install_binary_aliases!$/a\
    bin.install "wrong"'
refuses_formula 'binary installed on each platform' awk '/bin.install "cahoots"/&&!done{done=1;next}1'
refuses_formula 'BINARY_ALIASES should be empty' sed 's/^    "x86_64-apple-darwin": {},/    "x86_64-apple-darwin": { cahoots: %w[extra] },/'

cd "$root"
echo "test-hooks: $checks checks passed"
