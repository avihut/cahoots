#!/usr/bin/env bash
# The real-state tripwire. The suite must never touch what a person actually
# has: their cahoots config, state and eval suite, or the files `cahoots install` puts in
# their agent homes. Throwaway directories are what the tests are GIVEN; this
# is what proves they stayed in them.
#
#   scripts/real-state.sh fingerprint          names + checksums of the real paths
#   scripts/real-state.sh project              the repository whose builds are never a real cahoots
#   scripts/real-state.sh guard <command…>     run the command; fail if they changed
#
# (2026-09-20: a unit test ran a real `install` into a real ~/.claude and
# ~/.codex. The code now refuses that three ways; this is the fourth, and the
# only one that does not depend on the code being right.)
#
# It is strict: ANY change fails, whoever made it. cahoots is in real use on
# the machines that run the suite, so a real run elsewhere meanwhile can trip
# it too (#75) — and then the answer is to rerun, not to excuse. To tell the
# two apart, a failure names every changed path, says whether it is one a run
# writes or one only `install`, `refresh` and `settings` do, and says whether
# a real cahoots was seen running meanwhile. Seen or not, the guard fails.
#
# A real cahoots is a process whose argv[0] is named `cahoots` and is not a
# build: not under a `target/` directory, nor anywhere in this repository's
# project directory (every worktree of it), so neither this suite's binary nor
# the supervisors another worktree's suite leaves behind counts. And while the
# command runs, a `cahoots` that refuses comes first on its PATH: a suite that
# runs the installed cahoots by name fails, even if the test swallowed the
# refusal.
#
# For test-hooks.sh only: REAL_STATE_HOME overrides the home that is watched,
# and then a real cahoots must also live under it; REAL_STATE_OWN_ROOT
# overrides the project directory.
set -euo pipefail

home="${REAL_STATE_HOME:-$HOME}"
state="$home/.local/state/cahoots"
watched=(
    "$home/.config/cahoots"
    "$state"
    "$home/.local/share/cahoots"
    "$home/.agents/skills/cahoots"
    "$home/.claude/skills/cahoots"
    "$home/.claude/agents/cahoots-delegate.md"
    "$home/.codex/skills/cahoots"
    "$home/.codex/agents/cahoots-delegate.toml"
)
# What a run, or the sweep of a client verb, writes. For the words only:
# a change here fails like any other.
run_writes=(runs slots no-hooks worktrees history.jsonl)

fingerprint() {
    local path
    # One subagent per kind of task, expanded on every call so that one which
    # appears shows up. A pattern that matches nothing stays as written and
    # fingerprints as absent.
    local kinds=("$home/.claude/agents/cahoots-kind-"*.md "$home/.codex/agents/cahoots-kind-"*.toml)
    for path in "${watched[@]}" "${kinds[@]}"; do
        if [ -e "$path" ] || [ -L "$path" ]; then
            # Names catch what appeared or vanished; checksums, what changed.
            find "$path" -print | LC_ALL=C sort
            find "$path" -type f -exec cksum {} + | LC_ALL=C sort
        else
            echo "absent $path"
        fi
    done
}

# The paths two fingerprints differ on, one per line.
changed_paths() {
    diff <(echo "$1") <(echo "$2") | sed -n 's/^[<>] //p' |
        sed -E 's/^(absent |[0-9]+ [0-9]+ )//' | LC_ALL=C sort -u
}

is_run_write() {
    local path=$1 name
    for name in "${run_writes[@]}"; do
        case "$path" in
        "$state/$name" | "$state/$name/"*) return 0 ;;
        esac
    done
    [ "$path" = "$state" ]
}

# The repository this script belongs to — all of its worktrees. A hook
# exports GIT_DIR and friends, relative to where IT started, so they are
# cleared for the lookup; and with no answer the guard refuses, because
# this is what tells this suite's builds from a real cahoots.
project_root() {
    local common
    if [ -n "${REAL_STATE_OWN_ROOT:-}" ]; then
        echo "$REAL_STATE_OWN_ROOT"
        return
    fi
    common=$(
        # shellcheck disable=SC2046 # one variable name per word
        unset $(git rev-parse --local-env-vars)
        git -C "$(dirname "$0")" rev-parse --path-format=absolute --git-common-dir 2>/dev/null
    ) || return 1
    [ -n "$common" ] || return 1
    dirname "$common"
}

