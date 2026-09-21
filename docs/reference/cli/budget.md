# Budget CLI

Two commands read and change dollar caps while the room runs: `rimz agents budget` for one agent, and `rimz budget` for the room's fleet and each provider account. Neither edits a config file: a change is runtime state in RimZ's own state directory, and the config keeps the value you set there. Why and when to cap spend, the turn and loop-task caps, and what resumes a parked agent are in the [budgets guide](../../guide/budget.md); the enforcement engine is [budget.md](../../internals/harness/budget.md).

| Scope | Armed by | Window | Command |
| --- | --- | --- | --- |
| One agent | `--budget` at launch, or the profile `budget` field | session, or `/day` | `rimz agents budget <REF>` |
| Room fleet | `harness.budget` in `config.toml` | `/day` only | `rimz budget` |
| Provider account | `[accounts.budget]` in `config.toml` | `/day` only | `rimz budget --account <KIND>` |

A cap is a non-negative dollar amount with an optional `$`, such as `5` or `$4.50`. A bare amount caps the whole session; the `/day` suffix caps each local day, measured from midnight in the configured time zone. Neither command has `--json`; both take the [global flags](../cli.md#global-flags).

## Cap one agent

`rimz agents budget <REF> [VALUE]` inspects or changes one agent's cap. `REF` is any [address](./agents.md#addressing-agents) that resolves to exactly one agent. The agent needs no launch `--budget`: any agent can be given a cap here.

```sh
rimz agents budget @coder          # inspect
rimz agents budget @coder 10       # set a $10 session cap
rimz agents budget @coder 20/day   # set a $20 daily cap
rimz agents budget @coder +5       # raise the current cap by $5
rimz agents budget @coder clear    # remove the cap
```

| `VALUE` | Effect |
| --- | --- |
| omitted | Print the report and change nothing. |
| `AMOUNT` or `AMOUNT/day` | Set the cap and its window. Switching a session cap to `/day` counts only spend from that moment for the rest of today. |
| `+AMOUNT` | Raise the current cap by `AMOUNT`, keeping its window. On an agent that never had a cap, this sets an `AMOUNT` session cap. A cleared cap refuses the raise (`cannot raise a cleared budget; set an absolute cap first`), and `+AMOUNT/day` is refused. |
| `clear` | Remove the cap. `off` is not accepted here. |

Every form prints the report, after the change when there is one:

| Field | Shows |
| --- | --- |
| `agent` | The agent's [handle](./agents.md#addressing-agents), channel suffix included (`@coder#auth`). |
| `spend` | Spend counted against the cap in its window, as `$N.NN`, or `-` when the agent's cost is unknown. |
| `cap` | The cap in effect as `$N.NN`, or `none` when unset or cleared. |
| `window` | `session` or `day`, or `-` when the agent never had a cap. A cleared cap keeps its last window. |
| `parked` | `yes` when this agent's own cap has parked it, otherwise `no`. |

Any change lifts the park this agent's cap set. When that park had interrupted a running turn, RimZ also queues the configured continue prompt (`[resume] auto_continue_text`, default `continue`) to the agent; `--no-continue` lifts the park and leaves the agent at rest. A park set by the room or account cap stays until `rimz budget` lifts it.

## Cap the room and accounts

`rimz budget [VALUE]` inspects or changes the room's fleet cap, and `rimz budget --account <KIND> [VALUE]` the cap on this room's account of one provider. Config is the on-switch: the command adjusts a cap that `harness.budget` or `[accounts.budget].<kind>` armed, and refuses to set one that config never switched on.

```sh
rimz budget                        # fleet cap plus every configured account cap
rimz budget 30/day                 # replace the fleet cap
rimz budget +10                    # raise it by $10
rimz budget off                    # disable it
rimz budget +25 --account claude   # raise this room's Claude account cap
```

| `VALUE` | Effect |
| --- | --- |
| omitted | Print the report and change nothing. |
| `AMOUNT/day` | Replace the cap. The `/day` suffix is required, and a scope that config never armed refuses with the `rimz config set` command that would arm it (`harness.budget 50/day`, or `accounts.budget.<kind> 100/day`). |
| `+AMOUNT` | Raise the cap in effect by `AMOUNT`. A cleared or unarmed cap refuses the raise (``cannot raise a cleared or unset fleet budget; set an absolute `/day` cap first``), and `+AMOUNT/day` is refused. |
| `off`, `clear` | Disable the cap until a later `AMOUNT/day` re-arms it. Editing the config value does not re-arm it. |

An account cap applies to one provider login and sums that login's spend across every room running on it; each room parks only the panes it owns. `--account` resolves the login this room uses for `KIND`, and fails with ``cannot resolve this room's <kind> account; check `rimz accounts list` `` when the room has none.

Account caps need authoritative account-level dollar history, which the built-in Claude, Codex, Grok, OpenCode, and Pi adapters provide (the `spend` column of the [wiring matrix](../agent-support.md#the-wiring-matrix) reads ✓), as does a plugin that declares a spend probe. Any other kind in `[accounts.budget]` is refused by config validation and room start, and also by `rimz budget` whenever it inspects or runs with `--account`: that check covers every key in `[accounts.budget]`, so one unsupported key blocks `rimz budget --account claude` too. The refusal names the key to remove.

Every form prints the scope's report, after the change when there is one:

| Field | Shows |
| --- | --- |
| `scope` | `fleet`, or `<kind>@<login> account`. |
| `cap` | The cap in effect as `$N.NN/day`, or `none`. |
| `source` | Where the cap comes from: `config`, `override` (a fleet cap set with `AMOUNT/day`), `raised` (a `+AMOUNT` raise, or an account cap set with `AMOUNT/day`), `cleared` (disabled with `off` or `clear`), or `none` (config never armed it). |
| `spend` | Today's spend for the scope, as `$N.NN today`. |
| `parked` | `yes` when the cap has parked agents, otherwise `no`. |
| `turn cap` | Printed only when `harness.turn_budget` is set, as `$N.NN/turn (source: config; per turn)`. `rimz budget` cannot change it. |

Without `--account`, when `[accounts.budget]` has any key, a table follows with one row per configured login of each capped kind, including logins no agent in this room uses. Its columns `ACCOUNT`, `CAP`, `SOURCE`, `SPEND`, and `PARKED` carry the same values as the fields above, except that `SPEND` prints `$N.NN` without `today`.

Any change lifts the scope's park for this room's agents. When the cap had parked the scope, RimZ queues the configured continue prompt to each agent in this room that the park interrupted; `--no-continue` leaves them at rest. Agents another room interrupted on the same account stay at rest.

## What a cap blocks

An interactive agent that crosses any cap is parked: RimZ interrupts its turn, and its card shows `⏸` with the cap that stopped it, such as `fleet budget: $50.21 of $50.00/day`. A supervised run whose agent is parked by any cap ends with status `budget_exceeded` and exit `125`. A human message sent after the park waives that agent's next turn once. The full resume rules are in the [budgets guide](../../guide/budget.md#what-resumes-a-parked-agent).

While a room or account cap has no headroom, automation does not launch:

| Launch | Outcome |
| --- | --- |
| `rimz agents <SPEC> -p`, `rimz subagents <PROFILE>`, `rimz subagents fanout` | Refused with the cap's reason and exit `125`, before that attempt's run record or agent pane exists. |
| A scheduled loop fire | Recorded as `budget skipped`, with no strike. |
| An interactive launch | Allowed. |

A fresh managed Qwen launch on an Alibaba account meets the same refusals when that account's `5h`, `7d`, or `30d` quota window is exhausted. That limit is the provider's quota, and neither command changes it; the loop side is in the [loop reference](./loop.md).
