#!/usr/bin/env bash
# The release workflow's steps (.github/workflows/release-flow.yml). A release
# is ONE commit, `release: vX.Y.Z`, carrying the version (Cargo.toml +
# Cargo.lock), the CHANGELOG section and the spent notes fragment; it is built
# on the `release-pr` branch, and merging that PR is the release. The workflow
# then tags the merge with an annotated, unsigned tag whose annotation is the
# CHANGELOG section, and the tag starts release.yml (RELEASING.md).
#
#   scripts/release.sh plan          the next version on stdout, or nothing
#   scripts/release.sh commit        on release-pr: make the release commit
#   scripts/release.sh pr-body       the release PR's body, from HEAD
#   scripts/release.sh pending-tag   sha=… / version=… of an untagged release
#   scripts/release.sh tag <sha>     validate it, then tag it (MERGED_PR_* env)
#   scripts/release.sh bot           the release bot's login
#
# NEVER pushes and never signs. stdout is data; every message is on stderr.
#
# No reader ever leaves its writer mid-write (#68): under pipefail, a
# `grep -q` that stops early fails the pipeline. Whatever git writes goes to
# a file first, and the file is read.
set -euo pipefail

# git exports these to every hook and they outrank -C; the cwd is authoritative.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

usage='usage: release.sh plan|commit|pr-body|pending-tag|tag <sha>|bot'
release_bot='wheatley-the-moronic-ci-bot[bot]'
manifest=Cargo.toml
lockfile=Cargo.lock
changelog=CHANGELOG.md
notes=.release-notes/next.md
template='<!-- What the next release ships, in prose. This becomes the annotation of the
     next release tag — which is what GitHub shows as the release'"'"'s notes —
     and the top section of CHANGELOG.md.
     The FIRST LINE is the tag'"'"'s subject: make it a short title, then a blank
     line. This comment is stripped. -->'
# A PR body is capped at 65,536 characters; this leaves room to spare.
body_limit=60000
commit_list_limit=300

say() { printf 'release: %s\n' "$1" >&2; }
refuse() {
    printf 'release: %s\n' "$1" >&2
    shift
    [ $# -eq 0 ] || printf '%s\n' "$@" >&2
    exit 1
}

verb=${1:-}
case $verb in
plan | commit | pr-body | pending-tag | bot) [ $# -eq 1 ] || {
    echo "$usage" >&2
    exit 2
} ;;
tag) [ $# -eq 2 ] && [ -n "$2" ] || {
    echo "$usage" >&2
    exit 2
} ;;
*)
    echo "$usage" >&2
    exit 2
    ;;
esac

if [ "$verb" = bot ]; then
    printf '%s\n' "$release_bot"
    exit 0
fi

scratch_dir=$(mktemp -d)
trap 'rm -rf "$scratch_dir"' EXIT

# ── Shared internals ────────────────────────────────────────────────────────

# version_at <rev>: the [package] version of Cargo.toml at a rev, or nothing.
version_at() {
    git show "$1:$manifest" >"$scratch_dir/manifest" 2>/dev/null || return 0
    awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
         on && /^version *= *"/ && !found { gsub(/"/, "", $3); print $3; found = 1 }' \
        "$scratch_dir/manifest"
}

last_tag() { git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true; }

# section <version> <rev>: the body of CHANGELOG.md at <rev> under its
# `## v<version> — ` heading, up to the next `## v`, blank lines trimmed at
# both ends. The heading is compared as a fixed string, never a pattern.
section() {
    git show "$2:$changelog" >"$scratch_dir/changelog" 2>/dev/null || return 0
    awk -v heading="## v$1 — " '
        index($0, heading) == 1 { on = 1; next }
        on && /^## v/ { on = 0 }
        on { lines[++n] = $0 }
        END {
            first = 1; while (first <= n && lines[first] ~ /^[[:space:]]*$/) first++
            last = n; while (last >= first && lines[last] ~ /^[[:space:]]*$/) last--
            for (i = first; i <= last; i++) print lines[i]
        }' "$scratch_dir/changelog"
}

