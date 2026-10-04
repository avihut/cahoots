#!/usr/bin/env bash
# The two rulesets that reserve the release refs for the release app
# (.github/rulesets/release-tags-by-workflow.json, release-pr-by-workflow.json),
# and the one that keeps a pushed tag where it is (release-tags.json, `release
# tags are immutable`), checked as GitHub holds them.
#
#   scripts/release-rulesets-audit.sh
#       The audit: reads the three live rulesets with YOUR gh credentials and
#       fails unless each is active, covers its ref with the rules its record
#       names, and lets exactly the release app (integration 2607344) bypass
#       the first two and nobody at all bypass the immutable one — a bypass
#       covers every rule of its ruleset, so the app there could move or delete
#       a tag. Only a caller with write access to rulesets sees the bypass
#       list, so this is the maintainer's (or the driver's, with the
#       maintainer's credentials) — run it when the rulesets are applied and
#       right before every merge of a release PR (RELEASING.md).
#   scripts/release-rulesets-audit.sh coverage <tag|release-pr|tag-immutable> <ruleset.json>
#       Pure: the ruleset is active and covers the ref with the required
#       rules. Says nothing about who may bypass it. The release workflow's
#       fail-closed steps run this, because their token can't see bypass
#       actors.
#   scripts/release-rulesets-audit.sh exclusive <tag|release-pr> <ruleset.json>
#       Pure: coverage, and the bypass list is exactly the release app.
#   scripts/release-rulesets-audit.sh sealed tag-immutable <ruleset.json>
#       Pure: coverage, and the bypass list is empty — the app included.
#
# Coverage is read conservatively: an inclusion must name the whole ref space
# (`refs/tags/v*` or a wider pattern; `refs/heads/release-pr` or a wider one),
# and an exclusion whose literal prefix could match any of it — `~ALL`, a
# wildcard pattern over refs/tags/ or refs/heads/, any refs/tags/v… — cancels
# it. A ruleset this refuses may still be fine; one it accepts is not wrong.
set -euo pipefail

usage='usage: release-rulesets-audit.sh [coverage <tag|release-pr|tag-immutable> | exclusive <tag|release-pr> | sealed tag-immutable <ruleset.json>]'
repo=avihut/cahoots
app_id=2607344
scripts=$(cd "$(dirname "$0")" && pwd)
records="$(dirname "$scripts")/.github/rulesets"

say() { printf 'release-rulesets-audit: %s\n' "$1" >&2; }

# check <coverage|exclusive|sealed> <tag|release-pr|tag-immutable> <file>: exit 0 when it holds,
# 1 with the reason on stderr when it doesn't.
check() {
    local mode=$1 kind=$2 file=$3 target space includes rules why name what
    case $kind in
    tag)
        target=tag
        space=refs/tags/v
        includes='["refs/tags/v*","refs/tags/*","refs/tags/**","refs/tags/**/*","~ALL"]'
        rules='["creation"]'
        ;;
    tag-immutable)
        target=tag
        space=refs/tags/v
        includes='["refs/tags/v*","refs/tags/*","refs/tags/**","refs/tags/**/*","~ALL"]'
        rules='["deletion","non_fast_forward","update"]'
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
    # Slurped: the input must be exactly one JSON object. An empty or blank
    # response is zero values, and jq succeeds on zero values — so nothing
    # but the word "ok" means the ruleset holds.
    why=$(jq -rs --arg mode "$mode" --arg target "$target" --arg space "$space" \
        --argjson includes "$includes" --argjson rules "$rules" --argjson app "$app_id" '
        def literal: capture("^(?<p>[^*?\\[]*)").p;
        def cancels:
            if . == "~DEFAULT_BRANCH" then false
            elif startswith("~") then true
            else literal as $l | ($space | startswith($l)) or ($l | startswith($space))
            end;
        if length != 1 or (.[0] | type) != "object"
        then "the response is not exactly one ruleset object (\(length) JSON values)"
        else .[0]
        | (.conditions.ref_name.include // []) as $inc
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
          elif ($mode == "exclusive" or $mode == "sealed") and (has("bypass_actors") | not)
            then "its bypass list is not visible to these credentials"
          elif $mode == "exclusive" and ((.bypass_actors | map({actor_id, actor_type, bypass_mode})) != [{actor_id: $app, actor_type: "Integration", bypass_mode: "always"}])
            then "its bypass list is not exactly the release app (\(.bypass_actors | map("\(.actor_type) \(.actor_id // "") \(.bypass_mode)") | join(", ")))"
          elif $mode == "sealed" and (.bypass_actors != [])
            then "its bypass list is not empty (\(.bypass_actors | map("\(.actor_type) \(.actor_id // "") \(.bypass_mode)") | join(", ")))"
          else "ok" end
        end' "$file") || {
        say "$file is not a ruleset"
        return 1
    }
    if [ "$why" != ok ]; then
        name=$(jq -rs 'if length == 1 and (.[0] | type) == "object" then .[0].name // "a ruleset" else "the response" end' "$file")
        case $mode in
        sealed) what="keep $kind free of any bypass" ;;
        *) what="reserve $kind for the release app" ;;
        esac
        say "${name:-the response} in $file does not $what: ${why:-jq said nothing}"
        return 1
    fi
}

case ${1:-} in
coverage | exclusive | sealed)
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
for triple in exclusive:tag:release-tags-by-workflow exclusive:release-pr:release-pr-by-workflow sealed:tag-immutable:release-tags; do
    mode=${triple%%:*}
    pair=${triple#*:}
    kind=${pair%%:*}
    name=$(jq -r .name "$records/${pair#*:}.json")
    jq -r --arg name "$name" '.[] | select(.name == $name) | .id' "$scratch_dir/list.json" >"$scratch_dir/ids"
    if [ "$(wc -l <"$scratch_dir/ids" | tr -d ' ')" != 1 ]; then
        say "$repo has no single ruleset named '$name' — apply .github/rulesets/${pair#*:}.json"
        failures=$((failures + 1))
        continue
    fi
    gh api "repos/$repo/rulesets/$(cat "$scratch_dir/ids")" >"$scratch_dir/$kind.json"
    if check "$mode" "$kind" "$scratch_dir/$kind.json"; then
        if [ "$mode" = sealed ]; then
            echo "release-rulesets-audit: '$name' keeps $kind free of any bypass"
        else
            echo "release-rulesets-audit: '$name' reserves $kind for the release app alone"
        fi
    else
        failures=$((failures + 1))
    fi
done
[ "$failures" -eq 0 ] || exit 1
echo "release-rulesets-audit: all three release rulesets hold"
