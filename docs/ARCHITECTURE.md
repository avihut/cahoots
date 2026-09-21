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

- **Meters.** Two, and both must pass. The **ledger** is built in — runs and
  tokens per window per target, from cahoots' own records — so the gate works
  on day one with nothing else installed, and doubles as a runs-per-hour
  ceiling that bounds an agent's retry loop. The **usage meter** is a CLI the
  person already runs; `src/meter` asks it and turns the answer into the
  gate's verdict. One is on at a time:
  - `agent-usage` — the Agent Usage tracker's `usage-cli headroom`: each
    plan's own percentages, with their age and a forecast. Its exit codes
    are the gate's refusal codes, unchanged.
  - `ccusage` — token counts from the harnesses' own logs: `blocks --active`
    for Claude Code's 5-hour block, `codex daily --last 1` for Codex. No
    vendor publishes a plan's limit in tokens, so a percentage exists only
    against one the person declares (`claude_block_tokens`,
    `codex_day_tokens`); `cap` and `abort_at` are then percentages of it.
    Without one, Claude Code is still refused while its log says it hit its
    limit, and Codex is not measured. There is no forecast: ccusage's
    projection is a straight line through a burn rate that counts cache
    reads, and it would refuse nearly every run of a busy session. Always
    `--offline`, ccusage's switch against fetching a price list.

  A meter runs from a path a person pinned — never a PATH lookup, which the
  calling agent controls — from `/`, so no repository can hand it a config
  file, and with a PATH and HOME of its own. Any answer cahoots doesn't
  understand is *no usage data source* (13): fail closed.
- **Choosing the meter.** `cahoots install` looks for each one — ccusage on
  PATH; `usage-cli` where the tracker's launch agent runs it, on PATH and in
  the Applications folders — and probes it: ccusage's `--version` (20 or
  newer), the tracker's `headroom`, which it answers from its digest. One
  usable meter is used; several, and the person is asked. The choice goes in
  `<config>/meter.json`, a file of its own, so the hand-written config is
  never rewritten, and it stands until `install --meter` changes it. A
  `[meter.<id>]` table in the config outranks it.
- **Stale data, per meter and harness.** The tracker's Codex numbers only
  refresh when Codex runs locally, so a strict freshness guard would refuse it
  forever. For such snapshot-on-use readings, a stale one is re-asked without
  the age guard against `cap − 15`: a stale reading is a lower bound, and the
  run itself refreshes it. For polled ones, stale means the tracker's daemon
  is down: refuse. ccusage reads the logs themselves, so nothing it says is
  stale.
- **Slots.** Per-target `max_concurrent` (default 1) as lock files, fail-fast:
  `pick` skips a busy target, an explicit `--to` returns *busy*. A global
  `max_active_runs`. `CAHOOTS_DEPTH` is exported on every spawn, but an agent
  can scrub it — the slots are the real bound on recursion.
- A run stops at its wall-clock timeout, at the callee's own budget flag, or
  at the **watchdog** (M4): while a run is going, the supervisor re-asks the
  usage meter every `limits.watchdog_secs` (default 120) whether the target has
  crossed `harness.<id>.abort_at` — a threshold of its own, always above the
  cap (default cap + 10), because stopping work in flight is a higher bar than
  refusing to start it. It takes two over-threshold readings in a row, and
  only FRESH ones count: a stale, missing or unintelligible reading is "no
  reading", resets the count, and never stops anything. The run ends as
  `budget` (exit 43) with what it had said so far, and can be resumed after
  the limit resets. It needs a percentage that moves during the run — the
  tracker's, or ccusage's against a declared limit — so without one there is
  no watchdog: cahoots' own ledger cannot move during a run.

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
| 13, 21, 24, 25, 26 | gate refusals — `usage-cli headroom`'s codes, unchanged; every meter's answer maps onto them |
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
- **It chooses the usage meter** (see *The gate*) — before it writes a file,
  so a question left unanswered leaves nothing half-done. The question goes to
  stderr: stdout is the one JSON envelope, as for every verb. `--meter
  <agent-usage|ccusage|none>` answers it in advance, and `--meter-binary`
  names a copy install would not find.

