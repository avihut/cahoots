#!/usr/bin/env bash
# Runs a command and fails if it printed a warning, even when it exited 0.
# `-D warnings` only covers what the COMPILER diagnoses; the linker
# (`ld: warning:`) and cargo itself warn outside its reach, and a gate that
# says "builds without warnings" means those too. The command's own failure
# passes through (pipefail).
#
#   scripts/no-warnings.sh <command> [args…]
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$root/.cache"
log=$(mktemp "$root/.cache/no-warnings.XXXXXX")
trap 'rm -f "$log"' EXIT

"$@" 2>&1 | tee "$log"
if grep -Eiq '(^|[[:space:]])warning:' "$log"; then
    echo "no-warnings: the command succeeded but printed warnings — fix the diagnostics above" >&2
    exit 1
fi
