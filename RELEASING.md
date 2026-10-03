# Releasing

A release is the merge of the release PR. Nobody cuts one at a terminal: the
release workflow (`.github/workflows/release-flow.yml`) builds the release
commit, keeps it in a PR, and tags it once that PR is merged. No branch, no
other PR and no person makes a `release:` commit or a `v*` tag — the hooks,
the `pr-title` check and the rulesets refuse each.

## What a release is

One commit, `release: vX.Y.Z`, that carries

- the version, in `Cargo.toml` and `Cargo.lock` (this package's line only);
- a new top section in `CHANGELOG.md`, `## vX.Y.Z — <date>`, whose body is
  the release's notes, title line first;
- the spent notes fragment (`.release-notes/next.md`, reset to its template);

and one **annotated, unsigned tag** `vX.Y.Z` on that commit, whose annotation
is the `CHANGELOG.md` section: its first line is the tag's subject. Pushing
the tag starts `.github/workflows/release.yml` (cargo-dist): it builds
`cahoots` for macOS and Linux (x86_64 and aarch64), attests each archive's
build provenance, creates the GitHub release with the archives, checksums and
a shell installer, and pushes `Formula/cahoots.rb` to `avihut/homebrew-tap`.

The version is chosen from the conventional-commit subjects on `main` since the
last tag — which is why a PR title is held to that grammar: `feat` → minor,
`fix` → patch, a breaking change (`!` or `BREAKING CHANGE`) → major (a
*minor* while the major is 0: calling something 1.0 is a decision, not a side
effect). `release`, `chore`, `ci`, `docs`, `test`, `refactor` and `style`
release nothing on their own.

The notes are `.release-notes/next.md` when a PR wrote one (first line: a
short title; the template's comment is stripped), and otherwise a title line,
`cahoots X.Y.Z`, then the `feat`/`fix` (and other releasable) subjects since
the last tag.

## The flow

On every push to `main`, `release-flow.yml` runs two jobs, one after the
other, in the `release` environment and as the Wheatley app:

1. **`tag-merged-release`** looks for a `release: vX.Y.Z` commit on `main`'s
   first-parent history whose tag doesn't exist yet
   (`scripts/release.sh pending-tag`) — the merge of the release PR. It asks
   GitHub's API which PR merged that commit, and `scripts/release.sh tag`
   tags it only if every check holds: the subject is a release subject for
   the version the commit sets, the commit moves the version, it is on
   `main`, it was merged from this repository's `release-pr`, the PR was
   opened by the release bot, the `(#N)` in the subject is that PR, and
   `CHANGELOG.md` there has the section. Then it pushes the tag.
2. **`maintain-release-pr`** asks `scripts/release.sh plan` for the next
   version. If there is one, it rebuilds `release-pr` as `main` plus one
   release commit (`release.sh commit`), force-pushes it, and opens or
   updates the PR `release: vX.Y.Z` with `release.sh pr-body` as its body. If
   there is none, it closes the release PR and deletes the branch.

The release PR runs the whole gate like any PR. `pr-title` also checks that it
is the bot's, from this repository's `release-pr`, titled exactly
`release: vX.Y.Z`, moves the version to that, and changes nothing but
`Cargo.toml`'s and `Cargo.lock`'s version lines, `CHANGELOG.md` and the notes
fragment.

## Releasing

**Merge the release PR at its head** — that is the whole act. The release
button on the desk does it, after checking that the PR is fresh (its base is
`main`'s tip) and green, and after the driver's audit of the release rulesets
passes (below):

```sh
gh pr merge <N> --squash --match-head-commit <head sha> --body-file <the CHANGELOG section>
```

The squash subject is GitHub's default, `release: vX.Y.Z (#N)`. A PR merged
while `main` had just moved ships commits that its changelog doesn't list, so
don't merge a release PR whose base isn't `main`'s tip: wait for the workflow
to rebuild it.

**The rulesets audit.** The workflow's own token can see whether an active
ruleset covers `v*` tags and `release-pr` with the right rules, and its jobs
refuse to push when none does. It can't see a ruleset's bypass list, so it
can't tell whether anyone besides the app may bypass it. That is checked with
the maintainer's credentials — by the driver, when the rulesets are applied
and again right before every merge of a release PR:

```sh
mise run release-rulesets-audit
```

It fails unless both rulesets are active, cover their refs (inclusions minus
exclusions) with the rules their records name, and let exactly the Wheatley
app (integration 2607344) bypass them.

Before clicking, check the Homebrew formula dist would publish:

```sh
mise run formula
```

It renders `cahoots.rb` under `target/distrib` with dist and checks it against
`Cargo.toml`: name, description, homepage, license, the four archives and the
binary. It builds no archive, so the checksums in the render are
placeholders. Read the render too, and compare it with `Formula/daft.rb` in
`avihut/homebrew-tap`. A fault is fixed in `dist-workspace.toml` or
`Cargo.toml`, never in the render.

`mise run release -- plan` prints the verdict by hand: the next version on
stdout, the reason on stderr. The other steps of `release.sh` are the
workflow's; `commit` refuses to run anywhere but on `release-pr`.

Write the notes first if you want prose rather than a list of subjects: put
them in `.release-notes/next.md` on any PR. The release commit spends the
fragment.

Once a tag is pushed, the `release tags are immutable` ruleset means the only
fix for a wrong release is the next version.

## What the release PR's body carries

Tools read it, so its shape is fixed (`release.sh pr-body`):

```
<!-- cahoots-release version=X.Y.Z base=<sha of the PR's parent> since=<vA.B.C|none> -->
Merging this PR is the release of cahoots vX.Y.Z (from <current>). …

## Changelog

<the CHANGELOG section>

## Commits since <vA.B.C|the first commit>

- <sha> <subject>
```

The commit list is every commit since the last tag, newest first, up to 300
(then `- … and N more`). The body stays under 60,000 bytes, which never
undercounts characters: the list may take half of that, and if its subjects
are longer, each is shortened to the same length and ends in `…` — every line
keeps its sha. The changelog section gets the rest, and is cut with a line
saying so when it doesn't fit; `CHANGELOG.md` at the PR's head is always
whole, and it — not the editable body — is what the tag is annotated with.
`release.sh pr-body` refuses rather than print a body over the limit.

## What stands in for the signed tag

Releases used to carry a tag signed by the maintainer: "I authorised this
commit as the release". Now that is said by

- the merge of the release PR at a pinned head, from the maintainer's
  account;
- GitHub's signature on the squash commit (`main` requires signed commits);
- the `release tags are made by the release workflow` ruleset (only the app
  creates a `v*` tag) and `release tags are immutable` (nobody moves one).

What users gain on top is build provenance. To check that an archive was built
by this repository's release workflow, from the tag and the commit it names:

```sh
tag=vX.Y.Z
gh attestation verify <archive> -R avihut/cahoots \
  --signer-workflow avihut/cahoots/.github/workflows/release.yml \
  --source-ref "refs/tags/$tag" \
  --source-digest "$(gh api "repos/avihut/cahoots/commits/$tag" --jq .sha)"
```

`-R` alone proves only that something in this repository built the archive;
`--signer-workflow` pins it to `release.yml`, and `--source-ref` and
`--source-digest` to the tag and the release commit it names.

## After a release

dist 0.30 renders no `test do` block and has no setting for one, so
`brew test cahoots` has nothing to run. After the tag is pushed and the
formula is in the tap, check it by hand: `brew install avihut/tap/cahoots`,
`cahoots --version` (it prints the released version), and `brew test cahoots`.

Then publish the crate (crates.io is not part of the workflow, on purpose: a
registry token does not belong in CI for a one-maintainer project):

```sh
cargo publish --locked            # from the tagged commit, clean tree
```

## One-time setup

All of it is the maintainer's: each step is a credential or a repository
setting. Until the first three are done, both release-flow jobs fail closed —
red runs, and no harm.

1. **The Wheatley app** (integration 2607344) installed on `avihut/cahoots`
   with *Contents: read and write*, *Pull requests: read and write* and
   *Metadata: read*. It needs no *Workflows* permission.
2. **The `release` environment**, deploying from `main` only, with no
   required reviewers (the click on the desk is the approval).
   `WHEATLEY_BOT_APP_ID` and `WHEATLEY_BOT_PRIVATE_KEY` are **environment
   secrets** of `release`, never repository secrets — otherwise any workflow
   on any pushed branch could mint the token and create tags.
3. **The two rulesets** that reserve the release refs for the app:

   ```sh
   gh api -X POST repos/avihut/cahoots/rulesets --input .github/rulesets/release-tags-by-workflow.json
   gh api -X POST repos/avihut/cahoots/rulesets --input .github/rulesets/release-pr-by-workflow.json
   ```

   Confirm that 2607344 is the Wheatley app's id first, then run
   `mise run release-rulesets-audit`: both must hold.
4. **Keep Actions' default workflow permissions `read`.** With `write`, any
   branch's workflow could push a `v*` tag before the rulesets existed.
- **`HOMEBREW_TAP_TOKEN`** — a repository secret on `avihut/cahoots`: a
  fine-grained token with *Contents: read and write* on `avihut/homebrew-tap`
  only. Without it the release still builds and publishes; only the formula
  step fails.
- **crates.io** — `cargo login`, once, on the machine that publishes.

## Changing the release workflows

`release-flow.yml`, `release.sh`, `pr-title.sh` and the rulesets decide what
becomes a release, and `release-flow.yml` runs with the app's key: a change to
any of them is the maintainer's to merge (docs/THREAT-MODEL.md → Releases).

`release.yml` is generated by cargo-dist and then hand-maintained
(`allow-dirty = ["ci"]` in `dist-workspace.toml`). After any `dist generate`
(take `allow-dirty` out for the run; dist refuses otherwise):

1. `mise run pin-actions` — every action back to a full commit SHA.
2. Restore the tag filter `v[0-9]+.[0-9]+.[0-9]+` and the header comment.
3. Keep only what the change was for: the attestation step and its
   permissions came from `github-attestations = true`.

`dist plan` shows what a tag would build, without building it. On a pull
request the workflow runs in plan mode only.
