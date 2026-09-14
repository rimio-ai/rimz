# Providers CLI

`rimz providers` prints the account picture the sidebar's provider dashboard shows, one block per provider account: login status, plan and CLI version, rate-limit windows, paid usage and reset credits, spend, and the daily dollar cap. It runs anywhere, in or out of a room. To answer, it probes each provider login and runs any due usage reads, publishing the results to RimZ's shared account caches; it touches no agent and no room. What the dashboard's figures mean is the [Token Insight guide](../../guide/insight.md#per-account-the-provider-dashboard); how accounts are probed and cached is [providers internals](../../internals/agents/providers.md).

```sh
rimz providers                    # accounts with a login, known account facts, or spend
rimz providers codex              # only Codex accounts
rimz providers --all              # every registered provider, logged out included
rimz providers claude --refresh   # probe every login now and re-read Claude usage
rimz providers --json             # the report as a JSON array
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | Show only this provider kind's accounts, and run usage reads for that kind only. An unknown kind exits 1 with `unknown provider kind` and the list of known kinds. |
| `--all` | Show every registered provider's default account, including logged-out ones and ones with no spend. |
| `--refresh` | Ignore the cache lifetimes for this run: probe every login and re-read usage now ([How fresh the figures are](#how-fresh-the-figures-are)). |
| `--json` | Print the [JSON report](#json-output) instead of text blocks. |

Of the [global flags](../cli.md#global-flags), `--color` applies; `--root` and the backend flags are accepted and change nothing, because accounts are machine-wide. A probe that fails does not fail the command: the account prints with status `unavailable`.

## Which accounts appear

Every provider has a `default` account, its own login, and Claude and Codex can also have named accounts declared with [`rimz accounts`](./accounts.md). The rules for which of them print:

| Account | Shown when |
| --- | --- |
| `default` | It has account facts from a probe (logged in, or `unavailable` with facts from an earlier probe), or the provider has recorded spend in the last 365 days. `--all` shows it regardless. |
| Named | Always, for every account declared in the machine `config.toml`. If the `[accounts]` table fails to parse, no named account prints. |

`KIND` then drops every account of another kind.

Blocks print in the dashboard's usage ranking: providers ordered by sessions in the trailing 7 days, then 30 days, then 365 days. Providers whose `default` account has no account facts follow in registry order. Within a provider, `default` comes first and named accounts follow by name.

## Text output

Each account prints one block, separated by a blank line:

```console
$ rimz providers claude
Claude — Claude Max · logged in
  version:  v1.2.3
  account:  acct_123
  5h:       62% used · resets in 1h23m
  7d:       14% used · resets in 4d02h
  extra:    $12.40 used · $50.00 limit
  resets:   2 credits
            - 2023-11-17 17:13:20 -05:00 · in 3d00h
            - 2023-11-19 17:13:20 -05:00 · in 5d00h
  spend:    7d $31.20 · 30d $118.75
  budget:   $8.10 of $25.00/day
