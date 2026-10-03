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
    local formula=$1 problems=0 name version
    name=$(field name)
    version=$(field version)

    expect() {
        if ! grep -qxF -- "$1" "$formula"; then
            echo "formula: missing from $formula: $1" >&2
            problems=$((problems + 1))
        fi
    }

    # compare <what> <expected> <actual>: one block of the formula, whole.
    compare() {
        if [ "$2" != "$3" ]; then
            echo "formula: $1 is not what dist writes for this release:" >&2
            diff <(printf '%s\n' "$2") <(printf '%s\n' "$3") | sed 's/^/  /' >&2 || true
            problems=$((problems + 1))
        fi
    }

    expect "class Cahoots < Formula"
    expect "  desc \"$(field description)\""
    expect "  homepage \"$(field homepage)\""
    expect "  version \"$version\""
    expect '  license any_of: ["MIT", "Apache-2.0"]'

    # The four platforms, each branch selecting its own archive (with a
    # checksum) and installing exactly the binary. dist writes these as fixed
    # blocks, so they are compared as blocks: a swapped ARM/Intel condition, a
    # missing or extra archive or checksum, a wrong binary all differ.
    local os cpu target url want_urls='' want_installs=''
    for os in mac linux; do
        want_urls+="  if OS.$os?"$'\n'
        for cpu in arm intel; do
            case "$os-$cpu" in
                mac-arm) target=aarch64-apple-darwin ;;
                mac-intel) target=x86_64-apple-darwin ;;
                linux-arm) target=aarch64-unknown-linux-gnu ;;
                linux-intel) target=x86_64-unknown-linux-gnu ;;
            esac
            url="$(field repository)/releases/download/v$version/$name-$target.tar.xz"
            want_urls+="    if Hardware::CPU.$cpu?"$'\n'"      url \"$url\""$'\n'"      sha256 \"HEX\""$'\n'"    end"$'\n'
            want_installs+="    if OS.$os? && Hardware::CPU.$cpu?"$'\n'"      bin.install \"$name\""$'\n'"    end"$'\n'
        done
        want_urls+="  end"$'\n'
    done
    compare "the archive for each platform" "${want_urls%$'\n'}" "$(
        sed -n '/^  version "/,/^  license /p' "$formula" | sed '1d;$d' |
            sed -E 's/^( *sha256 )"[0-9a-f]{64}"$/\1"HEX"/'
    )"
    compare "the binary installed on each platform" "  def install"$'\n'"${want_installs}"$'\n'"    install_binary_aliases!" "$(
        sed -n '/^  def install$/,/^    install_binary_aliases!$/p' "$formula"
    )"
    # No install of anything else, and no alias for another name.
    if [ "$(grep -cE 'bin\.install[ (]' "$formula")" -ne 4 ]; then
        echo "formula: expected exactly four bin.install lines, one per platform" >&2
        problems=$((problems + 1))
    fi
    if [ "$(grep -cE '^    "[a-z0-9_-]+": \{\},?$' "$formula")" -ne 4 ]; then
        echo "formula: BINARY_ALIASES should be empty for each of the four targets" >&2
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
