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

## Layers: the logic apart from the interface

There are three layers, and only one of them holds both ends (`AGENTS.md`
hard rule 11, held by `scripts/guard.sh`):

- **The logic** is everything that decides or does: the registry, the gate
  and its meters, runs, install, learning and survival, the settings. It
  returns data:
  results, refusals, and, when it needs a person, a *question* and a way to
  take the answer back. For install's meter choice, `Decision::Ask { options }`
  goes out and `detect::picked(selection, …)` takes the answer back; for the
  settings page, every setting is data (`src/settings.rs`) and a change goes
  back through `settings::set` or `settings::reset`. It never
  prints, prompts or looks at a terminal. So every path through it runs the
  same for an agent, a script or a person, and its tests need no terminal.
  The supervisor, which runs detached, writes what it sees to its run's log
  (`RunDir::log`), not to a stream.
- **The interface** is how cahoots meets whoever called it. Agents and
  scripts get one JSON envelope and an exit code that means something
  (`src/exit.rs`, printed by `cli::emit`). A person at a terminal gets the
  Clack rail for a command's questions, the settings page on the whole
  screen, and, for a human verb, `doctor` or `report`, how it ended in words
  on the rail, printed where the envelope would go (`src/tui`, designed in
  `docs/TUI.md`). It
  presents what it is given, returns answers as plain data (which choice,
  which number, what order), and knows nothing of meters, gates, runs or
  settings.
- **The command layer** is `src/main.rs`, `src/cli.rs` and `src/cli/`. It
  parses the command line, calls the logic, and puts the logic's questions to
  a person. `src/cli/questions.rs` holds their words and what each answer
  means, and `src/cli/settings.rs` the settings' words and what each answer
  on the page changes. It hands the answers back to the logic and prints
  what the verb said: the envelope, or for a person, the same in words
  (`src/cli/endings.rs`). Which one is `cli::reader`'s to decide: a person
  reads a human verb whose stdout is a terminal, and `doctor` or `report`
  when stdin is a terminal too; a program reads everything else.

The TUI is layered the same way inside: the terminal itself, what its bytes
mean, each way of answering (a choice, a number, an order), the page's
state, how things look, and what runs them (the rail, the whole screen) are
separate files, and only the first touches a terminal.

## One run model (M1)

A caller's tool call times out in minutes (Claude Code: 120 s by default, 600 s
at most). Delegated runs take longer. So **every** run goes through a detached
supervisor, and there is no second, foreground path to keep honest.

- `run` is a thin client. It writes `runs/<id>/run.json` (`starting`),
  double-spawns (`__launch` → `__supervise`, which calls `setsid`), waits up to
  `--wait` (default 90 s), and returns either the result or *not finished* plus
  the run id. `wait`, `status`, `result` and `cancel` complete the set.
- **The run directory is the source of truth:**
  `<state>/runs/<id>/{run.json, brief, events.jsonl, final.md, supervisor.log, patch.diff, lock, cancel}`.
  `run.json` has one writer — the supervisor — and is replaced atomically. Ids
  are time-sortable and match `^[0-9A-Za-z_-]{1,64}$`. There is no separate
  ledger file; `report` reads the folded history, which outlives a run's
  retained content. Content is kept 7 days.
  `<data>` holds what a person keeps: the suite.
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

Blind runs snapshot `[review] blind` at launch, including each new resume.
Private records and Finished history keep the full candidate and original
policy. Run envelopes and `review next` omit model and effort until a folded
outcome for that run exists; run envelopes also omit `model_reported` and the
private `exploration` label (the key is absent, not false). Older
records and history default to open. The answer itself is never rewritten.

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
  newer), the tracker's `headroom`, which it answers from its digest. What it
  finds goes in `<config>/meter.json`: where each usable meter is, cahoots'
  own record of the machine. Which meter is used is a person's choice, and it
  lives with their other settings, as `[meter] use` in config.toml
  (agent-usage, ccusage or none): the answer to install's question,
  `install --meter`, or `cahoots settings` writes it, and it stands until
  they change it. With no choice made, one usable meter found is used, and
  several bring the question; the only one found is never written as a
  choice, so a second one found later brings the question too. When none is
  found, the ledger alone gates runs. A `[meter.<id>]` table only holds that
  meter's knobs, and a `binary` there outranks where install found it.