# One look at the process table: every real cahoots, as "pid argv0".
look() {
    ps -A -o pid=,args= 2>/dev/null | awk -v own="$own" -v own_p="$own_p" -v under="$under" '
        {
            argv0 = $2
            n = split(argv0, part, "/")
            if (part[n] != "cahoots") next
            if (argv0 ~ /\/target\//) next
            if (index(argv0, own "/") == 1 || index(argv0, own_p "/") == 1) next
            if (under != "" && index(argv0, under "/") != 1) next
            print $1, argv0
        }'
}

case "${1:-}" in
fingerprint)
    fingerprint
    ;;
project)
    project_root || { echo "real-state: cannot find the repository this script belongs to" >&2; exit 2; }
    ;;
guard)
    shift
    [ $# -gt 0 ] || { echo "usage: real-state.sh guard <command…>" >&2; exit 2; }
    own=$(project_root) || {
        echo "✗ real-state: cannot find the repository this script belongs to — refusing, since it is what tells this suite's builds from a real cahoots" >&2
        exit 2
    }
    own_p=$(cd "$own" 2>/dev/null && pwd -P || echo "$own")
    under="${REAL_STATE_HOME:-}"
    tmp=$(mktemp -d "${TMPDIR:-/tmp}/real-state.XXXXXX")
    sampler=""
    trap '[ -z "$sampler" ] || kill "$sampler" 2>/dev/null || true; rm -rf "$tmp"' EXIT

    mkdir "$tmp/bin"
    {
        echo '#!/bin/sh'
        echo "echo 'real-state: the suite may not run an installed cahoots by name' >&2"
        printf ': >%q\n' "$tmp/ran-cahoots"
        echo 'exit 1'
    } >"$tmp/bin/cahoots"
    chmod +x "$tmp/bin/cahoots"

    before=$(fingerprint)
    look >"$tmp/seen"
    (while :; do look; sleep 0.1; done) >>"$tmp/seen" &
    sampler=$!

    status=0
    PATH="$tmp/bin:$PATH" "$@" || status=$?

    kill "$sampler" 2>/dev/null || true
    wait "$sampler" 2>/dev/null || true
    sampler=""
    look >>"$tmp/seen"
    after=$(fingerprint)
    seen=$(LC_ALL=C sort -u "$tmp/seen")

    tripped=0
    if [ -e "$tmp/ran-cahoots" ]; then
        tripped=1
        {
            echo
            echo "✗ real-state: the command ran \`cahoots\` by name — that is the installed one, and its state is real"
            echo "  A test runs the binary it built (CARGO_BIN_EXE_cahoots), on throwaway directories."
        } >&2
    fi
    if [ "$before" != "$after" ]; then
        tripped=1
        run_paths=()
        other_paths=()
        while read -r path; do
            if is_run_write "$path"; then
                run_paths+=("$path")
            else
                other_paths+=("$path")
            fi
        done < <(changed_paths "$before" "$after")
        {
            echo
            echo "✗ real-state: the command changed REAL cahoots state under $home"
            if [ ${#run_paths[@]} -gt 0 ]; then
                echo "  What a run writes:"
                printf '    %s\n' "${run_paths[@]}"
            fi
            if [ ${#other_paths[@]} -gt 0 ]; then
                echo "  What only install, refresh and settings write — never a run:"
                printf '    %s\n' "${other_paths[@]}"
            fi
            if [ -n "$seen" ]; then
                echo "  A real cahoots ran meanwhile; rerun when it is idle:"
                while read -r line; do echo "    $line"; done <<<"$seen"
                if [ ${#other_paths[@]} -gt 0 ]; then
                    echo "  But no run writes the second list: a test likely touched real state."
                fi
            else
                echo "  No real cahoots was seen running meanwhile: a test likely touched real state."
                echo "  A test must only ever touch the throwaway directories it is given."
            fi
        } >&2
    fi
    [ "$tripped" = 0 ] || exit 1
    exit "$status"
    ;;
*)
    echo "usage: real-state.sh fingerprint | project | guard <command…>" >&2
    exit 2
    ;;
esac