# pending: the newest release commit on HEAD's first-parent history for the
# version HEAD holds, when that version has no tag yet. Sets pending_sha and
# pending_version, or leaves pending_sha empty and says why in pending_why.
pending() {
    pending_sha=
    pending_version=$(version_at HEAD)
    if [ -z "$pending_version" ]; then
        pending_why="no [package] version in $manifest"
        return 0
    fi
    if git rev-parse -q --verify "refs/tags/v$pending_version" >/dev/null; then
        pending_why="v$pending_version is tagged"
        return 0
    fi
    git log --first-parent --format='%H %s' HEAD >"$scratch_dir/history"
    pending_sha=$(awk -v want="release: v$pending_version" '
        {
            subject = substr($0, index($0, " ") + 1)
            rest = substr(subject, length(want) + 1)
            if (index(subject, want) == 1 && (rest == "" || rest ~ /^ \(#[0-9]+\)$/)) {
                print $1; exit
            }
        }' "$scratch_dir/history")
    [ -n "$pending_sha" ] || pending_why="no release commit for $pending_version"
}

refuse_if_pending() {
    pending
    [ -z "$pending_sha" ] ||
        refuse "v$pending_version is released on main but not tagged yet — the tag job comes first"
}

# decide: reads the subjects since the last tag and sets current, next and
# since (empty next: nothing releasable). Messages go to stderr.
decide() {
    current=$(version_at HEAD)
    [ -n "$current" ] || refuse "no [package] version found in $manifest"
    last=$(last_tag)
    since=${last:-the first commit}
    range=${last:+$last..}HEAD
    git log --format='%s%n%b' "$range" >"$scratch_dir/log"

    level="none"
    if grep -qE '^(feat|fix)(\([^)]*\))?!: ' "$scratch_dir/log" ||
        grep -q '^BREAKING CHANGE' "$scratch_dir/log"; then
        level="major"
    elif grep -qE '^feat(\([^)]*\))?: ' "$scratch_dir/log"; then
        level="minor"
    elif grep -qE '^fix(\([^)]*\))?: ' "$scratch_dir/log"; then
        level="patch"
    fi

    next=
    if [ "$level" = none ]; then
        say "nothing releasable since $since"
        return 0
    fi
    major=${current%%.*}
    rest=${current#*.}
    minor=${rest%%.*}
    patch=${rest##*.}
    # Pre-1.0 a breaking change is a minor bump, never an automatic jump to
    # 1.0.0 — calling something 1.0 is a product decision.
    if [ "$level" = major ] && [ "$major" = 0 ]; then
        say "a breaking change while the major is 0 — taking a minor bump, not 1.0.0"
        level="minor"
    fi
    case $level in
    major) next="$((major + 1)).0.0" ;;
    minor) next="$major.$((minor + 1)).0" ;;
    patch) next="$major.$minor.$((patch + 1))" ;;
    esac
    say "$level bump since $since: $current → $next"
}

# ── plan ────────────────────────────────────────────────────────────────────
if [ "$verb" = plan ]; then
    refuse_if_pending
    decide
    [ -z "$next" ] || printf '%s\n' "$next"
    exit 0
fi

# ── pending-tag ─────────────────────────────────────────────────────────────
if [ "$verb" = pending-tag ]; then
    pending
    if [ -z "$pending_sha" ]; then
        say "$pending_why"
        exit 0
    fi
    say "v$pending_version is released on $(git rev-parse --short "$pending_sha") but not tagged"
    printf 'sha=%s\nversion=%s\n' "$pending_sha" "$pending_version"
    exit 0
fi

# ── commit ──────────────────────────────────────────────────────────────────
if [ "$verb" = commit ]; then
    branch=$(git symbolic-ref --short -q HEAD || true)
    [ "$branch" = release-pr ] || refuse "the release commit is built on release-pr"
    if [ -n "$(git status --porcelain)" ]; then
        refuse "the worktree is not clean — refusing to build a release commit on top"
    fi
    refuse_if_pending
    decide
    [ -n "$next" ] || exit 0 # decide said "nothing releasable since …"

    message="$scratch_dir/notes"
    scratch="$scratch_dir/rewrite"
    # The notes: the fragment written on a branch beats a list of subjects.
    # The template's HTML comment is scaffolding — whole comment lines go,
    # then the blank lines they leave at the top.
    if [ -f "$notes" ]; then
        awk '
            /<!--/ { comment = 1 }
            !comment { print }
            /-->/ { comment = 0 }
        ' "$notes" | sed -e '/./,$!d' >"$message"
    else
        : >"$message"
    fi
    if [ -n "$(tr -d '[:space:]' <"$message")" ]; then
        say "notes from $notes"
    else
        git log --format='- %s' "$range" >"$scratch_dir/subjects"
        {
            printf 'cahoots %s\n\n' "$next"
            grep -vE '^- (release|chore|ci|test|docs|style|refactor)(\([^)]*\))?!?: ' "$scratch_dir/subjects" || true
        } >"$message"
        say "no notes in $notes — the notes are the subjects since $since"
    fi

    # The version in both files.
    awk -v next_version="$next" '
        /^\[package\]/ { on = 1 }
        /^\[/ && !/^\[package\]/ { on = 0 }
        on && /^version *= *"/ && !done { print "version = \"" next_version "\""; done = 1; next }
        { print }
    ' "$manifest" >"$scratch" && cat "$scratch" >"$manifest"
    git add "$manifest"

    # Cargo.lock names this package too. Edited in place — no cargo, no
    # network, no resolver surprises in a release commit; `--locked` builds
    # prove it right.
    if [ -f "$lockfile" ]; then
        name=$(awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
                    on && /^name *= *"/ && !found { gsub(/"/, "", $3); print $3; found = 1 }' "$manifest")
        awk -v name="$name" -v next_version="$next" '
            $0 == "name = \"" name "\"" { mine = 1; print; next }
            mine && /^version = "/ { print "version = \"" next_version "\""; mine = 0; next }
            /^\[\[package\]\]/ { mine = 0 }
            { print }
        ' "$lockfile" >"$scratch" && cat "$scratch" >"$lockfile"
        git add "$lockfile"
    fi

    # The section IS the notes, title line included: the tag job rebuilds the
    # annotation from it, since the squash merge's body is the merger's.
    {
        printf '# Changelog\n\n'
        printf '## v%s — %s\n\n' "$next" "$(date -u +%Y-%m-%d)"
        cat "$message"
        printf '\n'
        if [ -f "$changelog" ]; then
            sed -e '1{/^# Changelog$/d;}' "$changelog" | sed -e '/./,$!d'
        fi
    } >"$scratch"
    cat "$scratch" >"$changelog"
    git add "$changelog"

    if [ -f "$notes" ]; then
        printf '%s\n' "$template" >"$notes"
        git add "$notes"
    fi
    {
        printf 'release: v%s\n\n' "$next"
        cat "$message"
    } >"$scratch_dir/commit-message"
    git commit -q --no-gpg-sign -F "$scratch_dir/commit-message"
    say "committed release: v$next"
    exit 0