- **Stale data, per meter and harness.** The tracker's Codex numbers only
  refresh when Codex runs locally, so a strict freshness guard would refuse it
  forever. For such snapshot-on-use readings, a stale one is re-asked without
  the age guard against `cap − 15`: a stale reading is a lower bound, and the
  run itself refreshes it. Claude's numbers are polled, and the tracker
  polls a quiet Claude slowly — every 40 minutes at its default pace after
  four quiet hours, up to hourly, longer under a 429 backoff — so a stale
  reading alone does not say the tracker stopped (#73). On one, the gate
  asks `usage-cli status` when the tracker last published and when it polls
  next, and applies the tracker's own rule for an engine that stopped
  (`EngineHostBroker.heartbeatStale`): silent longer than twice the gap to
  its next poll, and longer than 3 minutes. Still polling, the reading is
  re-asked against `cap − 15` as Codex's is. A Claude run writes a
  transcript, which can prompt the tracker to poll sooner — but only as its
  pace and any backoff allow, so what admits the run is the heartbeat and
  the fixed lower cap, not the run refreshing the numbers. Stopped, or any answer that
  can't say (another harness's, an unknown shape, a stamp ahead of the
  clock), refuses, and the refusal says how old the data is, what the
  tracker's stamps say, and that `usage-cli status` shows both — never that
  a daemon is down. The cost: a tracker that dies just after publishing is
  noticed only after twice its poll horizon, and until then Claude is held
  to `cap − 15` on numbers that no longer move — the exposure Codex always
  has. ccusage reads the logs themselves, so nothing it says is stale.
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
to edit, by design: **data can never widen authority.** cahoots writes the
file too, but only through human verbs (`settings`, `enable`, `install`),
and in place (`config::edit`, on `toml_edit`): a setting changes, the rest
of the file — its comments, its order, its tables — stays as the person
wrote it, and nothing is written that `UserConfig::parse` would refuse.

**Task kinds are routing under roles.** Optional `[kinds.<name>]` tables in
config.toml each require a description, a role and an ordered list of typed
candidates. No kinds ship by default. `pick --kind <name>` and
`run --kind <name>` resolve that role and exact list together before any
placement, brief access or candidate probe. An explicit `--role` must match;
`--to` only narrows this list, and exhaustion never falls back to role
candidates. Distinct efforts of one model stay distinct. Descriptions are
metadata, never added to argv or a delegated brief.

The selected label is `kind` on pick, run records, summaries and folded
history stories; finished history events use `task_kind` because their
`kind` field already tags the event as `finished`. Role-only runs serialize
null; missing fields in older data mean none. Versions remain 1. Resume
copies the saved label, role and candidate without resolving the current
kind, so removal, rename or redefinition cannot change the session's fence.

Complete definitions are created, renamed or removed in config.toml. Existing
fields are editable through human settings commands. The page and human
registry show sorted `Kind · <name>` sections after Roles: the description
is fixed display metadata with a CLI edit route; role is a choice and
candidates are an order. Required kind fields have no defaults and cannot be
reset individually.

**Exploration.** A role has a share of new runs that try the NEXT listed
candidate first, so a second model or effort on the non-caller harness can
earn evidence: with a caller on one harness, a list's later entries on the
other were otherwise never reached. Config: `[explore] share = { advise = 0.1 }`
per role (`explore.share.<role>`) and `[kinds.<name>.explore] share = 0.2` per
kind. Every omitted role share is `0.0`; a kind inherits its role's share
unless it has its own, and an explicit kind `0.0` overrides a nonzero role. A
share is a finite fraction from 0 to 1, inclusive; anything else is a config
error (34). It needs neither `review.enabled`, `review.apply_routing` nor
`calibrate`. The two-candidates-on-a-harness list is a person's own
configuration, not a shipped default.

`run` generates the run's ID after its preselection checks and before
choosing a target, and that ID is the one recorded. The draw is a pure
function of it and the share: `fnv1a64(b"cahoots-explore-v1\0" + id) % 10_000`
is below `share * 10_000` (`src/explore.rs`; the prefix keeps it apart from the
review sample). There is no seed, flag or environment hook, and nothing is
drawn again after a target is chosen. The draw only reorders: caller, disabled
and `--to` filtering happen first, then, for a selected draw, entries 0 and 1
of the REMAINING list swap (at most one adjacent swap, tail untouched), then
the usual eligibility walk runs every candidate through slots, binary policy
and gate. Exploration is off with `--to`, with fewer than two candidates left,
and when the first two are the same candidate (harness, model and effort —
different efforts are distinct). It never falls back from a kind's list into
its role's. A promoted candidate that is refused is skipped like any other:
the run goes to the next one that passes and is labelled `exploration: false`.

