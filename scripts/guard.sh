#!/usr/bin/env bash
# Hard-rule tripwires: the rules of AGENTS.md that a grep can hold. Each is a
# tripwire, not a proof — it catches the careless regression, and its failure
# message names the rule so a deliberate change is made as one (amend the
# rule, then the list below, in the same commit).
#
#   scripts/guard.sh            the working tree (tracked files)
#   scripts/guard.sh --staged   the index — what a commit is about to record
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

grep_tree=(git grep -I)
show() { cat -- "$1"; }
if [ "${1:-}" = "--staged" ]; then
    grep_tree=(git grep -I --cached)
    show() { git show ":$1"; }
fi
# A file the rule needs but the tree lacks reads as empty, and the rule says so.
text_of() { show "$1" 2>/dev/null || true; }

failures=0
fail() {
    failures=$((failures + 1))
    printf '\n✗ %s\n' "$1" >&2
    shift
    [ $# -eq 0 ] || printf '  %s\n' "$@" >&2
}
where() { cut -d: -f1,2 <<<"$1" | head -5; }

# 1. No credential is ever tracked. Fixtures carry shapes and percentages,
#    never a token — and these are the shapes the three vendors' and GitHub's
#    tokens have wherever they appear.
tokens='sk-ant-[a-z0-9]+-[A-Za-z0-9_-]{8,}|sk-proj-[A-Za-z0-9_-]{20,}|AIza[0-9A-Za-z_-]{35}|gh[opsur]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{20,}'
if hits=$("${grep_tree[@]}" -nE "$tokens"); then
    fail "a token-shaped string is tracked (hard rule 2: no credentials, anywhere)" "$(where "$hits")"
fi

# 2. No network. deny.toml bans the crates; this holds the standard library.
if hits=$("${grep_tree[@]}" -nE 'std::net|TcpStream|TcpListener|UdpSocket|UnixStream|UnixListener|UnixDatagram' -- src build.rs); then
    fail "socket code in the crate (hard rule 1: cahoots has no network code)" "$(where "$hits")"
fi

# 3. A harness credential file is never read; login state comes from the
#    harness CLI's own status command.
if hits=$("${grep_tree[@]}" -nE 'auth\.json|\.credentials\.json|oauth_creds\.json' -- src build.rs); then
    fail "a harness credential file is named in the crate (hard rule 2)" "$(where "$hits")"
fi

# 4. One module spawns processes — it owns the scrubbed environment, argv
#    validation and process groups — and nothing is ever run through a shell.
if hits=$("${grep_tree[@]}" -nE 'Command::new' -- src ':!src/spawn.rs' ':!src/spawn'); then
    fail "Command::new outside src/spawn (hard rule 3: one module spawns)" "$(where "$hits")"
fi
if hits=$("${grep_tree[@]}" -nE 'Command::new\("(/usr)?(/bin/)?(sh|bash|zsh|dash|fish)"\)' -- src build.rs); then
    fail "a shell is spawned (hard rule 3: argv arrays only, never sh -c)" "$(where "$hits")"
fi

# 5. Agent homes are named only by the installer; the environment is read in
#    one module (which is where HOME is refused in favour of passwd).
if hits=$("${grep_tree[@]}" -nE '\.(claude|codex|gemini|agents)[/"]' -- src ':!src/install.rs' ':!src/install'); then
    fail "an agent-home path outside src/install (hard rule 4)" "$(where "$hits")"
fi
if hits=$("${grep_tree[@]}" -nE 'env::(var|vars|var_os|vars_os|set_var|remove_var)[[:space:]]*\(' -- src ':!src/env.rs'); then
    fail "the environment is read outside src/env.rs (hard rule 4)" "$(where "$hits")"
fi

# 6. The dependency list is closed. deny.toml holds the transitive graph;
#    this holds what Cargo.toml itself names.
allowed_crates="clap anyhow thiserror serde serde_json toml nix ctrlc uuid rusqlite fs2 assert_cmd predicates tempfile serial_test insta"
crates=$(text_of Cargo.toml | awk '
    /^\[/ {
        on = ($0 ~ /dependencies\]$/)
        if (match($0, /dependencies\.[A-Za-z0-9_-]+\]$/)) {
            name = substr($0, RSTART + 13, RLENGTH - 14); print name
        }
        next
    }
    on && /^[A-Za-z0-9_-]+ *=/ { print $1 }')
for crate in $crates; do
    case " $allowed_crates " in
    *" $crate "*) ;;
    *) fail "Cargo.toml depends on '$crate' (AGENTS.md: the dependency list is closed — amend it there first)" ;;
    esac
