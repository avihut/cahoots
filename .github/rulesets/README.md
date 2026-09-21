# Repository rulesets, as reviewable text

GitHub holds the live copy; these files are the record of what it should say.
Apply one with `gh` (create), or `PUT …/rulesets/<id>` to update an existing
one — `gh api repos/{owner}/{repo}/rulesets` lists the ids:

```sh
gh api -X POST repos/{owner}/{repo}/rulesets --input .github/rulesets/main-integrity.json
```

- **main: integrity** — no deletion, no force-push, linear history. NO bypass,
  the owner included: these exist to stop a slip (or an agent's), and a rule
  its only pusher can bypass stops nothing. Rewriting `main` means disabling
  the ruleset on purpose, in the UI.
- **main: PR gate** — a PR, squash only, threads resolved, the `gate` and
  `pr-title` checks (pinned to GitHub Actions as their source) green, signed
  commits. The repository admin bypasses it, so the maintainer's local flow —
  `daft merge`, the release commit, `git push origin main vX.Y.Z` — keeps
  working; CI still runs on that push. 0 approvals: a one-maintainer repo has
  nobody to approve the maintainer, and for everyone else the maintainer's
  merge IS the approval. The branch need NOT be up to date with `main`: no
  conflicts is enough, so a batch of PRs lands without an update-and-rerun
  cycle each. The cost is that two PRs, each green alone, can break `main`
  together where no PR run sees it; the CI run on every push to `main`
  catches that, and a red `main` is fixed forward.
- **release tags are immutable** — a pushed `v*` tag is never moved or deleted:
  the release workflow builds from it, and a release's notes are its
  annotation. NO bypass. Re-cut a release BEFORE pushing its tag, never after.

`gate` and `pr-title` are spelled in four places — the ruleset, the two jobs
that report them, and the auto-merge workflow's fail-closed test.
`scripts/guard.sh` (rule 8) holds them together.
