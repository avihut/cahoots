# Architecture

> This is the design the milestones build toward (README → Milestones); each
> section says which milestone lands it. M0 and M1 exist today. Where the
> spike changed the plan (`docs/SPIKE.md`), this document says what was built.

## Why a broker, and why a CLI

Every harness has a first-party **headless CLI** — `claude -p`, `codex exec`,
`agy -p` — and that is the only interface all of them share. MCP server modes
are rare and come and go (`codex mcp-server` was removed in Codex 0.154.0).
Subagent definitions differ per harness (Claude Code: Markdown + YAML; Codex:
TOML; Antigravity: Markdown + YAML), but a `SKILL.md` is portable.

So the shared logic lives in one binary, and each harness gets a thin, native
wrapper — a skill and an agent definition that say "call `cahoots`". Defining
harness N+1 is one registry entry and one installer target, not N new
integrations. The caller is excluded at run time, not by maintaining N
variants of the definitions.

## One run model (M1)

A caller's tool call times out in minutes (Claude Code: 120 s by default, 600 s
at most). Delegated runs take longer. So **every** run goes through a detached
supervisor, and there is no second, foreground path to keep honest.

- `run` is a thin client. It writes `runs/<id>/run.json` (`starting`),
  double-spawns (`__launch` → `__supervise`, which calls `setsid`), waits up to
  `--wait` (default 90 s), and returns either the result or *not finished* plus
  the run id. `wait`, `status`, `result` and `cancel` complete the set.
- **The run directory is the source of truth:**
  `<state>/runs/<id>/{run.json, brief, events.jsonl, final.md, supervisor.log, lock, cancel}`.
  `run.json` has one writer — the supervisor — and is replaced atomically. Ids
  are time-sortable and match `^[0-9A-Za-z_-]{1,64}$`. There is no separate
  ledger file; `report` scans run directories. Content is kept 7 days.
- **Liveness** is an exclusive lock on `runs/<id>/lock`, held for the
  supervisor's lifetime; the kernel releases it on death. `cancel` writes a
  marker the supervisor polls. Only the supervisor signals the callee — its
  own process group, SIGINT → 10 s → SIGTERM → 5 s → SIGKILL.
- Every invocation **reconciles**: a `running` record whose lock is free is
  marked `crashed`, and its recorded process group is killed only if the
  process's start time and command still match.
- Synchronous: `std::process`, reader threads feeding one `mpsc` channel
  (`recv_timeout` + `try_wait`, never an undeadlined `join`).
- **Resume** (M4): `cahoots resume <run> --brief <file>` is a new, gated run
  on the same harness, model, role and place, with the harness's own session
  picked up (`claude --resume <id>`; `codex exec resume <id>`). Claude's
  session id is preset with `--session-id`; Codex's is persisted at its first
  `thread.started` event — so a cancelled or timed-out run is resumable. A
  resumed writer goes back into the worktree it already has. Never
  `--ephemeral`: it also suppresses the rollout file that carries Codex's
  rate-limit snapshot, which is what the gate reads.

## The gate (M1)

Admit only if `used + reserve(role) ≤ cap`. The reserve (3 points for a
read-only role, 8 for `implement`, to start) is what stops check-then-act from
admitting the run that crosses the line.

- **Meters.** `agent-usage` shells out to the Agent Usage tracker's
  `usage-cli headroom` (absolute path in config). `ledger` is built in — runs
  and tokens per window per target, from cahoots' own records — so the gate
  works on day one without the tracker, and doubles as a runs-per-hour ceiling
  that bounds an agent's retry loop. Several meters may be configured; all
  must pass. Any meter exit cahoots doesn't know is treated as *no data*:
  fail closed.
- **Stale data, per provider.** Codex's numbers only refresh when Codex runs
  locally, so a strict freshness guard would refuse it forever. For such
  snapshot-on-use providers, a stale reading is re-asked without the age guard
  against `cap − 15`: a stale reading is a lower bound, and the run itself
  refreshes it. For polled providers, stale means the tracker's daemon is
  down: refuse.
- **Slots.** Per-target `max_concurrent` (default 1) as lock files, fail-fast:
  `pick` skips a busy target, an explicit `--to` returns *busy*. A global
  `max_active_runs`. `CAHOOTS_DEPTH` is exported on every spawn, but an agent
  can scrub it — the slots are the real bound on recursion.
- A run stops at its wall-clock timeout or the callee's own budget flag. A
  mid-run meter watchdog comes in M4, and acts only on fresh readings.

## Registry and command construction (M1)