```

The header reads `<name> — <plan> · <status>`. The name is the provider (`Claude`), or the provider and account for a named account (`Claude · work`). The plan prints `–` when unknown, and the status is `logged in`, `logged out`, or `unavailable`.

The rows print in this order, and a row with nothing to report is left out:

| Row | Shows |
| --- | --- |
| `version` | The agent CLI version as `v…`, or `–` when unknown. Always printed. |
| `account` | The provider's non-secret account id. |
| `provider` | The backing provider, for agents that run on another provider's account. |
| `scope` | `<provider>/<variant>`, when the account reading is scoped to one backing provider. |
| One row per window | Labelled by its length (`5h`, `7d`) or by the provider's quota name. The value is `N% used · resets in 1h23m`, `N% used · ready` for a window whose clock has not started, or `∞` for a limit the provider has lifted. |
| `usage` | `∞` for an account without subscription windows, or `–` for a metered account with no window reading yet. |
| `extra` | Paid usage beyond the windows: `disabled`, or whichever of `$ used`, `$ remaining`, and `$ limit` are known, or `–`. Without a provider-reported limit, `limit` is the display ceiling from [`[accounts.usage_limit_usd]`](../../guide/configuration.md#accounts). |
| `resets` | Codex reset credits: the count, then the soonest known expiries, up to three. |
| `spend` | The provider's spend over the trailing 7 and 30 days, or `–` when none is published. On the `default` account only. |
| `budget` | `$spent of $cap/day` for the account's daily cap, with `· parked` once the cap has parked agents ([Budget CLI](./budget.md#cap-the-room-and-accounts)). |

Each reset-credit expiry prints as `YYYY-MM-DD HH:MM:SS ±HH:MM` in the machine timezone, followed by `in <interval>`, or `due` once it has passed. When the cache holds only the soonest expiry, that one line prints.

Spend belongs to the provider, so it prints once, on the `default` account's block. Windows, paid usage, reset credits, and the daily cap belong to each account and print on every block.

With colour on, only the dollar figures in `extra`, `spend`, and `budget` take the money colour.

## How fresh the figures are

A plain run reuses the same caches the sidebar keeps, and refreshes only what is due:

| Reading | Plain run | With `--refresh` |
| --- | --- | --- |
| Login, plan, account id | Probed when the last result is older than 10 minutes. A probe that could not complete retries after 10 seconds, and the account keeps its facts from the last good probe. | Every login of every provider is probed, whatever `KIND` names. |
| Windows, paid usage, reset credits | Read from the provider's usage API for each logged-in account whose last read is older than 5 minutes, or 1 hour after a read that failed on missing or rejected credentials. Providers without a usage API show the windows their agents last reported. | Read now for every logged-in account of the selected kinds. |
| Spend | The last published `provider-spending.json`. | Same. |

Setting `RIMZ_OAUTH_USAGE_OFFLINE` skips the usage API reads, with or without `--refresh`.

Rooms, the sidebar, and concurrent `rimz providers` runs share one probe and one usage read per account at a time, so `--refresh` in a loop does not multiply provider requests.

Windows are keyed to the account behind the credentials. When a usage read finds a different account than the cached reading (you logged in as someone else), the cached windows, paid usage, and reset credits are dropped and read again for the new account ([internals](../../internals/agents/providers.md#the-credits-cache-and-usage-identity)).

This command never computes spend itself. On a machine where nothing has published it yet, `spend` prints `–` and `spending` is `null` until `rimz stats` runs or a room's sidebar publishes it ([Stats CLI](./stats.md#how-fresh-the-figures-are)).

## JSON output

`--json` prints an array with one object per account, in the same order as the text blocks. Every key below is always present, with `null` where there is no value. Credentials and the account fingerprint never appear in either format.

| Field | Meaning |
| --- | --- |
| `kind` | Provider kind, such as `claude`. |
| `account` | `default`, or the named account. |
| `product_name` | Display name; `Claude · work` for a named account. |
| `status` | `logged_in`, `logged_out` (the probe ran and found no login), or `unavailable` (the probe did not complete). An `unavailable` account can still carry facts from its last good probe. |
| `probed_at` | RFC 3339 time of the last account probe. |
| `plan`, `plan_label` | Raw plan tier (`max`) and its display label (`Claude Max`). |
| `account_id` | Non-secret provider account id. |
| `sub_provider` | Backing provider name, for agents that run on another provider's account. |
| `account_scope` | `{"kind": "kind_wide"}`, or `{"kind": "sub_provider", "provider": …, "variant": …}`. |
| `metered` | `true` when the account has subscription windows. |
| `version` | Agent CLI version. |
| `windows` | Array of rate-limit windows; empty when none are known. |
| `extra_credits` | The string `"disabled"`, or `{"known": {…}}` with any of `used_usd`, `remaining_usd`, `limit_usd`. |
| `reset_credits` | `{"count": N}` plus `soonest_expiry` and the sorted `expiries` array when known. |
| `spending` | Spend tally on the `default` account; `null` on named accounts. |
| `day_budget` | `{"cap_usd", "spend_usd", "parked"}` for the account's daily cap, where `spend_usd` is spend since local midnight. |
| `active_sessions` | Always `0` from this command. |

Keys inside `windows`, `extra_credits.known`, and `reset_credits` are left out when they have no value, so treat a missing key as unknown. Each window object carries:

| Window field | Meaning |
| --- | --- |
| `scope` | `{"id", "label"}` for a named quota or model sub-cap; absent for a plain time window. |
| `used_percentage` | Percent used, 0 to 100. |
| `resets_at` | RFC 3339 reset time. |
| `duration_mins` | Window length in minutes (`300` for 5h, `10080` for 7d). |
| `observed_at` | When the reading was taken. |
| `source` | `"authoritative"` for a reading from the provider's usage API; absent for one reported by a running agent. |
| `lifted` | `true` when the provider has stopped enforcing this window; absent otherwise. |

`spending` has four windows: `today` is the sidebar's headline window (`[sidebar] spend_window`), then `week`, `month`, and `year` cover the trailing 7, 30, and 365 days. Each window holds `usd`, `tokens`, `input`, `output`, `cache_write`, `cache_read`, and `sessions`, plus `tool_calls` and a `tools` name-to-count map when any tool calls were recorded.

## See also

- [Accounts CLI](./accounts.md): declare the named accounts this command reports.
- [Budget CLI](./budget.md): set and inspect the daily caps behind `budget`.
- [Stats CLI](./stats.md): machine-wide token and dollar history, and the command that publishes spend.
- [Token Insight guide](../../guide/insight.md#per-account-the-provider-dashboard): the same account picture in the sidebar.
- [Providers internals](../../internals/agents/providers.md): probes, window fusion, caches, and refresh cadences.
