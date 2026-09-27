# Security policy

Harnesses are told to let `cahoots` run outside their sandbox, and cahoots
starts other agents with access to your repositories. Reports about either
are taken seriously.

## Reporting a vulnerability

Report privately through GitHub: **Security → Report a vulnerability** on this
repository (private vulnerability reporting is enabled). Please don't open a
public issue for anything exploitable. This is a one-maintainer project;
expect an acknowledgement within a week.

## What counts

The hard rules are in `AGENTS.md`, and the boundary cahoots claims to hold is
in `docs/THREAT-MODEL.md`. A way to make cahoots break one of them is a
vulnerability — for example:

- **the command surface as an escape hatch:** a sandboxed or permission-gated
  agent using an agent-tier verb to read or write outside its workspace, to
  run a command of its choosing, or to start a callee with wider permissions
  than the role allows;
- **the gate:** a run admitted over its cap, on stale or missing usage data, or
  past the concurrency and depth limits; an inherited API key moving a callee
  onto per-token billing the meter never sees;
- **data widening authority:** a config file, learned state or a callee's
  output changing a cap, a sandbox mode, a flag or a command;
- **learning as an injection channel:** text from a callee's output reaching a
  future session's instructions through a review note;
- **the installer:** writing outside its manifest, following a symlink out of
  an agent home, editing a harness's settings or permission files, or
  `uninstall` removing something it did not write;
- **config.toml edited in place** (`settings`, `enable`, `install`): a change
  reaching more of the file than the setting asked for, a value written that
  the config's own checks refuse, or any of it reachable from an agent-tier
  verb;
- a credential file being read, anything token-shaped being logged or
  recorded, or any network access at all;
- run records or learned state being readable by another user.

Out of scope: what an agent can do once you have given it an unrestricted
shell, and the behaviour of the harness CLIs themselves.

## Supported versions

Only the newest tagged version.