done

# 7. Every action is pinned to a full commit SHA (the repository setting
#    refuses anything else; this says so before the push does).
if hits=$("${grep_tree[@]}" -nE '^[[:space:]]*(-[[:space:]]+)?uses:' -- .github/workflows |
    grep -vE 'uses:[[:space:]]+[^@[:space:]]+@[0-9a-f]{40}([[:space:]]|$)'); then
    fail "an action is not pinned to a full commit SHA (docs/WORKFLOW.md)" "$(where "$hits")"
fi

# 8. The required checks have ONE spelling. `gate` and `pr-title` are named by
#    the ruleset, by the job that reports each, and by the auto-merge
#    workflow's fail-closed test; a rename in one place is a PR gate that
#    waits forever, or one that no longer gates.
contexts=$(text_of .github/rulesets/main-pr-gate.json |
    grep -oE '"context": *"[^"]+"' | sed -E 's/.*"([^"]+)"$/\1/' | sort | tr '\n' ' ')
if [ "$contexts" != "gate pr-title " ]; then
    fail "main-pr-gate.json requires '${contexts% }', not 'gate pr-title'"
fi
job_named() { # job_named <workflow> <name>: the job's key AND its name
    local workflow
    workflow=$(text_of "$1")
    grep -qE "^  $2:\$" <<<"$workflow" && grep -qE "^    name: $2\$" <<<"$workflow"
}
job_named .github/workflows/ci.yml gate ||
    fail "ci.yml has no job whose key and name are both 'gate' (the required check)"
job_named .github/workflows/pr-title.yml pr-title ||
    fail "pr-title.yml has no job whose key and name are both 'pr-title' (the required check)"
automerge=$(text_of .github/workflows/dependabot-auto-merge.yml)
grep -qF 'index("gate")' <<<"$automerge" ||
    fail "dependabot-auto-merge.yml no longer refuses to arm when 'gate' is not required"

# 9. Every dev-lifecycle script is runnable as a mise task. `ls-files` reads
#    the index, so a script staged for its first commit is already counted.
tasks=$(text_of mise.toml)
while IFS= read -r script; do
    if ! grep -qF "$script" <<<"$tasks"; then
        fail "$script has no mise task (AGENTS.md: a script and its task land together)"
    fi
done < <(git ls-files -- 'scripts/*')

# 10. The interface and the logic are separate layers (hard rule 11). The
#     logic returns data and never touches the terminal; src/tui presents and
#     knows nothing of the logic; only the command layer holds both.
command_layer=(':!src/main.rs' ':!src/cli.rs' ':!src/cli')
if hits=$("${grep_tree[@]}" -nE '(^|[^A-Za-z0-9_])(e?print(ln)?!|(stdin|stdout|stderr)\(\))|IsTerminal' -- \
    src "${command_layer[@]}" ':!src/tui'); then
    fail "the terminal is touched outside the interface (hard rule 11: the logic returns data; src/cli and src/tui show it)" "$(where "$hits")"
fi
if hits=$("${grep_tree[@]}" -nE 'crate::' -- src/tui); then
    fail "src/tui reaches into the logic (hard rule 11: the interface knows nothing of it)" "$(where "$hits")"
fi
if hits=$("${grep_tree[@]}" -nE '(^|[^A-Za-z0-9_])tui::' -- src "${command_layer[@]}" ':!src/tui'); then
    fail "the logic reaches for the TUI (hard rule 11: only the command layer asks a person)" "$(where "$hits")"
fi

if [ "$failures" -gt 0 ]; then
    printf '\nguard: %d rule(s) tripped\n' "$failures" >&2
    exit 1
fi
echo "guard: repo rules hold"
