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

You need macOS or Linux, git 2.46 or newer (or a May 2024 security release
such as 2.39.4 or 2.45.1), Claude Code and Codex installed and signed in, and
Rust 1.95 or newer:

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
cahoots settings        # see and change every setting
```

`install` adds cahoots' skills and a delegate agent to Claude Code and Codex,
plus one subagent for each [task kind](#task-kinds) you define,
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
before you enable one. `install`, `enable`, `settings` and the other commands
that change what cahoots may do run only from a terminal, so an agent can't
run them for you.

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

The output names the worktree (`data.worktree`). It is cut without the
repository's hooks, so the repository's setup has not run there: run it
yourself before the tests. Read the change with `git -C <worktree> diff`, run
the tests there, and bring over what you want. cahoots never commits or
merges for you. `cahoots status <run>` shows the commit the worktree was cut
at (`data.base_commit`), so `git -C <worktree> diff <base_commit>` includes
anything the writer committed there too. In a
[daft](https://github.com/avihut/daft) repository the worktree is cut with
`daft start --fork`, with daft's hooks skipped too.

Each command an agent runs prints one JSON object and exits with a code that
means something (`cahoots exit-codes` lists them), so neither your agent nor
your scripts have to parse prose. The commands you run yourself, such as
`install`, `settings`, `doctor` and `report`, answer you in words at a
terminal. Pipe one, as in `cahoots install | jq`, and it prints the same
JSON instead.

## Configure

`cahoots settings` shows every setting, what it is now and where that comes
from. Move with the arrow keys, press Enter on one, and change it in the box
that opens: pick from a list, step a number with ← and →, or put a role's
agents in order. Enter saves it at once, and Esc closes the box, then the
page.

Settings live in `~/.config/cahoots/config.toml`, and that file stays yours.
cahoots changes only the setting you changed, keeps your comments and the
rest of your layout, and never writes a value the file couldn't hold; putting
a setting back to its default takes it out of the file. Edit the file by hand
whenever you like. Definitions of task kinds are optional, but every field
inside one is required. `cahoots registry` shows what
is in effect. Without the page:

```sh
cahoots settings set harness.codex.cap 80     # in the file's own units
cahoots settings reset harness.codex.cap      # back to the default
```

```toml
schema = 1

[harness.codex]
enabled = true   # what `cahoots enable codex` writes
cap = 80         # start a run only while Codex is under 80% of its plan (default 75)
abort_at = 92    # stop a running one that goes past 92% (default: cap + 10)

[harness.claude]
enabled = true
cap = 50

[meter]
use = "ccusage"            # the usage meter: agent-usage, ccusage or none

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

### Task kinds

Define a recurring task in config.toml with its own role and exact candidate
order. This list can differ from the role's list; only its candidates are
tried, including different efforts of the same model:

```toml
[kinds.rust-review]
description = "Review Rust changes for correctness and maintainability."
role = "review"
candidates = [
  { harness = "codex", model = "gpt-5.6-sol", effort = "high" },
  { harness = "codex", model = "gpt-5.6-sol", effort = "medium" },
  { harness = "claude", model = "opus", effort = "high" },
]
```

```sh
cahoots pick --kind rust-review --caller claude
cahoots run --kind rust-review --caller claude --brief review.md
cahoots run --kind rust-review --role review --to codex --brief review.md
```

`--role` is optional with `--kind`; when supplied it asserts the configured
role and must match exactly. `--to` narrows the kind's list to that harness,
never falls back to the role's list. The caller exclusion, enabled targets,
gate and slots apply as usual. A kind with `role = "implement"` requires
`--fork`, or `--in-place` only when `limits.allow_in_place` permits it; reader
kinds refuse placement flags.

Names are 1–64 ASCII characters, start with a lowercase letter and contain
only `[a-z0-9_-]`. Matching is exact. Descriptions must be nonblank, one line,
at most 1024 Unicode characters and contain no control characters or Unicode
line/paragraph separators. Description text is preserved as metadata and
never added to argv or a brief. Candidate lists must be nonempty, with no
exact duplicate harness/model/effort triples. Unknown fields are refused.

Each kind also becomes a subagent in Claude Code and Codex,
`cahoots-kind-<name>`, described in your words, so your agent hands a
matching task over by itself. `cahoots install` writes them: run it again
after you add, change or remove a kind, and `cahoots doctor` tells you when
it is due. A harness gets no subagent for a kind whose candidates are all on
that harness, since an agent never delegates to itself.

