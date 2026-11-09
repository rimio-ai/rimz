# Subscription plans and capacity

`rimz accounts --json`, `accounts list --json`, and `accounts show … --json` expose each login's plan identity and fixed weekly capacity. These fields are exposure only: no launch, routing, default switch, or score reads them, and RimZ never derives remaining units from them. The [JSON reference](../../reference/cli/accounts.md#json-output) defines the row contract. The [provider model](./providers.md) owns reading freshness, entitlement, and window fusion.

## One reading

`crates/rimz/src/store/snapshot/view/model.rs::ProviderPlan` carries a raw tier and an optional provider display name. The sidebar panel stores this reading in `SidebarProviderPanel::plan`, never a formatted label. `crates/rimz/src/store/snapshot/view/model.rs::SidebarProviderPanel::plan_label` formats it through the adapter's `PlanLabel` vocabulary (`crates/rimz/src/agents/definition.rs::PlanLabel::format`) when a reader needs text.

`crates/rimz/src/cli/accounts/report.rs::build_report` takes one reading per login and derives all four JSON fields from it:

- `plan` is the raw tier.
- `plan_label` is `PlanLabel::format` over the tier and display name.
- `plan_key` and `capacity` come from the adapter's capacity table, matched on the tier alone.

The reading is the panel's `plan` when the panel carries one, else the probed account's `plan` and `plan_name` from the accounts cache. There is no reading, and all four fields are null, when the panel or the cached account is undecided, when the panel's entitlement is `Lapsed`, or when neither source has a tier. A tier the table does not list still has a `plan` and a `plan_label`, but its `plan_key` and `capacity` are null. A display name never decides the key: `max_20x` is `ultra` whether or not the name `Ultra` is present.

## Capacity tables

`crates/rimz/src/agents/definition.rs::AgentSpec::plan_capacities` is an optional `PlanCapacities` beside the spec's `plan_label`. It names one unit and a list of `PlanCapacity` entries, each with a stable key, the raw tiers that name it, and its integer units. `crates/rimz/src/agents/definition.rs::PlanCapacities::resolve` returns the first entry whose tier list contains the tier exactly, so matching is case-sensitive (`Pro` resolves nothing). The upstream readers trim the tier before it is stored. Claude and Codex declare tables. Every other adapter and every process plugin declares `None` and resolves nothing. The tables are adapter constants, not configuration: a new adapter adds its own unit and entries to its spec.

One unit is 1% of the week of that provider's base plan. Units with different names never compare. The sizes are the plan lineup the user reported as settled on 2026-11-01. They are neither estimates nor a measurement of remaining capacity. `crates/rimz/src/agents/definition/tests.rs::plan_capacities_resolve_exact_raw_tiers_to_keys_and_units` pins every row below and a set of tiers that must resolve nothing.

Claude uses `claude_pro_week_percent`, based on Claude Pro:

| Key | Raw tiers | Weekly size | Units |
| --- | --- | --- | --- |
| `pro` | `pro` | 1x | 100 |
| `team_standard` | `team_standard` | 1.25x | 125 |
| `team_premium` | `team_premium` | 6.25x | 625 |
| `max` | `max_5x` | 5x | 500 |
| `ultra` | `max_20x` | 10x | 1000 |

Claude's auth status reports Max as bare `max` and Team as bare `team`, and two later reads refine the tier. For `max`, the stored OAuth credentials' `rateLimitTier` gives `max_5x` or `max_20x` (`crates/rimz/src/agents/adapters/claude/account.rs::refine_max_plan`). The live profile read then overrides the cached plan while its owner fingerprint and the plan it was read against both still match. That read maps a Team organization to its `organization.seat_tier` ([Account and balance](./adapter_claude.md#account-and-balance) owns the rules). A tier left bare (`max` with an unknown rate-limit tier, `team` without a seat answer), `enterprise`, and every other unlisted tier have null capacity. `team_standard` was observed upstream. The Premium spelling `team_premium` is unconfirmed.

Codex uses `chatgpt_plus_week_percent`, based on ChatGPT Plus. The raw tier is the usage response's `plan_type`:

| Key | Raw tiers | Weekly size | Units |
| --- | --- | --- | --- |
| `plus` | `plus` | 1x | 100 |
| `team_standard` | `team`, `business_standard` | 1x | 100 |
| `team_premium` | `premium`, `self_serve_business_prolite`, `business_premium` | 10x | 1000 |
| `pro_100` | `prolite` | 10x | 1000 |
| `pro_200` | `pro` | 20x | 2000 |
| `pro_500` | `promax` | 50x | 5000 |

`free`, `go`, `business`, the enterprise and education variants, `unknown`, and every other unlisted tier have null capacity. The Team aliases are user-reported `plan_type` values, except `business_premium`, which is an unconfirmed spelling. Each alias matches only itself: a guessed spelling implies nothing about any other unknown spelling.

## Five-hour windows

Claude Pro, both Claude Team seats, and Claude Max have a 5h limit, and Ultra does not. ChatGPT Plus and both Codex Team seats have a 5h limit, and the three Pro plans do not. For a plan with a 5h limit, its week holds five of its own 5h windows. RimZ publishes no derived 5h capacity field: a script reads the reported `windows` instead.

The capacity tables never create windows. Each adapter marks a missing 5h window itself:

- `crates/rimz/src/agents/adapters/claude/oauth_usage.rs::collect_rate_limits` adds a 300-minute window when `five_hour` is absent or null and `seven_day` parses to a window. See the [Claude response-shape caveat](./adapter_claude.md#account-and-balance).
- `crates/rimz/src/agents/adapters/codex/account.rs::normalize_usage` adds one when an unscoped authoritative window with a duration exists and no unscoped 300-minute window does.

Both synthesized windows carry `lifted: true`, an authoritative source, and no usage percentage. The JSON and the text tables print the adapter's windows as stored on the panel, with no inference of their own. `crates/rimz/src/cli/accounts/report_tests.rs::a_plan_without_a_five_hour_limit_reports_the_lifted_window` pins the JSON shape.
