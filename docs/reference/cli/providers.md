# Providers CLI

`rimz providers` queries the account-global provider picture RimZ shows in the sidebar: login status, plan and version, included rate-limit windows, paid and reset credits, published spend, and configured daily-cap state. It works inside or outside a room and reads only provider credentials and RimZ's user-scoped shared caches.

```sh
rimz providers                    # providers with a login, last-known account, or spend
rimz providers codex              # filter the default report to one provider kind
rimz providers --all              # include logged-out and empty registered providers
rimz providers claude --refresh   # force fresh account and usage reads for Claude
rimz providers --json             # stable report array for scripts
```

| Argument or flag | Effect |
| --- | --- |
| `KIND` | Keep one registered provider kind; an unknown kind fails with the known-kind list |
| `--json` | Emit the stable JSON report array instead of human-readable provider blocks |
| `--refresh` | Bypass account and account-usage TTLs for this invocation |
| `--all` | Include logged-out and empty registered kinds; without it, a kind needs a login, last-known account, or recorded spend |

## Freshness

A plain query uses the sidebar's refresh semantics: each successful or logged-out account probe stays cached for its normal TTL, unavailable probes retry on their shorter TTL, and each logged-in provider's direct usage query runs only when its own durable cadence is due. The account and usage paths are single-flighted across rooms and concurrent CLI calls.

`--refresh` makes every registered account probe due and invalidates the selected logged-in providers' direct-usage cadence before reading them. It still uses the same single-flight claims and cache publications, so concurrent refreshes do not stampede a provider.

Provider spend is always the last published `provider-spending.json` value. This command does not start the transcript spending walk; a cold cache leaves `spending` absent until `rimz stats` or a room producer publishes it.

## JSON fields

The document is an array in display order: providers with dashboard panels retain the dashboard's usage ranking, then providers without panels follow registry order.

| Field | Meaning |
| --- | --- |
| `kind`, `product_name` | Stable adapter kind and product display name |
| `status` | `logged_in`, `logged_out`, or `unavailable`; an unavailable probe may retain last-known account facts |
| `probed_at` | Account probe timestamp in RFC 3339 form, or `null` before the first probe |
| `plan`, `plan_label` | Raw provider plan tier and its formatted display label |
| `account_id`, `sub_provider`, `account_scope` | Non-secret provider identity and typed kind-wide or sub-provider scope |
| `metered`, `version` | Subscription-window metering state and agent CLI version |
| `windows` | Included quotas with usage percentage, reset, duration or named scope, provenance, and lifted state |
| `extra_credits`, `reset_credits` | Paid/API credit facts and redeemable reset-credit facts |
| `spending` | Published headline, 7-day, 30-day, and 365-day spend/token tally |
| `day_budget` | Configured provider daily cap, current local-day spend, and parked state |
| `active_sessions` | Currently bound root panes for this provider; zero outside a room-backed panel |

Account credential fingerprints and credential material are excluded from both output formats.