Commands are built **in code**. A `Harness` trait turns a typed `RunSpec` into
an argv array; a final `validate(role, argv)` requires the read-only proof for
read-only roles and rejects the flags that widen authority
(`--dangerously-*`, `--config`, `--add-dir`, `--settings`, …). Codex gets
exactly two `-c` overrides, each held to its exact shape:
`model_reasoning_effort=<level>`, and `approval_policy="never"` — which, with
`--ignore-rules`, is as much a part of Codex's fence as the sandbox flag
(docs/SPIKE.md S7). The caller hands the brief over as a FILE (`--brief <path>`: a
heredoc or a pipe defeats Codex's rule matching); cahoots hands it to the
callee on stdin, never in argv.

The user's file holds typed knobs only — binary path, enabled, models and
efforts, candidate order per role, caps — with `deny_unknown_fields`.
Precedence: CLI flag > user > learned > default. There is no command template
to edit, by design: **data can never widen authority.**

Harness CLIs drift. That is detected, not templated around: a tested-version
range per harness (`doctor`), checked-in `--help` captures with a test that
every flag cahoots emits appears in them, and detection by fingerprint — a
binary named `agy` may be the Antigravity IDE launcher, not the agent CLI.

## Writers (M4)

`cahoots run --role implement --fork` is the only way anything gets written.
The client decides and checks (`placement::decide`: a writer without `--fork`
is refused; `--in-place` needs `limits.allow_in_place`); the detached
supervisor does the cutting (`placement::cut`), because in a daft repository
a new worktree runs the repo's setup hooks and that can outlast a caller's
tool call. The run reports `worktree` and `changes`; bringing the change over
is the caller's job, after reading it. A worktree cahoots cut itself is
removed when its run ages out; one daft cut is daft's to remove.

## Exit codes (M0)

`src/exit.rs`; `cahoots exit-codes` prints them. Every exit also prints one
JSON envelope: `v`, `code`, `class`, `retry` (`never | later | after_reset |
other_target | fix_config`), and optionally `message` and `data`.

| Code | Meaning |
|---|---|
| 0 / 1 / 2 | ok / internal error / usage error |
| 13, 21, 24, 25, 26 | gate refusals — `usage-cli headroom`'s codes, unchanged |
| 30 / 31 / 32 | no eligible target / target unavailable / busy |
| 33 / 34 | refused by policy / configuration error |
| 40 / 41 / 42 / 43 | run failed / timed out / cancelled / stopped by budget |
| 50 / 51 | no such run / not finished |

A callee's exit code is never passed through. `run`, `wait` and `result`
return the same code for the same run.

## Install (M2)

`cahoots install` writes five plain files, each only if that harness's home
already exists (it never invents one):

| File | For |
|---|---|
| `~/.agents/skills/cahoots/SKILL.md` | the shared skills location |
| `~/.claude/skills/cahoots/SKILL.md`, `~/.claude/agents/cahoots-delegate.md` | Claude Code |
| `~/.codex/skills/cahoots/SKILL.md`, `~/.codex/agents/cahoots-delegate.toml` | Codex |

The texts are embedded in the binary (`src/install/assets/` — those files ARE
the reviewable source; the only substitution is the version). The skill is the
same everywhere and tells an agent to pass `--caller`; each agent definition
fixes it for its own harness.

- **Install is update.** Every file carries a `cahoots_version` stamp, and the
  stamp is the only thing that makes a file cahoots' to touch: `installed`,
  `updated {from}`, `refreshed`, `up_to_date` — or `skipped`, for a file of
  the same name that a person wrote.
- **`uninstall` removes exactly what `install` wrote:** files the manifest
  lists (`<state>/install-manifest.json`) that still carry the stamp, then the
  `cahoots` directories it emptied. A file someone adopted (stamp gone) stays.
- **Writes are confined to the home directory**, judged by where the path
  resolves and *before* anything is created — an agent home that is a symlink
  out of `$HOME` does not even get a directory made through it.
- **Permission rules are printed, never applied.** `install` ends by listing
  the rules still missing and the file each belongs in; `doctor` checks them —
  and the installed copies' freshness — read-only.

## Review and local learning (M5, opt-in)

- **Ground truth is `outcome`** — `accepted | reworked | discarded`, recorded by
  the caller right after it uses a result. Missing means unknown.
- A `sampled` bit is fixed at completion (a hash of the run id against the
  sample rate), so changing the rate never re-selects history.
- The delegating harness reviews; a harness that never does simply learns
  nothing.
- **Notes are structured**, because they are a persistent prompt-injection
  channel (callee output → review → text every future session reads): a closed
  `kind` enum plus a short, filtered detail line, rendered from fixed
  templates, shown only after two supporting reviews from different runs.
- **Routing moves on statistics, not opinions:** outcome and failure rates over
  all runs, a minimum sample, at most one position or one effort notch from
  the default, a cooldown, decay — one release in shadow mode before it
  applies anything.
- **Bounds are compile-time:** learned state deserialises into a struct that
  only has adjustable fields.
- Everything is local: state directories at 0700, no network code to leak
  through.
