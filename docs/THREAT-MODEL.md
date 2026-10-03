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

**Task kinds select a person's list.** `pick --kind <name>` and
`run --kind <name>` select a task kind from the person's `config.toml`,
never from the repository or a callee's output. Each kind names a role and
an ordered list of typed candidates. Its role supplies the same reserve,
placement checks and final command-line fence as an explicit `--role`.
When both flags are given, their roles must agree; an unknown kind or a
disagreement is refused before a run starts. `--to` can only narrow the
kind's list, and exhausting that list never falls back to the role's list.
The caller still cannot name a model or an effort, alter the list, enable
a target, or change a cap, reserve, sandbox, flag or command. Descriptions
do not enter a harness command or the delegated brief. A kind is recorded
as a label, and resuming preserves the recorded kind, role and candidate
under the existing resume checks; later edits to the kind cannot change
that session's fence.

**Exploration reorders a person's list; it adds no authority.** A person's
exploration share may make a new run try the next listed candidate first.
Exploration only reorders the selected list after the caller and disabled
targets are removed. Every attempted candidate passes the existing binary
policy, slots and gate, and the selected role supplies the same placement
checks and final command-line fence. Exploration never applies with `--to` or
to a resumed session. cahoots generates the run ID before selection; no
caller flag, brief, repository value or callee output supplies the ID or draw
seed. The label is recorded locally and changes no authority. The share is a
typed fraction in the person's `config.toml` (`explore.share.<role>`,
`kinds.<name>.explore.share`); learned data has no field for it.

`install` writes a kind's description — the person's own text — into that
kind's subagent definition in the caller's harness, where the caller's model
reads it to decide when to hand a task over. It sits in the definition's
description field only, quoted for that file's syntax and substituted after
every placeholder. The subagent's instructions are fixed text naming the
validated kind, so the description cannot change what the subagent runs.

**Resuming is not a side door.** `resume` continues a harness's own session,
and is a new run in every other respect: gated, slotted, depth-checked,
recorded. It carries the ROLE's fence again rather than trusting what the
session had — `codex exec resume` has no `--sandbox` flag and a resumed
session inherits its old sandbox, so the mode is restated as an exactly-shaped
`-c sandbox_mode="…"` that `validate` requires. The session id it puts on the
command line came out of a callee's output stream, so it is held to the shape
of an id first. A run is resumed only from the workspace it was started in,
and never by the harness that was its target, and a writer whose worktree
was never cut is not resumed: it would start in the caller's own tree.

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

**Binaries** — the harness, the meter, `git`, `daft`, `ps` — resolve to
canonical absolute paths; one inside the workspace (the caller's working
directory and repository, the directory a run works in, and the worktree cut
for it), or group- or world-writable, is refused. `git`, `daft`, `ps`, and a
harness asked its version, look things up on a PATH without the workspace's
directories in it, so `daft` cannot find a `git` that cahoots refused.

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

**Blind runs change presentation, not authority.** When a person sets
`[review] blind = true`, each new run records that choice. Until an outcome
is recorded for that run, its run envelopes and `review next` omit the
target's model and effort; run envelopes also omit the model reported by
the harness. They mark the run `blind: true`. Recording an outcome makes
subsequent views show the identity and `blind: false`. Until an outcome is
recorded for that run, blind run envelopes omit its exploration label as well
as the target's model and effort and the model reported by the harness. A
known list order and an exploration label could otherwise identify the
selected candidate. The private record and finished history retain the
label; an outcome reveals its saved value in subsequent run views. A blind
resume needs its own outcome to reveal its label. The full identity
remains in local records for supervision, accounting and learning; routing,
gates, command lines and sandbox fences are unchanged. The harness remains
visible: with two harnesses, an agent caller already knows its target is
the other one. This supports judging an answer before its attribution;
it is not anonymity or an access-control boundary. `pick`, aggregate
reports, configuration, and the answer's text can still reveal or suggest
identity, as can briefs, diagnostic text and related runs. Nothing in the
returned content is rewritten to conceal its author.

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

**No repository hooks.** Cutting a worktree runs no command the repository
defines. A repository's hooks are content an agent can write — `daft.yml` is
a tracked file, and `core.hooksPath` may point into the tree — and the cut
runs outside every sandbox, on an agent's verb. So `daft` runs with
`--skip-hooks all`, and with `--no-carry`, and at the exact commit the run
recorded — as is a plain `git worktree` — and every `git` that cuts, reads or removes a worktree —
and the one `daft` starts — is given an empty hooks directory of cahoots' own
and `core.fsmonitor=false`, as configuration above every config file.
(cahoots' other `git` calls read: `rev-parse` where a repository is, and the
commit every run starts from; and, for a writer, the `diff` and `ls-files`
that keep its patch. They get the same empty hooks directory and
`core.fsmonitor=false`.) The worktree therefore comes up without the repository's
setup; the caller runs it there, after reading the change. The path `daft`
prints is used only if it is a directory at the top of a worktree of the same
repository, not the tree it was cut from or inside it, not inside cahoots'
own directories (its `worktrees` aside), and a worktree the cut made — never
one that was there before, the caller's own among them; otherwise the run
fails before the writer starts. A fork that was never cut cannot be resumed —
it would start in the caller's own tree.

