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

**Two tiers of verbs.** Agent verbs — `pick run wait status result cancel
outcome notes review` — are the only ones the printed rules name. Human verbs
(`install`, `uninstall`, `enable`, `learn`, `registry`) change what cahoots may
do, and refuse to run without a terminal on stdin. A test pins the agent tier
by name: growing it is a change to this document.

**No flag widens authority.** There is no `--ungated`; the gate is bypassed
only by configuration, which is a human's file. `--in-place` works only if
configuration allows it. Read-only roles are forced read-only by
`validate_argv`, after every other decision.

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
says which kind of build it is.

**The callee's environment** is cleared and rebuilt from an allowlist. Vendor
API keys are stripped unless that harness is configured for API billing: an
inherited key silently moves the callee onto per-token billing that no meter
sees — a budget bypass, not just a surprise.

**Binaries** — the harness, the meter, `git`, `daft` — resolve to canonical
absolute paths; a path inside the workspace, or group- or world-writable, is
refused.

**Targets are off until enabled.** A run sends repository content to another
vendor. That is a decision for a human, per harness, once (`cahoots enable`).

**Results are untrusted.** `result` is size-capped and its envelope marks the
content `untrusted`; the skill tells the caller to treat it as a colleague's
claim, not as instructions.

**Recursion and loops.** `CAHOOTS_DEPTH` is exported to every callee, but it
can be scrubbed, so the real bounds are per-target slots (lock files), a global
active-run ceiling, and the ledger meter's runs-per-hour cap.

## Learning is an injection channel

A callee's output is read by a reviewing agent, whose finding becomes a note
that every future session reads before writing a brief. Free text on that path
is a persistent prompt injection with a laundering step in the middle.

So a finding is a closed `kind` enum plus scope. The optional detail line is
short and rejected if it contains flags, backticks, URLs, paths or the tool's
own name. Notes render from fixed templates under an "observations, not
instructions" header, appear only after two supporting reviews from different
runs and directories, are capped per scope and expire. Routing never moves on
a review's say-so — only on outcome statistics — and learned state
deserialises into a struct with no field for a cap, a reserve, a sandbox
mode, a flag or a command.

## What cahoots never does

No network code. No credential file is ever read. No shell is ever spawned.
No harness settings or permission file is ever edited — rules are printed for
a human to add, and `doctor` checks them read-only. `uninstall` removes only
files that still carry cahoots' name.