The label is `exploration: bool` on `run.json`, the finished history event
and its folded story, and on open run summaries (and `status`'s run list).
It is fixed when selection succeeds — a callee that then fails, times out or
is cancelled keeps it — and a later config change relabels nothing. Older
data means false. A resume draws nothing and records false; its
`resumed_from` names the session it continues. A blind run's envelopes omit
the key entirely until that run has its own outcome, then show the saved
boolean; the private record and history keep it throughout. `pick` previews
ordinary routing: no draw, no ID, and it adds `exploration_share` — the share
a run would use, or zero where exploration is suppressed. A run after a pick
may therefore pick the next candidate. Role calibration counts exploration
runs like any other role-only run; kind runs stay out of it.

Harness CLIs drift. That is detected, not templated around: a tested-version
range per harness (`doctor`), checked-in `--help` captures with a test that
every flag cahoots emits appears in them, and detection by fingerprint — a
binary named `agy` may be the Antigravity IDE launcher, not the agent CLI.

## Writers (M4)

`cahoots run --role implement --fork`, or `run --kind <name> --fork` for a
kind whose role is `implement`, selects the same writer.
The client decides and checks (`placement::decide`: a writer without `--fork`
is refused; `--in-place` needs `limits.allow_in_place`); the detached
supervisor does the cutting (`placement::cut`), because a checkout of a large
repository can outlast a caller's tool call. What cuts it is a provider
(`src/placement/`, a closed set like the harnesses and the meters), chosen
by a person in config.toml and never by a flag or the repository:
`fork.provider = "git"`, the default, cuts a detached `git worktree` under
cahoots' state directory, and `"daft"` cuts with `daft start --fork` where
the repository has a `daft.yml`, and git everywhere else. daft runs only
from `fork.daft.binary`; a chosen daft that is missing or unfit fails the
run rather than falling back to git, and each provider identifies itself by
its `--version` before it cuts. The cut runs none of the repository's hooks
or filters: daft gets `--skip-hooks all` unless a person sets
`fork.daft.hooks`, every git that cuts, reads or removes a worktree — and
the one daft starts — gets an empty hooks directory of cahoots' own and no
fsmonitor, and every filter driver the configuration defines is turned off
for the cut, so a Git LFS file comes up as its pointer and a git-crypt file
as its ciphertext. git cuts in two steps, so that the filters are the ones
git names for the new worktree, conditional includes (`includeIf`) and all:
`git worktree add --no-checkout`, then — once cahoots has read the
configuration through the new worktree's pinned git directory — the
checkout, by cahoots' own `git reset --hard`. daft checks out as it cuts,
so for daft every file an include could reach is read first, every
condition taken as holding, and every filter named in any of them is turned
off; an include that cannot be read refuses the cut. The worktree comes up without the repository's setup,
and the caller runs that there. The tool that cuts runs in a process group
of its own, killed when the cut returns. The path daft prints is checked before
a writer runs in it (`placement::unfit`), and the worktree's git directory is
read once, at the cut, and recorded (`gitdir`). The run reports `worktree` and
`changes`, a `git status` read against that recorded git directory — or, when
the status cannot be read, `changes: null` and `changes_error`, which says
why. Every run records the commit it started from (`base_commit`), and a fork
is cut at exactly that commit; a fork writer's patch against it is kept in the
run directory (`patch.diff`), and a summary of it — paths, line counts and a
hash per changed block — in the run and in the history, which outlives the
patch (`src/patch.rs`). In place, the supervisor records the git configuration status would
read (`git config --list --show-origin`) before the writer starts, in the run
directory, and status runs afterwards only if it is unchanged. Bringing the change over is the caller's job, after reading it. The
run records which provider cut its worktree, and reports who removes it
(`worktree_owner`: `"cahoots"` or `"daft"`). A worktree git cut is removed
once the last run on record that works in it ages out; one daft cut is
daft's to remove, and cahoots never does.

## Exit codes (M0)

`src/exit.rs`; `cahoots exit-codes` prints them. Every exit also prints one
JSON envelope on stdout: `v`, `code`, `class`, `retry` (`never | later |
after_reset | other_target | fix_config`), and optionally `message` and
`data`. The exceptions are for a person at a terminal, with the same exit
code: a human verb whose stdout is a terminal ends in words on the rail
instead (`docs/TUI.md`), as do `doctor` and `report` when stdin is a
terminal too, and a command line clap refuses ends in clap's own words,
unless it names a verb that prints the envelope there. Piped, as in
`cahoots install | jq`, everything prints the envelope, byte for byte, and
every other verb prints it at a terminal too. The envelope for a
refused command line says what was wrong: clap's first paragraph, or for a
command group given no command, which group needs one.

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

`cahoots install` writes plain files, each only if that harness's home
already exists (it never invents one):

| File | For |
|---|---|
| `~/.agents/skills/cahoots/SKILL.md`, `~/.agents/skills/cahoots-review/SKILL.md` | the shared skills location |
| `~/.claude/skills/cahoots/SKILL.md`, `~/.claude/skills/cahoots-review/SKILL.md`, `~/.claude/agents/cahoots-delegate.md` | Claude Code |
| `~/.codex/skills/cahoots/SKILL.md`, `~/.codex/skills/cahoots-review/SKILL.md`, `~/.codex/agents/cahoots-delegate.toml` | Codex |
| `~/.claude/agents/cahoots-kind-<name>.md`, `~/.codex/agents/cahoots-kind-<name>.toml` | one subagent per kind of task, per harness |

The texts are embedded in the binary (`src/install/assets/` — those files ARE
the reviewable source; the only substitution is the version). The skill is the
same everywhere and tells an agent to pass `--caller`; each agent definition
fixes it for its own harness.

**A subagent per kind.** Each `[kinds.<name>]` in config.toml gets
`cahoots-kind-<name>` in each harness, so a session hands a matching task
over by itself. It runs `cahoots run --kind <name>` (with `--fork` for an
`implement` kind) and nothing else. Its description is the person's own,
followed by a fixed sentence (`assets/kind-description.txt`), quoted for the
file's syntax and put in after every placeholder, so nothing in it is read
as one. It never enters the instructions. The kind's role is written next
to the stamp, so any role edit changes the file and `doctor` reports it. A harness gets no subagent for a
kind whose candidates are all on that harness: `pick` leaves the caller out,
so it could never be delegated from there. The prefix keeps a kind named
`delegate` off the fixed definition.

- **Install is update.** Every file carries a `cahoots_version` stamp, and the
  stamp is the only thing that makes a file cahoots' to touch: `installed`,
  `updated {from}`, `refreshed`, `up_to_date` — or `skipped`, for a file of
  the same name that a person wrote.
  The stamp is read only from a line that begins with it, so a description
  that mentions it does not make an adopted file cahoots' again.
- **`uninstall` removes exactly what `install` wrote:** files the manifest
  lists (`<state>/install-manifest.json`) that still carry the stamp, then the
  `cahoots` directories it emptied. A file someone adopted (stamp gone) stays.
- **Install follows the kinds.** It removes the subagents the current kinds
  no longer want (a kind removed or renamed, or left with no candidate on
  another harness), by uninstall's rule, only in the homes the run covers
  (`--harness`), and not on a dry run. `files::prune` is the only place it
  removes anything.
- **Refresh keeps it current without a terminal.** `cahoots refresh` does
  install's file work and nothing else: listed, stamped files from this
  binary's text; kinds' subagents added and removed only in homes the
  manifest covers. It never touches the meter, config.toml or the rules. It
  reports per file in install's vocabulary.
- **Writes and removals are confined to the home directory**, judged by
  where the path resolves and *before* anything is created or deleted — an
  agent home that is a symlink out of `$HOME` does not even get a directory
  made through it, and nothing in it is removed by `install` or `uninstall`.
  A file that cannot be read is left alone and stays in the manifest; only
  one that is gone leaves it.
- **Permission rules are printed, never applied.** `install` ends by listing
  the rules still missing and the file each belongs in: at a terminal, as
  lines to paste, flush left under that file. `doctor` checks them, and the
  installed copies' freshness, read-only: each file `install` would change,
  and why — `changed`, `not installed yet` (a kind's subagent, where install
  has run for that harness), or `no longer wanted`.
- **It chooses the usage meter** (see *The gate*) — before it writes a file,
  so a question left unanswered leaves nothing half-done. It asks on the
  Clack rail (`docs/TUI.md`), with the arrow keys, on stderr, and only when
  config.toml has no `[meter] use`. At a terminal, install's words go on from
  the question's rail on stdout; piped, the question closes its own rail and
  stdout is the envelope. `--meter <agent-usage|ccusage|none>` answers it in
  advance, and `--meter-binary` names a copy install would not find. Once the files
  are in, it writes what it found to meter.json and a person's answer to
  config.toml (`[meter] use`, and a `--meter-binary` path as that meter's
  `binary`); a dry run writes neither.

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
- `cahoots report [--days N]`: per role and target, and per kind, recorded
  role and full candidate (`by_kind_and_target`; two efforts are two
  candidates, and a kind redefined with another role keeps its old rows) —
  runs, how they ended, what became of them, tokens, median duration. Runs
  are the folded stories whose finish time is in the window; a late outcome
  does not move an old run in. Every candidate of every currently configured
  kind has a row, with zero runs if there are none; a removed one keeps the
  row its runs earned. Runs with no kind are in the role totals only.
  Each row carries its **evidence**: the one classification calibration uses
  (`Evidence::observe` — a recorded outcome, whatever the run's state, or
  else an unrated failure, crash or timeout; unknown, cancelled and budget
  stops count for nothing), its share of each kind of result with the
  standard error `sqrt(p(1-p)/n)`, and the score (accepted 1, reworked ½,
  the rest 0) with the standard error of its sample variance,
  `sqrt(max(0, Q - n·score²) / (n(n-1)))` for `Q = accepted + ¼·reworked`
  (two observations are needed). One standard error, not a confidence bound;
  runs are correlated and routing chose them, so these describe and never
  decide. `enough_evidence` is `n >= 8`, the report's own floor (`sample_floor`
  in the data), not calibration's and not a setting. `doctor` uses the same
  classification over the whole history, with no window and not cleared by
  `learn reset`, to warn about each candidate in a current kind list or a
  person-written role list that has none.
