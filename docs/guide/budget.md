# Budgets

You already cap spend where the provider lets you: a billing alert on the API dashboard, a plan whose included usage refills on a window. Those guard the account. They know nothing about the work: no provider setting says "this experiment is worth $5", "the nightly triage run gets $5", or "this room stops at $50 a day, whatever I forgot to close before bed". Once the fleet runs unattended, with [auto-continue](./loops.md#keep-the-fleet-moving) resuming parked turns and loop tasks firing at 02:00, the gap between what you meant to spend and what the account allows is where the surprise bill lives.

A budget is a dollar cap RimZ enforces at the scale you promise yourself. It can enforce one because it already computes the fleet's live spend from the transcripts your agents write ([Token Insight](./insight.md)); a budget turns that read into a stop. A cap that counts Codex turns paid from a ChatGPT plan reads the [plan-burn estimate](./insight.md#how-the-numbers-are-calculated), so Fast-heavy work reaches it sooner and long-context work later. Crossing a cap parks the agent, the same rest state a provider rate limit produces, and a parked agent is one message away from resuming.

One word collision to clear first. The `5h` and `7d` bars on the provider dashboard are also called budgets, and those are your subscription plan's included usage, metered by the provider and read-only to you ([budget is not spend](./insight.md#budget-is-not-spend)). Everything here is about the dollar caps you set.

## One model, four scopes

A cap is a dollar amount (`5`, `$4.50`), optionally windowed with `/day`, or with `/month` on an account billed by API key. Four scopes share the same model:

| Scope | Caps | Set with | Window |
| --- | --- | --- | --- |
| Agent | one agent's spend | `--budget` at launch, or the profile `budget` field | session, or `/day` |
| Turn | every agent turn in the room | `harness.turn_budget = "3"` | turn |
| Room | every agent under one project root, worktrees included | `harness.budget = "50/day"` | `/day` only |
| Account | one provider's `default` and shared accounts together, or one standalone account; every room on them | `[accounts.budget] claude = "100/day"`, or a standalone account's `budget` | `/day`, or `/month` for API-key billing |

A room or account `/day` window runs from local midnight in your configured [`timezone`](./configuration.md#sidebar-rendering). An account `/month` window starts at 00:00 UTC on the 1st. An agent's day window starts when its cap takes effect and restarts on the first check after midnight. Each scope is checked on its own, so the first cap crossed is the one that parks. When several time-windowed caps bind, automatic continue waits for the last reset; the card still names the usual highest-precedence scope.

One limit stops a launch without being a dollar cap. Qwen's Alibaba Coding Plan quota belongs to the exact account rather than to the CLI, so an unattended launch on an account whose quota window is spent exits `125` before a pane exists. Launches you make by hand ignore it ([agent support](../reference/agent-support.md#qwen)).

### Cap one agent

`--budget` at launch caps the session; `/day` caps each local calendar day instead, so a long-lived agent resumes with fresh headroom at midnight:

```sh
rimz agents codex --budget 5 "migrate the config parser"   # parks at $5 of session cost
rimz agents claude --budget 20/day                         # resets at local midnight
```

A daily agent cap starts counting from the moment the cap takes effect, so a cap you add at noon ignores the morning's spend. The count then restarts on the first check after midnight. In a profile the same cap is the `budget` field, so a preset carries its price ceiling with it.

The cap never reaches the stock CLI: RimZ's own launcher carries it and the room enforces it, so it works for any agent whose running cost RimZ can read. Kiro meters credits without a dollar price and Hermes reports tokens with no billed dollars, so neither supplies a running dollar total a cap could cross. Antigravity's total is priced from its transcript as the file grows, so a per-agent cap sees its spend mid-turn. Its transcripts name no workspace, so a room cap counts an Antigravity agent's spend only while its card is live, and the room's day figure counts a live session's spend since the day's start ([agent support](../reference/agent-support.md#notes-on-the-alpha-and-experimental-set)).

Inspect or change a running agent's cap without relaunching:

```sh
rimz agents tune @coder budget         # current spend, cap, window, park state
rimz agents tune @coder budget 10      # replace the cap
rimz agents tune @coder budget +5      # add headroom
rimz agents tune @coder budget off     # remove it; `clear` is an alias
rimz agents tune @coder budget reset   # restore its launch cap, if any
```

In a supervised `-p` run the park is also the run's ending: the run records `budget_exceeded` and exits `125`, distinct from a timeout or a failure, so a pipeline branches on it ([scripting](./scripting.md#one-run-one-exit-code)). A [loop task](./loops.md#start-an-agent-from-a-task) caps its agent the same way, with `--budget` on the launch in its command, and such a fire then ends `failed`.

### Cap every turn

`harness.turn_budget` bounds the dollars one agent can spend between the start and end of one turn, independently of the agent's session cap, the room's daily cap, and account dollar caps:

```sh
rimz config set harness.turn_budget 3
```

The key takes a plain amount such as `3` or `$2.50`, applies to human- and teammate-triggered turns alike, and defaults to no limit. It lives only in per-machine `config.toml`: remove the key to turn the cap off. `rimz budget` displays the configured turn cap but does not adjust it.

Crossing the cap parks the current turn with no clock-based reset. The next human prompt starts a fresh turn and therefore a fresh baseline; background and agent-to-agent deliveries remain parked, so automation cannot reopen the spend path after crossing the cap.

### Cap the room and the account

Set standing daily defaults in config:

```sh
rimz config set harness.budget 50/day              # this project's whole fleet
rimz config set accounts.budget.claude 100/day     # claude's shared accounts together, every room on them
```

Both keys live in your per-machine `config.toml`, run no command, and stay outside the project trust hash. The room cap requires `/day` and counts every agent under the project root, worktrees included. An account cap accepts `/day` or `/month` and follows the provider's history: `default` and every shared [account](./accounts.md) of that provider spend against one cap and park together, and each standalone account has a cap of its own. Either way the spend is summed across every room running on those accounts, while each room parks only the panes it owns.

For a monthly API-key budget, give the account standalone history and its own cap, which wins over the provider default:

```toml
[accounts.claude.api]
history = "standalone"
budget = "300/month"
```

A per-account `budget` on shared history is refused: use the kind's `[accounts.budget]` cap for the shared pool instead. Monthly caps count API-key billing only, never subscription usage. If any member of a shared pool is a subscription, the monthly cap does not apply to that entire pool; separate the API-key account with standalone history. Room start warns when it can identify that subscription. A cold account reading does not switch the budget off. Daily account caps retain their existing subscription behavior.

This is a local transcript budget, not the billing authority: usage from another machine is invisible, and the spend walk can lag a running turn. Keep the provider console's spending cap as the hard backstop. For subscription spending, use Claude's extra-usage limit, Codex prepaid credits, or the provider's own limit instead.

An account cap also needs a complete dollar history for that provider on disk, which only some agents publish: Claude, Codex, Grok, Pi, and OpenCode do today. For any other agent, config edits and launches refuse the key outright rather than enforce against incomplete dollars. Room start warns instead. `rimz budget --account KIND` refuses only if the targeted kind is ineligible, ignoring other keys. Fleet reports omit ineligible kinds and print a warning on stderr for each key to remove; fleet adjustments still proceed.

`rimz budget` reads, sets, and adjusts a cap for this room or one account pool, even without a configured default:

```sh
rimz budget                        # room cap, source, today's spend, park state, and every account cap
rimz budget 30/day                 # replace the room cap
rimz budget +10                    # add headroom
rimz budget off                    # disable it; `clear` is an alias
rimz budget +25 --account claude   # the same verbs against this room's claude account
rimz budget --account claude@api 300/month # set a monthly API-key cap
```

The report puts today's or this UTC month's spend beside the cap, says whether the scope is parked, and gives the commands to raise or lift it. With no cap it gives the command to set one. The account table includes runtime-only caps and lifted pools, not just config defaults. An account report names the login you asked for, the pool it shares, and the other members, so you can see which accounts an edit affects. A subscription-exempt month cap warns on stderr, even when the uncapped pool is omitted from the table; setting or raising it still succeeds.

For scripts, add `--json` to either budget command. Dollar amounts are numbers, unknown agent spend is null, and account scopes carry the pool and all its members. The [budget reference](../reference/cli/budget.md) lists the fields.

`source` says where the cap in force came from: `config` for the key you set, `override` for an absolute cap set through `rimz budget`, `raised` for a raise on a base cap, `cleared` after `off`, and `none` when no base cap is set or the cap is a `/month` cap on a subscription pool, which never applies.

These commands never edit `config.toml`: the room cap stays with the room's records, and account caps stay in a shared pool ledger that survives reboot. Removing a configured default disarms a raise based on it, but an absolute cap set through `rimz budget` remains until you lift it with `off`. To change the standing default instead, change the config key.

## What crossing a cap does

The enforcement is small enough to hold in your head:

1. The room's sidebar process re-checks the agent, turn, room, and account caps on every refresh it makes while the room is open, against the same transcript-derived spend that [Token Insight](./insight.md#how-the-numbers-are-calculated) reads.
2. When a running turn crosses a cap, RimZ presses Esc in that agent's pane (the same interrupt you would type) and records the park.
3. The card reads `⏸` and names the cap that stopped it: `budget: $5.04 of $5.00`, or `fleet budget: $50.21 of $50.00/day`. A crossed room cap, or an account cap on the account the room launches under, also turns the cockpit or provider row alarm-red and repeats the numbers there as `$50.21 of $50/day`. An agent parked on another account's cap shows it on its card only, since the dashboard does not show that account.

Agents already at rest keep their lifecycle status, and a waiting agent keeps its ask visible until the answered turn runs again; a cap interrupts spending, not conversation. Nothing exits: the CLI stays in its pane, the session files stay where the provider put them, and whatever the turn wrote before the interrupt is already in the transcript.

Two honest limits. A cap stops the next spend, never the request already in flight, because a request's cost arrives with the provider's response; how promptly it stops then depends on how often the agent's CLI reports what it spent, and Grok, for one, reports at a turn boundary rather than during a turn. And a cap can only count spend RimZ can price, so a model whose rate RimZ does not yet know contributes tokens but no dollars until a price for it lands ([how the numbers are calculated](./insight.md#how-the-numbers-are-calculated)).

## What resumes a parked agent

- **A new turn does.** A turn-budget park reopens on the next human prompt, which starts a fresh per-turn baseline. Teammate-triggered turns meet the cap too, and agent-to-agent traffic does not reopen an existing park.
- **You do.** A human message delivered after the park waives that agent's next turn, once; the waiver is consumed when the turn ends. Background and agent-to-agent deliveries stay parked, so a chatty team cannot spend through your cap.
- **The window does.** A `/day` park reopens at local midnight; an account `/month` park at 00:00 UTC on the next 1st. With [auto-continue](./loops.md#keep-the-fleet-moving) on, the continue prompt waits for the last binding window to reset and goes to agents RimZ interrupted, never to agents that were already at rest.
- **A raise does.** Raising or clearing a cap (`rimz agents tune @coder budget +5`, `rimz budget +10`) queues the configured continue prompt to the agents that cap parked in this room; add `--no-continue` to lift the cap and leave them at rest.
- **Automation never does.** While the room or account scope has no headroom, `-p` launches exit `125`, scheduled wake-ups record `budget skipped`, and a workflow step waits at its node. Interactive launches stay available, so a crossed cap never locks you out of your own room.

## See also

- [Token Insight](./insight.md): the cockpit, the provider dashboard, and `rimz stats`, where you read the spend a budget acts on.
- [Loops](./loops.md#keep-the-fleet-moving): auto-continue and hands-off recovery in one loop.
- [Scripting](./scripting.md): the `-p` exit-code contract, including `125` for a run over budget.
- [Configuration → dollar budgets](./configuration.md#dollar-budgets): the `harness.turn_budget`, `harness.budget`, and `[accounts.budget]` keys.
- [Budget reference](../reference/cli/budget.md): the complete `rimz budget` and `rimz agents tune … budget` surface.
- [Providers internals](../internals/agents/providers.md): spend windows, ledgers, and park mechanics in depth.