**How far.** Each harness's own fence, proven on the command line and checked
by `validate` against the ROLE — a reader can never be handed a writer's
command line, whatever a builder does:

| | Reader | Writer |
|---|---|---|
| Codex | `--sandbox read-only` | `--sandbox workspace-write`: writes under its working directory **and the temp directories** (`/tmp`, `$TMPDIR` — Codex's design, which build tools rely on), nowhere else; no network; Codex's repository check stays on |
| Claude Code | `--tools Read,Grep,Glob` · `dontAsk` | `--tools Read,Grep,Glob,Edit,Write` · `acceptEdits`: Claude Code confines edits to the working directory; **no Bash**, because without a sandbox a shell writes anywhere |

`validate` also refuses each harness's own worktree flag (Claude Code's
`-w`/`--worktree`, Codex's `--worktree`): a callee never cuts a tree of its
own, with its own hooks and outside these checks.

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
and cahoots never merges for anyone. `changes` is read with `git status`
against the git directory recorded when the worktree was cut, never the one
the worktree's `.git` names now; the writer's patch, kept in the run directory
against the commit the run started from, is read the same way — against the
pinned git directory, with `--ignore-submodules=all`, and only after the
callee's process group is killed — and everything that shapes how it is
written is fixed on the command line (`--no-ext-diff`, `--no-textconv`, the
diff algorithm, prefixes, order, `--full-index`), so neither the person's git
config nor the repository's can change it or run a diff helper. The capture
has 8 seconds and 32 MiB for all of its steps together; past either, no patch
is kept at all, and the run ends as the writer left it. What git
still honours is what it converts on the way in: a clean filter that the
repository's or the person's configuration defines for a path, the same
exposure `changes` has, and `core.autocrlf`, which checkout applied already.
A link is written as a link, from `read_link`, never followed. A file the
patch would read that has more than one hard link — perhaps another file's,
from outside the worktree, linked in — is never read, and a file that is
replaced or written while git reads it fails the capture: its device, inode,
type, link count, size, `ctime` and `mtime` are compared, since a replacement
can get back the inode number it took the place of. No patch is kept either
way. A submodule's changes are not in the patch. `changes` too always runs
with `--ignore-submodules=all`, so it never descends into a submodule, whose own
git directory and config — where a writer could name a filter or an
fsmonitor command — it does not see. A status that fails says so
(`changes_error`), never "no changes". In place, where the writer had the caller's own tree and perhaps its `.git`,
`git status` runs only if the git configuration it would read — every scope
and every included file, as git itself resolves them — is byte for byte what
it was before the writer started; otherwise `changes_error` says the
configuration changed, and nothing runs. A resumed in-place run is held to
the configuration the original run recorded, never a fresh reading, so a
first writer cannot set the baseline its own resume is judged against. And
when a run ends — a normal exit, or an error inside the supervisor,
included — the supervisor kills the callee's whole process group before the
run is marked terminal, so no process it left behind can still be writing the
tree, or its `.git/config`, while `changes` is read. A process that left the group (`setsid`, a double fork) is beyond
that reach, and so is a change `changes` is told to ignore in the submodule
or outside the configuration it snapshots; the caller is told to read a
change before trusting it for exactly these reasons. A worktree is removed
only when no run on record still works in it. One cahoots cut whose git
directory then fails the pin is refused before the writer starts and left
where it is: no run on record points at it, so the refusal names its path,
for a person to remove.

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
  `review submit` refuses a control character in it, and `learn list` shows
  any that reached the record anyway as its escape (`\u{1b}`), as it does
  for every word it prints at a terminal: no text on record can move the
  cursor, retitle the terminal, or hide a line from the person reading.
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
Role calibration uses only runs without a task kind; a kind's candidate
list is left in the order a person wrote.
The history keeps, for a writer, the repo-relative paths it touched and a
hash and line count of each changed block — derived from content, never
content — and like everything learned it stays on this machine.

## What cahoots never does

No network code. No credential file is ever read. No shell is ever spawned.
No harness settings or permission file is ever edited — rules are printed for
a human to add, and `doctor` checks them read-only. `install` and
`uninstall` remove only files the manifest lists that still carry cahoots'
name and whose directory resolves inside the home — `install` only the
subagents of kinds that are gone.
