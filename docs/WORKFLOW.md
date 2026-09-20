# Workflow — how a change lands

## One definition per check

Every check is a mise task (`mise tasks`). The git hooks (`lefthook.yml`),
daft's merge gate (`daft.yml`) and CI (`.github/workflows/ci.yml`) all call
those tasks, so a check has exactly one definition and `mise run gate` is the
whole set by hand:

| Task | What it holds |
|---|---|
| `guard` | the hard rules a grep can hold (`scripts/guard.sh`, nine of them) |
| `lint-shell` | shellcheck over `scripts/*.sh` |
| `fmt-check` | `cargo fmt --check` |
| `check-config` | `lefthook.yml`, `daft.yml`, `mise.toml` parse |
| `test-hooks` | the hook scripts' own pass **and** refusal paths, in a throwaway repo |
| `clippy` | every target, warnings denied |
| `test` | the suite; no real harness, no network — wrapped in `real-state.sh guard`, which fails it if anything a person really has (cahoots config/state, installed skill and agent files) changed |
| `deny` | advisories, licenses, sources, and the ban on network crates |
| `release-build` | the *deep* ring: what a person would install still builds |

A script and its mise task land together (`guard` rule 9).

`mise run smoke` is NOT part of the gate and never runs in CI: it makes one
tiny real run in each direction against the CLIs installed on your machine —
under YOUR configuration of them — in throwaway cahoots directories, and it
TRIES TO CROSS each fence: a reader is asked to write, and must fail. Run it
whenever you change how a harness is invoked or parsed; a command line that
looks fenced is not evidence that it is (docs/SPIKE.md S7).

## When each runs

- **pre-commit** — staged files: format, whitespace, shell lint, config,
  `guard --staged`.
- **commit-msg** — a conventional subject (`cog`, with `release` declared in
  `cog.toml`); a commit that moves `Cargo.toml`'s version must *be*
  `release: vX.Y.Z`.
- **pre-push** — clippy, the suite, deny, test-hooks, config, and
  `release-check`: a release commit being pushed is whole.
- **daft pre-merge** — all of the above plus `source-up-to-date` and
  `incoming-commits`, in the source worktree, before `main` moves.
  `daft merge --skip-tag deep` drops only the release build.
- **daft post-merge** — `landed-check`: the landed tree is the gated tree;
  `release-reminder`: unreleased `feat`/`fix` on `main` means a release is owed.
- **CI** — `mise run gate` on Ubuntu and macOS, aggregated into one check,
  plus `pr-title`.

## Branches and merges

This is a [daft](https://github.com/avihut/daft) repository in the contained
layout: `daft start <branch>` for new work, `daft go <branch>` for existing,
`daft remove <branch>` when done. Never `git checkout` / `git switch` /
`git worktree add` — only the daft paths run the hooks that set a worktree up.

Merges are squashes (`git config daft.merge.style squash`; GitHub allows
nothing else). A PR lands as one commit whose subject is the PR title, so the
title is held to the commit grammar by the `pr-title` check.

## What GitHub enforces

`.github/rulesets/` is the reviewable record; its README explains each.

- **main: integrity** — no deletion, no force-push, linear history. Nobody
  bypasses it.
- **main: PR gate** — a PR, squash only, threads resolved, signed commits,
  `gate` and `pr-title` green on an up-to-date branch. The repository admin
  bypasses it so the maintainer's local flow keeps working.
- **release tags are immutable** — a pushed `v*` tag never moves.

`gate` and `pr-title` are spelled in four places; `guard` rule 8 holds them
together. The `gate` job is an aggregate with `if: always()` and an explicit
verdict, because a required check that is *skipped* counts as green.

Repository settings that are not files: squash merges only, auto-merge on,
branches deleted on merge, Actions restricted to full-SHA pins, fork PR
workflows need approval, private vulnerability reporting on.

## Dependencies

Dependabot opens weekly PRs (7-day cooldown, like `mise.toml`'s
`minimum_release_age`). `dependabot-auto-merge.yml` arms auto-merge for
patches, and for minors except a cargo `0.x`; majors wait for a human. It
fails closed if `gate` is ever not a required check.

## Releases

Cut on `main`, by the maintainer, never on a branch and never by a PR:
`mise run release` makes a `release: vX.Y.Z` commit that carries the version
bump, and a signed annotated tag whose annotation is the release notes. It
never pushes — `git push origin main vX.Y.Z` is a person's step, because a
pushed `v*` tag can never move and it is what starts the release workflow
(cargo-dist: binaries, the GitHub release, the Homebrew formula).
`RELEASING.md` is the whole ritual.
