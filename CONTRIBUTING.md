# Contributing

Thanks for looking. This is a one-maintainer project with strong opinions
written down; this page is the short version. The long version is `AGENTS.md`
(the hard rules) and `docs/` — read `docs/THREAT-MODEL.md` before touching the
command surface.

## Setup

macOS or Linux, and [mise](https://mise.jdx.dev):

```sh
mise trust && mise run setup   # pinned tools, the crates, the git hooks
mise run gate                  # every check CI and the hooks run, in one go
```

`mise tasks` is the catalog of everything else. Builds made through mise are
*dev builds* (`cahoots --version` says so): they honour the `CAHOOTS_*_DIR`
overrides the tests use. Nothing a person installs does.

The git hooks (lefthook) format what you stage, check the commit message, and
run the suite before a push. Two of them validate `daft.yml`, so a push — or a
commit touching the hook/mise configuration — needs
[daft](https://github.com/avihut/daft): `brew install avihut/tap/daft`. You
don't need daft's worktree workflow to contribute; a plain clone is fine.

You do **not** need a Claude, Codex or Gemini subscription. The suite runs
against a fake harness and never calls a real one.

## The hard rules

Harnesses run `cahoots` outside their sandbox. These rules are why that is a
reasonable thing to allow, and a PR that bends one needs to say so up front
(`AGENTS.md` has the full list; most are enforced by `scripts/guard.sh` and
`deny.toml`):

- **No network code.** None.
- **No credentials.** Never read a harness credential file; nothing
  token-shaped in code, fixtures or logs.
- **One module spawns processes**, with argv arrays built in code. Never a
  shell. Never a config value spliced into a command.
- **Data never widens authority** — not config, not learned state, not a
  callee's output.
- **Everything learned stays local.**
- **The dependency list is closed.** `#![forbid(unsafe_code)]`. No async.

## Pull requests

- **The PR title is the commit.** PRs are squash-merged and the title becomes
  the subject on `main`, which the release cut reads to choose the next
  version. Make it a conventional commit with the area as the scope —
  `fix(gate): …`, `feat(codex): …` (never `codex: …`). CI checks it.
  Commits inside the PR are yours to shape; they're squashed away.
- **Don't touch the version.** No bump in `Cargo.toml`, no `release:` commit,
  no tag — the release is cut on `main` after the merge.
- Commits must be signed (the ruleset requires it).
- Behavior gets a test. Parsing a harness's output gets a fixture test, and
  malformed input must degrade, not crash.
- Warnings are errors.
- A new check in a hook script gets a pass case **and** a refusal case in
  `scripts/test-hooks.sh`.

## Conduct

`CODE_OF_CONDUCT.md`.
