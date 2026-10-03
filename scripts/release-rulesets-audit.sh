#!/usr/bin/env bash
# The two rulesets that reserve the release refs for the release app
# (.github/rulesets/release-tags-by-workflow.json, release-pr-by-workflow.json),
# checked as GitHub holds them.
#
#   scripts/release-rulesets-audit.sh
#       The audit: reads both live rulesets with YOUR gh credentials and fails
#       unless each is active, covers its ref with the rules its record names,
#       and lets exactly the release app (integration 2607344) bypass it. Only
#       a caller with write access to rulesets sees the bypass list, so this
#       is the maintainer's (or the driver's, with the maintainer's
#       credentials) — run it when the rulesets are applied and right before
#       every merge of a release PR (RELEASING.md).
#   scripts/release-rulesets-audit.sh coverage <tag|release-pr> <ruleset.json>
#       Pure: the ruleset is active and covers the ref with the required
#       rules. Says nothing about who may bypass it. The release workflow's
#       fail-closed steps run this, because their token can't see bypass
#       actors.
#   scripts/release-rulesets-audit.sh exclusive <tag|release-pr> <ruleset.json>
#       Pure: coverage, and the bypass list is exactly the release app.
#
# Coverage is read conservatively: an inclusion must name the whole ref space
# (`refs/tags/v*` or a wider pattern; `refs/heads/release-pr` or a wider one),
# and an exclusion whose literal prefix could match any of it — `~ALL`, a
# wildcard pattern over refs/tags/ or refs/heads/, any refs/tags/v… — cancels
# it. A ruleset this refuses may still be fine; one it accepts is not wrong.
set -euo pipefail

usage='usage: release-rulesets-audit.sh [coverage|exclusive <tag|release-pr> <ruleset.json>]'
repo=avihut/cahoots
app_id=2607344
scripts=$(cd "$(dirname "$0")" && pwd)
records="$(dirname "$scripts")/.github/rulesets"

say() { printf 'release-rulesets-audit: %s\n' "$1" >&2; }

# check <coverage|exclusive> <tag|release-pr> <file>: exit 0 when it holds,
# 1 with the reason on stderr when it doesn't.
check() {
    local mode=$1 kind=$2 file=$3 target space includes rules why
    case $kind in
    tag)
        target=tag
        space=refs/tags/v
        includes='["refs/tags/v*","refs/tags/*","refs/tags/**","refs/tags/**/*","~ALL"]'
        rules='["creation"]'
        ;;
    release-pr)
        target=branch
        space=refs/heads/release-pr
        includes='["refs/heads/release-pr","refs/heads/*","refs/heads/**","refs/heads/**/*","~ALL"]'
        rules='["creation","update","deletion","non_fast_forward"]'
        ;;
    *)
        echo "$usage" >&2
        exit 2
        ;;
    esac
    why=$(jq -r --arg mode "$mode" --arg target "$target" --arg space "$space" \
        --argjson includes "$includes" --argjson rules "$rules" --argjson app "$app_id" '
        def literal: capture("^(?<p>[^*?\\[]*)").p;
        def cancels:
            if . == "~DEFAULT_BRANCH" then false
            elif startswith("~") then true
            else literal as $l | ($space | startswith($l)) or ($l | startswith($space))
            end;
        (.conditions.ref_name.include // []) as $inc
        | (.conditions.ref_name.exclude // []) as $exc
        | [.rules[]?.type] as $have
        | if .enforcement != "active" then "it is not active (enforcement: \(.enforcement // "none"))"
          elif .target != $target then "it targets \(.target // "nothing"), not \($target)"
          elif ([$inc[] | select(. as $p | $includes | index($p))] | length) == 0
            then "it does not include all of \($space)… (include: \($inc | join(", ")))"
          elif ([$exc[] | select(cancels)] | length) > 0
            then "its exclusions cancel part of \($space)… (exclude: \([$exc[] | select(cancels)] | join(", ")))"
          elif ([$rules[] | select(. as $r | $have | index($r) | not)] | length) > 0
            then "it lacks the rules \([$rules[] | select(. as $r | $have | index($r) | not)] | join(", "))"
          elif $mode == "exclusive" and (has("bypass_actors") | not)
            then "its bypass list is not visible to these credentials"
          elif $mode == "exclusive" and ((.bypass_actors | map({actor_id, actor_type, bypass_mode})) != [{actor_id: $app, actor_type: "Integration", bypass_mode: "always"}])
            then "its bypass list is not exactly the release app (\(.bypass_actors | map("\(.actor_type) \(.actor_id // "") \(.bypass_mode)") | join(", ")))"
          else "" end' "$file") || {
        say "$file is not a ruleset"
        return 1
    }
    if [ -n "$why" ]; then
        say "$(jq -r '.name // "a ruleset"' "$file") does not reserve $kind for the release app: $why"
        return 1
    fi
}

case ${1:-} in
coverage | exclusive)
    [ $# -eq 3 ] || {
        echo "$usage" >&2
        exit 2
    }
    check "$1" "$2" "$3"
    exit $?
    ;;
"")
    [ $# -eq 0 ] || {
        echo "$usage" >&2
        exit 2
    }
    ;;
*)
    echo "$usage" >&2
    exit 2
    ;;
esac

# ── The live audit ──────────────────────────────────────────────────────────
scratch_dir=$(mktemp -d)
trap 'rm -rf "$scratch_dir"' EXIT
gh api "repos/$repo/rulesets?includes_parents=false" >"$scratch_dir/list.json"
failures=0
for pair in tag:release-tags-by-workflow release-pr:release-pr-by-workflow; do
    kind=${pair%%:*}
    name=$(jq -r .name "$records/${pair#*:}.json")
    jq -r --arg name "$name" '.[] | select(.name == $name) | .id' "$scratch_dir/list.json" >"$scratch_dir/ids"
    if [ "$(wc -l <"$scratch_dir/ids" | tr -d ' ')" != 1 ]; then
        say "$repo has no single ruleset named '$name' — apply .github/rulesets/${pair#*:}.json"
        failures=$((failures + 1))
        continue
    fi
    gh api "repos/$repo/rulesets/$(cat "$scratch_dir/ids")" >"$scratch_dir/$kind.json"
    if check exclusive "$kind" "$scratch_dir/$kind.json"; then
        echo "release-rulesets-audit: '$name' reserves $kind for the release app alone"
    else
        failures=$((failures + 1))
    fi
done
[ "$failures" -eq 0 ] || exit 1
echo "release-rulesets-audit: both release rulesets hold"
