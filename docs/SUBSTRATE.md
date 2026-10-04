# cahoots as the orchestrator's substrate

> Written on 2026-10-04 for #63. cahoots was read at `eb422d4`, and the
> skill (avihut/project-builder) at `4e6c5ac`. Nothing proposed here is
> built, and this document changes no rule. Where a ticket and this document
> differ, the ticket wins. A gap marked **new** has no issue yet; its title
> is a proposal for whoever files it.

Avihu decided on 2026-10-03 that cahoots becomes the base of the
project-builder skill now, and that the cahoots plan grows where the skill
needs it. This is the map of that growth: who owns what, what the skill does
today and which cahoots verb already does it, what is missing, and in what
order to close it.

## Summary

1. **The line holds.** cahoots owns the machinery: processes, the fence,
   admission and records, all on this machine. The orchestrator owns
   judgment, the network and the person. No gap below needs a hard rule
   bent.
2. **cahoots covers one shape of work today:** a one-shot job on the other
   vendor, with `resume` for follow-ups. For a Claude orchestrator that is
   the Codex reviewer, judge and explorer.
3. **The builders do not fit, and #48 does not change that.** `pick` leaves
   out the caller's harness, a Claude writer has no shell, and no cahoots
   writer commits or pushes. Most of a board's runs are builders, so most of
   the board stays outside the fence and the record until this is decided
   (gap 10).
4. **The review loop is the next thing to move** (gap 8). It works through
   `run --role review` and `resume` today. What stands in the way is small,
   and most of it is in the skill.
5. **The machine gets two owners with two scopes** (gap 9). cahoots
   regulates what it starts. The skill's `heavy.py` keeps regulating the
   commands an agent runs inside its own session, because cahoots must
   never run a caller's command.
6. **The eval gaps stay parked** with #39 to #46 (gaps 2, 5 and 6).
7. **Three things are Avihu's:** the builders, whether cahoots may hold a
   group of runs, and the machine's split.

## The skill, in a paragraph

One session, the driver, runs a board of tickets. This document calls it the
orchestrator. For each ticket it cuts a worktree, opens a herdr workspace,
and starts agents in panes: an explorer or a planner, then a builder, then a
reviewer from the other vendor. Agents are interactive sessions with a shell.
Builders commit, sign and push their own branch. Every agent's turn ends in
a `STATUS:` line, and watchers poll herdr for state changes. A pushed commit
is reviewed before any PR opens, the PR opens at the reviewed SHA, and the
orchestrator squash-merges it. What needs the person goes to the desk, a
live page on claude.ai. The skill's recurring actions are a table
(`ACTIONS.md`), each with one tool under `tools/`.

## 1. The line

**cahoots owns the machinery.** Starting a harness under a fence (rule 3),
admission by the plan's headroom, worktrees for writers, follow-ups on a
run, and the record of every run. All of it is local (rules 1 and 7), and
what it learns can only reorder (rule 6).

**The orchestrator owns judgment, the network and the person.** GitHub, the
board, PRs, merges and the desk need the network, which cahoots never
touches (rule 1). Which ticket is next, whether a plan is approved, what a
review's verdict means for a merge, and what reaches the person are
judgment. cahoots' logic returns data and never prompts (rule 11), and its
writes stay in its own directories (rule 5).

**The test for which side a new thing is on** is the one
`docs/PLUGINS-AND-EVALS.md` gives: who holds the plan of work. cahoots
"decides who and whether, carries one request, returns one answer, and
holds only runs". It would become an orchestrator "once it held tasks or a
roster, once messages flowed more than one way, or once a target delegated
onward".

### The target architecture, tested

The diagram in #63's first comment, line by line.

