# cahoots

Lets your coding agents work **in cahoots**: whichever harness you are working
in — Claude Code or Codex — can hand a task to the other as an advisor, a
reviewer, an explorer or a worker, the same way in either direction, without
burning out either subscription.

> **Status: pre-release.** The broker, the usage gate and the installer work,
> and `mise run smoke` proves them against the real CLIs — but no version has
> been released yet, so for now it is built from source (below).
> `docs/ARCHITECTURE.md` is the design, `docs/SPIKE.md` is what the real CLIs
> said about it, `docs/THREAT-MODEL.md` is what it defends against, and the
> milestones are at the bottom of this page.

## What it is

A harness never calls another harness's CLI directly. It calls `cahoots`:

```
claude ──┐                       ┌── codex exec …
codex  ──┼──►  cahoots run  ──►──┼── claude -p …
agy    ──┘   pick · gate · run   └── agy -p …
```

- **Pick.** Each harness and its models are defined once. For a role
  (`advise`, `review`, `explore` — read-only — or `implement`, which writes in
  a worktree of its own) cahoots chooses the target, the model and the effort
  level — and simply leaves out whoever is asking.
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
  the plan allows. It never touches a credential or a token, and it never
  talks to a vendor's servers.
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

## Trying it (from source, until the first release)

```sh
git clone https://github.com/avihut/cahoots && cd cahoots
cargo install --path . --locked     # a normal build — never a dev build
cahoots doctor                      # what is there, and what is missing
cahoots install                     # the skill + agent definitions; prints the
                                    # permission rules for YOU to add
cahoots enable codex                # each target is off until you say so
```

Caps and the usage meter go in `~/.config/cahoots/config.toml`:

```toml
schema = 1
harness.codex.cap = 80      # delegate to Codex while it is under 80% of its plan
harness.codex.abort_at = 92 # …and stop a run in flight if it crosses 92% (default: cap + 10)
harness.claude.cap = 50

[meter.agent-usage]         # optional; without it only the built-in ledger gates
binary = "/Applications/AgentUsage.app/Contents/MacOS/usage-cli"
```

## Where this stands with the vendors' terms

Read this before you enable a target; the account at risk is yours.
**`docs/VENDOR-TERMS.md`** has the sources, quotes and dates.

- **OpenAI (Codex):** scripted `codex exec` under your own ChatGPT sign-in is
  documented, and OpenAI invites other software to drive Codex. Supported.
- **Anthropic (Claude Code):** scripted `claude -p` under your own
  subscription is documented and currently counts against your plan — but the
  terms permit scripted access only "where we otherwise explicitly permit
  it", Anthropic prefers API keys for third-party tools and reserves the right
  to bill such use separately, and it has announced changes (now paused) that
  would alter this. Supported, with those caveats.
- **Google (Antigravity CLI):** the terms call using third-party software to
  access the service a breach, and accounts have been banned. **Not
  supported** under a personal sign-in until Google says otherwise in writing.

Every vendor's stated preference for programmatic use is an API key;
`billing = "api"` is that route.

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
| **M6** | Antigravity CLI — **on hold**: Google's terms, not a technical reason (`docs/VENDOR-TERMS.md`) |

## Contributing

`CONTRIBUTING.md` is the short version; `AGENTS.md` is the contract, for people
and agents alike. Security reports: `SECURITY.md`.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this work
by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
