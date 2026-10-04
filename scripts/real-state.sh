#!/usr/bin/env bash
# The real-state tripwire. The suite must never touch what a person actually
# has: their cahoots config, state and eval suite, or the files `cahoots install` puts in
# their agent homes. Throwaway directories are what the tests are GIVEN; this
# is what proves they stayed in them.
#
#   scripts/real-state.sh fingerprint          names + checksums of the real paths
#   scripts/real-state.sh project              the repository whose builds never count as real
#   scripts/real-state.sh guard <command…>     run the command; fail if they changed
#
# (2026-09-20: a unit test ran a real `install` into a real ~/.claude and
# ~/.codex. The code now refuses that three ways; this is the fourth, and the
# only one that does not depend on the code being right.)
#
# Two sets of paths, because cahoots is in real use on the machines that run
# the suite (#75: a real review run by another agent failed a suite twice):
#
# - STRICT: any change fails. Config, the eval suite, what `install` puts in
#   the agent homes, and everything in the state directory but the activity
#   below — what only `install`, `refresh` and `settings` write, never a run.
# - ACTIVITY: what a real run and the sweep of a client verb write — the run
#   directories, the slots, the no-hooks directory, the placements, and the
#   history, which they only ever append to. A change here fails too, unless
#   a REAL cahoots was seen running meanwhile and the history only grew.
#
# A real cahoots is a process whose argv[0] is named `cahoots` and is not a
# build: not under a `target/` directory, nor anywhere in this repository's
# project directory (every worktree of it), so neither this suite's binary nor
# the supervisors another worktree's suite leaves behind counts. While the
# command runs, a `cahoots` that refuses comes first on its PATH: a suite that
# ran the installed cahoots by name would be seen as one, so it fails instead,
# even if the test swallowed the refusal.
#
# What this leaves: a test-shaped write to the activity during a coincident
# real run passes — a test does the same thing every time, and the next run
# without one catches it. And a real verb that comes and goes between two
# looks (`review`, `outcome`, a `status` sweep) is not seen: that fails, and
# says to rerun.
#
# For test-hooks.sh only: REAL_STATE_HOME overrides the home that is watched,
# and then a real cahoots must also live under it; REAL_STATE_OWN_ROOT
# overrides the project directory.
set -euo pipefail

home="${REAL_STATE_HOME:-$HOME}"
state="$home/.local/state/cahoots"
history="$state/history.jsonl"
activity=(runs slots no-hooks worktrees history.jsonl)
strict=(
    "$home/.config/cahoots"
    "$home/.local/share/cahoots"
    "$home/.agents/skills/cahoots"
    "$home/.claude/skills/cahoots"
    "$home/.claude/agents/cahoots-delegate.md"
    "$home/.codex/skills/cahoots"
    "$home/.codex/agents/cahoots-delegate.toml"
)

# Names catch what appeared or vanished; checksums, what changed.
print_tree() {
    local path=$1
    shift
    if [ -e "$path" ] || [ -L "$path" ]; then
        find "$path" "$@" -print | LC_ALL=C sort
        find "$path" "$@" -type f -exec cksum {} + | LC_ALL=C sort
    else
        echo "absent $path"
    fi
}

fingerprint_strict() {
    local path name
    local prune=()
    for name in "${activity[@]}"; do
        prune+=(-path "$state/$name" -prune -o)
    done
    # One subagent per kind of task, expanded on every call so that one which
    # appears shows up. A pattern that matches nothing stays as written and
    # fingerprints as absent.
    local kinds=("$home/.claude/agents/cahoots-kind-"*.md "$home/.codex/agents/cahoots-kind-"*.toml)
    for path in "${strict[@]}" "${kinds[@]}"; do
        print_tree "$path"
    done
    # The state directory but its activity: an entry nobody knows yet is
    # strict. An absent one reads as an empty one, since a first run creates
    # it; anything but a directory there is strict too.
    if [ -d "$state" ] && [ ! -L "$state" ]; then
        print_tree "$state" -mindepth 1 "${prune[@]}"
    elif [ -e "$state" ] || [ -L "$state" ]; then
        echo "not a directory: $state"
        print_tree "$state"
    fi
}

