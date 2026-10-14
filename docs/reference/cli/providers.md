# Providers CLI

`rimz providers` compares provider account facts, where the sidebar's provider dashboard shows only the account each provider launches under in that room. Providers with a logged-in account or declared named accounts get a table: plan and CLI version, what is left of each rate-limit window, paid usage, reset credits, and pool spend. Other kinds fold into one closing status line. `rimz providers <KIND>` always prints that provider's accounts as blocks, which add probe and reading ages, account id, 30-day spend, daily dollar cap, and cross-room agent count. It runs anywhere, in or out of a room. To answer, it probes each provider login and runs any due usage reads, publishing the results to RimZ's shared account caches; it touches no agent and no room. What the dashboard's figures mean is the [Token Insight guide](../../guide/insight.md#per-account-the-provider-dashboard); how accounts are probed and cached is [providers internals](../../internals/agents/providers.md).

```sh
rimz providers                    # account tables and a folded line for other kinds
rimz providers codex              # only Codex accounts
rimz providers --all              # every registered provider, logged out included
rimz providers claude --refresh   # probe every login now and re-read Claude usage
rimz providers --json             # the report as a JSON array
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | Show only this provider kind's accounts, and run usage reads for that kind only. A known kind always prints, even with no cached facts, and exits 0. An unknown kind exits 1 with `unknown provider kind` and the list of known kinds. |
| `--all` | Show every registered provider's default account, including logged-out ones and ones with no spend. |
| `--refresh` | Ignore the cache lifetimes for this run: probe every login and re-read usage now ([How fresh the figures are](#how-fresh-the-figures-are)). |
| `--json` | Print the [JSON report](#json-output) instead of text. |

Of the [global flags](../cli.md#global-flags), `--color` applies, and `--root` selects the position whose active account gets the `●` marker, the position [`rimz accounts list`](./accounts.md#list) reads from; without it the position is the room you are in, else the project at the current directory. Outside a room no account is marked active; the machine default still takes `○`. Pins decide per provider; unpinned providers follow project and machine defaults live. When a kind's selection cannot be resolved, a warning on stderr carries the error and that kind has no `●`; healthy kinds retain theirs. An unreadable room record leaves all kinds without `●`. The command still prints and exits 0. The backend flags are accepted and change nothing. A probe that fails does not fail the command: it prints `probe failed`, with the cause in expanded tables and account blocks.

## Which accounts appear

Every provider has a `default` account, its own login, and Claude and Codex can also have named accounts declared with [`rimz accounts`](./accounts.md). The rules for which of them print:

A provider gets a table when any account reads logged in or it has declared named accounts. Every other kind folds into one closing `Also:` line: kind, known version, and `logged out`, `not installed`, `probe failed`, `not probed` (no probe yet), or `no login check`. A provider declaring no readable login surface, such as Kiro, reads `no login check`, not a verified logout. Spend history alone does not earn a table. `--all` expands every kind into a table, including a failed probe's cause.

`KIND` drops every other kind and always prints the selected kind's blocks. Named accounts are read from machine configuration; if the `[accounts]` table fails to parse, no named account prints.

Providers print in the dashboard's usage ranking: providers ordered by sessions in the trailing 7 days, then 30 days, then 365 days. Providers whose `default` account has no account facts follow in registry order. Within a provider, `default` comes first and named accounts follow by name.

## Text output

`rimz providers` separates provider tables with a blank line, then prints the folded kinds. Each table has one row per account. `rimz providers <KIND>` prints that provider's [account blocks](#account-blocks) instead.

### Provider tables

The heading is the provider and CLI version: the active account's version, else the default account's, else the first known. The default pool's 7-day spend follows as `$X spent in 7d` when recorded; shared named accounts add `(shared by default, a, b)`. The pool figure appears once, not on each member's row. `default` is the first row, then named accounts by name.

| Column | Shows |
| --- | --- |
| (marker) | `●` on the account a launch at the position uses, at most one per provider. `○` on the machine's default for new rooms (`[accounts.use]`, else `default`) when that row is not the `●` one. |
| `ACCOUNT` | Canonical `kind@name`; the default account is muted. |
| `PLAN` | The plan label, `–` when unknown, `lapsed` in the failure tone when subscription access was rejected, or the status (`logged out`, `not installed`, `not probed`, `no login check`, or `probe failed: <cause>`) when the account is not logged in. A lapse suppresses the cached plan and balances until a successful usage read clears it. |
| One column per window | Every window any account of the provider reports, labelled as in the block and suffixed `LEFT`. The cell is `N% · 1h23m · 0.9x` (what is left, time to reset, then pace), `100% · ready` before the window's clock starts, `N%` with no known reset, `no limit` for a lifted limit or an account without subscription windows, or `–` when unknown. |
| `EXTRA LEFT` | Only when some account reports paid usage: `off` when disabled, a dollar amount when the balance left is known, else `$ used`, else `–`. |
| `RESET CREDITS` | Only when some account reports reset credits: the count, then `exp` and the time to the soonest expiry (`2 · exp 3d00h`, or `2 · due` once it has passed), the bare count with no known expiry, `-` with none banked, `–` when unknown. |
| `SPEND 7d` | Only when the kind has a standalone account. Standalone rows show their own 7-day spend, `-` when unrecorded. Default-pool members leave this cell blank. |
| `READING` | Appears when a declared or previously observed usage window has no reading: `no reading since <date>` from its newest window observation, or `never read`. Omitted when no usage window is expected. |

Copilot's token-billed chat quota is labelled `AI credits`, rather than `cr`. Marker and pace legends appear whenever those markers or multipliers appear.

Pace is the used share divided by the elapsed share of the window: `1.0x` lasts exactly to reset; above it, the account runs dry early at that average rate. It is rounded to one decimal, with `>10x` at a rounded pace of 10.0 or higher. A trailing `–` means less than 5% of the window has elapsed, too early to tell. Pace is absent for a lifted, unknown, not-started or exhausted window, or without a duration, reset time or positive elapsed time. Its color follows `[theme.display.budget_bar.burn_rate]`, the same bands as the sidebar reset marker.

### Account blocks

`rimz providers <KIND>` prints one block per account, separated by a blank line:

```console
$ rimz providers claude
Claude — Claude Max · logged in
  probed:        0s ago
  version:       v1.2.3
  account:       acct_123
  5h:            38% left · resets in 1h23m · pace 0.9x · read 0s ago
  7d:            86% left · resets in 4d02h · pace 0.3x · read 0s ago
  extra:         $12.40 used · $50.00 limit
  reset credits: 2 credits
                 - 2023-11-17 17:13:20 -05:00 · in 3d00h
                 - 2023-11-19 17:13:20 -05:00 · in 5d00h
  spend:         7d $31.20 · 30d $118.75
  budget:        $8.10 of $25.00/day
  agents:        –
pace: used share ÷ elapsed share; 1.0x lasts exactly to reset
```

The header reads `<name> — <plan> · <status>`. The plan prints `–` when unknown; the status carries the same login or failure facts as the table. For a provider with named accounts every header names its account and starts with the marker or two spaces, and ends with the `DEFAULT FOR` words when there are any: `● Claude · claude@work — Claude Max · logged in · new rooms`, `  Claude · claude@default — Claude Max · logged in`.

The rows print in this order, and a row with nothing to report is left out:

| Row | Shows |
| --- | --- |
| `probed` | Age of the last login probe, or `never`. |
| `version` | The agent CLI version as `v…`, or `–` when unknown. Always printed. |
| `account` | The provider's non-secret account id. |
| `provider` | The backing provider, for agents that run on another provider's account. |
| `scope` | `<provider>/<variant>`, when the account reading is scoped to one backing provider. |
| One row per window | Labelled by its length (`5h`, `7d`) or by the provider's quota name. The value is `N% left · resets in 1h23m · pace 0.9x`, `100% left · ready` for a window whose clock has not started, `N% left · resets –` with no known reset, or `no limit` for a limit the provider has lifted. |
| `usage` | `no limit` for an account known to have no subscription windows, or the dated missing-reading reason when a usage window is expected. Otherwise omitted. |
| `extra` | Paid usage beyond the windows: `off`, or whichever of `$ used`, `$ left`, and `$ limit` are known, or `–`. Without a provider-reported limit, `limit` is the display ceiling from [`[accounts.usage_limit_usd]`](../../guide/configuration.md#accounts). |
| `reset credits` | Codex reset credits or Claude limit resets, only when at least one is banked: the count, then the soonest known expiries, up to three. |
| `spend` | Pool spend over the trailing 7 and 30 days, printed on the default or standalone account only. Shared members omit it. A default account with none published prints `–`. |
| `budget` | `$spent of $cap/day` for the account's daily cap, with `· parked` once the cap has parked agents ([Budget CLI](./budget.md#cap-the-room-and-accounts)). |

`agents` is the cross-room count for this account, or `–` when rooms could not be inventoried. Each window adds the age of its observation when known.

Window rows use the same pace rules as the table, with `· pace –` during the first 5% and no pace in the blank states listed above.

To spend a banked reset, use [`rimz accounts redeem codex` or `rimz accounts redeem claude`](./accounts.md#redeem). Claude's banked count can include grants that cannot be spent now; its redemption preview explains the hold.

Each reset-credit expiry prints as `YYYY-MM-DD HH:MM:SS ±HH:MM` in the machine timezone, followed by `in <interval>`, or `due` once it has passed. When the cache holds only the soonest expiry, that one line prints.

Windows, paid usage, and reset credits belong to the account, so each account reports its own. Spend and the daily cap belong to the history pool: `default` and the shared accounts of a provider report one figure, and a standalone account its own.

With colour on, the dollar figures take the money colour, and each window's percentage takes the tone the sidebar's budget bar has at that remainder: green at a full window, warming to red as it drains, through the stops in `[theme.display.budget_bar]`. Pace takes the sidebar reset marker's tone through `[theme.display.budget_bar.burn_rate]`. The same rules colour the provider tables.

## How fresh the figures are

A plain run reuses the same caches the sidebar keeps, and refreshes only what is due:

| Reading | Plain run | With `--refresh` |
| --- | --- | --- |
| Login, plan, account id | Probed when the last result is older than 10 minutes. A probe that could not complete retries after 10 seconds, and the account keeps its facts from the last good probe. | Every login of every provider is probed, whatever `KIND` names. |
| Windows, paid usage, reset credits | Read from the provider's usage API for each logged-in account whose last read is older than 5 minutes, or 1 hour after missing/rejected credentials or subscription access. Providers without a usage API show the windows their agents last reported. | Read now for every logged-in account of the selected kinds. A successful usage read clears a recorded lapse. |
| Spend | The last published `provider-spending.json`. | Same. |

Setting `RIMZ_OAUTH_USAGE_OFFLINE=1` skips the usage API reads, with or without `--refresh`. Any value but `0`, `false`, or empty turns it on, so `RIMZ_OAUTH_USAGE_OFFLINE=0` leaves the reads running.

Rooms, the sidebar, and concurrent `rimz providers` runs share one probe and one usage read per account at a time, so `--refresh` in a loop does not multiply provider requests.

Windows are keyed to the account behind the credentials. When a usage read finds a different account than the cached reading (you logged in as someone else), the cached windows, paid usage, and reset credits are dropped and read again for the new account ([internals](../../internals/agents/providers.md#the-credits-cache-and-usage-identity)).

This command never computes spend itself. On a machine where nothing has published it yet, `spend` prints `–` and `spending` is `null` until `rimz stats` runs or a room's sidebar publishes it ([Stats CLI](./stats.md#how-fresh-the-figures-are)).

## JSON output

`--json` prints an array with one object per account, in the same order as the text rows. Every key below is always present, with `null` where there is no value, except `probe_error`, which is present only after a failed probe. Credentials and the account fingerprint never appear in either format.

| Field | Meaning |
| --- | --- |
| `kind` | Provider kind, such as `claude`. |
| `account` | Canonical `kind@name`, replacing the former bare account name. |
| `pool` | History pool as `kind@name`. Rows with equal pools share a spend tally; deduplicate by this key. |
| `probe_error` | Cause text, such as `timed out` or `exited 1`, present only after a failed probe. |
| `product_name` | Display name; `Claude · work` for a named account. |
| `status` | `logged_in`, `logged_out` (the probe ran and found no login), or `unavailable` (the probe did not complete). An `unavailable` account can still carry facts from its last good probe. |
| `entitlement` | `"ok"`, or `{"lapsed":{"since_ms":N}}` when subscription access was rejected; `N` is the first-seen Unix-millisecond timestamp. Independent of `status`. |
| `probed_at` | RFC 3339 time of the last account probe. |
| `plan`, `plan_label` | Raw plan tier (`max`) and its display label (`Claude Max`); both `null` while lapsed. |
| `account_id` | Non-secret provider account id. |
| `sub_provider` | Backing provider name, for agents that run on another provider's account. |
| `account_scope` | `{"kind": "kind_wide"}`, or `{"kind": "sub_provider", "provider": …, "variant": …}`. |
| `metered` | `true` when the account has subscription windows. |
| `version` | Agent CLI version. |
| `windows` | Array of rate-limit windows; empty when none are known. |
| `extra_credits` | The string `"disabled"`, or `{"known": {…}}` with any of `used_usd`, `remaining_usd`, `limit_usd`. |
| `reset_credits` | `{"count": N, "effect": …}` plus `soonest_expiry` and the sorted `expiries` array when known. `effect` says what redeeming a credit does to the window's reset: `"restarts_window"` (Codex: the reset moves a full window from the redemption) or `"keeps_schedule"` (Claude: the reset stays where it was). |
| `spending` | The history pool's spend projection; `null` when no tally is recorded. |
| `day_budget` | `{"cap_usd", "spend_usd", "parked"}` for the account's daily cap, where `spend_usd` is spend since local midnight. |
| `agents` | Cross-room count for this account, or `null` when rooms cannot be inventoried. Replaces the former session-count field. |
| `active` | `true` on this room's active account; false outside a room. |
| `default_for` | The layers the account is the default for: any of `this_room` (a room pin only), `this_project`, `new_rooms`, or an empty array. |

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

`spending.today` is spend since local midnight in the configured `timezone`, or `null` when the cache was published for another local day. `headline` is the configured sidebar headline, with sibling `window` naming `session`, `24h`, or `today`. `week`, `month`, and `year` cover the trailing 7, 30, and 365 days. Each window holds `usd`, `tokens`, `input`, `output`, `cache_write`, `cache_read`, and `sessions`, plus `tool_calls` and a `tools` name-to-count map when any tool calls were recorded.

## See also

- [Accounts CLI](./accounts.md): declare the named accounts this command reports.
- [Budget CLI](./budget.md): set and inspect the daily caps behind `budget`.
- [Stats CLI](./stats.md): machine-wide token and dollar history, and the command that publishes spend.
- [Token Insight guide](../../guide/insight.md#per-account-the-provider-dashboard): the same account picture in the sidebar.
- [Providers internals](../../internals/agents/providers.md): probes, window fusion, caches, and refresh cadences.
