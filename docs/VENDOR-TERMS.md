# Where cahoots stands with each vendor's terms

*Read on 2026-09-20, from the vendors' own pages. Terms change; the dates and
links are here so you can check. This is a maintainer's honest reading, not
legal advice — and the account at risk is yours, so read the parts that
apply to you.*

## What cahoots does, precisely

It runs each vendor's **own, unmodified, first-party CLI** — `claude -p`,
`codex exec` — on your machine, signed in **as you**, through that CLI's own
login. It has no network code. It never reads, stores, forwards or refreshes a
credential or a token; it never talks to a vendor's servers; it shares no
account; and its purpose is to keep your usage *under* a share of each plan
that you choose.

That is one side of a line both Anthropic and OpenAI draw in their
documentation: *your own sign-in to the vendor's own client, driven by a
script on your machine* — as opposed to *a third-party application using your
subscription credentials*. cahoots is the first and never the second.

**No vendor's terms are written in terms of that line**, though. So what
follows is, for each vendor, what their text says, what it plausibly means
for a tool like this, and what could go wrong for you.

## Anthropic (Claude Code) — documented; the terms are ambiguous, and the ground is moving

What the text says:

- The Consumer Terms (§3, effective 2025-10-08) prohibit accessing the
  services "through automated or non-human means, whether through a bot,
  script, or otherwise" — "except when you are accessing our Services via an
  Anthropic API Key or where we otherwise explicitly permit it".
  <https://www.anthropic.com/legal/consumer-terms>
- Claude Code documents headless use as a feature: "It's available as a CLI
  for scripts and CI/CD", and `claude -p` is "the Agent SDK via the CLI".
  <https://code.claude.com/docs/en/headless>
- Subscription sign-in for scripts is documented: "generate a one-year OAuth
  token with `claude setup-token`" — "This token authenticates with your
  Claude subscription". <https://code.claude.com/docs/en/authentication>
- The Help Center, on a plan to meter this use separately: "For now, nothing
  has changed: Claude Agent SDK, claude -p, and third-party app usage still
  draw from your subscription's usage limits."
  <https://support.claude.com/en/articles/15036540-use-the-claude-agent-sdk-with-your-claude-plan>
- The line Anthropic draws: it "does not permit third-party developers to
  offer Claude.ai login into their own applications, or to route requests
  through Free, Pro, or Max plan credentials on behalf of their users" — and
  that does not "prevent an end user from signing in to the unmodified Claude
  Code binary with their own Claude subscription".
  <https://code.claude.com/docs/en/legal-and-compliance>
- And against: the "preferred way to access Anthropic services using
  third-party software… including open-source projects, is through API key
  authentication"; Anthropic "reserves the right to draw use of such
  third-party tools from usage credits rather than subscription limits", and
  prohibits tools that "attempt to route third-party traffic against
  subscription limits".
  <https://support.claude.com/en/articles/13189465-log-in-to-your-claude-account>

What it means: scripted `claude -p` under your own subscription is documented
and, today, expected. That the documentation *is* the "explicit permission"
the Consumer Terms require is an inference — Anthropic does not say so.
"Third-party traffic" and "ordinary, individual usage" are not defined.

What to know before you enable Claude Code as a target:

1. Anthropic may treat use through a tool like this as billable to usage
   credits instead of your plan, and may enforce "without prior notice".
2. Two announced changes would break or change this: a **paused** plan to move
   `claude -p` off plan limits onto a separate metered credit (cahoots' "share
   of the plan" would then mean something else), and `--bare` — API key only,
   no subscription login — which the docs say "will become the default for
   `-p` in a future release".
3. If `ANTHROPIC_API_KEY` is set, `claude -p` silently bills the API instead
   of your plan. cahoots strips it from the callee's environment unless you
   configure `billing = "api"` — which is also the route Anthropic prefers, if
   you would rather be unambiguous.

## OpenAI (Codex) — documented and supported; not blessed in so many words

What the text says:

- "Non-interactive mode lets you run Codex from scripts… You invoke it with
  `codex exec`", to "produce output you can pipe into other tools";
  "`codex exec` reuses saved CLI authentication by default".
  <https://developers.openai.com/codex/noninteractive>
- OpenAI documents automation that runs as your ChatGPT-plan account, while
  recommending otherwise: "The right way to authenticate automation is with an
  API key. Use this guide only if you specifically need to run the workflow as
  your Codex account." <https://developers.openai.com/codex/auth/ci-cd-auth>
- OpenAI invites third-party software to drive Codex — "Create your own agent
  that can engage with Codex" — where "Codex owns the ChatGPT OAuth flow".
  <https://learn.chatgpt.com/docs/codex-sdk> · <https://learn.chatgpt.com/docs/app-server>
- The Terms of Use (effective 2026-01-01) contain a general prohibition:
  "Automatically or programmatically extract data or Output". OpenAI has not
  said how that relates to `codex exec`, whose documented purpose is piping
  output. <https://openai.com/policies/terms-of-use/>

What it means: this is the clearest case. OpenAI's line is between running
Codex itself and "generic OAuth clients outside Codex", and cahoots only does
the former. There is no official sentence that says a broker may drive
`codex exec` on a ChatGPT plan — and none that says it may not.

What to know: OpenAI's written preference for programmatic use is an API key
(`billing = "api"`); plan usage is "subject to fair-use limits".

## Google (Antigravity CLI) — on a plain reading, prohibited under a personal sign-in. Not supported.

What the text says:

- "Using third party software, tools, or services to access the Service (e.g.
  using OpenClaw with Antigravity OAuth) is a breach of this Agreement", and
  abuse includes "using the Service in connection with products not provided
  by us". Such actions "may be grounds for suspension or termination".
  <https://antigravity.google/terms>
- The FAQ repeats it without qualification, and recommends "a Vertex or AI
  Studio API key" for third-party agents. <https://antigravity.google/docs/faq/>
- Yet the documentation shows `agy` driven from a subprocess "with your cached
  credentials". <https://antigravity.google/docs/cli/headless/>
- A Google maintainer has described automated bans, permanent on a second
  violation, aimed at tools that "harvest or piggyback on" the CLI's OAuth.
  <https://github.com/google-gemini/gemini-cli/discussions/20632>
- Public requests to Google to confirm that wrapping the official binary
  locally is compliant have had no staff answer since July 2026.

What it means: Google's one example is credential reuse, which cahoots does
not do, and a narrow reading is reasonable. But the contract's words cover a
broker that spawns `agy`, the vendor has not said which reading it enforces,
and the penalty is your Google account.

**So cahoots does not support Antigravity CLI under a personal Google
sign-in**, and will not until Google says in writing that running its own CLI
from another local program is permitted. The planned milestone is on hold for
that reason — not for a technical one.

## If you want to be unambiguous

All three vendors say the same thing about programmatic use: they prefer an
API key. `billing = "api"` for a harness passes your key through to that
vendor's own CLI and takes your subscription out of the question. It also
takes it out of cahoots' usage gate — an API key has no plan to be a share of
— which leaves the built-in ledger (runs per hour, tokens per day) as the
budget.
