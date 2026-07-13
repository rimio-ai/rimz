# Budgets

You already cap spend where the provider lets you: a billing alert on the API dashboard, a plan whose included usage refills on a window. Those guard the account. They know nothing about the work: no provider setting says "this experiment is worth $5", "the nightly triage loop gets $20 a day", or "this room stops at $50 a day, whatever I forgot to close before bed". Once the fleet runs unattended — [auto-continue](./loops.md#built-in-recovery) resuming parked turns, loop tasks firing at 02:00 — the gap between what you meant to spend and what the account allows is exactly where the surprise bill lives.

A budget is a dollar cap RimZ enforces at the scale you promise yourself. It can enforce one because it already computes the fleet's live spend from the transcripts your agents write ([Token Insight](./insight.md)); a budget turns that read into a stop. Crossing a cap parks the agent — the same rest state a provider rate limit produces — and a parked agent is one message away from resuming.

One term collision to clear first: the `5h` and `7d` bars on the provider dashboard are also called budgets. Those are your subscription plan's included usage, metered by the provider and read-only to you ([budget is not spend](./insight.md#budget-is-not-spend)); starting those windows on your clock is a [scheduled ping](./loops.md#prime-the-budget-window), and scheduled work can [soak up only their weekly surplus](./loops.md#soak-up-the-weekly-surplus). This page is about the dollar caps you set.

## One model, four scopes

A cap is a dollar amount (`5`, `$4.50`), optionally windowed with `/day`, which measures from local midnight in your configured `timezone`. Four scopes wear the same model:

| Scope | Caps | Set with | Window |
| --- | --- | --- | --- |
| Agent | one agent's spend | `--budget` at launch, or the profile `budget` field | session, or `/day` |
| Loop task | one task's scheduled runs | `rimz loop add … --budget 5 --budget-per-day 20` | per run, per day |
| Room | every agent under one project root, worktrees included | `harness.budget = "50/day"` | `/day` only |
| Account | one provider login, every room on the machine | `[accounts.budget] claude = "100/day"` | `/day` only |

Every scope is checked independently, so the first cap crossed is the one that parks.

### Cap one agent

`--budget` at launch caps the session; `/day` measures each local calendar day instead, so a long-lived agent resumes with fresh headroom at midnight:

```sh
rimz agents codex --budget 5 "migrate the config parser"   # parks at $5 of session cost
rimz agents claude --budget 20/day                         # resets at local midnight
```

In a profile the same cap is the `budget` field, so a preset carries its price ceiling with it. The cap never reaches the stock CLI: RimZ's own launcher carries it, and the room enforces it, so it works identically for every agent kind.

Inspect or change a running agent's cap without relaunching:

```sh
rimz agents budget @coder         # current spend, cap, window, park state
rimz agents budget @coder 10      # replace the cap
rimz agents budget @coder +5      # add headroom
rimz agents budget @coder clear   # remove it
```

A supervised `-p` run treats its `--budget` as a run outcome instead of a park: the run records `budget_exceeded` and exits `125`, distinct from a timeout or failure, so a pipeline branches on it ([scripting](./scripting.md#one-turn-one-exit-code)).

### Cap a loop task

A scheduled task takes the same `--budget` to cap each fired run, and adds `--budget-per-day`: the scheduler sums that task's completed run costs for the local day and skips a fire that cannot fund its per-run cap, recording `budget skipped`. `rimz loop list` shows each task's spend against its daily cap, and `rimz loop show` reads back per-run costs and the rolling average. The schedule grammar and run forensics are the [loops guide](./loops.md); the flag semantics are the [loop reference](../reference/cli/loop.md).

### Cap the room, cap the login

The two daily scopes switch on in config, and only in config:

```sh
rimz config set harness.budget 50/day              # this project's whole fleet
rimz config set accounts.budget.claude 100/day     # one login, every room on the machine
```

Both keys live in your per-machine `config.toml`, require the `/day` form, run no command, and stay outside the project trust hash. The room cap counts every agent under the project root, worktrees included. The account cap sums one provider login across every room on the machine, but each room parks only the panes it owns, so a cap crossed in one project never reaches into another room's panes.

`rimz budget` reads and adjusts what config armed:

```sh
rimz budget                        # room cap, source, today's spend, park state, and every account cap
rimz budget 30/day                 # replace the room cap
rimz budget +10                    # add headroom
rimz budget off                    # disable it; `clear` is an alias
rimz budget +25 --account claude   # the same verbs against one login
```

Adjustments are runtime state under RimZ's own state directory, never edits to your files: `config.toml` keeps the number you committed to, and `rimz budget` refuses to arm a cap that config never switched on. To change the standing promise, change the key.

## What crossing a cap does

The enforcement is small enough to hold in your head:

1. The room's sidebar process re-checks every scope on its regular tick, against the same transcript-derived spend that [Token Insight](./insight.md#how-the-numbers-are-calculated) reads, plus each card's live cost.
2. When a running turn crosses a cap, RimZ presses Esc in that agent's pane (the same interrupt you would type) and records the park.
3. The card reads `⏸` with the reason. A crossed room or account cap also turns the cockpit or provider row alarm-red and explains itself as `$50.21 of $50/day`.

Agents already at rest keep their lifecycle status, and a waiting agent keeps its ask visible until the answered turn runs again; a cap interrupts spending, not conversation. Nothing exits and nothing is lost: the CLI stays in its pane, the session files stay where the provider put them, and the turn's work up to the interrupt is in the transcript.

Two honest limits. Cost arrives with provider responses, so the last tool call can overshoot the cap slightly before the Esc lands: the cap stops the next spend, never the request already in flight. And a cap can only count spend it can price: a model missing from the price table contributes tokens but zero dollars until the weekly price refresh knows it ([how the numbers are calculated](./insight.md#how-the-numbers-are-calculated)).

## What resumes a parked agent

- **You do.** A human message delivered after the park waives that agent's next turn, once; the waiver is consumed when the turn ends. Background and agent-to-agent deliveries stay parked, so a chatty team cannot spend through your cap.
- **The day does.** A `/day` park reopens at the next local midnight, and with [auto-continue](./loops.md#built-in-recovery) on, the continue prompt goes to the agents RimZ interrupted — never to agents that were already at rest.
- **A raise does.** Raising or clearing a cap (`rimz agents budget @coder +5`, `rimz budget +10`) queues the configured continue prompt to the agents that cap parked in this room; add `--no-continue` to lift the cap and leave them at rest.
- **Automation never does.** While the room or account scope has no headroom, `-p` launches exit `125` and loop fires record `budget skipped`. Interactive launches stay available, so a crossed cap never locks you out of your own room.

## See also

- [Token Insight](./insight.md) — reading the spend that budgets act on: the cockpit, the provider dashboard, and `rimz stats`.
- [Loops](./loops.md#built-in-recovery) — auto-continue, task budgets, and hands-off recovery in one loop.
- [Scripting](./scripting.md) — the `-p` exit-code contract, including `125` for a run over budget.
- [Configuration → daily dollar budgets](./configuration.md#daily-dollar-budgets) — the `harness.budget` and `[accounts.budget]` keys.
- [Budget reference](../reference/cli/agents.md#inspect-and-change-a-budget) — the complete `rimz budget` and `rimz agents budget` surface.
- [Providers internals](../internals/agents/providers.md) — spend windows, ledgers, and park mechanics in depth.