fingerprint_activity() {
    local name
    for name in "${activity[@]}"; do
        print_tree "$state/$name"
    done
}

# What the history is, to tell an append from a rewrite: `absent`, `other`
# (not a regular file), or `file <crc> <bytes>` — both numbers from ONE read,
# so a real run appending meanwhile cannot set them apart.
history_mark() {
    local sum
    if [ -L "$history" ]; then
        echo other
    elif [ -f "$history" ]; then
        sum=$(cksum <"$history") || { echo other; return; }
        echo "file $sum"
    elif [ -e "$history" ]; then
        echo other
    else
        echo absent
    fi
}

# Whether the history is still, or now, a regular file that starts with
# exactly what it held before. Absent may stay absent.
history_appended() {
    local kind crc bytes
    read -r kind crc bytes <<<"$1"
    case "$kind" in
    absent)
        if [ ! -e "$history" ] && [ ! -L "$history" ]; then
            return 0
        fi
        [ -f "$history" ] && [ ! -L "$history" ]
        ;;
    file)
        [ -f "$history" ] && [ ! -L "$history" ] || return 1
        [ "$(head -c "$bytes" "$history" | cksum)" = "$crc $bytes" ]
        ;;
    *)
        return 1
        ;;
    esac
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
    fingerprint_strict
    fingerprint_activity
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

    strict_before=$(fingerprint_strict)
    activity_before=$(fingerprint_activity)
    history_before=$(history_mark)
    look >"$tmp/seen"
    (while :; do look; sleep 0.1; done) >>"$tmp/seen" &
    sampler=$!

    status=0
    PATH="$tmp/bin:$PATH" "$@" || status=$?

    kill "$sampler" 2>/dev/null || true
    wait "$sampler" 2>/dev/null || true
    sampler=""
    look >>"$tmp/seen"
    strict_after=$(fingerprint_strict)
    activity_after=$(fingerprint_activity)
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
    if [ "$strict_before" != "$strict_after" ]; then
        tripped=1
        {
            echo
            echo "✗ real-state: the command changed REAL cahoots state under $home"
            diff <(echo "$strict_before") <(echo "$strict_after") || true
            echo "  No run writes these, so a real cahoots meanwhile is no excuse."
            echo "  A test must only ever touch the throwaway directories it is given."
        } >&2
    fi
    if [ "$activity_before" != "$activity_after" ]; then
        if ! history_appended "$history_before"; then
            tripped=1
            {
                echo
                echo "✗ real-state: the command REWROTE the real history at $history, or removed or replaced it"
                echo "  cahoots only ever appends to it, so nothing real did this."
                echo "  A test must only ever touch the throwaway directories it is given."
            } >&2
        elif [ -z "$seen" ]; then
            tripped=1
            {
                echo
                echo "✗ real-state: the command changed REAL cahoots run state under $home"
                diff <(echo "$activity_before") <(echo "$activity_after") || true
                echo "  No real cahoots was seen running meanwhile. If one ran only briefly"
                echo "  (\`review\`, \`outcome\`, a \`status\` sweep), rerun; otherwise a test"
                echo "  touched more than the throwaway directories it is given."
            } >&2
        else
            {
                echo
                echo "real-state: run state under $home changed while a real cahoots ran:"
                while read -r line; do echo "  $line"; done <<<"$seen"
                diff <(echo "$activity_before") <(echo "$activity_after") | grep '^[<>]' | sed 's/^/  /' || true
                echo "  Excused: a run writes these. Everything else stayed exactly as it was."
            } >&2
        fi
    fi
    [ "$tripped" = 0 ] || exit 1
    exit "$status"
    ;;
*)
    echo "usage: real-state.sh fingerprint | project | guard <command…>" >&2
    exit 2
    ;;
esac
