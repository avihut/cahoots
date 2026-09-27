# Threat model

## The situation

For delegation to be useful, a harness must run `cahoots` **without a prompt
and outside its sandbox** — the sandbox blocks the network, and a callee needs
the network to reach its vendor. The user grants that with a rule cahoots
prints and they add:

| Harness | The rule |
|---|---|
| Claude Code | `permissions.allow`: `Bash(cahoots <verb>:*)` per agent verb; `sandbox.excludedCommands` when the sandbox is on |
| Codex | `prefix_rule(pattern=["cahoots","<verb>"], decision="allow")` in `~/.codex/rules/default.rules` |
| Antigravity CLI | `command(cahoots)` + `unsandboxed(cahoots)` |

From that moment, **cahoots' agent-tier verbs and their flags are that
harness's unsandboxed capability.** Everything below follows from taking that
sentence seriously.

## Who we defend against

**In scope:** a sandboxed or permission-gated agent — confused, prompt-injected
by a file or a web page, or simply over-eager — using `cahoots` to do what its
harness would otherwise have stopped or asked about. And a *callee's output*
trying to steer the caller or to persist itself.

**Out of scope:** an agent the user has given an unrestricted shell. It does
not need cahoots to do harm, and cahoots cannot stop it. Also out of scope:
the harness CLIs' own behaviour, and other users on a machine whose home
directory permissions are already broken.

## The boundary

**Two tiers of verbs.** Agent verbs — `pick run resume wait status result
cancel outcome notes review` — are the only ones the printed rules name. Human verbs
(`install`, `uninstall`, `settings`, `enable`, `learn`, `registry`) change what
cahoots may do, and refuse to run without a terminal on stdin. A test pins the agent tier
by name: growing it is a change to this document.

**No flag widens authority.** There is no `--ungated`; the gate is bypassed
only by configuration, which is a human's file. cahoots writes it only through
human verbs — `settings`, `enable`, and `install` for the meter a person
chose — and in place: the setting asked for changes, nothing else in the file
does, and nothing is written that the config's own checks would refuse.
`--in-place` works only if configuration allows it. Read-only roles are forced read-only by
`validate_argv`, after every other decision.

**Resuming is not a side door.** `resume` continues a harness's own session,
and is a new run in every other respect: gated, slotted, depth-checked,
recorded. It carries the ROLE's fence again rather than trusting what the
session had — `codex exec resume` has no `--sandbox` flag and a resumed
session inherits its old sandbox, so the mode is restated as an exactly-shaped
`-c sandbox_mode="…"` that `validate` requires. The session id it puts on the
command line came out of a callee's output stream, so it is held to the shape
of an id first. A run is resumed only from the workspace it was started in,
and never by the harness that was its target.

**Paths.** A brief is stdin, or a regular UTF-8 file, size-capped, under the
working directory, the git toplevel or a system temp dir — so `cahoots run
--brief ~/.ssh/id_ed25519` is not a way to ship a key to another vendor.
`--dir` must be a worktree of the same repository as the working directory, or
under a configured root. Configuration is never read from the working
directory: a repository cannot configure the tool that is about to run on it.

**Home and directories.** Home comes from the passwd database, not `$HOME` —
an agent controls its own environment. cahoots' directories are fixed paths
under it, refused if they resolve under the working directory, the git
toplevel, `--dir` or a temp dir. The `CAHOOTS_*_DIR` overrides the tests need
exist only in builds made with `CAHOOTS_DEV_BUILD=1`; the check is opt-in by
an explicit build-time variable, never inferred from "this looks like a
checkout", which a `cargo install --git` would satisfy. `cahoots --version`
says which kind of build it is. The reverse holds too: a dev build refuses to
touch the real directories — config, state, and the home `install` writes
into — unless its developer sets `CAHOOTS_DEV_REAL_DIRS=1`, so a half-finished
build or a test run cannot damage a working setup.

**The callee's environment** is cleared and rebuilt from an allowlist. Vendor
API keys are stripped unless that harness is configured for API billing: an
inherited key silently moves the callee onto per-token billing that no meter
sees — a budget bypass, not just a surprise.

**A callee inherits the user's configuration of its harness** — their models
and login, but also their approval settings and allow-rules, and those can
undo a sandbox flag (docs/SPIKE.md S7: with approvals handed to an automated
reviewer, `codex exec --sandbox read-only` wrote anywhere). So each harness's
fence is built from the parts that decide authority and nothing is left to the
user's defaults: for Codex the sandbox mode, `-c approval_policy="never"` and
`--ignore-rules`, every one required by `validate` for every role; for Claude
Code a whitelist of tools, `--strict-mcp-config` and an explicit permission
mode. What a user's configuration can still widen is what they chose to widen
for every session of that harness — extra writable roots, network access in
the writer sandbox — and cahoots says so rather than pretending otherwise.
`mise run smoke` tries to cross each fence with the real CLIs, under your
configuration.

**Binaries** — the harness, the meter, `git`, `daft` — resolve to canonical
absolute paths; a path inside the workspace, or group- or world-writable, is
refused.