- **Survival** — how much of a fork writer's diff is still part of the
  repository's change since its base commit: the share of its blocks (two
  lines or more, matched by path and hash) found in the net `git diff
  <base> <HEAD>`, so a block a later commit rewrote falls out again. HEAD is
  the run's own tree's, read against the git directory pinned when the run
  started (`base_repo`), or the repository's if that worktree was removed.
  `outcome` measures once; `report` measures once more after a 14-day window,
  and each row, role or kind, shows the settled share, how many fell from
  their first measure, the share still settling, and what is unknown. Each
  measurement is a history line: a commit hash and two counts. `survival.rs`
  is the logic and returns data; its words are `endings::reported`'s.
  Nothing in routing reads it.

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

Role calibration uses evidence only from runs without a task kind. Kind lists
stay in the person's order even when learned routing is enabled. Ordinary
report totals still include kind runs, and outcomes, sampling and role-scoped
notes retain their behavior. Report groups kind runs (above); no per-kind
calibration exists yet.

## The eval suite (#38)

A writer's run whose result the caller accepted already holds a task: the
commit it started from, its brief, its kind and the patch it made. All of it
ages out with the run directory, so `cahoots evals add <run>` copies it into
the data directory (`~/.local/share/cahoots`), which nothing ages out:

```text
<data>/evals/tasks/<task>/   0700; a task's id is its run's
  task.json      the record: run, role, kind, target, repo, git_common_dir,
                 base_commit, hidden_tests, solution                0600
  brief          the run's brief, byte for byte                     0600
  tests.diff     the hidden tests: the patch's sections for test files 0600
  solution.diff  every other section of the patch                   0600
```

