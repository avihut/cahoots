#!/usr/bin/env bash
# Cuts the release `main` has earned: reads the conventional subjects since the
# last `v*` tag, picks the bump, and makes ONE commit — `release: vX.Y.Z` —
# that carries the version (Cargo.toml + Cargo.lock), the CHANGELOG section
# and the spent notes fragment, plus a signed annotated tag whose annotation
# IS the release notes. Nothing on a branch ever names a version.
#
#   scripts/release.sh [--dry-run]
#
# NEVER pushes. `git push origin main vX.Y.Z` is a person's step, because it
# is the irreversible one: the tag ruleset makes a pushed `v*` tag immutable,
# and the tag is what starts the release workflow. release-check (pre-push) is
# the backstop if this ever leaves a release half made.
set -euo pipefail

# git exports these to every hook and they outrank -C; the cwd is authoritative.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

dry_run=false
[ "${1:-}" = "--dry-run" ] && dry_run=true

manifest=Cargo.toml
lockfile=Cargo.lock
changelog=CHANGELOG.md
notes=.release-notes/next.md
template='<!-- What the next release ships, in prose. This becomes the annotation of the
     next release tag — which is what GitHub shows as the release'"'"'s notes —
     and the top section of CHANGELOG.md.
     The FIRST LINE is the tag'"'"'s subject: make it a short title, then a blank
     line. This comment is stripped. -->'

say() { printf 'release: %s\n' "$1"; }
refuse() {
    printf 'release: %s\n' "$1" >&2
    shift
    [ $# -eq 0 ] || printf '%s\n' "$@" >&2
    exit 1
}

branch=$(git symbolic-ref --short -q HEAD || true)
if [ "$branch" != "main" ]; then
    say "'$branch' is not main — releases are cut from main"
    exit 0
fi
if [ -n "$(git status --porcelain)" ]; then
    refuse "the worktree is not clean — refusing to build a release commit on top" \
        "  commit or discard the changes, then: mise run release"
fi

version_of() {
    awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
         on && /^version *= *"/ { gsub(/"/, "", $3); print $3; exit }'
}
current=$(version_of <"$manifest")
[ -n "$current" ] || refuse "no [package] version found in $manifest"

# The annotation IS the release notes. A fragment written on the branch beats
# a list of subjects, and it rode the gate like any other file.
written_notes() { # → the fragment on stdout, or 1 when it says nothing
    [ -f "$notes" ] || return 1
    # The template's HTML comment is scaffolding, not notes: whole comment
    # lines go, then the blank lines they leave at the top.
    stripped=$(awk '
        /<!--/ { comment = 1 }
        !comment { print }
        /-->/ { comment = 0 }
    ' "$notes" | sed -e '/./,$!d')
    [ -n "$stripped" ] || return 1
    printf '%s\n' "$stripped"
}

tag_head() { # <version> <notes file>
    if $dry_run; then
        say "would tag v$1 on $(git rev-parse --short HEAD)"
        return 0
    fi
    git tag -a "v$1" -F "$2"
    say "tagged v$1"
}

message=$(mktemp)
scratch=$(mktemp)
trap 'rm -f "$message" "$scratch"' EXIT

# ── A release commit is already the tip ─────────────────────────────────────
# Don't release twice — just make sure it has its tag, since release-check
# refuses to push a release commit without one.
if [ "$(git log -1 --format=%s)" = "release: v$current" ]; then
    if [ -n "$(git tag --points-at HEAD --list "v$current")" ]; then
        say "v$current is already tagged on this commit"
        exit 0
    fi
    git log -1 --format=%b >"$message"
    [ -s "$message" ] || refuse "v$current has no notes to annotate with"
    say "annotating v$current from the release commit's own body"
    tag_head "$current" "$message"
    exit 0
fi

# ── Decide whether main has earned a release ────────────────────────────────
last=$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)
range=${last:+$last..}HEAD
log=$(git log --format='%s%n%b' "$range")

level="none"
if printf '%s\n' "$log" | grep -qE '^(feat|fix)(\([^)]*\))?!: ' ||
    printf '%s\n' "$log" | grep -q '^BREAKING CHANGE'; then
    level="major"
elif printf '%s\n' "$log" | grep -qE '^feat(\([^)]*\))?: '; then
    level="minor"
elif printf '%s\n' "$log" | grep -qE '^fix(\([^)]*\))?: '; then
    level="patch"
fi

if [ "$level" = none ]; then
    say "nothing releasable since ${last:-the first commit}"
    exit 0
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

say "$level bump since ${last:-the first commit}: $current → $next"
if $dry_run; then
    say "would commit 'release: v$next' and tag it"
    exit 0
fi

if written_notes >"$message"; then
    say "notes from $notes"
else
    {
        printf 'cahoots %s\n\n' "$next"
        git log --format='- %s' "$range" | grep -vE '^- (chore|ci|test|docs|style|refactor)(\([^)]*\))?: ' || true
    } >"$message"
    say "no $notes — annotating with the subjects since ${last:-the first commit}"
fi

# One commit: the version in both files, the changelog, the spent fragment.
awk -v next_version="$next" '
    /^\[package\]/ { on = 1 }
    /^\[/ && !/^\[package\]/ { on = 0 }
    on && /^version *= *"/ && !done { print "version = \"" next_version "\""; done = 1; next }
    { print }
' "$manifest" >"$scratch" && cat "$scratch" >"$manifest"
git add "$manifest"

# Cargo.lock names this package too. Edited in place — no cargo, no network,
# no resolver surprises in a release commit; `--locked` builds prove it right.
if [ -f "$lockfile" ]; then
    name=$(awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
                on && /^name *= *"/ { gsub(/"/, "", $3); print $3; exit }' "$manifest")
    awk -v name="$name" -v next_version="$next" '
        $0 == "name = \"" name "\"" { mine = 1; print; next }
        mine && /^version = "/ { print "version = \"" next_version "\""; mine = 0; next }
        /^\[\[package\]\]/ { mine = 0 }
        { print }
    ' "$lockfile" >"$scratch" && cat "$scratch" >"$lockfile"
    git add "$lockfile"
fi

{
    printf '# Changelog\n\n'
    printf '## v%s — %s\n\n' "$next" "$(date -u +%Y-%m-%d)"
    # The notes' first line is the title; the changelog heading already has one.
    sed -e '1{/^cahoots /d;}' "$message" | sed -e '/./,$!d'
    printf '\n'
    [ -f "$changelog" ] && sed -e '1{/^# Changelog$/d;}' "$changelog" | sed -e '/./,$!d'
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
} | git commit -q -F -
tag_head "$next" "$message"
say "not pushed — review it (git show v$next), then: git push origin main v$next"