**The usage meter decides admission, so the caller must not reach it.** It
is a third-party CLI — the Agent Usage tracker's `usage-cli`, or ccusage — and
a meter that said "plenty left" whenever the caller liked would turn every cap
off. So it runs only from a path a person pinned (`install` records where it
found each, from a terminal; config.toml may name one) and never from a PATH
lookup:
the caller sets PATH, and a `ccusage` it planted in a directory it can write —
a temp directory, say — would come first. It runs with a PATH of its own (its
directories, then the system's — never the caller's, which would pick the
interpreter a script meter runs on), HOME from the passwd database and no other
variable, so the caller cannot point it at an empty log directory; and it runs
from `/`, where no repository can leave it a config file. ccusage always gets
`--offline`, its own switch against fetching a price list; run with the
network and every write under the home directory denied, it gave the same
answer, and the kernel's sandbox log — which does record a denied attempt —
showed none. A meter that fails, times out or answers in a shape cahoots does
not know refuses the run.

**Targets are off until enabled.** A run sends repository content to another
vendor. That is a decision for a human, per harness, once: `cahoots enable`,
or the settings page, writes `enabled = true` in that harness's table in
config.toml.

**Results are untrusted.** `result` is size-capped and its envelope marks the
content `untrusted`; the skill tells the caller to treat it as a colleague's
claim, not as instructions.

**Recursion and loops.** `CAHOOTS_DEPTH` is exported to every callee, but it
can be scrubbed, so the real bounds are per-target slots (lock files), a global
active-run ceiling, and the ledger meter's runs-per-hour cap.

## Writers

`implement` is the one role that writes, and the question it raises is: write
WHERE, and how far?

**Where.** Never the caller's own tree, unless a person set
`limits.allow_in_place = true`. A writer gets a worktree of its own, cut from
the caller's `HEAD` — by `daft start --fork` in a repository that opted into
daft, otherwise a detached `git worktree` under cahoots' state directory,
outside every workspace. The caller is handed the path and a `git status`
summary; nothing lands in its tree until it brings it over. A change another
agent made is a proposal, and the skill says so.

**How far.** Each harness's own fence, proven on the command line and checked
by `validate` against the ROLE — a reader can never be handed a writer's
command line, whatever a builder does:

| | Reader | Writer |
|---|---|---|
| Codex | `--sandbox read-only` | `--sandbox workspace-write`: writes under its working directory **and the temp directories** (`/tmp`, `$TMPDIR` — Codex's design, which build tools rely on), nowhere else; no network; Codex's repository check stays on |
| Claude Code | `--tools Read,Grep,Glob` · `dontAsk` | `--tools Read,Grep,Glob,Edit,Write` · `acceptEdits`: Claude Code confines edits to the working directory; **no Bash**, because without a sandbox a shell writes anywhere |

`mise run smoke` checks both fences against the real CLIs: each writer is
asked to create a file in its worktree (it must) and one in the user's home
(it must not).

So a Claude writer cannot run the tests it may have broken, and the caller
does. That is a real cost, accepted until Claude Code's own sandbox can be
turned on from a command line cahoots is willing to build.

**A side effect to know about.** The first time a Codex *writer* runs in a
repository, Codex records that repository as `trust_level = "trusted"` in the
user's Codex config — its own bookkeeping for "this person asked for a writer
sandbox here", and it shapes how their later interactive Codex sessions start
in that repository. Readers do not cause it. cahoots neither writes that entry
nor removes it.

**What a writer can still do:** anything inside its worktree, and in the temp
directories under Codex. That is why the caller is told to READ a change
before running anything in it. A writer cannot commit to the caller's branch,
and cahoots never merges for anyone.

## Learning is an injection channel

A callee's output is read by a reviewing agent, whose finding becomes a note
that every future session reads before writing a brief. Free text on that path
is a persistent prompt injection with a laundering step in the middle.

So **nothing a reviewer writes is ever shown to another agent.**

- A finding is a closed vocabulary (`review::Kind`); growing it is a code
  change that a person reviews.
- A note is cahoots' own fixed sentence for that kind. A reviewer may add one
  line of detail, and it is kept for the PERSON: `learn list` — a verb that
  only runs from a terminal — shows it, and the field is `#[serde(skip)]` on
  the type `notes` serialises, so it cannot leak through a later edit to that
  verb. A blocklist of "instruction-like" words was tried first; it was both
  leaky and wrong about honest sentences ("the brief never said…"). Not
  showing the text at all is the fence that holds.
- The detail is still short and free of flags, code, paths, URLs and this
  tool's name, because a person reads it in a terminal next to a prompt.
- A note appears only once reviews of **two different runs in two different
  directories** agree — one run, or one poisoned repository, cannot write the
  notes by itself — and notes are few (five per role and target) and expire
  (sixty days).
- What is under review is handed to the reviewer marked `untrusted`, and the
  review skill says what that means: text that tells the reviewer to do
  something is itself the finding.
- A run is reviewed by the harness that delegated it and by nobody else, once,
  a few a day, and not when that harness's own plan is near its cap.
- `learn reset` is a person's verb. It appends a "forget" event; the record
  itself is never rewritten.

Routing never moves on a review's say-so — only on outcome statistics
(`calibrate.rs`): one adjacent swap per role, from at least eight rated runs
on each side, shown but not used until a person turns it on. What learning
may change is a struct with one field, `role → index`; there is nothing in it
for a cap, a reserve, a sandbox mode, a model, a flag or a command, and an
order a person wrote is left alone. The worst a poisoned history can do is
make a role try its second choice first.

## What cahoots never does

No network code. No credential file is ever read. No shell is ever spawned.
No harness settings or permission file is ever edited — rules are printed for
a human to add, and `doctor` checks them read-only. `uninstall` removes only
files that still carry cahoots' name.