fi

# ── pr-body ─────────────────────────────────────────────────────────────────
if [ "$verb" = pr-body ]; then
    subject=$(git log -1 --format=%s HEAD)
    if [[ ! "$subject" =~ ^release:\ v([0-9]+\.[0-9]+\.[0-9]+)$ ]]; then
        refuse "HEAD is not a release commit"
    fi
    version=${BASH_REMATCH[1]}
    base=$(git rev-parse HEAD^)
    last=$(last_tag)
    if [ -n "$last" ]; then
        git log --format='- %h %s' "$last..HEAD^" >"$scratch_dir/commits"
    else
        git log --format='- %h %s' HEAD^ >"$scratch_dir/commits"
    fi
    count=$(wc -l <"$scratch_dir/commits" | tr -d ' ')
    if [ "$count" -gt "$commit_list_limit" ]; then
        head -n "$commit_list_limit" "$scratch_dir/commits" >"$scratch_dir/listed"
        printf -- '- … and %d more\n' "$((count - commit_list_limit))" >>"$scratch_dir/listed"
    else
        cp "$scratch_dir/commits" "$scratch_dir/listed"
    fi

    {
        printf '<!-- cahoots-release version=%s base=%s since=%s -->\n' "$version" "$base" "${last:-none}"
        # shellcheck disable=SC2016 # the backticks are Markdown
        printf 'Merging this PR is the release of cahoots v%s (from %s). The release flow then tags the merge `v%s`, and the tag builds and publishes it.\n' \
            "$version" "$(version_at HEAD^)" "$version"
        printf '\n## Changelog\n\n'
    } >"$scratch_dir/head"
    {
        printf '\n## Commits since %s\n\n' "${last:-the first commit}"
        cat "$scratch_dir/listed"
    } >"$scratch_dir/tail"
    section "$version" HEAD >"$scratch_dir/section"

    cut_line='(cut short — CHANGELOG.md at the PR head is whole)'
    fixed=$(cat "$scratch_dir/head" "$scratch_dir/tail" | wc -c | tr -d ' ')
    whole=$(wc -c <"$scratch_dir/section" | tr -d ' ')
    if [ $((fixed + whole)) -gt "$body_limit" ]; then
        room=$((body_limit - fixed - ${#cut_line} - 2))
        awk -v room="$room" '
            { if (used + length($0) + 1 > room) exit; used += length($0) + 1; print }
        ' "$scratch_dir/section" >"$scratch_dir/cut"
        printf '%s\n' "$cut_line" >>"$scratch_dir/cut"
        mv "$scratch_dir/cut" "$scratch_dir/section"
    fi
    cat "$scratch_dir/head" "$scratch_dir/section" "$scratch_dir/tail"
    exit 0
fi

# ── tag <sha> ───────────────────────────────────────────────────────────────
# Every check reads git or the environment the workflow filled from GitHub's
# API — never the PR's own text — and the first that fails refuses.
sha=$(git rev-parse -q --verify "$2^{commit}" 2>/dev/null) ||
    refuse "$2 is not a release commit — it is not a commit at all"
short=$(git rev-parse --short "$sha")
subject=$(git log -1 --format=%s "$sha")
if [[ ! "$subject" =~ ^release:\ v([0-9]+\.[0-9]+\.[0-9]+)(\ \(#([0-9]+)\))?$ ]]; then
    refuse "$short '$subject' is not a release commit"
fi
version=${BASH_REMATCH[1]}
named_pr=${BASH_REMATCH[3]}
tag="v$version"
there=$(version_at "$sha")
[ "$there" = "$version" ] ||
    refuse "$short '$subject' but Cargo.toml's version there is ${there:-missing}"
[ "$(version_at "$sha^")" != "$version" ] ||
    refuse "$short '$subject' does not move the version"
git merge-base --is-ancestor "$sha" HEAD ||
    refuse "$short is not on this branch's history"
if [ -z "${MERGED_PR_NUMBER:-}" ] || [ -z "${MERGED_PR_HEAD_REF:-}" ] ||
    [ -z "${MERGED_PR_CROSS_REPO:-}" ] || [ -z "${MERGED_PR_AUTHOR:-}" ]; then
    refuse "$short: no merged pull request was given (MERGED_PR_NUMBER, MERGED_PR_HEAD_REF, MERGED_PR_CROSS_REPO, MERGED_PR_AUTHOR)"
fi
[ "$MERGED_PR_HEAD_REF" = release-pr ] ||
    refuse "$short was not merged from release-pr, but from '$MERGED_PR_HEAD_REF'"
[ "$MERGED_PR_CROSS_REPO" = false ] ||
    refuse "$short came from a fork"
[ "$MERGED_PR_AUTHOR" = "$release_bot" ] ||
    refuse "$short was not opened by the release bot, but by '$MERGED_PR_AUTHOR'"
if [ -n "$named_pr" ] && [ "$named_pr" != "$MERGED_PR_NUMBER" ]; then
    refuse "$short names #$named_pr but was merged from #$MERGED_PR_NUMBER"
fi
if existing=$(git rev-parse -q --verify "refs/tags/$tag^{commit}"); then
    if [ "$existing" = "$sha" ]; then
        say "$tag is already tagged"
        exit 0
    fi
    refuse "$tag already exists on $(git rev-parse --short "$existing")"
fi
section "$version" "$sha" >"$scratch_dir/annotation"
[ -s "$scratch_dir/annotation" ] ||
    refuse "$short: CHANGELOG.md has no section for $tag"
git tag -a --no-sign -F "$scratch_dir/annotation" "$tag" "$sha"
say "tagged $tag on $short"
