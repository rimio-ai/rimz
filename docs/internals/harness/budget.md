# Dollar budgets

> The budget engine: the scopes it enforces, where each cap and each spend figure comes from, the ledgers it keeps, the verdict that decides a park, the one human waiver, the pane interrupt, and the fail-fast gate programmatic callers hit. The code is [`harness/budget.rs`](../../../crates/rimz/src/harness/budget.rs). [fleet.md](./fleet.md) is the map for this area. For users, the guide is [budget.md](../../guide/budget.md) and the commands are [cli/agents.md](../../reference/cli/agents.md#inspect-and-change-a-budget).

## What the engine does

A budget is a dollar cap RimZ enforces itself, at a scale the provider has no concept of: this agent, this turn, this room, this login. Crossing one **parks** the agent: RimZ presses Esc in its pane, stamps the park in a ledger, and leaves everything else alone. The CLI keeps running, the session files stay where the provider put them, and the turn's work up to the interrupt is already in the transcript.

The engine reads spend that already exists. Agents write transcripts, the spending walk prices them, and the walk publishes per-room and per-account local-day tallies ([spending.md § What reads the totals](../agents/spending.md#what-reads-the-totals)). Budgets compare those figures against caps and turn a crossing into a stop.

Whether an agent is parked is answered by its ledger, never by its pane. The Esc keypress is only what makes the park visible to the agent. If the keypress fails, the ledger still says parked and a later tick interrupts again, so correctness never depends on the keypress landing.

`budget.rs` never writes to the store. Evaluation runs in the sidebar producer, and the [sidebar is read-only on the store](../sidebar/state.md), so the module writes cache-class ledger files only and hands every durable effect to the hidden `rimz agents budget-park` helper, which owns the pane keypress and the supervised-run transition. `cargo xtask invariants` enforces the boundary: `ensure_sidebar_library_boundaries` lists `budget.rs` among the harness modules in the sidebar import graph and fails the build when one of them imports a store writer, the run-wake sender, or the broker.

## The scopes

Four scopes live in this module, and the sidebar producer evaluates them together on every refresh tick. A fifth, a loop task's daily cap, belongs to the scheduler.

| Scope | Caps | Cap comes from | Window | Evaluated by |
| --- | --- | --- | --- | --- |
| Agent | one agent session | launch identity: `--budget`, a profile, or a team role | session, or `/day` | `evaluate` |
| Turn | every agent turn in one room | machine config `harness.turn_budget` | one turn | `evaluate_turn_scope` |
| Room fleet | every agent under one project root | machine config `harness.budget` | `/day` only | `evaluate_daily_scope` |
| Provider account | one provider account, across every room on it | machine config `[accounts.budget].<kind>`, applied to each account of the kind | `/day` only | `evaluate_daily_scope` |
| Loop task | one task's scheduled runs | the task's `--budget-per-day` | `/day` only | `run_log::daily_budget_gate` ([loops.md § One fire](./loops.md#one-fire)) |

Enforcement treats the scopes independently: the first cap crossed parks the agent. The displayed park picks one scope, in the order agent, turn, fleet, account (`project_parks`), so a card names the narrowest reason.

Every cap parses through [`BudgetSpec`](../../../crates/rimz/src/harness/budget.rs): a finite, non-negative dollar amount with an optional `$` prefix and `/day` suffix, such as `5`, `$4.50`, or `20/day`. A bare amount is `BudgetWindow::Session` and the suffix makes it `BudgetWindow::Day`, measured from local midnight in the configured `timezone`. A leading `+` is rejected, because a raise is a CLI verb, not a value. Machine config wraps the grammar in cents-backed types in `config/harness.rs`: `DayCap` requires `/day`, and `TurnCap` requires a plain amount. `BudgetWindow::Turn` exists only on projected turn parks, and the parser has no `/turn` form. The window, scope, and park vocabulary (`BudgetWindow`, `BudgetScope`, `BudgetPark`) lives in [`agents/state.rs`](../../../crates/rimz/src/agents/state.rs) so the read side can use it without the engine.

## Where a cap comes from

An agent cap enters through launch identity and is then owned by its ledger. `ledger_for_agent` re-reads the launch spec on every tick and copies it onto the ledger when it differs, so relaunching with a different `--budget` takes effect. Any runtime change through `rimz agents budget` (an absolute cap, a raise, or `clear`) sets `raised_cap_usd` or `disabled`, which pins the ledger and stops that refresh, so the next tick does not undo a CLI adjustment.

The turn cap is read straight from machine config on every tick. It has no launch flag, profile field, runtime override, raise, clear, or waiver, and removing the key drops every turn entry on the next tick.

The two daily scopes combine a configured cap with a runtime ledger. `DailyBudgetScope::effective_cap_usd` returns the cap that applies, and `DailyBudgetScope::cap_source` returns the `BudgetCapSource` that `rimz budget` prints beside it:

| Source | Means |
| --- | --- |
| `none` | machine config never armed this scope, or the account's kind is ineligible; nothing is enforced |
| `config` | the configured cap applies unchanged |
| `override` | a runtime absolute `/day` cap replaces the configured one (fleet only) |
| `raised` | a runtime cap in `raised_cap_usd` applies: the frozen total of a `+AMOUNT` raise, or, for an account, an absolute cap |
| `cleared` | a runtime `off` or `clear` disables the scope; only a later absolute cap re-arms it, and config edits do not |

Machine config is the on-switch and the ledger is the adjustment. A daily scope with no configured cap cannot be armed at runtime: `rimz budget 30/day` on an unconfigured scope fails with the `rimz config set` line that would arm it (`DailyBudgetScope::require_configured`). The per-agent CLI has no such rule, because an agent cap belongs to one session rather than to a standing promise.

An account cap also requires an eligible provider: `configured_cap_usd` honours `[accounts.budget].<kind>` only when the adapter reports authoritative account-level dollar history. The eligibility rule and its rejection paths are [providers.md § Daily dollar caps](../agents/providers.md#daily-dollar-caps).

## Where the spend comes from

Each scope reads a different figure.

| Scope | Spend |
| --- | --- |
| Agent, session window | `total_cost_usd`: the card's cumulative cost, admitted only when its coverage `contributes_to_live_spend()` and the value is finite and non-negative ([spending.md § Live cost coverage](../agents/spending.md#live-cost-coverage)) |
| Agent, `/day` window | the same cumulative cost minus the ledger's `day_baseline` |
| Turn | the same cumulative cost minus the `baseline_cost_usd` stamped when the turn started |
| Room fleet | the workspace spending cache's local-day total, with the live overlay applied so costs not yet flushed into the walk still count |
| Provider account | the machine-shared provider spending cache's `day_by_login` entry for the agent's `LoginKey` (kind plus launch login, such as `claude@work`); a missing entry reads as zero, never as the kind-wide total |

The `/day` baseline is stamped on the first evaluation of a local date and re-stamped when the date changes, so one long-lived session measures each calendar day separately while the provider's counter keeps climbing.

A turn entry in `budget.scopes.json` is keyed by the agent and holds the `turn_started_at` it was stamped for. When the agent's `turn_started_at` changes, the entry is replaced before comparison: the baseline moves to the current cost and the old park and interrupt throttle go with it, so the first tick of a new turn reads zero even when the prior turn ended over cap. An agent with no `turn_started_at` loses its entry.

Both daily reads are guarded against the wrong day. Each compares the cache's `day_cutoff_secs` with the local-day start computed from `now`, and a cache stamped for another day contributes nothing. A stale cache therefore holds a cap open instead of parking the fleet on yesterday's total.

## The ledgers on disk

| File | Location | Holds |
| --- | --- | --- |
| `budget.<digest>.json` | room runtime root | one agent: the spec, a runtime raise or disable, the day baseline, the park stamp, the interrupt throttle, the waiver |
| `budget.scopes.json` | room runtime root | per-agent turn entries (baseline, park, throttle), plus each agent's fleet and account waiver, park threshold, and interrupt throttle |
| `budget.fleet.json` | room runtime root | this room's fleet scope: a runtime override, raise, or disable, and the park stamp |
| `budget.account.<kind>@<account>.json` | machine-shared state root | one account: a runtime raise or disable, and the park stamp |

The room runtime root is `RuntimePaths::root`; the account ledger sits under `RuntimePaths::persistent_shared_root`, with its lock file under `shared_root`. A kind name with characters outside ASCII alphanumerics, `-`, and `_` is replaced in the filename by `kind-` and 16 hex characters of its SHA-256 (`account_ledger_component`).

The agent digest comes from [`store/sidecar.rs`](../../../crates/rimz/src/store/sidecar.rs): the first 32 hex characters of a SHA-256 over the kind and session id, which keeps session ids out of filenames. Auto-continue parks (`auto-continue.<digest>.json`) and idle-compaction fire records (`idle-compact/<digest>.json`) use the same digest.

Every ledger is cache-class: written through `write_temp_then_rename_cache`, rebuildable, and safe to delete. Losing one loses a park, never money.

The fleet and account ledgers are shared between the producer and `rimz budget`, so their writes take a lock file. The producer never writes a whole scope ledger: `ScopeLedgerFile::merge_park` re-reads the file under the lock and replaces only `parked`, so a tick cannot overwrite a cap change the CLI just made. The CLI reads without the lock and writes the whole ledger under it; the park it might overwrite is one the CLI clears anyway.

The account ledger is the only machine-shared one. Every room with an agent on that account evaluates the same account spend against the same ledger, but each room interrupts only its own panes, and interrupt and waiver state stay in each room's `budget.scopes.json`. A raise from one room clears the shared park, so every room's next tick stops projecting the account park onto its agents. Only the room that ran the command queues continue prompts, and only to the agents it interrupted; agents another room interrupted stay at rest.

## The verdict

[`evaluate`](../../../crates/rimz/src/harness/budget.rs) folds one agent's spend and its ledger into a `BudgetVerdict`. It is pure: it updates the ledger in memory and returns a decision, and its caller owns every write and side effect.

```text
                    ┌── no effective cap ──────────────► Disabled   (clears the park)
                    │
spend vs cap  ──────┼── under cap ─────────────────────► Under      (clears the park and interrupt throttle)
                    │
                    └── at or over cap
                          └─ waiver check
                               ├─ a human delivery landed after the park,
                               │  and the current turn started at or after it
                               │     ├─ agent still Running ─────────► Waived
                               │     └─ turn finished ───────────────► Park (waiver consumed,
                               │                                        park re-stamped at now)
                               └─ otherwise ─────────────────────────► Park
```

`Under` is restorative. Clearing the park stamp and throttle means that raising a cap above current spend un-parks the agent on the next tick, with no separate reset path. A `/day` rollover goes further and clears the park, the throttle, and the waiver together with the baseline.

The two daily scopes run a smaller fold, `evaluate_daily_scope`, once per tick per scope: a cap, a spend, and a park stamp, with no waiver. Their waivers are per agent instead, kept in `budget.scopes.json` by `evaluate_scope_waiver`, so one person answering one agent does not un-park the whole fleet.

The turn scope runs `evaluate_turn_scope` per root agent after the rebase described in [Where the spend comes from](#where-the-spend-comes-from). An at-or-over reading parks with no waiver. The next human prompt starts a new turn and clears the park through that rebase. A teammate-triggered turn meets the same cap, while message delivery gates keep background and agent-to-agent traffic from reopening a `Paused` agent ([messaging.md](./messaging.md#status-lifecycle)).

## The waiver

The waiver is the one place a human overrides a cap, and it is narrow on purpose.

A message qualifies only when it was delivered, its sender is `MessageSender::Human`, it is not marked `automated`, and its gate is not `Resume` (`is_budget_waiving_delivery`). System-authored text (auto-continue, the budget continue prompt, supervised verification prompts, and `rimz message --no-from`) carries `MessageSender::System` and never qualifies, so internal control text cannot spend through a human's cap. The `automated` and `Resume` checks keep the same boundary for records built by hand.

A qualifying delivery after the park stamps a waiver at its delivery time. A turn that starts at or after the stamp runs while the agent stays `Running`; when that turn finishes, the waiver is consumed and the park is re-stamped at `now`, past the delivery that granted it. One message therefore waives exactly one turn.

Programmatic entry points ignore the waiver; see [the fail-fast gate](#the-fail-fast-gate).

## The park

[`enforce`](../../../crates/rimz/src/harness/budget.rs) runs on the producer's refresh tick (`sidebar/refresh/mod.rs`) against a snapshot with the live day spend applied. It evaluates the daily scopes once, then walks every root agent, skipping provider-native subagents and empty or provisional ids:

1. **Evaluate.** One agent verdict from the agent's own ledger, one turn verdict from its `budget.scopes.json` entry, and one scope verdict from the first parked daily scope that binds it (the fleet, or its own account) combined with its scope waiver.
2. **Classify.** When every verdict is under or disabled, the agent's budget auto-continue record is cleared and the walk moves on. When none parks (a waiver is running), nothing else happens.
3. **Arm the day reset.** A daily scope park, or a parked `/day` agent cap, arms a budget auto-continue record with the next local midnight as its deadline, provided `pause_applies` holds for the agent. Every other park, including a turn-only park whose reset is a prompt, clears the record. The `Budget` park class is [providers.md § Auto-continue](../agents/providers.md#auto-continue).
4. **Interrupt.** A `Running` agent with a live bound pane gets the detached `rimz agents budget-park` helper, unless an interrupt for one of its current parks was sent within `INTERRUPT_RETRY_SECS` (120 seconds). An agent that keeps running past a park is interrupted again every two minutes. The throttle stamps are set only when the helper spawns.
5. **Persist.** A changed agent ledger is written back, each changed daily park is merged into its scope ledger, and a changed `budget.scopes.json` is rewritten.

The helper, [`cli/agents_cmd/budget_park.rs`](../../../crates/rimz/src/cli/agents_cmd/budget_park.rs), does the two things the producer may not: touch a pane and write to the store. It checks that the pane is still bound to the agent and refuses otherwise, presses Esc, then moves every non-terminal run record of that agent to `budget_exceeded` through `harness::run::budget_exceeded` and wakes each run it wrote. The run transition happens even when the keypress fails, and the helper reports the keypress error afterwards. The cost it records is the agent's own (`total_cost_usd`, falling back to the agent ledger's park stamp), never the fleet or account figure: a run record describes its own spend, and the broader figure only explains why the pane stopped.

Which agents a park touches is one predicate, `pause_applies`: an agent that is `Running`, or one this park already interrupted that is not `Waiting`. Agents at rest keep their lifecycle status, and a waiting agent keeps its ask visible until the answered turn runs again. A cap interrupts spending, not conversation.

On the read side, `project_parks` (called from `sidebar/enrich.rs`) stamps an `agents::state::BudgetPark` onto each agent from the ledgers, in the display order from [The scopes](#the-scopes). That projection is what makes [`effective_status()`](../agents/model.md#displayed-status) report `Paused`, which message delivery gates read ([messaging.md](./messaging.md#status-lifecycle)). `project_budget_views` fills the cockpit and provider-dashboard cap rows from the same ledgers.

## The fail-fast gate

Interactive work gets a park and a waiver. Automation gets a refusal before it starts.

[`scope_gate`](../../../crates/rimz/src/harness/budget.rs) checks the fleet ledger, then the caller's account ledger, against the same local-day caches, and returns a reason string or nothing. It consults no waiver, spawns nothing, and touches no pane. One extra rule covers cache lag: when a scope's park stamp is from the current local day, the gate takes the greater of the cached spend and the spend at the park, so a lagging cache cannot reopen a scope that already parked today.

| Caller | On a refusal |
| --- | --- |
| Supervised run ([scripting.md](./scripting.md#status-and-exit-codes)) | `SupervisedRunOutcome::BudgetExceeded` and exit `125`, before a run record or agent pane exists |
| Loop fire ([loops.md](./loops.md#one-fire)) | one `budget skipped` history row, no strike |

Supervised runs check the gate at the top of every attempt, so a retry ladder stops as soon as a cap closes mid-sequence. Exit `125` has a second source: a run already under way whose agent is parked by any cap ends as `budget_exceeded` through the helper in [The park](#the-park). Both callers also check an exact managed-launch provider quota; that gate belongs to [providers.md](../agents/providers.md).

A loop fire checks its task's own daily cap first, in `run_log::daily_budget_gate`. The ladder and the reservation rule are [loops.md § One fire](./loops.md#one-fire).

## The CLI surface

Two commands write the ledgers, and neither edits a config file. Both lift a park on mutation and then queue the configured continue prompt (`[resume] auto_continue_text`) only to agents this room interrupted, so a raise does not nudge agents that were resting for their own reasons. `--no-continue` lifts the park without the prompt.

[`rimz agents budget`](../../../crates/rimz/src/cli/agents_cmd/budget.rs) inspects or sets one agent's cap. An absolute value is stored as `raised_cap_usd` with its window written onto the spec: switching to `/day` stamps a baseline at the current cost, and switching to a session window drops it. `+AMOUNT` adds to the effective cap and refuses a cleared one, and `clear` disables the cap. Every mutation clears the park, the throttle, and the waiver. An agent launched without a budget can be given one this way.

[`rimz budget`](../../../crates/rimz/src/cli/budget.rs) inspects or sets a daily scope: the fleet by default, or with `--account <kind>` this room's account of that kind. An absolute value must be `/day` and requires a configured cap; `+AMOUNT` requires an effective cap; `off` and `clear` disable. With no `--account`, the output adds a table of every configured account cap, and when `harness.turn_budget` is set it prints a read-only `turn cap` row with source `config`.

## Tests

[`budget/tests.rs`](../../../crates/rimz/src/harness/budget/tests.rs) covers the module: the spec grammar, coverage admission, agent waivers, day and turn rebasing, stale turn projection, scope ledgers and their config gate, display precedence, the fail-fast gate, account isolation, the interrupt throttle, and auto-continue arming. Tests reach `spawn_budget_park` normally; `child_process::spawn_detached_rimz` builds the helper command and skips the subprocess under `cfg(test)`, so a park is asserted without a pane.

## See also

- [budget.md](../../guide/budget.md): the user-facing model, the five scopes, and what a park means in practice.
- [providers.md](../agents/providers.md#daily-dollar-caps): account cap eligibility and the budget auto-continue class.
- [spending.md](../agents/spending.md): the spend caches and cost coverage.
- [scripting.md](./scripting.md): supervised runs and the exit-code contract that carries `125`.
- [loops.md](./loops.md): the fire gate ladder and the per-task daily cap.
- [messaging.md](./messaging.md): delivery gates, the `automated` flag, and what a waiving message looks like.