Create, rename or remove a kind by editing its complete table in config.toml.
No kinds ship by default. Description, role and candidates inside each
one are all required, have no defaults and cannot be reset individually.
For an existing definition:

```sh
cahoots settings set kinds.rust-review.description 'Review Rust for correctness.'
cahoots settings set kinds.rust-review.role review
cahoots settings set kinds.rust-review.candidates 'codex:gpt-5.6-sol:high,codex:gpt-5.6-sol:medium'
```

The settings page and registry show each kind after Roles, sorted by name.
The page displays the description with its CLI edit route, chooses the role
and reorders candidates with their efforts visible.

Pick, run records and all run summaries retain the selected `kind`, or null
for role-only runs. Finished history events retain `"kind":"finished"` and
use `task_kind` for the task's label; folded stories use `kind`. Older records
without a label read as none. Resume keeps the original kind, role and chosen
candidate even after a definition is removed, renamed or redefined. Kind
lists are not calibrated yet, and their runs do not feed role calibration;
ordinary role report totals still include them.

`cahoots report` includes role totals and groups by recorded kind, role, and
full candidate. Each current kind candidate appears even without runs. Rates
use rated-or-failed evidence (a result you recorded with `outcome`, or a run
that failed by itself), with standard errors; below eight observations the
terminal says "not enough evidence". JSON keeps defined estimates and their
sample size. `cahoots doctor` warns about current kind-list and
person-supplied role-list candidates with no such evidence on record.

### Exploration

When one of your agents asks for help, the agent's own harness is left out of
the list, so a role's later entries on the other harness may never get a turn
and never earn a result you can learn from. Exploration gives a share of new
runs to the next listed candidate. It is off until you set a share, per role
or per task kind (a fraction from 0 to 1):

```toml
[explore]
share = { advise = 0.1, review = 0.05 }

[kinds.rust-review]
description = "Review Rust."
role = "review"
candidates = [
  { harness = "codex", model = "gpt-5.6-sol", effort = "high" },
  { harness = "codex", model = "gpt-5.6-sol", effort = "medium" },
]

[kinds.rust-review.explore]
share = 0.2
```

```sh
cahoots settings set explore.share.advise 0.1
cahoots settings set kinds.rust-review.explore.share 0.2
cahoots settings reset kinds.rust-review.explore.share   # inherit the role's again
```

A role you leave out explores nothing. A kind takes its role's share unless
it has its own, and its own `0` turns exploration off for it. For a selected
run, the first two candidates left after the caller and disabled targets are
removed swap places, and then every candidate is checked as usual: its slot,
binary and usage cap still apply, and one that is refused is skipped like
any other (the run is then not an exploration). There is no exploring with
`--to` or in a `resume`, with one candidate left, or when the first two are
the same model and effort. Efforts of one model count as different. For a
second model or effort to be tried on the other harness, list it yourself;
cahoots ships no such default.

The run records `exploration: true` or `false`, in its record and in
history, and shows it in its summaries — except in a [blind run](#learning-from-your-own-results)
until you record an outcome for it. `pick` stays a preview of the ordinary
first choice and adds `exploration_share`; a run that follows may try the
next candidate. Results from explored runs count towards role calibration
like any other.

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

`install` looks for both. It uses the one it finds, or asks you when it finds
both, and saves your answer in `config.toml` as `use` under `[meter]`. You
pick with the arrow keys and Enter; Esc leaves without writing anything.
Until you choose, the one it found is used, and it looks again each time you
run `cahoots install`, asking once it finds both. Once you've chosen, it
doesn't ask again. To choose without being asked, or to change your mind,
pick the meter in `cahoots settings`, or run `cahoots install --meter
agent-usage` (or `ccusage`, or `none`), with `--meter-binary <path>` if
install can't find it. A `[meter.ccusage]` section only holds ccusage's
settings: `use` is what turns a meter on.

## Learning from your own results

Set `[review] blind = true` (off by default) to judge results before seeing
their model and effort. Each new run, including a resume, keeps its launch-time
policy. Run envelopes and `review next` mark withheld identity with
`blind: true` and omit model and effort; run envelopes also omit
`model_reported` and the run's `exploration` label. After an honest `outcome` for that run, subsequent
`status`/`result` views restore identity with `blind: false`. This works
independently of review being enabled. The harness remains visible; `pick`,
aggregate reports, config, answer and brief text, diagnostics, known kinds and
related runs may still disclose identity. No content is rewritten, and this
is not anonymity or access control. Change it on the settings page or with
`cahoots settings set review.blind true`; reset returns to false.

Review is off unless you set `[review] enabled = true`. Then, for about one run
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
