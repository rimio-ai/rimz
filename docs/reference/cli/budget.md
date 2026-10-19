# Budget CLI

Two commands read and change dollar caps while the room runs: `rimz agents budget` for one agent, and `rimz budget` for the room's fleet and each provider account. Neither edits a config file: a change is runtime state in RimZ's own state directory, and the config keeps the value you set there. Why and when to cap spend, the turn and loop-task caps, and what resumes a parked agent are in the [budgets guide](../../guide/budget.md); the enforcement engine is [budget.md](../../internals/harness/budget.md).

| Scope | Armed by | Window | Command |
| --- | --- | --- | --- |
| One agent | `--budget` at launch, or the profile `budget` field | session, or `/day` | `rimz agents budget <REF>` |
| Room fleet | `harness.budget` in `config.toml`, or `rimz budget N/day` | `/day` only | `rimz budget` |
| Provider account | `[accounts.budget]` in `config.toml`, or `rimz budget --account KIND N/day` | `/day` only | `rimz budget --account <KIND>` |

A cap is a non-negative dollar amount with an optional `$`, such as `5` or `$4.50`. A bare amount caps the whole session; the `/day` suffix caps each local day in the configured time zone. Room and account spend is measured from midnight; an agent's day window starts when its cap takes effect and restarts on the first check after midnight. Both commands support `--json` and take the [global flags](../cli.md#global-flags).

## Cap one agent

`rimz agents budget <REF> [VALUE]` inspects or changes one agent's cap. `REF` is any [address](./agents.md#addressing-agents) that resolves to exactly one agent. The agent needs no launch `--budget`: any agent can be given a cap here.

```sh
rimz agents budget @coder          # inspect
rimz agents budget @coder 10       # set a $10 session cap
rimz agents budget @coder 20/day   # set a $20 daily cap
rimz agents budget @coder +5       # raise the current cap by $5
rimz agents budget @coder off      # remove the cap
```

| `VALUE` | Effect |
| --- | --- |
| omitted | Print the report and change nothing. |
| `AMOUNT` or `AMOUNT/day` | Set the cap and its window. Switching a session cap to `/day` counts only spend from that moment for the rest of today. |
| `+AMOUNT` | Raise the current cap by `AMOUNT`, keeping its window. On an agent that never had a cap, this sets an `AMOUNT` session cap. A cleared cap refuses the raise (`cannot raise a cleared budget; set an absolute cap first`), and `+AMOUNT/day` is refused. |
| `off`, `clear` | Remove the cap. |

Every form prints one line naming the handle and window (`session` or `day`). With a cap it shows spend of cap, parked or not, the source, and the raise and lift commands. Without a cap it shows the command to set one. Unknown spend is labelled `unknown spend`.

With `--json`, the report is one object: `agent` (handle, channel included), `spend_usd` and `cap_usd` (numbers or null), `window` (`session`, `day`, or null when never capped), `source`, and `parked` (boolean). A cleared cap keeps its last window. Source is `launch` for the original launch/profile cap, `override` for runtime edits (absolute or relative), `cleared` after lifting, and `none` when never capped.

Any change lifts the park this agent's cap set. When that park had interrupted a running turn, RimZ also queues the configured continue prompt (`[resume] auto_continue_text`, default `continue`) to the agent; `--no-continue` lifts the park and leaves the agent at rest. A park set by the room or account cap stays until `rimz budget` lifts it.

## Cap the room and accounts

`rimz budget [VALUE]` inspects or changes the room's fleet cap, and `rimz budget --account <KIND> [VALUE]` the cap on this room's default account of one provider. Use `--account <KIND>@<NAME>` to target a particular declared login, including one still used by running agents after a room switch. A cap belongs to a history pool, so `default` and every shared account of a kind name the same scope, identified as `<KIND>@default`; a standalone account names its own. Config supplies the default through `harness.budget` or `[accounts.budget].<kind>`; the command can also set a cap without either key, without editing config.

```sh
rimz budget                        # fleet cap plus every active or adjusted account pool
rimz budget 30/day                 # replace the fleet cap
rimz budget +10                    # raise it by $10
rimz budget off                    # disable it
rimz budget +25 --account claude   # raise this room's Claude account cap
```

| `VALUE` | Effect |
| --- | --- |
| omitted | Print the report and change nothing. |
| `AMOUNT/day` | Set or replace the cap, including when config has no cap. The `/day` suffix is required. |
| `+AMOUNT` | Raise the cap in effect by `AMOUNT`. A cleared or unarmed cap refuses the raise (``cannot raise a cleared or unset fleet budget; set an absolute `/day` cap first``), and `+AMOUNT/day` is refused. |
| `off`, `clear` | Disable the cap until a later `AMOUNT/day` re-arms it. Editing the config value does not re-arm it. |

An account cap applies to one history pool and sums spend across every room using its members; each room parks only the panes it owns. A bare `KIND` resolves the room's default login, and fails with ``cannot resolve this room's <kind> account; check `rimz accounts list` `` when the room has none. An explicit `KIND@NAME` must exist in the machine's account catalog. Each pool has its own ledger, while the configured cap stays per provider kind.

Account caps need authoritative account-level dollar history, which the built-in Claude, Codex, Grok, OpenCode, and Pi adapters provide (the `spend` column of the [wiring matrix](../agent-support.md#the-wiring-matrix) reads ✓), as does a plugin that declares a spend probe. Any other kind in `[accounts.budget]` is refused by config validation and launches of that kind; room start warns instead. `rimz budget --account KIND` validates only the targeted kind and names the key to remove if it is ineligible; ineligible siblings do not block it or produce warnings.

Every form prints one line: with a cap, spend of cap today, parked or not, the source, and the raise and lift commands; without a cap, `no daily cap` and the command to set one. Source is `config`, `override` (an absolute runtime cap), `raised` (a raise on a base), `cleared`, or `none`. A configured turn cap prints separately and cannot be changed here.

An account scope line names the requested account, its history pool, and the other pool members. A bare kind also says it resolved through this room's account. Without `--account`, a table lists every eligible pool with an effective cap or a non-`none` source, including runtime-only caps and cleared pools. The columns are `ACCOUNT`, `CAP`, `SOURCE`, `SPEND`, and `PARKED`. Each ineligible configured key is omitted and gets one warning on stderr naming the key to remove and why. Fleet writes still proceed.

With `--json`, the report is one object with `fleet` (a scope object, or null under `--account`), `accounts` (the table's scopes, or the one requested pool), and `turn_cap_usd` (number or null). Each scope has `scope` (`fleet` or the pool's `kind@name`), `cap_usd` (number or null), `source`, `spend_usd` (local-day spend), and `parked` (boolean). Account scopes add `members` (all pool members as `kind@name`); the targeted view also adds `requested`, the requested token resolved to `kind@name`. JSON replaces human output, including after an edit.

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
