# cahoots

[![CI](https://github.com/avihut/cahoots/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/avihut/cahoots/actions/workflows/ci.yml)
![macOS | Linux](https://img.shields.io/badge/platform-macOS%20%7C%20Linux-blue)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue)](#license)

Let your coding agents ask each other for help. From Claude Code, get Codex's
opinion on a design; from Codex, have Claude Code review a diff. Your agent
hands the task to `cahoots`, which picks the other agent, the model and the
effort level, checks the usage limits you set, runs it in the background and
brings back the answer.

```
claude ──┐                       ┌──► codex exec …
         ├──►  cahoots run  ──►──┤
codex  ──┘   pick · check · run  └──► claude -p …
```

> **Pre-release.** It works end to end with Claude Code and Codex, but no
> version has been released yet, so for now you build it from source.

## What you can ask for

| Role | Ask for | The other agent may |
|---|---|---|
| `advise` | a second opinion on an approach, a design or a decision | read |
| `review` | what is wrong with a change or a piece of code | read |
| `explore` | a read of part of the codebase, reported back | read |
| `implement` | a change, for you to review | write, in a worktree of its own |

The agent that asks is never the one picked, so the same setup works in both
directions.

## Install

You need macOS or Linux, git, Claude Code and Codex installed and signed in,
and Rust 1.95 or newer:

```sh
cargo install --git https://github.com/avihut/cahoots --locked
```

If mise manages your Rust, run it through `mise exec` instead:

```sh
mise exec rust -- cargo install --git https://github.com/avihut/cahoots --locked
```

Through mise's shims the first form fails with `Config files in
~/.cargo/git/checkouts/cahoots-…/mise.toml are not trusted`: cargo's copy of
this repository carries the project's `mise.toml`, and the shims won't build
under a config you haven't trusted. `mise exec` runs the toolchain directly.

Prebuilt binaries and a Homebrew formula come with the first release.

## Set up

```sh
cahoots doctor          # what it found, and what is still missing
cahoots install         # teach Claude Code and Codex to use cahoots
cahoots enable codex    # let cahoots send work to Codex
cahoots enable claude   # …and to Claude Code
```

`install` adds cahoots' skills and a delegate agent to Claude Code and Codex,
and looks for a [usage meter](#usage-meters): if it finds one it uses it, and
if it finds more than one it asks you which. It also prints the permission
rules that let each agent call cahoots without asking you every time. It
never edits their settings: add the rules yourself, Claude Code's to
`~/.claude/settings.json` and Codex's to `~/.codex/rules/default.rules`, then
run `cahoots doctor` again to check them. If you run Claude Code with its
sandbox on, also list `cahoots` in `sandbox.excludedCommands`, because a run
has to reach the other agent's vendor.

Every agent starts switched off as a target, because a run sends your code to
that agent's vendor. Read [where the vendors stand](#your-accounts-and-the-vendors-terms)
before you enable one. `install`, `enable` and the other commands that change
what cahoots may do run only from a terminal, so an agent can't run them for
you.

## Use it

From your agent, just ask: *"get a second opinion from another agent on this
plan"*, *"have Codex review my last commit"*, *"ask Claude to find where we
parse the config"*. The skill tells your agent how to write the brief, which
role to use, and what to do with the answer.

From a terminal:

```sh
cahoots pick --role advise --caller claude    # who would get it; nothing runs
cahoots run --role advise --caller claude --brief brief.md
cahoots status                                # the runs started here
cahoots result <run>
cahoots outcome <run> accepted                # or reworked, or discarded
```

`--caller` says who is asking, so that agent isn't picked; inside Claude Code
or Codex it is detected. The brief is a file in your repository or a temp
directory.

A run carries on in the background. `run` waits 90 seconds for the answer,
then hands back a run id you can `wait` on, check with `status` or `cancel`,
so a slow answer never times out your agent's tool call. A run is stopped
after 30 minutes, or sooner with `--timeout`.

A change comes back in a worktree of its own, cut from your `HEAD`:

```sh
cahoots run --role implement --caller claude --fork --brief change.md
```

The output names the worktree (`data.worktree`). Read the change with
`git -C <worktree> diff`, run the tests there, and bring over what you want.
cahoots never commits or merges for you. In a
[daft](https://github.com/avihut/daft) repository the worktree is cut with
`daft start --fork`.

Each command an agent runs prints one JSON object and exits with a code that
means something (`cahoots exit-codes` lists them), so neither your agent nor
your scripts have to parse prose.

## Configure

Settings live in `~/.config/cahoots/config.toml`. All of it is optional, and
`cahoots registry` shows what is in effect.

```toml
schema = 1

[harness.codex]
cap = 80         # start a run only while Codex is under 80% of its plan (default 75)
abort_at = 92    # stop a running one that goes past 92% (default: cap + 10)

[harness.claude]
cap = 50

[meter.ledger]
max_runs_per_hour = 12     # per agent (the default)

[roles.review]             # your own order for a role, first choice first
candidates = [
  { harness = "claude", model = "opus", effort = "high" },
  { harness = "codex", model = "gpt-5.6-sol", effort = "high" },
]

[limits]
timeout_secs = 1800        # how long a run may take (the default)
```

Before every run, cahoots asks its meters, and refuses with the reason if one
says no:

- **The ledger** is built in and always on. It counts runs per hour per agent
  from cahoots' own records.
- **The usage meter** says how much of each plan you've used, so that `cap`
  and `abort_at` mean something. It is a tool you already run; see below.

### Usage meters

cahoots reads your usage from one of these:

- **[Agent Usage](https://github.com/avihut/coding-agent-usage-tracker)**
  meters Claude Code's and Codex's plan limits as the vendors report them, so
  `cap = 80` means 80% of the plan. cahoots needs its `usage-cli headroom`
  command, which hasn't been released yet; until it is, `install` finds
  Agent Usage but doesn't offer it.
- **[ccusage](https://github.com/ccusage/ccusage)** (20 or newer) counts the
  tokens in Claude Code's and Codex's own logs. It can't see your plan's
  limit, so you say how many tokens make a whole plan, and `cap` and
  `abort_at` become percentages of that:

  ```toml
  [meter.ccusage]
  claude_block_tokens = 300_000_000   # a whole plan, in one 5-hour block of Claude Code
  codex_day_tokens = 60_000_000       # …and in one day of Codex
  ```

  Until you set them, a run to Claude Code is refused only when Claude Code's
  own log says it has hit its limit, and Codex isn't metered at all.
  `cahoots doctor` shows the counts so far, to size them by. cahoots always
  runs ccusage offline.

`install` picks the one it finds, or asks you when it finds both, and
remembers your answer. You pick with the arrow keys and Enter; Esc leaves
without writing anything. If it found only one, it looks again each time you
run `cahoots install`, and asks once it finds both. To change your answer, or
to choose without being asked, run `cahoots install --meter agent-usage` (or
`ccusage`, or `none`), and add `--meter-binary <path>` if install can't find
it. A `[meter.<name>]` section in `config.toml` wins over what install picked,
and while there is one, install doesn't ask.

## Learning from your own results

This is off unless you set `[review] enabled = true`. Then, for about one run
in five, the agent that delegated it is asked to review it, choosing from a
fixed list of possible findings. When reviews of two different runs in two
different directories agree, your agents see a short note about it before
they write their next brief. Reviews spend a little of the reviewing agent's
plan.

What you record with `cahoots outcome` can also reorder a role's choices, by
one place at most. `cahoots report --suggest` shows what it would change, and
nothing changes until you set `apply_routing = true` under `[review]`. An
order you wrote under `[roles]` stays as you wrote it unless you add
`calibrate = true` to that role.

All of it stays on this machine. From a terminal, `cahoots learn list` shows
what reviews have taught it, and `cahoots learn reset` forgets that.

## What cahoots will and won't do

- **Readers can't change anything.** For `advise`, `review` and `explore`,
  Claude Code gets only its read and search tools, and Codex runs in its
  read-only sandbox with no way to ask for more.
- **Writers work somewhere else.** `implement` never touches your working
  tree. Codex writes only in its worktree and the temp directories, with no
  network unless your own Codex settings allow it. Claude Code edits files
  there but can't run commands, so run the tests yourself.
- **Your agents' settings stay yours.** cahoots never edits Claude Code's or
  Codex's settings or permission files, and never reads their credentials.
  Codex does one thing on its own: it marks each worktree a Codex writer
  works in as trusted in `~/.codex/config.toml`. Those entries are safe to
  delete once the worktree is gone.
- **Nothing leaves your machine except the runs.** There is no telemetry, no
  update check and no network code at all. A run shows the other agent your
  brief and your repository, and that agent's vendor sees what it reads, just
  as if you had used that agent yourself.
- **Your agent can't give itself more.** The commands an agent may run can't
  raise a limit, enable a target, loosen a sandbox or change a setting.

cahoots guards against an agent that is sandboxed, or behind permission
prompts, using it to get around them. It can't protect you from an agent you
have already given an unrestricted shell. [The threat
model](docs/THREAT-MODEL.md) is the long version.

## Your accounts and the vendors' terms

A run drives one of your subscriptions from a script. Here is where each
vendor stands (sources and dates are in
[docs/VENDOR-TERMS.md](docs/VENDOR-TERMS.md)):

- **Codex: supported.** OpenAI documents scripted `codex exec` under a
  ChatGPT sign-in, and invites other software to drive Codex.
- **Claude Code: supported, with caveats.** Scripted `claude -p` is
  documented and counts against your plan. But Anthropic's terms allow
  automated access only where it explicitly permits it, and it prefers API
  keys for third-party tools.
- **Antigravity: not supported.** Google's terms call using third-party
  software to access the service a breach, and accounts have been banned.

Every vendor's preferred route for programmatic use is an API key. Set
`billing = "api"` on an agent and cahoots passes the key through; that agent
then bills per token, and plan caps no longer apply to it.

## Uninstall

```sh
cahoots uninstall                                  # what install added, and nothing else
rm -rf ~/.config/cahoots ~/.local/state/cahoots    # settings, run records, what was learned
cargo uninstall cahoots
```

Run `uninstall` before the `rm`: the list of what `install` wrote lives in
the state directory. A skill file you made your own by deleting its
`cahoots_version` line is left alone. Then take out the permission rules you
added.

## More

[How it works](docs/ARCHITECTURE.md) · [Threat model](docs/THREAT-MODEL.md) ·
[Vendor terms](docs/VENDOR-TERMS.md) · [Contributing](CONTRIBUTING.md) ·
[Security](SECURITY.md)

Unofficial and independent: not affiliated with or endorsed by Anthropic,
OpenAI or Google. Claude Code, Codex and Antigravity are their owners'
trademarks.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option. Unless you explicitly state
otherwise, any contribution intentionally submitted for inclusion in this work
by you, as defined in the Apache-2.0 license, shall be dual licensed as above,
without any additional terms or conditions.
