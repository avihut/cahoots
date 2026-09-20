#!/usr/bin/env bash
# The real-state tripwire. The suite must never touch what a person actually
# has: their cahoots config and state, or the files `cahoots install` puts in
# their agent homes. Throwaway directories are what the tests are GIVEN; this
# is what proves they stayed in them.
#
#   scripts/real-state.sh fingerprint          names + checksums of the real paths
#   scripts/real-state.sh guard <command…>     run the command; fail if they changed
#
# (2026-09-20: a unit test ran a real `install` into a real ~/.claude and
# ~/.codex. The code now refuses that three ways; this is the fourth, and the
# only one that does not depend on the code being right.)
#
# REAL_STATE_HOME overrides the home that is watched — for test-hooks.sh only.
set -euo pipefail

home="${REAL_STATE_HOME:-$HOME}"
watched=(
    "$home/.config/cahoots"
    "$home/.local/state/cahoots"
    "$home/.agents/skills/cahoots"
    "$home/.claude/skills/cahoots"
    "$home/.claude/agents/cahoots-delegate.md"
    "$home/.codex/skills/cahoots"
    "$home/.codex/agents/cahoots-delegate.toml"
)

fingerprint() {
    local path
    for path in "${watched[@]}"; do
        if [ -e "$path" ] || [ -L "$path" ]; then
            # Names catch what appeared or vanished; checksums, what changed.
            find "$path" -print | LC_ALL=C sort
            find "$path" -type f -exec cksum {} + | LC_ALL=C sort
        else
            echo "absent $path"
        fi
    done
}

case "${1:-}" in
fingerprint)
    fingerprint
    ;;
guard)
    shift
    [ $# -gt 0 ] || { echo "usage: real-state.sh guard <command…>" >&2; exit 2; }
    before=$(fingerprint)
    status=0
    "$@" || status=$?
    after=$(fingerprint)
    if [ "$before" != "$after" ]; then
        {
            echo
            echo "✗ real-state: the command changed REAL cahoots state under $home"
            diff <(echo "$before") <(echo "$after") || true
            echo "  A test must only ever touch the throwaway directories it is given."
        } >&2
        exit 1
    fi
    exit "$status"
    ;;
*)
    echo "usage: real-state.sh fingerprint | guard <command…>" >&2
    exit 2
    ;;
esac