| The target says | Verdict | Why |
|---|---|---|
| The orchestrator holds the board, GitHub, PRs, merges and the desk | Holds | Rule 1. Nothing below moves any of it |
| The orchestrator decides the next ticket, plan approval, the gate's verdict, what reaches the person | Holds | Judgment, and the plan of work |
| The interface is the CLI: envelopes and exit codes | Holds, with a condition | `run` and `resume` only reach the repository the caller stands in (gap 13) |
| `pick` by kind, evidence and headroom | Holds | Kinds (#33), the report by kind (#34), exploration (#35) and the gate exist. Ordering by evidence is #41, parked |
| `pick` by the plan's reset time | Not built, and nothing to order yet | The gate's meter answer has no field for a reset time. With two harnesses and an agent caller, one harness is left after the caller is dropped (gap 3) |
| `run`, `resume`, `wait`, `cancel` for one run | Holds for readers on the other vendor and for Codex writers in a fork | Not for the builders (gap 10) |
| The same, as N sealed arms | Does not hold as drawn | An agent cannot name a candidate, a Claude caller cannot reach a Claude arm, and a group is more than a run (gap 2) |
| The fence, records and outcomes | Holds | This is what the switch buys |
| Blind runs | Holds | A person's setting (#31) |
| Evals | Does not hold as drawn | Every `evals` verb is a person's and refuses without a terminal. Replay is not built (#39) |
| Harness CLIs: claude, codex, agy | Two of three | `HarnessId` is `Claude` and `Codex` |
| Meters, worktree providers | Holds | Closed seams, chosen by a person's setting (#32) |
| A herdr host (#48) | Holds, with a condition | Only if the launch never goes through a shell (rule 3) |
| An MCP server as a thin adapter (#59) | Holds | A new agent-tier surface, so a threat-model change of its own |
| One plugin install (#54) | Holds | The same files from the same writer (rule 5) |

**What the diagram leaves out.** It draws every agent below cahoots. Today
only the other vendor's readers can be. The builders, planners and explorers
are Claude sessions the orchestrator starts in panes, and cahoots has no way
to start them. Section 3c says why, and what would change it.

## 2. The map

Each row is one thing the skill does today, the cahoots verb that does it,
and a verdict: **covered**, **stays** with the orchestrator, or a **gap**
from section 3.

### Routing and admission

| The skill today | Through cahoots | Verdict |
|---|---|---|
| A card per ticket: five scores, a path, a prediction | nothing | Stays: judgment |
| A model and an effort per job, from the runbook's table, typed into `herdr agent start` | `--role`, `--kind` and `--to`. A caller cannot name a model or an effort | Covered, once the table is written as kinds in config.toml, which is a person's file |
| Codex efforts up to `ultra` | `Effort` ends at `max` | Gap 14 |
| Plan usage read before a wave (`usage-cli limits`) | The gate on every run: cap 75, a reserve of 3 for a reader and 8 for a writer, the watchdog, and 12 runs an hour per target | Covered for the target's plan. Agents started in panes are not gated |
| Spend the plan that resets sooner | Nothing reads a reset time | Gap 3 |
| Reroute after each eval | The report by kind, and one learned swap per role, in shadow | #41, parked |

### Placement

| The skill today | Through cahoots | Verdict |
|---|---|---|
| `daft start <branch>`, then `open_ticket.py`: a herdr workspace with lazygit, a shell and the agent | `--fork` cuts a detached worktree at HEAD with no repository hooks, by git or daft. `--dir` names another worktree of the same repository | Stays: the named branch, its setup hooks and the layout |
| The orchestrator works from a repository of its own (cahoots-evals) | `run` and `resume` judge `--dir` and the brief against the caller's working directory | Gap 13 |
| One writer per worktree | Not enforced. A fork is cut for one run, but `resume` starts a new run in the same worktree and checks only that the run it names has ended. The locks are per run and per harness slot, and in-place writers share none | Stays for now: the orchestrator runs one writer, and one `resume` round, per worktree and session at a time. Gap 15 |
| A plan is read-only after the handoff, by permission mode or by instruction | A reader's fence is in code | Covered for runs through cahoots |

### Starting, watching and talking to an agent

| The skill today | Through cahoots | Verdict |
|---|---|---|
| `herdr agent start` with typed flags, a prompt that points at the brief, then a check that it is working | `run --brief <file>`: argv built in code, the brief on stdin, a detached supervisor, a run id | Covered for a one-shot headless run. Gap 10 for the builders |
| Briefs from `templates/`, in the worktree's `.cache/` or the orchestrator's ticket folder | A brief is a file of at most 256 KiB in the working directory, its repository or a temp directory | Covered when the brief is in the ticket's worktree. Otherwise gap 13 |
| Names a person can read, `<slug>-<role>`, on a watch list | Run ids, and a `kind` label | Stays: the orchestrator maps a run to its ticket and role |
| `watch.py` polls herdr every 5 seconds for blocked, done, idle, gone | `wait`, in the background where the harness reports an exit (#55), and `status` | Covered. A headless run never blocks on a dialog |
| A `STATUS:` line ends every turn | The run's state, its exit code and its answer | Covered |
| Plan approval: Enter on the plan dialog, then the decisions as a prompt | No dialog. A planner's answer is the plan, and approval is a `resume` with the decisions | Covered in shape. Gap 10: planners are Claude sessions |
| `needs-decision`, then an answer typed into the pane | The run ends with the question. `resume` carries the answer | Covered, one question a run |
| A builder asks its planner (`herdr agent prompt`) | The orchestrator relays with two `resume` calls, both recorded | Gap 4 |
| A person watches a pane and steps in | Headless only | Gap 1 |
| Safety, model-switch and folder-trust dialogs, answered by the person | None in a headless run. The record keeps `model_reported` | Gap 12 |
| Pause everything: Esc to each working agent | `cancel <run>`, and a cancelled run can be resumed | Covered per run. The loop is the orchestrator's |
| Close-out: stop the agents, close the workspace, `daft remove`, `daft sync` | cahoots removes a worktree it cut once no run on record works in it | Stays |

### The review gate

| The skill today | Through cahoots | Verdict |
|---|---|---|
| `start_review.py`: a Codex pane with `-s workspace-write`, read-only by its brief | `run --role review --to codex`: read-only by code | Gap 8 |
| The same reviewer confirms each new SHA, by a prompt to its session | `resume` | Covered, one round at a time (gap 15) |
| The reviewer writes `tickets/<n>/review-<sha7>.md` | A reader writes nothing. Its answer comes back through `result` | Stays: the orchestrator writes the file |
| The verdict is pinned to a SHA | `base_commit`: HEAD in the run's directory, read once before launch. A record, not a pin | Stays: the orchestrator attests the SHA in every round (3a) |
| The plan holder confirms conformance | `resume` of the planner's run | Gap 10 |
| What became of a review | `outcome` | Covered |
| CI, `gh pr merge --squash --match-head-commit`, PR comments, the merge and CI watchers | nothing | Stays: rule 1 |

### Evals (parked since 2026-10-04)

| The skill today | Through cahoots | Verdict |
|---|---|---|
| Arms: one base, brief and time box, started together, nothing committed | Per arm, `run --kind <k> --fork`: cut at HEAD, `base_commit` recorded, the patch kept | Gap 2 |
| Sealing: arm names hide the model, and `blind.py` relabels and shuffles the diffs | `[review] blind`: the envelope withholds model and effort until an outcome. The harness stays visible | Partly. The blind diffs are part of gap 2 |
| A hidden-test author writes tests from the plan | Fits a Codex writer in a fork. Not tried | Section 6 |
| `grade.py`: the gate, a tampering check, the hidden tests | cahoots never runs repository code on an agent verb (#30) | Stays, until #39 and #44 |
| Judges from both vendors, in both orders | `run --role review` for the Codex judge | Gap 6, and gap 10 for a Claude judge |
| How much of a diff survived | `outcome` measures it, and `report` measures again (#37) | Covered |
| Wins, ties and losses with an honest n | The report by kind, with standard errors (#34) | #40, parked |

### Meters and records

| The skill today | Through cahoots | Verdict |
|---|---|---|
| `meter.py`: model, effort, cost and context from herdr, the diff, plan usage, the load | The run record: tokens, times, the admission reading, `base_commit`, a patch summary. `report` sums them | Covered for cahoots runs. Not kept: usage after the run, the load, nudges, questions asked |
| `plan_usage.py`, for the desk's usage rings | The gate asks the same tracker. No verb hands the reading out | Stays: the orchestrator asks `usage-cli` itself |
| `meters.jsonl`, `grades.jsonl` | `history.jsonl` | Gap 5 |
| `events.log`, the agents' state changes | nothing | Stays for pane agents. Gap 11 for cahoots runs |

### The machine

| The skill today | Through cahoots | Verdict |
|---|---|---|
| `heavy.py -- <command>`: two machine-wide slots, a lower priority, a share of the cores by whether the person is there | Counts of runs: `harness.<id>.max_concurrent` (1, up to 8) and `limits.max_active_runs` (3, up to 16) | Gap 9 |
| `heavy.py --status` before a wave | nothing | Gap 9 |

### The person, the desk, upkeep

| The skill today | Through cahoots | Verdict |
|---|---|---|
| The desk: decisions, feed, documents, the board, chat, pause (`desk_sync.py`) | nothing | Stays: rule 1, and the person |
| `flow.py`: what each ticket waits on, and the bottleneck | nothing | Stays. Gap 11, so a cahoots run is counted |
| Estimates, the record of the person's answers, the milestone reflection | nothing | Stays: judgment |
| A new project: the repository, rulesets, the board | nothing | Stays: rule 1 |
| `install_cahoots.sh` after every merge, then `cahoots refresh` as a command of its own | `refresh` needs no terminal, and no rule names it, so the harness asks | Covered (#66) |
| Once per machine: `install`, `enable`, `settings set`, pasting the rules | Human verbs | Gap 7 |
| Surviving a compaction: the state file, the hooks, `self_compact.py` | nothing | Stays: it is the orchestrator's own session |

## 3. The gaps

Sizes are XS to L. "Whose" follows section 1.

| # | What is missing | Whose, and why | Size | Needs | Issue or proposed title |
|---|---|---|---|---|---|
| 1 | A writer a person can watch and step into | cahoots': where a target runs is a seam. It does not move the builders (3c) | L | #32 (merged), the launch decision, gap 10's answer | #48 |
| 2 | N runs on one brief as a unit: one base, sealed labels, gathered for grading | cahoots' for the label and the gathering. Which arms and who won stay judgment | L | #35, #39, #31, gap 10 for Claude arms | **new**, or widen #43: `feat(run): a group of runs on one brief` |
| 3 | The plan that resets sooner goes first | cahoots': an order over the person's list, as #41. The meter's answer has no field for a reset time today | M | A reset time in the meter's answer, and two harnesses left to order (gap 10) | **new** `feat(pick): among candidates with headroom, the plan that resets sooner goes first` |
| 4 | A consult on record, tied to the run that asked | Relaying stays the orchestrator's: a target that asks another agent itself is delegating onward. cahoots' part is one link in the history | XS | Both agents being cahoots runs (gap 10) | **new** `feat(history): a follow-up names the run it was asked for` |
| 5 | The skill's `grades.jsonl` and `meters.jsonl` as history | cahoots' for grades, as trials. Meters hold what herdr reported for pane agents and have no run to attach to, so they stay the skill's | M | #38 (merged), #40 | #46: add grades as an import source |
| 6 | A judge for writer roles, with a closed score vocabulary | cahoots': a fenced reader, like #45's. On #29 the hidden tests tied three arms, and only a judge's reading found the hole | M | #39 | **new** `feat(evals): a judge for writer roles` |
| 7 | Steps only a person can run | cahoots'. Pasting the permission rules stays a person's (rule 5) | XS to M each | #61 (PR #88) | #71, then #89 to #93. #94 and #95 are parked |
| 8 | The board's reviews through `run` and `resume` | Mostly the skill's (3a) | S | Gaps 12 and 13 | A proposal to project-builder, not a cahoots ticket |
| 9 | The machine as a budget | Split (3b) | S, S, M | nothing | Three **new** tickets, in 3b |
| 10 | The builders, planners and explorers: Claude agents a Claude orchestrator starts | A decision first (3c) | S for the memo | Avihu | **new** `docs(design): the builders, by same-harness targets and a sandboxed shell` |
| 11 | What a run is doing now. `status` gives state, tokens and notes, and the desk and `flow.py` read only herdr | cahoots' for the verb. Reading it is the skill's | S | nothing. More untrusted callee text on an agent verb, so the threat model changes in the same PR | **new** `feat(status): a run's latest activity` |
| 12 | What a withheld reply and a model switch look like in a headless run | Unknown. In a pane, a vendor's filter has withheld a long review reply, and a model switch is a dialog | XS | The first board reviews through cahoots are the trial | **new**, only if a withheld reply ends as an empty success: `fix(run): a withheld reply ends the run as failed` |
| 13 | A run started from another repository. `--dir` takes the working directory or a worktree of its repository, and a brief must sit there or in a temp directory. THREAT-MODEL also allows "a configured root", and no such setting was found in the code | cahoots': the orchestrator's home is a repository of its own, and the rule that lets it call cahoots matches only a plain command line, so it cannot `cd` in the same call | S | nothing. The root is a person's setting | **new** `feat(run): --dir reaches a root a person configured` |
| 14 | Codex's `ultra` effort | cahoots', if it is wanted | XS | nothing | **new** `feat(registry): the ultra effort for Codex` |
| 15 | One live run per worktree and per session. `resume` checks that the run it names has ended, not that nothing else is live in its worktree or on its session. With more than one slot for a harness, two rounds can overlap | cahoots': a lock is machinery, and a brief cannot hold one. Until it exists the orchestrator serializes | S | nothing | **new** `fix(run): one live run per worktree and per session` |

### 3a. Gap 8: the board's reviews

**What works today.** From the ticket's worktree, checked out at the pushed
commit:

    cahoots run --role review --to codex --caller claude --brief .cache/ticket/review-brief.md
    cahoots wait <run>
    cahoots result <run>
    cahoots resume <run> --caller claude --brief .cache/ticket/review-delta.md
    cahoots outcome <run> accepted

`resume` is the delta round: the same session, model and place, as a new
gated run. The reviewer is read-only by code. In a pane it runs with
`-s workspace-write` and is read-only because its brief says so.

**The SHA is the orchestrator's to attest.** cahoots records `base_commit`,
and it is not a pin. It is one reading of HEAD in the run's directory,
taken before launch. A reader works in the caller's live tree. A dirty tree
is not refused, nothing stops the tree from moving while the review runs, a
reading that fails is recorded as null without refusing the run, and a
`resume` in a live tree reads HEAD again. So in every round, the first and
each delta:

1. The brief names the pushed SHA in full, read from `git ls-remote`, and
   the exact range to read: `git diff <base>...<sha>` in the first round,
   `git diff <last reviewed sha>..<sha>` in a delta. Every command in the
   brief names commits, never `HEAD` or the working tree, so the reviewer
   reads commit objects.
2. The verdict's first line names that SHA in full.
3. The worktree stays at that SHA, clean, for the whole round. The builder
   waits, as the gate already has it wait.
4. When the round ends, the orchestrator checks three things: the verdict
   names the expected SHA, the run's `base_commit` is not null and equals
   it, and the worktree's HEAD still equals it.
5. A round that fails any check does not count. No PR opens on it, and
   nothing merges on it.

**What changes or is missing.**

| What | Where it is closed |
|---|---|
| The reviewer cannot write the review file. Its answer is the review: `result` prints up to 64 KiB, and the path of the full answer beyond that | The skill: the orchestrator writes `review-<sha7>.md` from the answer. The brief's "reply with only the verdict" turns around |
| The reply is now the whole review, and a vendor's filter has withheld long security replies | Gap 12, open. Nothing in cahoots saves a withheld review today. A round whose reply is withheld does not count, and that round goes to a pane reviewer, as now |
| The issue, the plan and the earlier verdicts live in the orchestrator's repository | The skill: copy them into the worktree's `.cache/ticket/`, or inline them in the brief |
| The orchestrator stands in another repository | Gap 13. Until then it changes directory first, as a call of its own |
| A read-only reviewer cannot run the tests | Accepted. The review brief already treats that as optional: the builder ran the gate at this SHA, and CI runs on the PR |
| Review rounds at a board's pace | A person's settings, set on this Mac already: Codex slots, the active-run ceiling, the hourly ledger, the timeout. Every `resume` counts as a run |
| A gate refusal stalls a review | Policy, in the skill: a refusal is an answer. The ticket waits on the plan, and the desk says so |
| Two rounds on one session must not overlap, and `resume` does not check | Gap 15. Until then the orchestrator waits for a round to end before it starts the next |
| A ticket that outlives 7 days | A run's content is kept 7 days. After that the next round is a new run with the earlier verdicts in its brief, which the skill already does when a reviewer's context fills |
| Nobody can watch the review, and the desk and `flow.py` do not see it | Gap 11, then the skill reads `cahoots status` |

**Recommendation: move it.** It is the primitive that fits cahoots best, a
one-shot read on the other vendor with follow-ups, and rule 4 of the runbook
already says a cross-vendor call goes through cahoots. The fence becomes
code, every round is gated and recorded, and each review earns an outcome.
The SHA stays the orchestrator's to attest, as it is today. The cost is a
reviewer nobody watches and that runs no tests. Order: try a
few board reviews now, by hand, to answer gap 12. Then the skill moves
`start_review.py` onto cahoots. Gaps 11 and 13 make it comfortable and do
not block it.

### 3b. Gap 9: the machine

**Two owners, two scopes.** One tool cannot own both.

- **cahoots regulates what it starts.** It owns those processes, and
  lowering a priority or refusing a run only narrows (rule 6), so a
  person's setting may choose it.
- **`heavy.py` stays in the skill**, for the commands an agent runs inside
  its own session. cahoots cannot see those. A cahoots verb that took a
  caller's command and ran it in a slot would be a way to run anything
  outside the sandbox, on the agent tier. Rules 3 and 6 rule it out.

The two do not share slots. cahoots counts runs, `heavy.py` counts heavy
commands, and a writer under cahoots that builds would call `heavy.py` from
its brief like any agent.

| Proposed ticket | What | Size |
|---|---|---|
| **new** `feat(spawn): a callee runs below the person's apps` | A lower priority for the callee's process group, set in `src/spawn`. Needs a way to set it inside the closed dependency list and without unsafe code | S |
| **new** `feat(run): a writer's share of the cores` | A typed setting that cahoots turns into the known variables (`CARGO_BUILD_JOBS` and the like) in code. Never a free-form environment table, which would let a file choose what runs | S |
| **new** `feat(gate): a busy machine refuses a run as busy` | Admission by the machine's load beside the plan's headroom: over a person's threshold, exit 32 with `retry: later` | M |

**How much this buys today: little.** The runs cahoots hosts now are readers,
and a reader's work happens at the vendor. On 2026-10-03 the heaviest load in
sight was the standing cost of open sessions, which neither tool regulates.
These tickets matter once writers that build run under cahoots, so they
follow gap 10.

### 3c. Gap 10: the builders

Three separate things keep the orchestrator's own agents out of cahoots.

1. **The caller's harness is left out.** `pick` drops every candidate on the
   caller's harness, and `--to` naming it is refused: "a harness does not
   delegate to itself". `resume` refuses the same way. A Claude orchestrator
   gets Codex and nothing else, and the runbook's builders and planners are
   Opus and Sonnet.
2. **A Claude writer has no shell.** Its tools are
   `Read,Grep,Glob,Edit,Write`. THREAT-MODEL: "without a sandbox a shell
   writes anywhere". So it cannot run the tests it may have broken. A Claude
   reader has no shell either, so a planner under cahoots could not run
   `git log` or the binary.
3. **No writer commits or pushes.** A Codex writer's sandbox has no network.
   A callee's environment is an allowlist with nothing for a signing agent.
   The contract says a change is a proposal the caller brings over, and
   "cahoots never merges for anyone". The runbook asks the opposite of a
   builder: pass the gate, commit, sign, push.

**#48 changes where a writer runs, and none of the three.**

The routes, which combine:

| Route | Closes | Cost |
|---|---|---|
| **Same-harness targets**, by a person's setting | 1 | The contract's sentence, the skill text, and the blind runs' assumption that the target is the other harness. The bounds on recursion are already slots and the ledger, not the exclusion |
| **A shell for the Claude writer inside Claude Code's own sandbox** | 2 | The Writers table, `validate` and `smoke`. THREAT-MODEL names this as the condition. Whether Claude Code can turn its sandbox on from a command line cahoots would build was not verified |
| **The orchestrator commits**: the writer writes, the orchestrator runs the gate, sends failures back with `resume`, then commits, signs and pushes | 3 | None in cahoots: it is the contract as written. In the skill, "agents commit and push" goes, and the orchestrator, already the busiest session, takes on more |

Ruled out: a shell with no sandbox, a list of allowed commands from config
(rules 3 and 6), cahoots starting an agent with the person's own permissions
on an agent verb (rule 6), and cahoots pushing (rule 1).

**Recommendation: not yet, and in two steps.** Keep the builders in panes.
First decide same-harness targets on their own: it is the cheapest route, it
brings Claude judges and one-shot explorers under the gate and the record,
and gaps 2 and 3 need it. Then check the sandbox switch. Only with all three
routes open is a builder under cahoots better than a builder in a pane, and
until then moving them would trade Opus builders that test their own work
for a fence. The evals so far say those builders matter.

## 4. The order

What is open on 2026-10-04, and where each gap goes. In flight: #61
(PR #88), then #86, #71 and #54.

| Step | What | Gap | Issue | Size |
|---|---|---|---|---|
| 0 | Finish the wave in flight: pinned paths, the Homebrew path, agent paths, the plugin install | 7 | #61, #86, #71, #54 | in flight |
| 1 | The narrow agent paths | 7 | #89 to #93 | XS to M |
| 2 | Board reviews through cahoots, by hand, as the trial | 8, 12 | none: the orchestrator's work | XS |
| 3 | A run from another repository, and one live run per worktree and session | 13, 15 | two **new** | S, S |
| 4 | A run's latest activity | 11 | **new** | S |
| 5 | The skill moves `start_review.py` onto cahoots, and the desk and `flow.py` read `cahoots status` | 8 | a project-builder proposal | S |
| 6 | The builders' decision | 10 | **new** memo, then Avihu | S |
| 7 | By that decision: same-harness targets, then the sandboxed shell | 10 | **new**, from the memo | M, L |
| 8 | The plan that resets sooner | 3 | **new** | M |
| 9 | The machine: priority, the share of the cores, admission by load | 9 | three **new** | S, S, M |
| 10 | Writers in panes, re-read against step 6 | 1 | #48 | L |
| 11 | The consult link | 4 | **new** | XS |
| later | The MCP decision, the release and brew, the Codex teammate, worktrunk, the skill's eval suite | | #59, #60, #58, #47, #56 | |
| parked | With the evals, until a new model or a drop in the record | 2, 5, 6 | #39 to #46, #94, #95, and the two **new** eval tickets | |
| any time | The `ultra` effort | 14 | **new** | XS |

Why this order:

- **Steps 2 to 5 make cahoots the daily path for the one shape it already
  covers.** They are small, they need no decision, and they give cahoots
  real load to learn from.
- **Step 6 comes before #48.** #48 is the largest ticket on the board, and
  what it should build depends on whether builders ever run under cahoots.
- **Steps 8 and 9 follow step 7** because each does little until a Claude
  caller has two harnesses to choose between and writers that build run
  under cahoots.

## 5. For Avihu

1. **The builders (gap 10).** Is cahoots meant to be the engine for the
   orchestrator's own agents, or for cross-vendor work only? *Recommended:*
   for all of them in the end, in two steps. Decide same-harness targets
   now, as a person's setting. It changes the contract's "a harness does not
   delegate to itself", which is his to change. Keep the builders in panes
   until a Claude writer can have a sandboxed shell.
2. **A group of runs (gap 2).** May cahoots hold N runs as one unit? It is
   the first thing it would hold that is more than a run. *Recommended:*
   yes, as a label on runs that one call started and a way to gather them.
   No queue, no roster, and no next step: which arms run and who won stay
   the orchestrator's. Parked with the evals, so nothing is needed now.
3. **The machine (gap 9).** One owner or two? *Recommended:* two. cahoots
   regulates what it starts, and `heavy.py` stays in the skill, since
   cahoots must never run a caller's command. The three tickets wait for
   step 7.

Nothing here bends a hard rule. Gap 11 and same-harness targets each change
THREAT-MODEL in their own PR.

## 6. How this was checked

Read at `eb422d4`: `AGENTS.md`, `docs/ARCHITECTURE.md`,
`docs/THREAT-MODEL.md`, `docs/PLUGINS-AND-EVALS.md`, the installed skill
text, and in `src/`: `cli.rs` (the tiers), `pick.rs`, `paths.rs`,
`run/client.rs`, `run/supervise.rs`, `placement/mod.rs`, `patch.rs`,
`model.rs`, `registry.rs`, `config.rs`, `env.rs`,
`harness/claude.rs`, `harness/codex.rs`, `meter/mod.rs` and
`install/rules.rs`. Of the skill, at `4e6c5ac`: `SKILL.md`, `ACTIONS.md`,
`tools/` and the review brief. Of the orchestrator's repository: the
runbook, the state file, and two research notes (the machine's load, and an
earlier inventory written at `ae3c8c0`, which this document starts from and
re-checks). Issues #39, #41, #43, #45, #46, #48, #54, #58, #59, #61, #63,
#71, #86, #89, #92, #93 and #94. Nothing was built or run.

Not verified:

- what a withheld reply or a model switch looks like in a headless run
  (gap 12);
- whether Claude Code has a command-line switch for its sandbox that cahoots
  would build (3c);
- that a Codex writer cannot commit in a linked worktree, and that the gate
  runs offline inside its sandbox, which the hidden-test author would need;
- that a Codex read-only run can read files outside its working directory,
  which is why 3a puts everything the reviewer reads inside the worktree;
- that a Codex read-only run can run `git diff` between two named commits,
  which every review round in 3a needs;
- that no code path other than `paths::run_dir` gives `--dir` a configured
  root (gap 13);
- whether the Agent Usage tracker's `headroom` answer carries a reset time
  that gap 3 could read without a change to the tracker;
- how a callee's priority can be lowered inside the closed dependency list
  and without unsafe code (3b).
