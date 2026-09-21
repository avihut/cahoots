# cahoots — the contract for anyone (or anything) changing this repo

cahoots is a broker. A coding-agent harness (Claude Code, Codex, Antigravity
CLI) that wants another harness's help calls `cahoots`, never the other
harness's CLI. cahoots picks the target, checks that the target's subscription
has headroom, runs the target's headless CLI under a detached supervisor, and
records the run. It is a small, synchronous Rust CLI with no network code.

Read `docs/ARCHITECTURE.md` for the design, `docs/THREAT-MODEL.md` before you
touch the command surface, and `docs/WORKFLOW.md` for how a change lands.

## Hard rules

A harness allows `cahoots` to run **outside its sandbox, without a prompt**.
That is the trust this project has to deserve, and these rules are how. A PR
that bends one says so up front. Most are held by `scripts/guard.sh` and
`deny.toml`; the rule stands where the grep can't reach.

1. **No network code.** No telemetry, no update check, no fetch. cahoots
   reaches the world only by running a harness CLI the user already trusts.
   `deny.toml` bans the crates; `guard.sh` bans the std types.
2. **No credentials, ever.** Never read a harness credential file
   (`auth.json`, `.credentials.json`, `oauth_creds.json`, a keychain). Login
   state comes from the harness CLI's own status command. No token-shaped
   string in code, fixtures or logs.
3. **One module spawns processes** — `src/spawn`. It owns the scrubbed
   environment, argv validation and process groups. Commands are argv arrays
   built in code from typed values; nothing is ever run through a shell, and
   no configuration or learned value is ever spliced into a command line.
4. **Agent-home paths appear only in `src/install`; the environment is read
   only in `src/env.rs`.** The user's home comes from the passwd database,
   not `$HOME`.
5. **Writes stay home.** cahoots writes under its own state/config/data
   directories, and — only through `install`, only files listed in its
   manifest — the skill and agent definitions. `install` never edits a
   harness's permission or settings files; it prints the rule for a human to
   add.
6. **Data never widens authority.** Config, learned adjustments and a callee's
   output can reorder candidates and add briefing notes. They cannot change a
   cap, a reserve, a sandbox mode, a flag or a command. Learned state
   deserialises into a struct that simply has no such fields.
7. **Everything learned stays on this machine.** No sharing, no sync, no
   upload. Shipped defaults change only through human PRs.
8. **Tests never call a real harness, never touch the network, and never
   touch a person's real state** — their cahoots config and state, or what
   `install` puts in their agent homes. A fake harness binary stands in, and
   every test works in throwaway directories. This is held four ways: a unit
   test cannot resolve the real directories at all; a dev build refuses them
   unless `CAHOOTS_DEV_REAL_DIRS=1`; policy is tested through pure functions
   (`cli::refusal`), never by running a verb; and `scripts/real-state.sh`
   fingerprints the real paths around the whole suite and fails on any
   change. `mise run smoke` is the one exception to "no real harness", is
   local only, and says what it costs.
9. **The dependency list is closed** (`Cargo.toml`, held by `guard.sh`). A new
   crate is a change to this file first.
10. **`#![forbid(unsafe_code)]`**, synchronous code (`std::process`, threads,
    one channel — no async runtime), Unix only.

The agent tier of verbs (`pick run resume wait status result cancel outcome
notes review`) is what harnesses are told to allow. Adding a verb or a flag to that
tier is a threat-model change: update `docs/THREAT-MODEL.md` in the same PR.

Exit codes are API (`src/exit.rs`, `cahoots exit-codes`). Codes 11–26 belong
to the Agent Usage tracker's `usage-cli`; cahoots reuses its five `headroom`
codes with their meanings and numbers its own from 30. Never renumber.

## Working here

- `mise run check-all` before you show work. It is exactly what the hooks,
  daft's merge gate and CI run. `mise tasks` is the catalog; **a script and its mise
  task land together**.
- This is a [daft](https://github.com/avihut/daft) repository: `daft start
  <branch>` for new work, `daft go <branch>` for existing, never `git
  checkout` / `git switch` / `git worktree add`.
- Changes arrive as PRs. **The PR title is the commit** (squash merge) and
  must be a conventional commit with the area as scope: `fix(gate): …`.
- Never touch the version on a branch. No `release:` commit, no tag.
- Commit only when the person you are working for asks. Commits and tags are
  signed; if signing fails, stop and say so — never disable it.
- Warnings are errors, everywhere (`scripts/no-warnings.sh`).
- A gate nobody has seen fail is a gate nobody knows works: a new check in a
  script gets a pass case **and** a refusal case in `scripts/test-hooks.sh`.
