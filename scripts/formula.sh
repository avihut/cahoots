#!/usr/bin/env bash
# `mise run formula`: render the Homebrew formula cargo-dist would publish and
# read it against Cargo.toml, before a release rather than after. Writes only
# under target/distrib; publishes nothing, needs no credential.
#
# The checksums in the render are PLACEHOLDERS (dist is run with
# --artifacts=lies, so no archive is built); the real ones exist only once
# release.yml has built the archives. What this checks is the formula's shape.
#
#   formula.sh                  render, then check target/distrib/cahoots.rb
#   formula.sh --check <file>   check a formula that is already rendered
#
# dist 0.30 renders no `test do` block and has no setting for one, so there is
# none to check for; RELEASING.md says how the release is checked by hand.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
cd "$root"

field() { sed -n "s/^$1 = \"\(.*\)\"\$/\1/p" Cargo.toml | head -1; }

# check <formula>: every line of the formula that Cargo.toml or the target list
# decides, compared against them.
check() {
    local formula=$1 problems=0 name version want
    name=$(field name)
    version=$(field version)

    expect() {
        if ! grep -qxF -- "$1" "$formula"; then
            echo "formula: missing from $formula: $1" >&2
            problems=$((problems + 1))
        fi
    }

    expect "class Cahoots < Formula"
    expect "  desc \"$(field description)\""
    expect "  homepage \"$(field homepage)\""
    expect "  version \"$version\""
    expect '  license any_of: ["MIT", "Apache-2.0"]'

    # The four targets: each one's archive, from this release, with a checksum
    # on the line after its url.
    for target in aarch64-apple-darwin x86_64-apple-darwin \
        aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu; do
        want="      url \"$(field repository)/releases/download/v$version/$name-$target.tar.xz\""
        expect "$want"
        if ! grep -A1 -xF -- "$want" "$formula" | tail -1 | grep -qE '^      sha256 "[0-9a-f]{64}"$'; then
            echo "formula: no sha256 after the $target url" >&2
            problems=$((problems + 1))
        fi
    done
    if [ "$(grep -c '^      url ' "$formula")" -ne 4 ]; then
        echo "formula: expected exactly four archive urls" >&2
        problems=$((problems + 1))
    fi

    # The binary, once per platform, and no other install.
    if [ "$(grep -cxF "      bin.install \"$name\"" "$formula")" -ne 4 ]; then
        echo "formula: expected bin.install \"$name\" once per platform" >&2
        problems=$((problems + 1))
    fi

    if [ "$problems" -ne 0 ]; then
        echo "formula: $problems problem(s) — fix them in dist-workspace.toml or Cargo.toml, never in the render" >&2
        return 1
    fi
}

if [ "${1:-}" = "--check" ]; then
    [ -n "${2:-}" ] || {
        echo "usage: formula.sh [--check <formula.rb>]" >&2
        exit 2
    }
    check "$2"
    exit 0
fi

command -v dist >/dev/null || {
    echo "formula: dist (cargo-dist) is not installed — see https://axodotdev.github.io/cargo-dist" >&2
    exit 1
}

dist build --artifacts=lies --tag="v$(field version)" >/dev/null
check target/distrib/cahoots.rb
echo "formula: target/distrib/cahoots.rb matches Cargo.toml (checksums are placeholders)"
echo "formula: review it, and compare with Formula/daft.rb in avihut/homebrew-tap"
