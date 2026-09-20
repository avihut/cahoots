#!/usr/bin/env bash
# LOCAL ONLY — never in CI, never in the gate. One tiny REAL read-only run in
# each direction (Claude Code → Codex, Codex → Claude Code), to prove that the
# command lines cahoots builds are ones the real CLIs accept and that their
# real output parses. It is the one exception to "tests never call a real
# harness", and it spends a little of both plans: two one-word answers on the
# cheapest models at the lowest effort.
#
# It uses a dev build pointed at a throwaway config and state directory, so it
# neither reads nor writes your real cahoots setup.
set -euo pipefail

cd "$(dirname "$0")/.."
cargo build --locked --quiet
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/config" "$tmp/state" "$tmp/home" .cache

cat >"$tmp/config/config.toml" <<'TOML'
schema = 1
[roles.explore]
candidates = [
  { harness = "codex", model = "gpt-5.6-luna", effort = "low" },
  { harness = "claude", model = "haiku", effort = "low" },
]
TOML
printf '{"v":1,"enabled":["claude","codex"]}' >"$tmp/config/enabled.json"
brief=".cache/smoke-brief.md"
printf 'Reply with the single word: pong\n' >"$brief"

status=0
for caller in claude codex; do
    echo "── caller: $caller"
    set +e
    out=$(env -u CLAUDECODE -u CODEX_THREAD_ID -u CODEX_SANDBOX -u CAHOOTS_DEPTH \
        CAHOOTS_CONFIG_DIR="$tmp/config" CAHOOTS_STATE_DIR="$tmp/state" CAHOOTS_HOME_DIR="$tmp/home" \
        target/debug/cahoots run --role explore --caller "$caller" --brief "$brief" --wait 300)
    code=$?
    set -e
    echo "$out"
    if [ "$code" -ne 0 ] || ! grep -qi '"text":"pong' <<<"$out"; then
        echo "smoke: the run for caller $caller did not come back with pong (exit $code)" >&2
        status=1
    fi
done
rm -f "$brief"
[ "$status" -eq 0 ] && echo "smoke: both directions answered"
exit "$status"
