# Dollar budgets

> The budget engine: the scopes it enforces, the ledgers it keeps, the verdict that decides a park, the one human waiver, the pane interrupt, and the fail-fast gate programmatic callers hit. The code is [`harness/budget.rs`](../../../crates/rimz/src/harness/budget.rs). [harness.md](./harness.md) is the map for this area. For users, the guide is [budget.md](../../guide/budget.md) and the commands are [cli/agents.md](../../reference/cli/agents.md#inspect-and-change-a-budget).

## What the engine does

A budget is a dollar cap RimZ enforces itself, at a scale the provider has no concept of: this agent, this room, this login. Crossing one **parks** the agent. RimZ presses Esc in its pane, marks the ledger, and leaves everything else alone. The CLI keeps running, the session files stay where the provider put them, and the turn's work up to the interrupt is already in the transcript.

The engine can do this because the spend already exists. Agents write transcripts, the spending walk prices them, and the producer publishes per-room and per-account tallies ([providers.md](../agents/providers.md#live-cost-coverage)). Budgets read those numbers and turn a reading into a stop.

Two rules shape the module, and between them they explain most of its shape.

> **The ledger is the truth. The pane interrupt is an effect.**

Whether an agent is parked is answered by reading a small JSON ledger, never by inspecting a pane. The Esc keypress is what makes the park visible to the agent; if it fails, the ledger still says parked and the next tick tries again. Nothing about correctness depends on the keypress landing.

> **`budget.rs` never writes to the store.**

Evaluation runs inside the sidebar producer, and the [sidebar is read-only on the store](../sidebar/state.md). So the module writes cache-class files only, and hands every durable effect to a hidden helper: `rimz agents budget-park` owns the pane keypress and the supervised-run transition. That split is what lets the budget tick live in the sidebar's import graph at all. `cargo xtask invariants` guards the `sidebar/` tree itself; here the rule is carried by the module header and the [harness contract](../../../crates/rimz/src/harness/AGENTS.md), so a change that reaches for a store writer is a review question rather than a gate failure.

## The five scopes

Four scopes live in this module and are evaluated together on every producer tick. The fifth is a loop task's own daily cap, which the scheduler checks before it fires ([loops.md § One fire](./loops.md#one-fire)).

| Scope | Caps | Cap comes from | Window | Ledger |
| --- | --- | --- | --- | --- |
| Agent | one agent session | launch identity: `--budget`, a profile, or a team role | session, or `/day` | `budget.<digest>.json`, per room |
| Turn | every agent turn in one room | machine config `harness.turn_budget` | one turn | `budget.scopes.json`, per room |
| Room fleet | every agent under one project root | machine config `harness.budget` | `/day` only | `budget.fleet.json`, per room |
| Provider account | one provider login, every room on the machine | machine config `[accounts.budget].<kind>` | `/day` only | `budget.account.<kind>.json`, machine-shared |
| Loop task | one task's scheduled runs | the task's `--budget-per-day` | `/day` only | derived from the run log, no ledger |

Enforcement treats the scopes independently: the first cap crossed parks the agent. Display picks one, and agent beats turn beats fleet beats account, so a card names the tightest reason rather than the broadest.

A cap parses as a [`BudgetSpec`](../../../crates/rimz/src/harness/budget.rs): a non-negative dollar amount with an optional `/day` suffix, accepting `5`, `$4.50`, and `20/day`. Bare amounts are `BudgetWindow::Session`; the suffix makes it `BudgetWindow::Day`, measured from local midnight in the configured `timezone`. `BudgetWindow::Turn` is internal to turn-park projection and the public parser rejects `/turn`. A leading `+` is rejected at parse time, because raising a cap is a CLI verb rather than a value.

## Where a cap comes from

An agent cap enters through launch identity and is then owned by its ledger. `ledger_for_agent` re-reads the launch spec on every tick and refreshes `ledger.spec` when it differs, so relaunching with a different `--budget` takes effect. A runtime raise or a `clear` pins the ledger and stops that refresh, so a CLI adjustment is not silently undone by the next tick.

The turn cap comes directly from machine config as a cents-backed `TurnCap`. It accepts only a plain amount and has no launch flag, profile field, runtime override, raise, clear, or waiver.

The two daily scopes resolve through [`DailyBudgetScope::effective_cap_usd`](../../../crates/rimz/src/harness/budget.rs), and the result carries a `BudgetCapSource` that `rimz budget` prints verbatim.

| Source | Means |
| --- | --- |
| `none` | machine config never armed this scope; nothing is enforced |
| `config` | the configured cap applies unchanged |
| `override` | a runtime absolute cap replaces it (fleet only) |
| `raised` | a runtime `+AMOUNT` raise applies on top |
| `cleared` | a runtime `off` disables the scope until config or another verb re-arms it |

Machine config is the on-switch and the runtime ledger is the adjustment. A daily scope with no configured cap cannot be armed at runtime at all: `rimz budget 30/day` on an unconfigured scope fails with the `rimz config set` line that would arm it. The per-agent CLI has no such rule, because an agent cap is a property of one session rather than a standing promise.

Account scopes carry one more precondition. `[accounts.budget].<kind>` is honored only for a provider whose adapter reports authoritative account-level dollar history; the eligibility rule and its rejection paths are [providers.md § Daily dollar caps](../agents/providers.md#daily-dollar-caps).

## Where the spend comes from

Each scope reads a different tally, and each read is guarded against reading the wrong day.

**An agent's session spend** is `total_cost_usd`: the card's cumulative cost, admitted only when its coverage `contributes_to_live_spend()`, and only when finite and non-negative. Coverage that describes a window rather than a session never reaches a budget ([providers.md § Live cost coverage](../agents/providers.md#live-cost-coverage)).

**An agent's `/day` spend** subtracts a `day_baseline` from that same cumulative number. The baseline is stamped on first evaluation of a date and re-stamped when the date changes, so one long-lived session measures each calendar day separately without the provider ever resetting its counter.

**An agent's turn spend** subtracts the cumulative session cost observed when `turn_started_at` first appears or advances. Rebase happens before comparison, clears the old park and interrupt throttle, and makes the first tick of a new turn read zero even when the prior turn ended over cap. A missing `turn_started_at` clears the entry.

**Fleet spend** is the workspace spending cache for the current local day, with the live overlay applied so costs that have not yet flushed into the walk still count.

**Account spend** is the machine-shared provider spending cache, per kind, for the current local day.

Both daily reads compare the cache's own `day_cutoff_secs` against the cutoff computed from `now`. A cache stamped for a different day contributes zero rather than yesterday's total. Staleness therefore reads as *no spend recorded yet*, and the cap holds off rather than parking the fleet on a stale number.

## The ledgers on disk

| File | Scope | Holds |
| --- | --- | --- |
| `budget.<digest>.json` | one agent | the spec, a runtime raise or disable, the day baseline, the park stamp, the interrupt throttle, the waiver |
| `budget.fleet.json` | this room | a runtime override, raise, or disable, and the park stamp |
| `budget.account.<kind>.json` | one login, machine-wide | a runtime raise or disable, and the park stamp |
| `budget.scopes.json` | this room | per-agent turn baselines, parks, and interrupt throttles, plus fleet and account waivers, park thresholds, and interrupt throttles |

The agent digest is the first 32 hex characters of a SHA-256 over the kind and session id, which keeps a session id out of a filename. [`auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs) reuses the same digest for its park records, so the two files for one agent sit side by side.

All of these are cache-class: `write_temp_then_rename_cache`, rebuildable, and safe to delete. Losing one loses a park, not money. The two scope ledgers additionally take a lock file around read-modify-write, so a producer tick merging a fresh park cannot clobber a cap change a CLI made in the same instant. The producer merges only the `parked` field back and leaves the cap fields as it found them.

Only the account ledger is machine-shared. Every room on the machine evaluates the same account spend against the same ledger, but each room interrupts only the panes it owns. Raising an account cap from one room therefore nudges the agents *that room* interrupted, while agents in other rooms stay at rest until their own producer sees the cleared park.

## The verdict

[`budget::evaluate`](../../../crates/rimz/src/harness/budget.rs) folds one agent's spend and its ledger into a `BudgetVerdict`. It is pure: it mutates the ledger in memory and returns a decision, and the caller owns every write and every side effect.

```text
                    ┌── no effective cap ──────────────► Disabled
                    │
spend vs cap  ──────┼── under cap ─────────────────────► Under
                    │                                    (clears any park and interrupt throttle)
                    └── at or over cap
                          └─ waiver check
                               ├─ a human delivery landed after the park,
                               │  and the current turn started after it
                               │     ├─ turn still running ──────────► Waived
                               │     └─ turn finished ───────────────► Park (waiver consumed,
                               │                                        park re-stamped at now)
                               └─ otherwise ─────────────────────────► Park
```

`Under` is restorative, not merely negative: it clears the park stamp and the interrupt throttle, so raising a cap above current spend un-parks the agent on the next tick with no separate reset path. A `/day` rollover does the same thing more broadly, clearing the park, the throttle, and the waiver together with the baseline.

The two daily scopes run a smaller version of the same fold ([`evaluate_daily_scope`](../../../crates/rimz/src/harness/budget.rs)): a cap, a spend, a park stamp, no waiver. Their waivers are per-agent rather than per-scope, and live in `budget.scopes.json`, because one person answering one agent should not un-park the whole fleet.

The turn scope runs [`evaluate_turn_scope`](../../../crates/rimz/src/harness/budget.rs) per root agent. A matching turn entry compares cumulative cost minus its baseline; a changed `turn_started_at` rebases before comparing, and an at-or-over reading parks with no waiver. The next human prompt starts a new turn and clears the park through that rebase. A teammate-triggered turn uses the same cap, while delivery gating keeps background and agent-to-agent traffic from reopening a paused agent.

## The waiver

The waiver is the one place a human overrides a cap, and it is deliberately narrow.

A message qualifies only if it was **delivered**, sent by a **human**, not marked `automated`, and not carried on the `Resume` gate. That last exclusion matters: auto-continue records render as human-authored so the transcript attributes them correctly, and without the gate check they would waive the cap they exist to wait out.

A qualifying delivery stamps a waiver at its delivery time. A turn that starts at or after the stamp runs while the agent is `Running`, and the terminal transition consumes the waiver and re-stamps the park at `now`, moving it past the delivery that granted it. One message therefore waives exactly one turn, and a second message is required for a second turn. Background and agent-to-agent traffic never waives at all, so a chatty team cannot spend through a cap that a human set.

Programmatic entry points do not consult the waiver at all. See [the fail-fast gate](#the-fail-fast-gate).

## The park

[`budget::enforce`](../../../crates/rimz/src/harness/budget.rs) runs on the producer's refresh tick against a snapshot with live day spend applied. Per agent, skipping empty and provisional ids:

1. **Evaluate.** One agent verdict from its own ledger, one turn verdict from `budget.scopes.json`, plus one scope verdict from the binding fleet or account park and this agent's waiver state.
2. **Classify.** All-under, agent-parked, turn-parked, or scope-parked. All-under includes the turn verdict, clears the auto-continue budget record, and stops there.
3. **Arm the day reset.** A parked daily scope, or a parked `/day` agent cap, arms an auto-continue park with the next local midnight as its deadline. A turn-only park clears any armed budget auto-continue because its reset is a prompt rather than a clock. The `Budget` park class is described in [loops.md § Recovery the elder runs](./loops.md#recovery-the-elder-runs), and it is checked before any provider-derived classification.
4. **Interrupt.** A `Running` agent with a live bound pane, past its interrupt throttle, gets the detached `rimz agents budget-park` helper. The throttle is 120 seconds, so an agent that keeps running past a park is re-interrupted every two minutes rather than every tick.
5. **Persist.** Changed agent ledgers, merged scope parks, and changed scope state are written back.

The helper ([`budget_park.rs`](../../../crates/rimz/src/cli/agents_cmd/budget_park.rs)) is small, and it exists to do the two things the producer may not: touch a pane, and write to the store. It re-checks that the target pane is still bound to that agent, presses Esc, then transitions every non-terminal run record for that agent to `budget_exceeded` and wakes its waiters. It always carries the agent's *own* cost, not the fleet or account figure, because a run record describes its own spend while the broader figure only explains why the pane stopped.

Which agents a park may touch is one predicate, [`pause_applies`](../../../crates/rimz/src/harness/budget.rs): an agent that is `Running`, or one this park already interrupted and that is not `Waiting`. Agents at rest keep their lifecycle status, and a waiting agent keeps its ask visible until the answered turn runs again. A cap interrupts spending rather than conversation.

`project_parks` then stamps the resulting `BudgetPark` onto each agent for the read side. That projection is what makes [`effective_status()`](../agents/model.md#displayed-status) report `Paused`, which is in turn what message delivery gates read ([messaging.md](./messaging.md#status-lifecycle)). `project_budget_views` fills the cockpit and provider-dashboard cap rows from the same ledgers.

## The fail-fast gate

Interactive work gets a park and a waiver. Automation gets a refusal.

[`budget::scope_gate`](../../../crates/rimz/src/harness/budget.rs) reads the fleet and account ledgers against the same local-day caches and returns a reason string, or nothing. It consults no waiver, spawns nothing, and touches no pane. One extra rule guards it against cache lag: when a park stamp was written on or after the current local day start, the gate takes the greater of the cached spend and the cost at the park, so a lagging cache cannot re-open a scope that already parked today.

| Caller | On a refusal |
| --- | --- |
| Supervised run ([scripting.md](./scripting.md#status-and-exit-codes)) | `SupervisedRunOutcome::BudgetExceeded`, exit `125`, before any record or pane exists |
| Loop fire ([loops.md](./loops.md#one-fire)) | one `budget skipped` history row, no strike |

Supervised runs re-check the gate at the top of every attempt, so a retry ladder stops the moment a cap closes mid-sequence. Both callers check an exact managed-launch provider quota alongside this gate; that boundary is a separate mechanism and belongs to [providers.md](../agents/providers.md).

The per-task daily cap is the scheduler's own gate ([`run_log::daily_budget_gate`](../../../crates/rimz/src/harness/schedule/run_log.rs)) and runs before this one. It sums the task's completed runs for the local day and skips the fire when the day is already spent or when the task's per-run reservation would not fit under the cap, which is why `--budget-per-day` requires `--budget` to mean anything.

## The CLI surface

Two commands write these ledgers, and neither edits your config files.

[`rimz agents budget`](../../../crates/rimz/src/cli/agents_cmd/budget.rs) inspects or sets one agent's cap. An absolute value lands as a raise over the launch spec, with the window written onto the spec; switching to `/day` stamps a fresh baseline at the current cost, and switching back to a session window drops it. Every mutation clears the park, the throttle, and the waiver.

[`rimz budget`](../../../crates/rimz/src/cli/budget.rs) inspects or sets a daily scope, defaulting to the fleet and taking `--account <kind>` for a login. It refuses to arm what config never switched on, and refuses to raise a cleared or unset cap.

When configured, the read-only output also shows `harness.turn_budget` as a per-turn cap with source `config`. Its mutation verbs remain daily-scope only.

Both queue the configured continue prompt after lifting a park, and only to agents this room actually interrupted, so clearing a cap does not nudge agents that were resting for their own reasons. `--no-continue` lifts the cap and leaves them alone.

## Tests

[`budget/tests.rs`](../../../crates/rimz/src/harness/budget/tests.rs) covers the module, and `spawn_budget_park` is compiled out under `cfg(test)` so a park can be asserted without a pane. The contract cases cover agent waivers, day rebasing, turn rebasing and stale projection, scope gates, interrupt throttles, and auto-continue classification.

## See also

- [budget.md](../../guide/budget.md): the user-facing model, the five scopes, and what a park means in practice.
- [providers.md](../agents/providers.md#daily-dollar-caps): account cap eligibility, the spend caches, and cost coverage.
- [scripting.md](./scripting.md): supervised runs and the exit-code contract that carries `125`.
- [loops.md](./loops.md): the fire gate ladder and the `Budget` auto-continue park class.
- [messaging.md](./messaging.md): delivery gates, the `automated` flag, and what a waiving message looks like.
