#!/usr/bin/env bash
# The `pr-title` check (.github/workflows/pr-title.yml). A PR lands as ONE
# squash commit whose subject is the PR title, and the release workflow reads
# those subjects to choose the next version — so the title is held to the
# grammar the commit-msg hook holds a commit to. A `release:` title belongs to
# the release workflow's own PR alone, and that PR may change nothing but the
# version, the changelog and the notes fragment.
#
#   PR_TITLE PR_HEAD_REF PR_HEAD_REPO PR_BASE_REPO PR_AUTHOR (environment)
#   HEAD = the PR's merge commit, HEAD^1 = the base
#
# A PR can edit its own copy of this check (`pull_request` runs the PR's
# code); the tag job's provenance check, read from GitHub's API, is what holds
# after that (docs/THREAT-MODEL.md → Releases).
set -euo pipefail

# git exports these to hooks; the cwd is authoritative.
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

scripts=$(cd "$(dirname "$0")" && pwd)
for name in PR_TITLE PR_HEAD_REF PR_HEAD_REPO PR_BASE_REPO PR_AUTHOR; do
    if [ -z "${!name+set}" ]; then
        echo "usage: PR_TITLE, PR_HEAD_REF, PR_HEAD_REPO, PR_BASE_REPO and PR_AUTHOR set; HEAD the PR's merge commit — $name is unset" >&2
        exit 2
    fi
done

refuse() {
    printf 'pr-title: %s\n' "$1" >&2
    exit 1
}

scratch_dir=$(mktemp -d)
trap 'rm -rf "$scratch_dir"' EXIT

# Every file is read whole before awk sees it (#68: no reader leaves its
# writer mid-write).
version_at() {
    git show "$1:Cargo.toml" >"$scratch_dir/manifest" 2>/dev/null || return 0
    awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
         on && /^version *= *"/ && !found { gsub(/"/, "", $3); print $3; found = 1 }' \
        "$scratch_dir/manifest"
}

# <rev> <path> → the file at rev with this package's version line blanked,
# so two of them differ only if something besides the version moved.
without_version() {
    git show "$1:$2" >"$scratch_dir/raw" 2>/dev/null || {
        : >"$scratch_dir/raw"
    }
    case $2 in
    Cargo.toml)
        awk '/^\[package\]/ { on = 1; print; next } /^\[/ { on = 0 }
             on && /^version *= *"/ && !done { print "version = <version>"; done = 1; next }
             { print }' "$scratch_dir/raw"
        ;;
    Cargo.lock)
        awk -v name="$package" '
            $0 == "name = \"" name "\"" { mine = 1; print; next }
            mine && /^version = "/ { print "version = <version>"; mine = 0; next }
            /^\[\[package\]\]/ { mine = 0 }
            { print }' "$scratch_dir/raw"
        ;;
    esac
}

release_pr=false
# shellcheck disable=SC2153 # PR_HEAD_REF comes from the environment, checked above
# A deleted fork has no head repository: that is cross-repository too.
if [ "$PR_HEAD_REF" = release-pr ] && [ -n "$PR_HEAD_REPO" ] && [ "$PR_HEAD_REPO" = "$PR_BASE_REPO" ]; then
    release_pr=true
fi
release_title=false
case "$PR_TITLE" in
release:* | release\(*) release_title=true ;;
esac

if $release_title && ! $release_pr; then
    refuse "a release title comes only from the release workflow's release-pr"
fi
if $release_pr && ! $release_title; then
    refuse "release-pr carries only the release"
fi

head_version=$(version_at HEAD)
base_version=$(version_at HEAD^1)

if $release_pr; then
    [ "$PR_AUTHOR" = "$("$scripts/release.sh" bot)" ] ||
        refuse "the release PR is opened by the release bot, not '$PR_AUTHOR'"
    [[ "$PR_TITLE" =~ ^release:\ v([0-9]+\.[0-9]+\.[0-9]+)$ ]] ||
        refuse "a release title is exactly 'release: vX.Y.Z' — got '$PR_TITLE'"
    version=${BASH_REMATCH[1]}
    [ "$head_version" = "$version" ] ||
        refuse "'$PR_TITLE' but Cargo.toml's version is ${head_version:-missing}"
    [ "$base_version" != "$version" ] ||
        refuse "'$PR_TITLE' does not move the version"
    git diff --name-only HEAD^1 HEAD >"$scratch_dir/changed"
    while IFS= read -r path; do
        case $path in
        Cargo.toml | Cargo.lock | CHANGELOG.md | .release-notes/next.md) ;;
        *) refuse "the release PR changes only the version, the changelog and the notes fragment — not $path" ;;
        esac
    done <"$scratch_dir/changed"
    git show HEAD:Cargo.toml >"$scratch_dir/head-manifest"
    package=$(awk '/^\[package\]/ { on = 1; next } /^\[/ { on = 0 }
                   on && /^name *= *"/ && !found { gsub(/"/, "", $3); print $3; found = 1 }' \
        "$scratch_dir/head-manifest")
    for path in Cargo.toml Cargo.lock; do
        without_version HEAD^1 "$path" >"$scratch_dir/base"
        without_version HEAD "$path" >"$scratch_dir/head"
        cmp -s "$scratch_dir/base" "$scratch_dir/head" ||
            refuse "the release PR changes only the version, the changelog and the notes fragment — not $path"
    done
elif [ "$head_version" != "$base_version" ]; then
    refuse "this PR moves Cargo.toml's version ${base_version:-none} → ${head_version:-none} — only the release workflow's PR does"
fi

# cog opens the repository even to verify a bare string; its failure is the
# refusal, in its own words.
cog verify "$PR_TITLE" || refuse "'$PR_TITLE' is not a conventional commit subject"
echo "pr-title: ok"
