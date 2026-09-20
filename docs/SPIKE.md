# Spike — the real CLIs, before any product code

2026-09-20 · macOS (arm64) · Claude Code 2.1.278 · Codex CLI 0.155.1 ·
Antigravity CLI not installed, so not covered (M6 starts with its own spike).

The design assumes things about how the harnesses behave when one runs
another. This spike tested the assumptions that would be expensive to get
wrong, with a few lines of shell standing in for the broker
(`docs/spike/cahoots-standin.sh`) and a dozen tiny real runs — about $0.10 of
Claude (Haiku) and a handful of low-effort Codex turns.

**Verdict: the design holds. Four findings change M1**, marked ⚠ below.

## S1 — A delegated run in both directions, under per-verb allow rules

| | Claude Code as the caller | Codex as the caller |
|---|---|---|
| Rule | `--allowedTools "Bash(cahoots run:*)"`, `--permission-mode default` | `prefix_rule(pattern=["cahoots","run"], decision="allow")` |
| `cahoots run … --brief <file>` | ran, no prompt | ran **outside the sandbox**, nested Claude answered |
| heredoc brief (`<<'BRIEF'`) | ran — the rule still matches | ⚠ rule does **not** match: ran *inside* seatbelt |
| piped brief (`echo … \| cahoots run`) | ran (`echo` is on Claude's safe list) | ⚠ rule does **not** match: ran *inside* seatbelt |
| a verb with no rule (`cahoots install`) | **denied** (listed in `permission_denials`) | ran, but sandboxed — no approval is asked in `exec` mode |

- ⚠ **The brief is a file argument, never stdin, from a caller's point of
  view.** Codex only matches a prefix rule when the command line parses as a
  plain argv; a heredoc or a pipe falls back to the whole `zsh -lc` string and
  matches nothing. The skill tells every harness to write the brief to a file
  and pass `--brief <path>`. (cahoots itself still hands the brief to the
  *callee* on stdin.)
- Inside Codex's sandbox a nested `claude -p` fails with **"Not logged in"** —
  the keychain is unreachable, as is the network. That is the symptom a user
  sees when the rule is missing, so `run` detects `CODEX_SANDBOX=seatbelt` and
  exits 33 with the rule to add, instead of starting a run that cannot work.
- ⚠ **`CODEX_SANDBOX_NETWORK_DISABLED=1` is not a sandbox signal.** It is set
  even for a command the rule let out of the sandbox. `CODEX_SANDBOX`
  (`seatbelt`, or empty) is the signal.
- ⚠ **Caller detection by environment is ambiguous when harnesses nest.** A
  Codex session started from Claude Code carries both `CLAUDECODE=1` and
  `CODEX_THREAD_ID`. The installed skill therefore passes `--caller <harness>`
  explicitly; the environment is only a fallback, and a conflict is an error,
  not a guess.
- `CAHOOTS_DEPTH`, exported by the stand-in, reached the nested harness's shell
  in both directions.

## S2 — The caller's tool call dies mid-run

`cahoots hang` detached a supervisor-shaped process (double fork, `setsid`)
and then blocked for 90 s; the caller was told to use a 5 s tool timeout.

- **Claude Code** did not kill the command at the timeout — it moved it to the
  background — and killed it when the session ended. The detached process
  survived both, reparented to PID 1.
- **Codex** reported the timeout and "command remained running". The detached
  process survived there too.

The run model — a detached supervisor, a thin `run` client that returns
"not finished" with a run id — is sound in both harnesses.

## S3 — Session ids, signals, exit codes, resume

Each harness was started in its own session and its **process group**
signalled mid-run (`docs/spike/signal_probe.py`).

| | Codex | Claude Code |
|---|---|---|
| Session id known | `thread.started` is the first stdout line, ~0.1 s | preset with `--session-id` |
| SIGINT / SIGTERM | INT: exit **1** after 1.2 s, nothing left in the group | TERM: exit **143** after 0.5 s, nothing left in the group |
| stdout on a killed run | the JSONL so far | **nothing at all** with `--output-format json` |
| SIGKILL | — | exit −9; the session still resumes |
| Resume after the signal | `codex exec resume <id>` works | `claude -p --resume <id>` works, same session id |

- ⚠ **Codex runs its tool commands in their own process group.** Killing
  Codex's group does not reach them: a run that was torn down abruptly left a
  `zsh` loop running under a dead parent. On a graceful SIGINT Codex cleans up
  itself. So the supervisor's ladder is INT → wait → TERM → wait → KILL, and
  before the KILL it snapshots the descendant tree (pid/ppid) and signals
  those too.
- **Claude must be run with `--output-format stream-json --verbose`**, not
  `json`: a killed `json` run prints nothing, so tokens, cost and the final
  text so far would be lost. The stream carries `system` events first
  (including the *user's own hooks* firing in the callee — callees inherit the
  user's Claude Code configuration), then `init`, then messages and a result.
- `codex exec resume` without `-m` resumes on the *default* model and emits an
  `item` of type `error` that is only a warning. Resume passes the recorded
  model, and the parser treats `item.type == "error"` as a note, never as the
  run's failure — `turn.failed` and the exit code are the failure signals.
- Codex prints non-JSON lines on stdout in some cases (`Reading additional
  input from stdin...`): skip what does not parse.

## S4 — The meter: latency and resolution

Agent Usage's Codex snapshot (`fetchedAt`) trailed the latest local Codex run
by about a minute. Across six small Codex runs the weekly meter did not move
from 35%: **an integer percent cannot account for a single small run.** So:

- the *gate* reads the tracker (is there headroom at all?);
- *per-run accounting* — what `report` and the learning statistics use — comes
  from the tokens in the callee's own stream, which is what the built-in
  `ledger` meter counts. A before/after meter reading is recorded, flagged
  `confounded`, and not used for arithmetic.

## S5 — A closed pipe

Not tested against the harnesses (cahoots always reads a callee's output to
the end). For cahoots' own stdout: `println!` panics on EPIPE, so output goes
through one writer that treats `BrokenPipe` as a quiet exit.

## S6 — The state directory, seen from inside a sandbox

From a sandboxed Codex command (`--sandbox workspace-write`, no rule): the
state directory under `~/.local/state` was **readable** and **not writable**,
and there was no network. So a sandboxed `status` or `result` could work, but
anything that records — `run`, `cancel`, `outcome` — needs the rule. cahoots
reports that as exit 33 with the rule, not as an I/O error.

## S7 — The fence, under a real user's configuration (found after M1)

The spike tested the harnesses' sandboxes with the flags alone. That was not
enough: **a callee inherits the user's configuration of the harness**, and the
configuration can undo the flag.

On the development machine, `~/.codex/config.toml` sets `approvals_reviewer =
"auto_review"`. With it, `codex exec --sandbox read-only` — the whole of
cahoots' read-only fence for Codex at the time — behaved like this when asked
to create two files:

| Flags | File in the working directory | File in `$HOME` |
|---|---|---|
| `--sandbox read-only` | **created** | **created** |
| `--sandbox workspace-write` | created | **created** |
| … plus `-c approval_policy="never"` | refused (reader) / created (writer) | refused |
| … plus `--ignore-rules` as well | the same | refused |

The sandbox did block the write. Codex's patch tool then asked for approval to
escalate, and the automated reviewer granted it. Nothing in the run's output
marks this as unusual. The same configuration also carries `prefix_rule(…,
decision="allow")` entries — which is how a Codex *caller* reaches cahoots at
all (S1) — and those run their commands outside the sandbox for a callee too.

So the Codex fence has three parts, all required for every role by
`validate`: the sandbox mode, `-c approval_policy="never"` (no escalation out
of it, whatever the user's approval settings say), and `--ignore-rules` (none
of the user's allow-rules). `--ignore-user-config` also closes the hole but
throws away everything else the user configured, so it is not used.

Claude Code's fence did not have this problem, because it is a whitelist of
tools (`--tools`) rather than a sandbox with an escalation path: a reader has
no tool that writes.

**The lesson is about method.** A fence is only tested by a real run, under a
real configuration, that TRIES to cross it. `mise run smoke` now does exactly
that in both directions — and it was run against the unfixed code to see it
fail before it was trusted to pass.

One side effect worth knowing: Codex records a `trust_level = "trusted"` entry
in the user's `config.toml` for a repository it is run in with a writer
sandbox. That is Codex's own bookkeeping, not cahoots', but a cahoots run can
cause it.

## S8 — Resuming a Codex session (before building `resume`)

`codex exec resume <thread>` accepts `--json`, `-m`, `-c`, `--ignore-rules`
and `--skip-git-repo-check` — but **not `--sandbox`**. Probed against the real
CLI, with `approval_policy="never"` and `--ignore-rules` throughout:

| Session started as | Resumed with | Wrote in its directory | Wrote in `$HOME` |
|---|---|---|---|
| `--sandbox read-only` | nothing about the sandbox | no | — |
| `--sandbox read-only` | `-c sandbox_mode="read-only"` | no | — |
| `--sandbox workspace-write` | nothing about the sandbox | **yes** | — |
| `--sandbox workspace-write` | `-c sandbox_mode="workspace-write"` | yes | no |

A resumed session inherits the sandbox it was started with. cahoots does not
rely on that: a resume restates the role's mode as `-c sandbox_mode="…"`, the
third and last `-c` shape `validate` allows. Both resumed sessions remembered
their first brief. (`claude --resume <id>` needs no such care: its fence is the
`--tools` whitelist, stated on every command line.)

## What changes in M1 because of this

1. `run` takes `--brief <path>`; the skill never uses a heredoc or a pipe.
2. `--caller` is explicit; environment detection is a fallback that refuses to
   guess.
3. Sandbox detection is `CODEX_SANDBOX`, and a sandboxed writer verb is
   exit 33 with the rule to add.
4. Claude is driven with `stream-json`; both parsers skip what they cannot
   read and never treat a warning item as failure.
5. The kill ladder signals the descendant tree, not just the process group.
6. Effort for Codex is `-c model_reasoning_effort=<level>`: `validate_argv`
   allows exactly that key, built in code from an enum, and no other `-c`.

## Still open

- Whether a callee should run with the user's hooks and skills, or isolated
  from them (`--bare`, which needs its own check that login still works).
- Antigravity CLI — nothing verified.
- The Codex rule was tested by appending to `~/.codex/rules/default.rules` and
  restoring the file afterwards; a project-scoped rules location was not found.