## Review and local learning (M5, opt-in)

**What exists: the long memory.** A run's content (brief, answer) is kept for
days; what it WAS is kept in `<state>/history.jsonl`, one short line per
event, for as long as statistics need it — ids, enums, counts and the working
directory, never content. It is append-only with several writers (a
supervisor finishing, a caller recording an outcome), each event one line
written with `O_APPEND`, and a run's story is the fold of its events. So the
file is its own rebuildable index: there is no database to fall out of step
with it. (The plan named SQLite; at hundreds of records a JSONL event log does
the same job with no native dependency.)

- **Ground truth is `outcome`** — `cahoots outcome <run>
  accepted|reworked|discarded`, recorded by the caller once it knows. The last
  word wins. Missing means unknown, and `report` keeps "unknown" as a column of
  its own: it is never folded into "accepted".
- A **`sampled` bit is fixed when a run ends**, from a hash of the run id
  against the sample rate of that moment — and only if review is on. Changing
  the rate later never re-selects history, and nobody picks which runs get
  reviewed.
- `cahoots report [--days N]`: per role and target — runs, how they ended,
  what became of them, tokens, median duration.

**The review loop (opt-in: `[review] enabled = true`).**

- The harness that DELEGATED a run reviews it: `cahoots review next --caller
  <h>` hands over the newest pending run's brief and answer (capped, marked
  `untrusted`) with a rubric; `cahoots review submit <run> --finding …`
  records findings from a closed vocabulary. Pending means: sampled, or thrown
  away by the caller; not yet reviewed; at most 14 days old; a backlog of ten.
  Finished runs carry a `pending_reviews` count so a harness finds out without
  asking, and a `cahoots-review` skill says when and how.
- Reviewing spends the reviewer's own plan: five a day, and none while that
  harness is within twenty points of its own cap.
- `cahoots notes --role <r>`: what reviews agree on, per target — DERIVED from
  the history each time, so there is no notes file to tamper with. Fixed
  sentences only; see `docs/THREAT-MODEL.md` for why a reviewer's own words
  never reach another agent.
- `learn list` (what was learned, from how much, and what reviewers wrote) and
  `learn reset` are a person's verbs.

**Routing calibration — statistics, never opinions.** Which candidate a role
tries first is tuned by what callers did with results (`outcome`) and whether
runs failed by themselves; a reviewer's findings feed the notes and nothing
else. `calibrate::suggest` is a PURE FUNCTION of the history and the current
candidate list, recomputed whenever it is needed — so there is no learned
state to go stale or to tamper with, a changed list is simply re-evaluated,
and evidence that ages out of the 90-day window decays the adjustment back to
the default by itself.

- A candidate moves up ONE place past its neighbour when both have at least 8
  rated-or-failed runs in the window and it scored at least 0.15 better
  (accepted = 1, reworked = ½, discarded or failed = 0). One swap per role.
  Unknown outcomes are not evidence; a run that was cancelled or stopped for
  budget is not held against its candidate.
- **Bounds are compile-time.** `LearnedAdjustments` holds `role → index` and
  nothing else: there is no field in it for a cap, a reserve, a model, an
  effort, a flag or a command, and a test shows that applying it only ever
  reorders.
- **Shadow mode is the default.** `cahoots report --suggest` shows the
  evidence per candidate and the swap it supports; nothing is used until a
  person sets `review.apply_routing = true`. An order a person WROTE is left
  alone unless they add `calibrate = true` to that role.
- `learn reset` starts the evidence over, as it does for notes.

**Designed, not built:** tuning the effort level (the plan allowed one notch),
and a `learn revert` finer than `learn reset`.
