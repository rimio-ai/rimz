# Dollar budgets

> The budget engine: the scopes it enforces, where each cap and each spend figure comes from, the ledgers it keeps, the verdict that decides a park, the one human waiver, the pane interrupt, and the fail-fast gate programmatic callers hit. The code is [`harness/budget.rs`](../../../crates/rimz/src/harness/budget.rs). [fleet.md](./fleet.md) is the map for this area. For users, the guide is [budget.md](../../guide/budget.md) and the commands are [cli/budget.md](../../reference/cli/budget.md).

## What the engine does

A budget is a dollar cap RimZ enforces itself, at a scale the provider has no concept of: this agent, this turn, this room, this login. Crossing one **parks** the agent: RimZ presses Esc in its pane, stamps the park in a ledger, and leaves everything else alone. The provider CLI keeps running and the session files stay where the provider put them. A supervised turn run is the one exception: the park ends its run as `budget_exceeded`, and a blocking run launched without `--keep` then closes the agent's pane ([scripting.md](./scripting.md)). A staying workflow step's session run stays open, and the step stays with the user ([workflow.md § How an attempt ends](./workflow.md#how-an-attempt-ends)).

The engine reads spend that already exists. Agents write transcripts, the spending walk prices them, and the walk publishes per-room and per-history-pool local-day tallies ([spending.md § What reads the totals](../agents/spending.md#what-reads-the-totals)). Budgets compare those figures against caps and turn a crossing into a stop.

Whether an agent is parked is answered by its ledger, never by its pane. The Esc keypress only makes the park visible to the agent. If the keypress fails, the ledger still says parked and a later tick interrupts again, so correctness never depends on the keypress landing.

Enforcement never writes to the store. It runs in the sidebar producer, and the [sidebar is read-only on the store](../sidebar/state.md#the-shape-of-the-problem), so the producer's path writes cache-class ledger files only. It hands every durable effect to the hidden `rimz agents budget-park` helper, which owns the pane keypress and the supervised-run transition. The module's one durable write, the fleet record in `DailyBudgetScope::write_ledger`, runs only from `rimz budget`. `cargo xtask invariants` enforces the boundary: `ensure_sidebar_library_boundaries` lists `budget.rs` among the harness modules in the sidebar import graph and fails the build when one of them imports a store writer, the run-wake sender, or the broker.

## The scopes

Four scopes live in this module, and the sidebar producer evaluates them together on every refresh tick. A fifth, a loop task's daily cap, belongs to the scheduler.

| Scope | Caps | Cap comes from | Window | Evaluated by |
| --- | --- | --- | --- | --- |
| Agent | one agent session | launch identity: `--budget`, a profile, or a team role | session, or `/day` | `evaluate` |
| Turn | every agent turn in one room | machine config `harness.turn_budget` | one turn | `evaluate_turn_scope` |
| Room fleet | every agent under one project root | machine config `harness.budget`, or `rimz budget` | `/day` only | `evaluate_daily_scope` |
| Provider account | one history pool, across every room that runs on it | machine config `[accounts.budget].<kind>` for each pool of the kind, or `rimz budget --account` for one pool | `/day` only | `evaluate_daily_scope` |
| Loop task | one task's scheduled runs | the task's `--budget-per-day` | `/day` only | `run_log::daily_budget_gate` ([loops.md § One fire](./loops.md#one-fire)) |

A history pool is `default` together with every shared account of a kind, or one standalone account on its own. `DailyBudgetScope::account` maps a login to its pool through `LoginCatalog::pool`, so a cap, a ledger, and the agents it binds always agree on the pool.

Enforcement treats the scopes independently: the first cap crossed parks the agent. The displayed park picks one scope, in the order agent, turn, fleet, account (`project_parks`), so a card names the narrowest reason.

Every cap parses through [`BudgetSpec`](../../../crates/rimz/src/harness/budget.rs): a finite, non-negative dollar amount with an optional `$` prefix and `/day` suffix, such as `5`, `$4.50`, or `20/day`. A leading `+` is rejected, because a raise is a CLI verb and not a value. Machine config wraps the grammar in cents-backed types in `config/harness.rs`:

| Type | Accepts | Used by |
| --- | --- | --- |
| `BudgetSpec` | a bare amount (`BudgetWindow::Session`) or a `/day` amount (`BudgetWindow::Day`) | `--budget`, profile and role budgets, `rimz agents budget` |
| `DayCap` | a `/day` amount only | `harness.budget`, `[accounts.budget]`, absolute `rimz budget` values |
| `TurnCap` | a bare amount only | `harness.turn_budget` |

`BudgetWindow::Turn` exists only on projected turn parks: the parser has no `/turn` form. A `/day` window uses local dates in the configured `timezone`. The window, scope, and park vocabulary (`BudgetWindow`, `BudgetScope`, `BudgetPark`) lives in [`agents/state.rs`](../../../crates/rimz/src/agents/state.rs), so the read side can use it without the engine.

## Where a cap comes from

An agent cap enters through launch identity and is then owned by its ledger. `ledger_for_agent` re-reads the launch spec on every tick and copies it onto the ledger when it differs, so relaunching with a different `--budget` takes effect. A runtime change through `rimz agents budget` (an absolute cap, a raise, or `clear`) sets `raised_cap_usd` or `disabled`. Either one pins the ledger and stops that refresh, so the next tick does not undo a CLI adjustment.

The turn cap is read straight from machine config on every tick. It has no launch flag, profile field, runtime override, raise, clear, or waiver. Removing the key drops every turn entry on the next tick.

The two daily scopes combine a configured default with a runtime ledger, and `DailyBudgetScope::effective_cap_usd` resolves them in this order:

1. A cleared ledger (`disabled`) disables the scope. Config edits do not re-arm it. Only a later absolute cap does.
2. An account scope applies no cap at all unless its provider reports authoritative account-level dollar history. The eligibility rule and its rejection paths are [providers.md § Daily dollar caps](../agents/providers.md#daily-dollar-caps).
3. The base is the ledger's absolute `override_spec` when present, and the configured cap otherwise. With no base, no cap applies.
4. A raise in `raised_cap_usd` replaces the base with its frozen total.

So a runtime absolute cap arms a scope that config leaves unset, and it outlives the removal of the config key: `off` is what lifts it. A raise needs a base: a raise made over a configured cap goes inert when that key is removed, and the scope reads `none`.

`DailyBudgetScope::cap_source` returns the `BudgetCapSource` that `rimz budget` prints beside the cap:

| Source | Means |
| --- | --- |
| `none` | no cap applies: no base, an ineligible account, or a raise whose configured base was removed |
| `config` | the configured cap applies unchanged |
| `override` | a runtime absolute `/day` cap replaces any configured default |
| `raised` | a `+AMOUNT` raise applies, as its frozen total over the base |
| `cleared` | a runtime `off` or `clear` disables the scope |

## Where the spend comes from

Each scope reads a different figure.

| Scope | Spend |
| --- | --- |
| Agent, session window | `total_cost_usd`: the card's cumulative cost, admitted only when its coverage `contributes_to_live_spend()` and the value is finite and non-negative ([spending.md § Live cost coverage](../agents/spending.md#live-cost-coverage)) |
| Agent, `/day` window | the same cumulative cost minus the ledger's `day_baseline` |
| Turn | the same cumulative cost minus the `baseline_cost_usd` stamped when the turn started |
| Room fleet | the workspace spending cache's local-day total, with the live overlay (`sidebar/refresh/live_spend.rs::apply_live_day_spend`) added, so costs the walk has not flushed yet still count |
| Provider account | the machine-shared provider spending cache's `day_by_login` entry for the agent's pool: `claude@default` for a shared `claude@work`, the account's own key for a standalone one. A missing entry reads as zero, never as the kind-wide total |

The `/day` baseline is stamped on the first evaluation of a local date and re-stamped when the date changes. One long-lived session therefore measures each calendar day separately while the provider's counter keeps climbing.

A turn entry is keyed by the agent and holds the `turn_started_at` it was stamped for. When the agent's `turn_started_at` changes, `evaluate_turn_scope` replaces the entry before comparing: the baseline moves to the current cost, and the old park and interrupt throttle go with it. The first tick of a new turn therefore reads zero, even when the prior turn ended over cap. An agent with no `turn_started_at` loses its entry.

Both daily reads are guarded against the wrong day. Each compares the cache's `day_cutoff_secs` with the local-day start computed from `now`, and a cache stamped for another day contributes nothing. A stale cache therefore holds a cap open instead of parking the fleet on yesterday's total.

## The ledgers on disk

| File | Location | Holds | Writers |
| --- | --- | --- | --- |
| `budget.<digest>.json` | room runtime `lanes/` | one agent: the spec, a runtime raise or disable, the day baseline, the park stamp, the interrupt throttle, the waiver | producer, `rimz agents budget` |
| `budget.scopes.json` | room runtime `lanes/` | `BudgetScopeState`: per-agent turn entries (baseline, park, throttle), plus each agent's fleet and account waiver, park threshold, and interrupt throttle | producer |
| `budget.fleet.json` | room state `records/` | the standing fleet override, raise, or disable | `rimz budget` |
| `budget.fleet-park.json` | room runtime `lanes/` | the fleet park stamp | producer, cleared by `rimz budget` |
| `budget.account.<pool key>.json` | `~/.rimz/cache/providers/` (`RuntimePaths::persistent_shared_root`) | one pool: a runtime override, raise, or disable, and the park stamp | producer, `rimz budget --account` |

The agent digest comes from [`store/sidecar.rs`](../../../crates/rimz/src/store/sidecar.rs): the first 32 hex characters of a SHA-256 over the kind and session id, which keeps session ids out of filenames. Auto-continue parks (`lanes/auto-continue.<digest>.json`) and idle-compaction fire records (`live/idle-compact/<digest>.json`) use the same digest. In an account ledger's name, a kind with characters outside ASCII alphanumerics, `-`, and `_` is replaced by `kind-` and 16 hex characters of its SHA-256 (`account_ledger_component`).

Fleet and account choices survive reboot and soft reset. The fleet record sits in room state and is written with `write_temp_then_rename`. The account ledger sits in the machine's persistent cache. Neither edits machine config. The agent and turn ledgers, and the fleet park lane, are disposable session state written through `write_temp_then_rename_cache`.

Each ledger has its own locking rule, set by who writes it:

- The fleet's choices and its park live in separate files with separate steady writers, so no park merge needs a lock. `DailyBudgetScope::write_ledger` writes the record under `locks/budget.fleet.lock` and then clears the park lane. A CLI clear raced by a producer tick is corrected on the next `Under` or `Disabled` verdict.
- The account ledger holds both choices and park in one file that the producer and `rimz budget` share, so every write takes its lock under the runtime `shared/` root (`RuntimePaths::shared_account_budget_lock`). The producer never writes the whole ledger: `ScopeLedgerFile::merge_park` re-reads the file under the lock and replaces only `parked`, so a tick cannot overwrite a cap change the CLI just made. The CLI reads without the lock and writes the whole ledger under it, and the park it might overwrite is one it clears anyway.
- The agent ledger and `budget.scopes.json` take no lock. `budget.scopes.json` has one writer, the producer. The agent ledger has two, and the producer rewrites it whole before each helper spawn and on any tick that changed it, so a tick that read the ledger before a `rimz agents budget` write and writes after it can undo that change.

The account ledger is the only machine-shared ledger. Every room with an agent on the pool evaluates the same account spend against the same ledger, but each room interrupts only its own panes, and interrupt and waiver state stay in each room's `budget.scopes.json`. A raise from one room clears the shared park. Unless the pool's spend still meets the new cap, every room's next tick then stops projecting the account park onto its agents. Only the room that ran the command queues continue prompts, and only to the agents it interrupted. Agents another room interrupted stay at rest.

An account ledger is read only through its pool's key. A shared account's own-key ledger, and a kind-only `budget.account.<kind>.json` with no account name, are never read. `rimz gc` removes them in either scope through `sweep_orphan_account_ledgers` ([store.md → Maintenance](../store.md#maintenance)).

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
                               │     └─ agent not Running ───────────► Park (waiver consumed,
                               │                                        park re-stamped at now)
                               └─ otherwise ─────────────────────────► Park
```

`Under` is restorative. Clearing the park stamp and throttle means that raising a cap above current spend un-parks the agent on the next tick, with no separate reset path. A `/day` rollover goes further: it clears the park, the throttle, and the waiver together with the baseline.

The two daily scopes run a smaller fold, `evaluate_daily_scope`, once per tick per scope: a cap, a spend, and a park stamp, with no waiver. Their waivers are per agent instead, kept in `budget.scopes.json` by `evaluate_scope_waiver`, so one person answering one agent does not un-park the whole fleet. Both waivers decide through the same `waiver_step`.

The turn scope runs `evaluate_turn_scope` per root agent, after the rebase in [Where the spend comes from](#where-the-spend-comes-from). An at-or-over reading parks, with no waiver. The next human prompt starts a new turn and clears the park through that rebase. A teammate-triggered turn meets the same cap, while message delivery gates keep background and agent-to-agent traffic from reopening a `Paused` agent ([messaging.md](./messaging.md#status-lifecycle)).

## The waiver

The waiver is the one place a human overrides a cap, and it is narrow on purpose.

A message qualifies only when it was delivered, its sender is `MessageSender::Human`, it is not marked `automated`, and its gate is not `Resume` (`is_budget_waiving_delivery`). A message from another agent carries `MessageSender::Agent` and never qualifies. System-authored text carries `MessageSender::System` and never qualifies either: auto-continue, the budget continue prompt, supervised verification prompts, and `rimz message --no-from`. So internal control text cannot spend through a human's cap. The `automated` and `Resume` checks keep the same boundary for records built by hand.

A qualifying delivery after the park stamps a waiver at its delivery time. A turn that starts at or after the stamp runs while the agent stays `Running`. Once the agent leaves `Running` (the turn ends, or it stops on an ask and reads `Waiting`), the waiver is consumed and the park is re-stamped at `now`, past the delivery that granted it. One message therefore waives exactly one turn.

Programmatic entry points ignore the waiver: see [the fail-fast gate](#the-fail-fast-gate).

## The park

[`enforce`](../../../crates/rimz/src/harness/budget.rs) runs as the `budget` step of the producer's refresh tick (`sidebar/refresh/mod.rs`), against a snapshot with the live day spend applied. It evaluates the fleet and every account pool an agent in the snapshot uses once, then walks every root agent, skipping provider-native subagents and empty or provisional ids:

1. Evaluate. One agent verdict from the agent's own ledger, one turn verdict from its `budget.scopes.json` entry, and one scope verdict: the first parked daily scope that binds it (the fleet, then its own pool), combined with its scope waiver.
2. Classify. When every verdict is under or disabled, the agent's budget auto-continue record is cleared and the walk moves on. When none parks (a waiver is running), nothing else happens.
3. Arm the day reset. A daily scope park, or a parked `/day` agent cap, arms a budget auto-continue record with the next local midnight as its deadline, provided `pause_applies` holds for the agent. Every other park clears the record, including a turn-only park, whose reset is a prompt. The `Budget` park class is [providers.md § Auto-continue](../agents/providers.md#auto-continue).
4. Interrupt. A `Running` agent with a live bound pane gets the detached `rimz agents budget-park` helper, unless an interrupt for one of its current parks was sent within `INTERRUPT_RETRY_SECS` (120 seconds). An agent that keeps running past a park is interrupted again every two minutes. The agent ledger is written before the spawn, so the helper can read the park stamp, and the throttle stamps are set only when the helper spawns.
5. Persist. A changed agent ledger is written back, each changed daily park is merged into its scope ledger, and a changed `budget.scopes.json` is rewritten.

The helper, [`cli/agents_cmd/budget_park.rs`](../../../crates/rimz/src/cli/agents_cmd/budget_park.rs), does the two things the producer may not: touch a pane and write to the store. In order, it:

1. Refuses unless the pane is still bound to the agent.
2. Wakes the pane if hibernation froze it (`harness::hibernate::wake_pane`). A wake failure ends the helper before anything else runs.
3. Presses Esc.
4. Moves every non-terminal turn run record of that agent to `budget_exceeded` through `harness::run::budget_exceeded`, and wakes each run it wrote. That transition leaves a session run open ([workflow.md § How an attempt ends](./workflow.md#how-an-attempt-ends)).
5. Reports the keypress error, if there was one. The run transition happens even when the keypress fails.

The cost the helper records is the agent's own (`total_cost_usd`, falling back to the agent ledger's park stamp), never the fleet or account figure. A run record describes its own spend, and the broader figure only explains why the pane stopped.

Which agents a park touches is one predicate, `pause_applies`: an agent that is `Running`, or one this park already interrupted that is not `Waiting`. Agents at rest keep their lifecycle status, and a waiting agent keeps its ask visible until the answered turn runs again. A cap interrupts spending, not conversation.

On the read side, `project_parks` (called from `sidebar/enrich.rs`) stamps an `agents::state::BudgetPark` onto each agent from the ledgers, in the display order from [The scopes](#the-scopes). A turn park is projected only while `harness.turn_budget` is set, and a daily scope park only when `pause_applies` holds and no scope waiver is running. That projection is what makes [`effective_status()`](../agents/model.md#displayed-status) report `Paused`, which message delivery gates read ([messaging.md](./messaging.md#status-lifecycle)). `project_budget_views` fills the cockpit and provider-dashboard cap rows from the same ledgers.

## The fail-fast gate

Interactive work gets a park and a waiver. Automation gets a refusal before it starts.

[`scope_gate`](../../../crates/rimz/src/harness/budget.rs) checks the fleet ledger, then the caller's account pool ledger, against the same local-day caches, and returns a reason string or nothing. It consults no waiver, spawns nothing, and touches no pane. One extra rule covers cache lag: when a scope's park stamp is from the current local day, the gate takes the greater of the cached spend and the spend at the park. A lagging cache therefore cannot reopen a scope that already parked today.

| Caller | On a refusal |
| --- | --- |
| Supervised run ([scripting.md](./scripting.md#status-and-exit-codes)) | `SupervisedRunOutcome::BudgetExceeded` and exit `125`, before a run record or agent pane exists |
| Loop fire ([loops.md](./loops.md#one-fire)) | one `budget skipped` history row, no strike |

Supervised runs check the gate at the top of every attempt, so a retry ladder stops as soon as a cap closes mid-sequence. Exit `125` has a second source: a turn run already under way whose agent is parked by any cap ends as `budget_exceeded` through the helper in [The park](#the-park). Both callers also check an exact managed-launch provider quota, and that gate belongs to [providers.md](../agents/providers.md).

A loop fire checks its task's own daily cap first, in `run_log::daily_budget_gate`. The ladder and the reservation rule are [loops.md § One fire](./loops.md#one-fire).

## The CLI surface

Two commands write the ledgers, and neither edits a config file. Every mutation clears the park and the budget auto-continue record of each agent it covers. When the park it lifted had interrupted an agent in this room, the command then queues the configured continue prompt (`[resume] auto_continue_text`) to that agent, as `MessageSender::System` at the `Done` gate. A raise therefore does not nudge agents that were resting for their own reasons. `--no-continue` lifts the park without the prompt, and an empty `auto_continue_text` sends none.

[`rimz agents budget`](../../../crates/rimz/src/cli/agents_cmd/budget.rs) inspects or sets one agent's cap. An agent launched without a budget can be given one this way.

- An absolute value is stored as `raised_cap_usd`, with its window written onto the spec. Switching into `/day` stamps a baseline at the current cost, and switching to a session window drops it.
- `+AMOUNT` adds to the effective cap. It refuses a cleared cap, a `/day` delta, and a non-finite total.
- `off` or `clear` disables the cap.
- Every mutation also clears the throttle and the waiver.
- The report prints source `override` for both absolute and relative edits, since the ledger does not record which one it was.

[`rimz budget`](../../../crates/rimz/src/cli/budget.rs) inspects or sets a daily scope: the fleet by default, or one pool with `--account`. `--account claude` resolves to this room's `claude` account, and `--account claude@work` names one declared account. Either way the command acts on that account's pool.

- An absolute value must be `/day` and can arm an unconfigured scope.
- `+AMOUNT` requires an effective cap.
- `off` and `clear` disable the scope.
- With no `--account`, the report adds a table of every eligible pool with an effective cap or a source other than `none`, so runtime-only caps and cleared pools show.
- When `harness.turn_budget` is set, the report prints a read-only turn cap line with source `config`.

Neither command checks who calls it, so an agent shell can raise or clear a cap the same way a person can. Both expose structured facts with `--json`, and the [CLI reference](../../reference/cli/budget.md) owns the report fields.

## Tests

[`budget/tests.rs`](../../../crates/rimz/src/harness/budget/tests.rs) covers the module: the spec grammar, coverage admission, agent waivers, day and turn rebasing, stale turn projection, scope ledgers and their configured or runtime base, display precedence, the fail-fast gate, account isolation, the interrupt throttle, and auto-continue arming. Tests reach `spawn_budget_park` normally. `child_process::spawn_detached_rimz` skips the subprocess under `cfg(test)`, so a park is asserted without a pane.

## See also

- [budget.md](../../guide/budget.md): the user-facing model, the five scopes, and what a park means in practice.
- [spending.md](../agents/spending.md): the spend caches and cost coverage.
- [providers.md](../agents/providers.md#daily-dollar-caps): account cap eligibility and the budget auto-continue class.
- [messaging.md](./messaging.md): delivery gates, the `automated` flag, and the message sender kinds.
