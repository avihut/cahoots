#!/usr/bin/env bash
# Runs a command and fails if it left a process running from this worktree's
# build directory (target/), even when it exited 0. A test that drives
# `cahoots settings` at a terminal and then ends used to leave it behind for
# days, reparented to PID 1 (#85). The command's own failure passes through.
#
#   scripts/no-strays.sh <command> [args…]
#
# What is looked at: processes whose executable is a file under the build
# directory ($CARGO_TARGET_DIR, else <repository>/target; a relative one is
# taken from the directory this runs in, where cargo will run too) and that
# were not already running when the command began, so one an earlier run left
# behind is not blamed on this one. The executable is the real file the
# process runs, not how it was invoked (/proc/<pid>/exe, or lsof where there
# is no /proc), so `./cahoots` counts like its absolute path. A detached
# supervisor another test started may still be winding down when the command
# returns, so a stray gets a grace period to exit (10 seconds) before it
# fails. This reports; it never kills anything.
#
# A process list that cannot be read is a refusal, never an empty list: at the
# start (the command is not run), at the end and while waiting.
#
# For test-hooks.sh only: NO_STRAYS_TARGET overrides the build directory,
# NO_STRAYS_GRACE the grace period in seconds, and NO_STRAYS_LOOKUP a program
# that stands in for lsof (and is used even where /proc exists).
set -uo pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
target="${NO_STRAYS_TARGET:-${CARGO_TARGET_DIR:-$root/target}}"
grace="${NO_STRAYS_GRACE:-10}"

refuse() {
    echo "no-strays: $1" >&2
    exit 1
}

# The build directory as the real path processes run from. One that does not
# exist yet has nothing running from it.
build_dir() {
    local dir="${target%/}"
    case "$dir" in
        /*) ;;
        *) dir="$PWD/$dir" ;;
    esac
    if [ -d "$dir" ]; then
        (cd "$dir" && pwd -P)
    else
        echo "$dir"
    fi
}

# `pid executable` for every process whose executable can be read. Fails, with
# nothing printed, when none can: the table was not read.
executables() {
    local out="" exe dir
    if [ -z "${NO_STRAYS_LOOKUP:-}" ] && [ -d /proc/self ]; then
        for dir in /proc/[0-9]*; do
            exe=$(readlink "$dir/exe" 2>/dev/null) || continue
            out+="${dir#/proc/} $exe"$'\n'
        done
    else
        # lsof -Fpn: a `p<pid>` line, then the `n<path>` of each text file the
        # process maps, its executable first.
        out=$("${NO_STRAYS_LOOKUP:-lsof}" -nP -a -d txt -Fpn 2>/dev/null |
            awk '/^p/ { pid = substr($0, 2); first = 1; next }
                 /^n/ && first { print pid " " substr($0, 2); first = 0 }') || return 1
    fi
    [ -n "$out" ] || return 1
    printf '%s' "$out"
}

# `pid executable` for the processes running from the build directory.
snapshot() {
    local all dir
    all=$(executables) || return 1
    dir=$(build_dir)
    awk -v t="$dir/" '{ pid = $1; sub(/^[0-9]+ /, "") } index($0, t) == 1 { print pid " " $0 }' <<<"$all"
}

before=$(snapshot) || refuse "the process list cannot be read, so nothing was run (needs /proc or lsof)"

"$@"
status=$?

# New since the command began, one `pid executable` per line.
strays() {
    local now
    now=$(snapshot) || return 1
    grep -vxF -f <(printf '%s\n' "$before") <<<"$now" || true
}

left=$(strays) || refuse "the process list cannot be read after the command, so it is not known whether it left anything running"
waited=0
while [ -n "$left" ] && [ "$waited" -lt "$grace" ]; do
    sleep 1
    waited=$((waited + 1))
    left=$(strays) || refuse "the process list cannot be read while waiting for what the command left to exit"
done

if [ -n "$left" ]; then
    echo "no-strays: the command left processes running from $(build_dir)/ (still alive after ${grace}s):" >&2
    while IFS= read -r line; do echo "  $line" >&2; done <<<"$left"
    echo "no-strays: a terminal session must end its command and the command's group (tests/common/mod.rs: AtTerminal)" >&2
    exit 1
fi
exit "$status"
