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
  `daft merge` and its push — keeps working; CI still runs on that push. 0 approvals: a one-maintainer repo has
  nobody to approve the maintainer, and for everyone else the maintainer's
  merge IS the approval. The branch need NOT be up to date with `main`: no
  conflicts is enough, so a batch of PRs lands without an update-and-rerun
  cycle each. The cost is that two PRs, each green alone, can break `main`
  together where no PR run sees it; the CI run on every push to `main`
  catches that, and a red `main` is fixed forward.
- **release tags are immutable** — a pushed `v*` tag is never moved or deleted:
  the release workflow builds from it, and a release's notes are its
  annotation. NO bypass, the release app included: a wrong release is fixed
  by the next version, never by moving a tag.
- **release tags are made by the release workflow** — creating a `v*` tag is
  refused to everyone but the Wheatley app (integration 2607344), which the
  release flow's tag job runs as. A separate ruleset from the immutable one
  because a bypass covers *every* rule of its ruleset: in that one, the app
  could move and delete tags too. No admin bypass: agents act with the
  maintainer's credentials, and an admin bypass would let any of them cut a
  release by pushing a tag. The tag job refuses to tag unless an active
  ruleset covers every `v*` tag with a creation rule.
- **release-pr is the release workflow's** — creating, pushing, force-pushing
  or deleting `release-pr` is refused to everyone but the same app. Separate
  for the same reason, and without admin bypass for the same reason: the
  release PR's branch is what a merge turns into a release, so only the
  workflow that builds it may write it. The maintain job refuses to push
  unless an active ruleset covers `release-pr` with these rules.

The app's token can't see bypass lists, so the jobs check coverage only. That
the two rulesets above let the app and nobody else bypass them, and that
`release tags are immutable` lets nobody, is the driver's audit with the
maintainer's credentials, `mise run release-rulesets-audit`
(`scripts/release-rulesets-audit.sh`), run when they are applied and before
every merge of a release PR.

`gate` and `pr-title` are spelled in four places — the ruleset, the two jobs
that report them, and the auto-merge workflow's fail-closed test.
`scripts/guard.sh` (rule 8) holds them together.
