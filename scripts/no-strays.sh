#!/usr/bin/env bash
# Runs a command and fails if it left a process running from this worktree's
# build directory (target/), even when it exited 0. A test that drives
# `cahoots settings` at a terminal and then ends used to leave it behind for
# days, reparented to PID 1 (#85). The command's own failure passes through.
#
#   scripts/no-strays.sh <command> [args…]
#
# What is looked at: processes whose program is a file under the build
# directory ($CARGO_TARGET_DIR, else <repository>/target) and that were not
# already running when the command began, so one an earlier run left behind is
# not blamed on this one. A detached supervisor another test started may still
# be winding down when the command returns, so a stray gets a grace period to
# exit (10 seconds) before it fails. This reports; it never kills anything.
#
# For test-hooks.sh only: NO_STRAYS_TARGET overrides the build directory and
# NO_STRAYS_GRACE the grace period, in seconds.
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
target="${NO_STRAYS_TARGET:-${CARGO_TARGET_DIR:-$root/target}}"
target="${target%/}"
grace="${NO_STRAYS_GRACE:-10}"

# `pid args` for every process whose program lives under the build directory.
under_target() {
    ps -A -o pid=,args= 2>/dev/null | awk -v t="$target/" '
        { pid = $1; sub(/^[ ]*[0-9]+[ ]+/, "") }
        index($0, t) == 1 { print pid " " $0 }'
}

before=$(under_target | awk '{ print $1 }')

"$@"
status=$?

# New since the command began, one `pid args` per line.
strays() {
    under_target | BEFORE="$before" awk '
        BEGIN { n = split(ENVIRON["BEFORE"], b, "\n"); for (i = 1; i <= n; i++) seen[b[i]] = 1 }
        !($1 in seen)'
}

left=$(strays)
waited=0
while [ -n "$left" ] && [ "$waited" -lt "$grace" ]; do
    sleep 1
    waited=$((waited + 1))
    left=$(strays)
done

if [ -n "$left" ]; then
    echo "no-strays: the command left processes running from ${target}/ (still alive after ${grace}s):" >&2
    while IFS= read -r line; do echo "  $line" >&2; done <<<"$left"
    echo "no-strays: a terminal session must end its command and the command's group (tests/common/mod.rs: AtTerminal)" >&2
    exit 1
fi
exit "$status"
