#!/usr/bin/env bash
# SPIKE stand-in for the cahoots binary — a few lines of shell that do what the
# broker will do, so the risky questions get answered by the real harnesses.
set -uo pipefail
SPIKE="${SPIKE_DIR:?set SPIKE_DIR to a scratch directory}"
STATE="$HOME/.local/state/cahoots-spike"
stamp() { date +%H:%M:%S; }
log() { echo "$(stamp) [$$] $*" >>"$SPIKE/out/standin.log" 2>/dev/null || true; }
who() {
    echo "caller-env: CLAUDECODE=${CLAUDECODE:-} CODEX_THREAD_ID=${CODEX_THREAD_ID:+set} CODEX_SANDBOX=${CODEX_SANDBOX:-} CODEX_SANDBOX_NETWORK_DISABLED=${CODEX_SANDBOX_NETWORK_DISABLED:-} CAHOOTS_DEPTH=${CAHOOTS_DEPTH:-}"
}
verb=${1:-}; shift || true
log "verb=$verb args=$* $(who)"
case "$verb" in
run)
    target=${1:?target}; shift
    if [ "${1:-}" = "--brief" ]; then brief=$(cat "$2"); else brief=$(cat); fi
    who
    echo "brief-bytes: ${#brief}"
    export CAHOOTS_DEPTH=$(( ${CAHOOTS_DEPTH:-0} + 1 ))
    case "$target" in
    codex)
        printf '%s' "$brief" | codex exec --json --sandbox read-only --skip-git-repo-check -m gpt-5.6-luna -c model_reasoning_effort=low - 2>"$SPIKE/out/nested-codex.err" | tee "$SPIKE/out/nested-codex.jsonl" | grep -E '"agent_message"|turn.failed|"error"' | head -5
        echo "nested-exit: ${PIPESTATUS[1]}"
        ;;
    claude)
        printf '%s' "$brief" | claude -p --model haiku --output-format json --permission-mode default 2>"$SPIKE/out/nested-claude.err" | tee "$SPIKE/out/nested-claude.json" | python3 -c 'import sys,json
try:
    d=json.load(sys.stdin); print("result:", d.get("result")); print("is_error:", d.get("is_error"), "session:", bool(d.get("session_id")))
except Exception as e: print("unparseable nested output:", e)'
        echo "nested-exit: ${PIPESTATUS[1]}"
        ;;
    esac
    ;;
hang)
    # S2: detach a supervisor-shaped process (new session, double fork), then
    # block longer than the caller's tool-call timeout.
    marker="$SPIKE/out/hang-$$.marker"
    python3 - "$marker" <<'PY'
import os, sys, time
marker = sys.argv[1]
if os.fork() == 0:
    os.setsid()
    if os.fork() == 0:
        for fd in (0, 1, 2):
            try: os.close(fd)
            except OSError: pass
        time.sleep(25)
        open(marker, "w").write("supervisor survived, pid %d ppid %d\n" % (os.getpid(), os.getppid()))
    os._exit(0)
PY
    echo "detached; marker=$marker"
    log "hang: detached, now blocking 90s"
    sleep 90
    echo "hang returned normally (the tool call was NOT killed)"
    ;;
probe)
    # S6: what can a (possibly sandboxed) caller do to the state dir?
    who
    mkdir -p "$STATE" 2>&1 && echo "mkdir state: ok" || echo "mkdir state: REFUSED"
    (echo x >"$STATE/probe-$$" ) 2>&1 && echo "write state: ok" || echo "write state: REFUSED"
    cat "$STATE/seed" 2>&1 | sed 's/^/read state: /'
    (curl -sS -m 5 -o /dev/null -w 'network: http %{http_code}\n' https://api.github.com/zen) 2>&1 | tail -1
    ;;
install)
    echo "install ran — this verb has no allow rule, so a gated caller should never get here"
    ;;
*) echo "unknown verb $verb"; exit 2 ;;
esac
