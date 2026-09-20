<!-- The PR TITLE becomes the squash commit's subject, and the release script
     reads it: make it a conventional commit with the area as the scope —
     `fix(gate): …`, `feat(codex): …`. CI checks it. -->

## What and why

## How it was verified

- [ ] `mise run gate` is green locally
- [ ] Anything that changes how a harness is called was tried against the real CLI, not only the fake one

## Hard rules (AGENTS.md)

- [ ] No network code, no new dependency, no new process spawn outside `src/spawn` — or the PR says which, and why
- [ ] No credential, token or account identifier in code, fixtures or logs; no harness credential file is read
- [ ] Nothing here lets data — config, learned state, a callee's output — widen what cahoots may do
