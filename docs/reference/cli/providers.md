# Providers CLI (deprecated alias)

`rimz providers [KIND] [--json] [--refresh] [--all]` is a hidden, one-release compatibility alias, removed in the next release. Use [`rimz accounts`](./accounts.md#list) for the overview or [`rimz accounts show KIND`](./accounts.md#show) for one provider. Human output uses the same overview and detail layouts. Each invocation writes this line to stderr without changing stdout:

```text
rimz: warning: `rimz providers` is deprecated and goes in the next release; use `rimz accounts` (`rimz accounts show <kind>` for one provider)
```

Flags follow the accounts command. The alias retains cached login records within their refresh interval, including logged-out or ambiguous records; unlike `accounts`, it does not re-probe those records on every run. `--refresh` forces a fresh read. `KIND` selects one provider, not a `kind@name` account. Only `--json` retains the previous report rather than the merged accounts row.

## JSON output

`--json` preserves the legacy array, one row per catalog login of every known kind (or the selected `KIND`), regardless of `--all`. It excludes the merged report's undeclared-selected and failed-configuration rows. Every key below is present, with `null` where there is no value, except `probe_error`, which is present only after a failed probe. Credentials and account fingerprints never appear. Differences from the [merged row](./accounts.md#json-output): `status` means auth state, `active` is false outside a room, `spend_step` is never null, and there are no `home`, `history`, `machine_default`, `problem`, or `login` fields.

| Field | Meaning |
| --- | --- |
| `kind` | Provider kind, such as `claude`. |
| `account` | Canonical `kind@name`, replacing the former bare account name. |
| `pool` | History pool as `kind@name`. Rows with equal pools share a spend tally; deduplicate by this key. |
| `probe_error` | Cause text, such as `timed out` or `exited 1`, present only after a failed probe. |
| `product_name` | Dashboard title: the theme-resolved product, with ` · <label>` for a named account with a valid display label, otherwise ` · <name>`; `default` has no suffix. |
| `status` | `logged_in`, `logged_out` (no login found, or the provider offers no login check), or `unavailable` (the probe did not complete). An `unavailable` account can still carry facts from its last good probe. |
| `entitlement` | `"ok"`, or `{"lapsed":{"since_ms":N}}` when subscription access was rejected; `N` is the first-seen Unix-millisecond timestamp. Independent of `status`. |
| `probed_at` | RFC 3339 time of the last account probe. |
| `plan`, `plan_label` | Plan tier and display label; both `null` while lapsed. Claude prefers a recognised live profile bound to the current login, falling back to auth status and credential metadata. Normalized values and labels follow the [accounts JSON contract](./accounts.md#json-output). |
| `account_id` | Non-secret provider account id. |
| `sub_provider` | Backing provider name, for agents that run on another provider's account. |
| `account_scope` | `{"kind": "kind_wide"}`, or `{"kind": "sub_provider", "provider": …, "variant": …}`. |
| `metered` | `true` when the account has subscription windows. |
| `version` | Agent CLI version. |
| `windows` | Array of rate-limit windows; empty when none are known. |
| `extra_credits` | The string `"disabled"`, or `{"known": {…}}` with any of `used_usd`, `remaining_usd`, `limit_usd`. Reported unlimited credit is `{"known":{"unlimited":true}}`. |
| `reset_credits` | `{"count": N, "effect": …}` plus `soonest_expiry` and the sorted `expiries` array when known. `effect` says what redeeming a credit does to the window's reset: `"restarts_window"` (Codex: the reset moves a full window from the redemption) or `"keeps_schedule"` (Claude: the reset stays where it was). |
| `spend_step` | Always a value: `subscription`, `paid_credit`, `reset_credit`, or `spent`, as the [accounts JSON contract](./accounts.md#json-output) defines them. Defaults to `subscription` without a reading. |
| `spending` | The history pool's spend projection; `null` when no tally is recorded. |
| `day_budget` | `{"cap_usd", "spend_usd", "parked"}` for the account's daily cap, where `spend_usd` is spend since local midnight; `null` without a day cap. |
| `month_budget` | The same shape for a counting monthly cap, with spend since 00:00 UTC on the 1st; `null` otherwise, including subscription-exempt pools. |
| `agents` | Cross-room count for this account, or `null` when rooms cannot be inventoried. Replaces the former session-count field. |
| `active` | `true` on this room's active account; false outside a room. |
| `default_for` | The layers the account is the default for: any of `this_room` (a room pin only), `this_project`, `new_rooms`, or an empty array. |

Keys inside `windows`, `extra_credits.known`, and `reset_credits` are left out when they have no value, so treat a missing key as unknown. Each window object carries the keys listed under [`rimz accounts` window fields](./accounts.md#json-output).

`spending.today` is spend since local midnight in the configured `timezone`, or `null` when the cache was published for another local day. `headline` is the configured sidebar headline, with sibling `window` naming `session`, `24h`, or `today`. `week`, `month`, and `year` cover the last 7, 30, and 364 local calendar days, including today. Each window holds `usd`, `tokens`, `input`, `output`, `cache_write`, `cache_read`, and `sessions`, plus `tool_calls` and a `tools` name-to-count map when any tool calls were recorded.

## See also

- [Accounts CLI](./accounts.md): the replacement command, human output, freshness rules, and merged JSON contract.
- [Budget CLI](./budget.md): set and inspect the day or month caps behind `budget`.
- [Stats CLI](./stats.md): machine-wide token and dollar history, and the command that publishes spend.
- [Token Insight guide](../../guide/insight.md#per-account-the-provider-dashboard): the same account picture in the sidebar.
- [Providers internals](../../internals/agents/providers.md): probes, window fusion, caches, and refresh cadences.
