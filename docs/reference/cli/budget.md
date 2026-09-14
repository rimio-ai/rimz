# Budget CLI

Two commands read and change dollar caps while the room runs: `rimz agents budget` for one agent, and `rimz budget` for the room's fleet and each provider account. Neither edits your config files: changes are runtime state in RimZ's own state directory. Why and when to cap spend, the turn and loop-task caps, and what a park looks like are in the [budgets guide](../../guide/budget.md); the enforcement engine is [budget.md](../../internals/harness/budget.md).

| Scope | Armed by | Window | Command |
| --- | --- | --- | --- |
| One agent | `--budget` at launch, or the profile `budget` field | session, or `/day` | `rimz agents budget <REF>` |
| Room fleet | `harness.budget` in `config.toml` | `/day` only | `rimz budget` |
| Provider account | `[accounts.budget]` in `config.toml` | `/day` only | `rimz budget --account <KIND>` |

A cap is a dollar amount such as `5` or `$4.50`. A `/day` cap measures from local midnight in the configured time zone.

## Cap one agent

`rimz agents budget <REF> [VALUE]` inspects or changes one agent's cap. `REF` is any [address](./agents.md#addressing-agents) that resolves to exactly one agent.

```sh
rimz agents budget @coder          # inspect
rimz agents budget @coder 10       # set a $10 session cap
rimz agents budget @coder 20/day   # set a $20 daily cap
rimz agents budget @coder +5       # raise the current cap by $5
rimz agents budget @coder clear    # remove the cap
```

| `VALUE` | Effect |
| --- | --- |
| omitted | Print the cap and change nothing. |
| `AMOUNT` or `AMOUNT/day` | Set the cap and its window. An agent launched without `--budget` can get a cap this way. Switching to `/day` counts only spend from that point today. |
| `+AMOUNT` | Raise the current cap. A raise takes no `/day`, and a cleared or unset cap cannot be raised: set an absolute cap first. |
| `clear` | Remove the cap. |

The report prints these fields:

| Field | Shows |
| --- | --- |
| `agent` | The agent's session id as an address (`@<session id>`). |
| `spend` | Spend counted against the cap in its window, or `-` when unknown. |
| `cap` | The effective cap, or `none`. |
| `window` | `session` or `day`, or `-` without a cap. |
| `parked` | `yes` when the cap has parked the agent. |

Any change lifts the agent's park. When the cap had interrupted a running turn, RimZ also queues the configured continue prompt (`[resume] auto_continue_text`, default `continue`) to the agent; `--no-continue` lifts the park and leaves the agent at rest.

## Cap the room and accounts

`rimz budget [VALUE]` inspects or changes the room's fleet cap, and with `--account <KIND>` the cap on this room's account of one provider. Config is the on-switch: the command adjusts a cap that `harness.budget` or `[accounts.budget].<kind>` armed, and refuses to set one that config never switched on, naming the `rimz config set` command that would.

```sh
rimz budget                        # fleet cap plus every configured account cap
rimz budget 30/day                 # replace the fleet cap
rimz budget +10                    # raise it by $10
rimz budget off                    # disable it (clear is an alias)
rimz budget +25 --account claude   # raise this room's Claude account cap
```

| `VALUE` | Effect |
| --- | --- |
| omitted | Print the caps and change nothing. |
| `AMOUNT/day` | Replace the cap. The `/day` suffix is required. |
| `+AMOUNT` | Raise the current cap. A raise takes no `/day`, and a disabled or unset cap cannot be raised: set an absolute cap first. |
| `off`, `clear` | Disable the cap. |

An account cap applies to one provider login and sums its spend across every room running on it; each room parks only the panes it owns. It is available only for providers whose complete dollar history RimZ can read from disk. For any other kind, `--account`, config validation, and room start refuse the key before a ledger is written. `--account` fails with ``cannot resolve this room's <kind> account; check `rimz accounts list` `` when the room has no login for the kind.

Inspecting prints the scope's fields, then, for the fleet with any account caps configured, one table row per account:

| Field | Shows |
| --- | --- |
| `scope` | `fleet`, or `<login> account`. |
| `cap` | The effective cap as `$N.NN/day`, or `none`. |
| `source` | Where the cap comes from: `config`, `override` (a fleet cap set with `rimz budget AMOUNT/day`), `raised` (a raise, or an account cap set with an absolute value), `cleared`, or `none`. |
| `spend` | Today's spend for the scope. |
| `parked` | `yes` when the cap has parked agents. |
| `turn cap` | `$N.NN/turn` when `harness.turn_budget` is set. `rimz budget` shows it but cannot change it. |

The account table has the columns `ACCOUNT`, `CAP`, `SOURCE`, `SPEND`, and `PARKED`.

Any change lifts the scope's park for the affected agents in this room. When the cap had parked the room, RimZ queues the configured continue prompt to each agent it interrupted; `--no-continue` leaves them at rest.

## What a cap blocks

An interactive agent that crosses a cap is parked: RimZ interrupts its turn, and the card reads `paused`. A supervised run that crosses its own `--budget` ends with exit `125` instead. A human message sent after the park waives that agent's next turn once. The full resume rules are in the [budgets guide](../../guide/budget.md#what-resumes-a-parked-agent).

While a room or account cap has no headroom, automation does not launch:

| Launch | Outcome |
| --- | --- |
| `rimz agents <SPEC> -p` | Refused before any run record or pane, exit `125`. |
| A scheduled loop fire | Recorded as `budget skipped`. |
| An interactive launch | Allowed. |

A fresh managed Qwen launch gets the same outcomes when the exact Alibaba account's quota window is exhausted. That is a provider quota, not a dollar cap, and neither command changes it.
