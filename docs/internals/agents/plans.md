# Subscription plans and capacity

`accounts --json`, `accounts list --json`, and `accounts show … --json` expose plan identity and fixed weekly capacity. They do not route launches, score accounts, switch defaults, or calculate remaining units. The [JSON reference](../../reference/cli/accounts.md#json-output) defines the row contract; the [provider model](./providers.md) owns reading freshness, entitlement, and window fusion.

## One reading

`crates/rimz/src/store/snapshot/view/model.rs::ProviderPlan` carries a raw tier and optional provider display name. The panel stores this reading, not a formatted label. `crates/rimz/src/store/snapshot/view/model.rs::SidebarProviderPanel::plan_label` applies the adapter's existing label vocabulary when a reader needs text.

`crates/rimz/src/cli/accounts/report.rs::build_report` uses the panel reading when a decided panel exists, otherwise the probed account's tier and display name. `plan`, `plan_label`, `plan_key`, and `capacity` follow that same reading. All are null while lapsed or undecided. A recognized raw tier supplies a stable `plan_key` and `capacity`; an unmatched tier can still have a `plan` and `plan_label`, but its key and capacity are null. Display names never decide the key. In particular, `max_20x` means `ultra` with or without the name `Ultra`.

## Capacity tables

`crates/rimz/src/agents/definition.rs::AgentSpec` declares optional `plan_capacities` (`PlanCapacities`), beside its label vocabulary. `crates/rimz/src/agents/definition.rs::PlanCapacities::resolve` matches raw tiers exactly and case-sensitively after the upstream reader's existing trim. Each entry supplies a key, raw aliases, and integer units. Providers without a table resolve nothing. A future adapter declares its own unit and `PlanCapacity` entries here; these are adapter constants, not configurable policy.

One unit is 1% of that provider's base plan's week. Different unit names are never comparable. The sizes below are the user-supplied, settled lineup reported on 2026-11-01, not estimates or a measurement of remaining capacity.

Claude uses `claude_pro_week_percent`, based on Claude Pro:

| Key | Raw tiers | Weekly size | Units |
| --- | --- | --- | --- |
| `pro` | `pro` | 1x | 100 |
| `team_standard` | `team_standard` | 1.25x | 125 |
| `team_premium` | `team_premium` | 6.25x | 625 |
| `max` | `max_5x` | 5x | 500 |
| `ultra` | `max_20x` | 10x | 1000 |

Bare `max`, `team` without a seat answer, `enterprise`, and other unmatched tiers have null capacity. The bound profile's `organization.seat_tier` refines a Team account's existing `plan` from `team` to the seat tier. `team_standard` was observed; the Premium spelling `team_premium` is unconfirmed.

Codex uses `chatgpt_plus_week_percent`, based on ChatGPT Plus:

| Key | Raw tiers | Weekly size | Units |
| --- | --- | --- | --- |
| `plus` | `plus` | 1x | 100 |
| `team_standard` | `team`, `business_standard` | 1x | 100 |
| `team_premium` | `premium`, `self_serve_business_prolite`, `business_premium` | 10x | 1000 |
| `pro_100` | `prolite` | 10x | 1000 |
| `pro_200` | `pro` | 20x | 2000 |
| `pro_500` | `promax` | 50x | 5000 |

`free`, `go`, `business`, enterprise and education variants, `unknown`, and other unmatched tiers have null capacity. The Team aliases are user-reported `plan_type` values; `business_premium` alone is an unconfirmed spelling. Matching a guessed alias does not infer anything from another, unknown spelling.

## Five-hour windows

Claude Pro, both Team seats, and Max have a 5h limit; Ultra does not. ChatGPT Plus and both Team seats have a 5h limit; the three Pro plans do not. For a plan with a 5h limit, its week holds five of its own 5h windows. Scripts use the reported windows rather than a second derived capacity field.

The tables never synthesize windows. Claude's `crates/rimz/src/agents/adapters/claude/oauth_usage.rs::collect_rate_limits` infers a lifted 300-minute window only from absent or null `five_hour` beside a usable `seven_day`. Codex's `crates/rimz/src/agents/adapters/codex/account.rs::normalize_usage` infers one from an authoritative unscoped timed window without an unscoped 300-minute window. Both produce `lifted: true`, authoritative source, and no usage percentage. See the [Claude response-shape caveat](./adapter_claude.md#account-and-balance). JSON and text consume those same adapter readings, without a report-only inference.
