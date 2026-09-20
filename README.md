# cahoots

Lets your coding agents work **in cahoots**: any harness you are working in —
Claude Code, Codex, Antigravity CLI — can hand a task to one of the others as
an advisor or a worker, the same way in every direction, without burning out
any of your subscriptions.

> **Status: pre-release.** The broker works — Claude Code ⇄ Codex, read-only
> roles, gate, run records (`mise run smoke` proves it against the real CLIs)
> — but there is no installer and no release yet. `docs/ARCHITECTURE.md` is the design, `docs/SPIKE.md` is what the real
> CLIs said about it, and the milestones are at the bottom of this page.

## What it is

A harness never calls another harness's CLI directly. It calls `cahoots`:

```
claude ──┐                       ┌── codex exec …
codex  ──┼──►  cahoots run  ──►──┼── claude -p …
agy    ──┘   pick · gate · run   └── agy -p …
```

- **Pick.** Each harness and its models are defined once. For a role
  (`advise`, `review`, `implement`, …) cahoots chooses the target, the model
  and the effort level — and simply leaves out whoever is asking.
- **Gate.** Before a run, cahoots asks how much of the target's plan is used
  and refuses above a cap *you* set per harness (say 50% for one, 80% for
  another), keeping a reserve for the run itself. Stale or missing numbers
  fail closed. Usage comes from
  [Agent Usage](https://github.com/avihut/coding-agent-usage-tracker)'s
  `usage-cli`, or from a built-in ledger if you don't run it.
- **Run.** The target's first-party headless CLI runs under a detached
  supervisor with a scrubbed environment, a timeout and a cancel path. The
  caller gets a run id, and the result when it's done.
- **Learn, locally (opt-in).** The harness that delegated a run can review a
  random sample of its own delegations. What it learns — briefing notes,
  small routing adjustments — stays on your machine, because what works for
  you is not what works for someone else.

Every exit is a documented code plus one line of JSON, so an agent never
parses prose: `cahoots exit-codes`.

## What it is not

- **Not a way around anyone's limits.** It drives each vendor's own CLI, signed
  in as you, under your own plan — and its whole point is to use *less* than
  the plan allows.
- **Not a network service.** cahoots has no network code at all: no telemetry,
  no update check. `deny.toml` bans the crates.
- **Not affiliated** with Anthropic, OpenAI or Google. Claude Code, Codex and
  Antigravity are their owners' trademarks.

## The safety boundary, stated honestly

For a harness to delegate without a prompt on every call, you allow a handful
of `cahoots` verbs to run outside that harness's sandbox. From then on those
verbs *are* that harness's unsandboxed capability, and cahoots is built
around that fact: commands are assembled in code from typed values and
validated before they run, read-only roles are forced read-only, a target is
disabled until you enable it (a run sends repository content to another
vendor), verbs that change what cahoots may do refuse to run without a
terminal, and `install` never edits a harness's permission files — it prints
the rule for you to add.

cahoots defends against a sandboxed or permission-gated agent using it as an
escape hatch. It cannot defend you from an agent you have already given an
unrestricted shell. `docs/THREAT-MODEL.md` is the long version.

## Platforms

macOS and Linux. No Windows.

## Milestones

| | |
|---|---|
| **M0** | Repository, gates, CI; a spike against the real CLIs (`docs/SPIKE.md`) |
| **M1** | The broker: Claude Code ⇄ Codex, read-only roles, gate, run records |
| **M2** | `cahoots install` — the skill and agent definitions, per harness |
| **M3** | v0.1: binaries, Homebrew, crates.io |
| **M4** | Writer roles in a private worktree, resume, a mid-run usage watchdog |
| **M5** | Review and local learning |
| **M6** | Antigravity CLI |

## Contributing

`CONTRIBUTING.md` is the short version; `AGENTS.md` is the contract, for people
and agents alike. Security reports: `SECURITY.md`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this work
by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
