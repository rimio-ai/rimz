# Budgets

You already cap spend where the provider lets you: a billing alert on the API dashboard, a plan whose included usage refills on a window. Those guard the account. They know nothing about the work: no provider setting says "this experiment is worth $5", "the nightly triage loop gets $20 a day", or "this room stops at $50 a day, whatever I forgot to close before bed". Once the fleet runs unattended, with [auto-continue](./loops.md#keep-the-fleet-moving) resuming parked turns and loop tasks firing at 02:00, the gap between what you meant to spend and what the account allows is where the surprise bill lives.

A budget is a dollar cap RimZ enforces at the scale you promise yourself. It can enforce one because it already computes the fleet's live spend from the transcripts your agents write ([Token Insight](./insight.md)); a budget turns that read into a stop. Crossing a cap parks the agent, the same rest state a provider rate limit produces, and a parked agent is one message away from resuming.

One word collision to clear first. The `5h` and `7d` bars on the provider dashboard are also called budgets, and those are your subscription plan's included usage, metered by the provider and read-only to you ([budget is not spend](./insight.md#budget-is-not-spend)). Everything here is about the dollar caps you set, except the [last section](#the-surplus-gate), which reads the provider's bars to decide when background work may run.

## One model, five scopes

A cap is a dollar amount (`5`, `$4.50`), optionally windowed with `/day`. Five scopes share the same model:

| Scope | Caps | Set with | Window |
| --- | --- | --- | --- |
| Agent | one agent's spend | `--budget` at launch, or the profile `budget` field | session, or `/day` |
| Turn | every agent turn in the room | `harness.turn_budget = "3"` | turn |
| Loop task | one task's scheduled runs | `rimz loop add … --budget 5 --budget-per-day 20` | per run, per day |
| Room | every agent under one project root, worktrees included | `harness.budget = "50/day"` | `/day` only |
| Account | one provider account, every room on that account | `[accounts.budget] claude = "100/day"` | `/day` only |

A `/day` window runs from local midnight in your configured [`timezone`](./configuration.md#sidebar-rendering). Each scope is checked on its own, so the first cap crossed is the one that parks.

One limit stops a launch without being a dollar cap. Qwen's Alibaba Coding Plan quota belongs to the exact account rather than to the CLI, so an unattended launch on an account whose quota window is spent exits `125`, or records `budget skipped` on a loop task, before a pane exists. Launches you make by hand ignore it ([agent support](../reference/agent-support.md#qwen)).

### Cap one agent

`--budget` at launch caps the session; `/day` caps each local calendar day instead, so a long-lived agent resumes with fresh headroom at midnight:

```sh
rimz agents codex --budget 5 "migrate the config parser"   # parks at $5 of session cost
rimz agents claude --budget 20/day                         # resets at local midnight
```

A daily agent cap starts counting from the moment the cap takes effect, so a cap you add at noon ignores the morning's spend. The count then restarts on the first check after midnight. In a profile the same cap is the `budget` field, so a preset carries its price ceiling with it.

The cap never reaches the stock CLI: RimZ's own launcher carries it and the room enforces it, so it works for any agent whose running cost RimZ can read. Two agents publish nothing to read. Kiro exposes no machine-readable usage, and Antigravity prices the current turn rather than the session, so neither accumulates a total a cap could cross ([agent support](../reference/agent-support.md#notes-on-the-alpha-and-experimental-set)).

Inspect or change a running agent's cap without relaunching:

```sh
rimz agents budget @coder         # current spend, cap, window, park state
rimz agents budget @coder 10      # replace the cap
rimz agents budget @coder +5      # add headroom
rimz agents budget @coder clear   # remove it
```

In a supervised `-p` run the park is also the run's ending: the run records `budget_exceeded` and exits `125`, distinct from a timeout or a failure, so a pipeline branches on it ([scripting](./scripting.md#one-run-one-exit-code)).

### Cap every turn

`harness.turn_budget` bounds the dollars one agent can spend between the start and end of one turn, independently of the agent's session cap and the daily room and account caps:

```sh
rimz config set harness.turn_budget 3
```

The key takes a plain amount such as `3` or `$2.50`, applies to human- and teammate-triggered turns alike, and defaults to no limit. It lives only in per-machine `config.toml`: remove the key to turn the cap off. `rimz budget` displays the configured turn cap but does not adjust it.

Crossing the cap parks the current turn with no clock-based reset. The next human prompt starts a fresh turn and therefore a fresh baseline; background and agent-to-agent deliveries remain parked, so automation cannot reopen the spend path after crossing the cap.

### Cap a loop task

A scheduled `--agent` task takes the same `--budget` to cap each fired run, and adds `--budget-per-day`, which needs a `--budget` beside it to know what one run costs at most. The scheduler sums the task's completed run costs for the local day and skips a fire it cannot fund at that per-run cap, recording `budget skipped`. `rimz loop list` shows each task's spend against its daily cap, and `rimz loop show` breaks the costs down per run. The schedule grammar and run forensics are in the [loops guide](./loops.md#budgets-and-strikes), and the flag semantics in the [loop reference](../reference/cli/loop.md#budgets).

### Cap the room and the account

The two daily scopes switch on in config, and only in config:

```sh
rimz config set harness.budget 50/day              # this project's whole fleet
rimz config set accounts.budget.claude 100/day     # each claude account, every room on it
```

Both keys live in your per-machine `config.toml`, require the `/day` form, run no command, and stay outside the project trust hash. The room cap counts every agent under the project root, worktrees included. An account cap applies to each [account](./accounts.md) of that provider separately and sums that account across every room running on it, while each room parks only the panes it owns.

An account cap also needs a complete dollar history for that provider on disk, which only some agents publish: Claude, Codex, Grok, Pi, and OpenCode do today. For any other agent, config edits, room start, and `rimz budget --account` refuse the key outright rather than enforce against incomplete dollars.

`rimz budget` reads and adjusts what config armed:

```sh
rimz budget                        # room cap, source, today's spend, park state, and every account cap
rimz budget 30/day                 # replace the room cap
rimz budget +10                    # add headroom
rimz budget off                    # disable it; `clear` is an alias
rimz budget +25 --account claude   # the same verbs against this room's claude account
```

```console
$ rimz budget
scope:    fleet
cap:      $50.00/day
source:   config
spend:    $0.00 today
parked:   no
turn cap: $3.00/turn (source: config; per turn)

ACCOUNT         CAP          SOURCE  SPEND  PARKED
claude@default  $100.00/day  config  $0.00  no
```

`source` says where the cap in force came from: `config` for the key you set, `override` or `raised` once `rimz budget` changed it, `cleared` after `off`, and `none` when config never armed the scope at all.

An adjustment is runtime state RimZ keeps with the room's own files, never an edit to yours: `config.toml` keeps the number you committed to, and `rimz budget` refuses to arm a cap that config never switched on. To change the standing promise, change the key.

## What crossing a cap does

The enforcement is small enough to hold in your head:

1. The room's sidebar process re-checks the agent, turn, room, and account caps on every refresh it makes while the room is open, against the same transcript-derived spend that [Token Insight](./insight.md#how-the-numbers-are-calculated) reads. A loop task's own daily cap is checked separately, when its next fire is prepared.
2. When a running turn crosses a cap, RimZ presses Esc in that agent's pane (the same interrupt you would type) and records the park.
3. The card reads `⏸` and names the cap that stopped it: `budget: $5.04 of $5.00`, or `fleet budget: $50.21 of $50.00/day`. A crossed room or account cap also turns the cockpit or provider row alarm-red and repeats the numbers there as `$50.21 of $50/day`.

Agents already at rest keep their lifecycle status, and a waiting agent keeps its ask visible until the answered turn runs again; a cap interrupts spending, not conversation. Nothing exits: the CLI stays in its pane, the session files stay where the provider put them, and whatever the turn wrote before the interrupt is already in the transcript.

Two honest limits. A cap stops the next spend, never the request already in flight, because a request's cost arrives with the provider's response; how promptly it stops then depends on how often the agent's CLI reports what it spent, and Grok, for one, reports at a turn boundary rather than during a turn. And a cap can only count spend RimZ can price, so a model whose rate RimZ does not yet know contributes tokens but no dollars until a price for it lands ([how the numbers are calculated](./insight.md#how-the-numbers-are-calculated)).

## What resumes a parked agent

- **A new turn does.** A turn-budget park reopens on the next human prompt, which starts a fresh per-turn baseline. Teammate-triggered turns meet the cap too, and agent-to-agent traffic does not reopen an existing park.
- **You do.** A human message delivered after the park waives that agent's next turn, once; the waiver is consumed when the turn ends. Background and agent-to-agent deliveries stay parked, so a chatty team cannot spend through your cap.
- **The day does.** A `/day` park reopens at the next local midnight, and with [auto-continue](./loops.md#keep-the-fleet-moving) on, the continue prompt goes to the agents RimZ interrupted, never to agents that were already at rest.
- **A raise does.** Raising or clearing a cap (`rimz agents budget @coder +5`, `rimz budget +10`) queues the configured continue prompt to the agents that cap parked in this room; add `--no-continue` to lift the cap and leave them at rest.
- **Automation never does.** While the room or account scope has no headroom, `-p` launches exit `125` and loop fires record `budget skipped`. Interactive launches stay available, so a crossed cap never locks you out of your own room.

## The surplus gate

Dollar caps guard your wallet against spend you never meant to authorize. Background work threatens something subtler: the subscription window your own sessions run on. A recurring refactor or triage task is worth running only while it leaves your workday untouched, and you already make that call by eye. A glance at the `7d` bar on a light Thursday says the background refactor can run; on a heavy Tuesday everything waits for the real work. A task firing at 03:00 cannot make that call unless the schedule can read the bar.

The surplus gate is that glance, computed. Before a gated fire, RimZ reads the provider's longest pacing window and divides the share of budget remaining by the share of time remaining. `1.0x` means the current pace lands exactly on the reset; anything above it is budget the rest of the window does not need. Three days into a 7-day window with 40% used, 60% of the budget remains against 57% of the time, and headroom reads `1.05x`: sustainable, but no real slack. With only 20% used, the same clock reads `1.4x`, and a task gated at `--surplus 1.4x` or below fires.

The pacing window is the weekly bar on today's Claude and Codex plans. For a managed Qwen launch it is the exact account's longest Alibaba window up to seven days, so the `30d` window never sets the pace, though an exhausted one still blocks the launch the way any spent quota window does.

An elapsed floor runs ahead of the ratio: `--surplus-after 3d` keeps a task quiet until that much of the window has passed, and used alone it still requires `1.0x` once the floor clears.

The gate is a read, nothing more. Its headroom comes from the provider's own usage reporting, the same account-scoped reading that draws the dashboard bars, cached on disk by the sessions you already run. It fails closed: with no usable window reading for that exact account the gate stays shut, and a window that has expired or has not started yet counts as no reading. An account that publishes no quota window at all, a plain API key among them, therefore never fires a surplus-gated task. A closed gate writes `surplus skipped` into the task's run history with the reading it saw, `claude 7d window surplus 1.4x below 1.5x`, and the schedule keeps polling until real surplus appears.

The gate applies to loop tasks and nothing else. The recipe, and what a closed gate costs a task, are in [loops → gate a task on surplus](./loops.md#gate-a-task-on-surplus); the flag grammar is in the [loop reference](../reference/cli/loop.md#the-surplus-gate).

## See also

- [Token Insight](./insight.md): the cockpit, the provider dashboard, and `rimz stats`, where you read the spend a budget acts on.
- [Loops](./loops.md#keep-the-fleet-moving): auto-continue, task budgets, and hands-off recovery in one loop.
- [Scripting](./scripting.md): the `-p` exit-code contract, including `125` for a run over budget.
- [Configuration → dollar budgets](./configuration.md#dollar-budgets): the `harness.turn_budget`, `harness.budget`, and `[accounts.budget]` keys.
- [Budget reference](../reference/cli/budget.md): the complete `rimz budget` and `rimz agents budget` surface.
- [Providers internals](../internals/agents/providers.md): spend windows, ledgers, and park mechanics in depth.