- **The split.** The run's `patch.diff` is cut at its `diff --git` headers
  (`patch::sections`, which names each file as `summarize` does). A section
  whose path is a test — under a `test`, `tests`, `__tests__`, `spec`, `specs`
  or `testdata` directory, or named like `foo_test.go`, `test_foo.py`,
  `foo.test.ts` or `FooTest.java` (`evals::is_test_path`) — goes to
  `tests.diff`, the rest to `solution.diff`. Sections are independent, so
  each file applies on its own at `base_commit` — except where a test and a
  file of the rest collide: one inside the other (a file became a directory,
  or the reverse), or the same path but for case. Paths are compared as the
  bytes they name, git's quoting undone, name by name: an ASCII name exactly
  but for case, and a name that is not ASCII as one that may be any other,
  since std can neither fold nor normalise Unicode as a filesystem does. Then
  one diff would not apply without the other, and the run makes no task. A patch with no test file
  still makes a task, with no hidden tests. Rust's inline `#[cfg(test)]`
  modules count as solution.
- **Only an accepted writer's own answer.** The run must be finished, an
  `implement` run in a fork, not a resume (its brief is only the follow-up),
  with `accepted` as its last outcome in the history, a patch kept and not
  empty, and within the 7 days a run directory is kept.
- **The rot check** runs `git rev-parse --verify` for the base commit in the
  repository's common git directory, read once from the fork's pinned git
  directory when the task is made. A linked worktree (daft's layout) is
  removed after a merge, and the repository outlives it. `evals list` marks
  each task `none`, `commit_gone`, `repository_gone` or `unknown`; a check
  that cannot run is `unknown`, never "gone".
- **All of it is a person's.** `evals` is a human verb: `add`, `list` and
  `remove` run only from a terminal. Nothing executes a task yet (replay is
  #39), and nothing sends one anywhere.
