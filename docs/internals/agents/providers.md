# Provider accounts and balances

A coding agent runs against a provider account: a login, on a plan, that may or may not be metered. Its balance has two tiers: included subscription windows that refill on their own clocks, and paid usage beyond them. This page owns how RimZ learns those facts, fuses them across sessions and sources, caches them across rooms, and acts on them (parking, auto-redeem, auto-continue). Transcript spend and the token price table are [spending.md](./spending.md).

The facts flow from sources, through shared caches, into one top-level provider panel per login, with Pi credential alternates inside it. The dashboard shows the room's current login per kind:

```text
sources                                    user-scoped caches
  a live session's rich context ─────── (rides the rollup, model.md)
  the out-of-band account probe ──────► accounts.json
  the account-usage refresh ─────────► rate_limits.json · credits.json
  the spending walk (spending.md) ────► provider-spending.json
                                                │
                                                ▼
                        producer aggregation + window fusion
                                                │
                                                ▼
                        one SidebarProviderPanel per kind
              plan · version · budget bars · paid usage · spend headline
```

Panel enrichment is best-effort. A missing binary, a logged-out account, or an unreachable API degrades to an omitted plan label, a `v?` version placeholder, or an unknown budget track. One reading persists past its read: a confirmed subscription-access rejection records a lapse that suppresses the cached plan, windows, and credits until a direct read clears it ([Entitlement](#entitlement)). Three consumers treat the facts as control inputs: [parked turns](#spent-windows-and-paused-rows) read fused windows, a fresh managed Qwen launch may gate on an exact account binding ([Account scope](#account-scope)), and account month caps read subscription status to decide applicability. None turns a quota reading into a provider billing statement: [account dollar caps](#account-dollar-caps) still count transcript spend, not provider-probe dollars.

For a month cap, any pool member with `metered = true` makes the pool subscription-backed and exempts it. `false` or an unknown reading does not exempt it, so a configured budget remains armed on a cold cache. Daily caps do not consult this classification. The shared predicate also supplies the room-start subscription warning; account classification is re-read for enforcement on every pass, but warnings are retained at room entry rather than re-judged mid-room.

The native surfaces each provider reads are in its adapter page ([Per-provider mapping](#per-provider-mapping)), and the raw endpoints are in the [upstream references](../../externals/agent-adapter/claude-reference.md#auth-surface). The dashboard on screen is [the interface reference](../../interface/sidebar.md#the-provider-dashboard).

## The model

Five account-scoped facts feed a panel. Account identity and included balance ride the session's [`AgentContext`](../../../crates/rimz/src/agents/context.rs) ([model.md](./model.md#rich-context)) or the out-of-band probe, and the producer lifts them to the account at aggregation time. Paid usage, reset credits, and entitlement ride the shared `credits.json` cache.

| Fact | Type | Carries |
| --- | --- | --- |
| Account identity | [`AgentAccount`](../../../crates/rimz/src/agents/context.rs) | the non-secret `account_id`, the raw `plan` tier (`max`, `team`, `pro`), `metered`, the typed account scope, an optional `sub_provider` id, the probed version, and the credential file's mtime |
| Included balance | [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs) | ordered [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs)s, each with `used_percentage`, an optional integer `amount` (`used`, `limit`), a typed `resets_at`, `observed_at`, a `source`, and either a `duration_mins` or a provider scope (stable id plus compact label) |
| Paid usage | [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs) | used USD, remaining USD, a limit, or a disabled state, each optional |
| Reset credits | [`ResetCredits`](../../../crates/rimz/src/agents/credits.rs) | Codex and Claude: the banked count, every known valid expiry, and the earliest expiry. Claude's claimability is separate from its banked count ([source and mapping](./adapter_claude.md#oauth-usage-probe)) |
| Entitlement | [`Entitlement`](../../../crates/rimz/src/agents/credits.rs) | `Ok`, or `Lapsed` with the first-seen Unix-millisecond timestamp, independent of login credentials |

A window's identity is its duration or its scope id. Duration windows sort short to long (`5h`, `7d`) and support reset roll-forward, not-started detection, pace, and surplus. A window with an unknown duration makes each of those claims fail closed. Named provider windows (Copilot's monthly `cr` or `cht` and `prm`) fuse by scope id, display their real reset, and never roll forward. Qwen's Alibaba windows are duration windows: their [account scope](#account-scope) says whose account they belong to and gives them no window identity (`crates/rimz/src/agents/adapters/qwen/alibaba_usage.rs::push_window`).

Paid usage keeps missing fields missing: an uncapped `ex` row invents no cap, and an absent reading, a limit alone, or a zero balance leaves no `ex` row (`CreditDisplay::is_available` in `sidebar_pane/render/sections/provider.rs`). Disabled or exhausted paid usage does not make a parked turn terminal while a subscription window still has a future reset. Reset credits draw the compact dashboard marker, which shows the earliest expiry within a week and never blinks. For Codex its glyph also forecasts what [auto-redeem](#auto-redeem) would do. `rimz accounts` lists the deadlines when credits are banked.

### Metered and unmetered

Metering decides which balance row a panel paints. A subscription or ChatGPT login is metered: it draws on rate-limit windows, painted as draining bars. An API-key login, a Claude key helper, or a positively identified Claude Console profile is unmetered: it has no included windows, so the panel paints one `api` row from the history pool's trailing-month transcript spend against an optional display ceiling, with no provider balance. A Claude environment or setup token remains metered ([classification](./adapter_claude.md#account-and-balance)). `metered: None` is unknown, and aggregation infers it ([Producer aggregation](#producer-aggregation)).

### Account scope

An account scope ([`ProviderAccountScope`](../../../crates/rimz/src/agents/context.rs)) says which sessions an enrichment belongs to. `KindWide` covers every session of one provider kind. `SubProvider` names a canonical provider plus a variant, such as Alibaba International or China, so a multi-provider client displays only its effective account's enrichment. Qwen sets a user-facing `sub_provider` label for these (for example "Alibaba Coding Plan (International)"), while Pi and OpenCode store the raw backing-provider id. Ordinary harness capacity reads are kind-wide: `crates/rimz/src/agents/account.rs::ProviderCapacity::read` and `ProviderCapacity::read_all` skip every entry whose scope is not `KindWide`, and auto-continue, auto-redeem (through the spend ladder), and loop window triggers read capacity through them. `ProviderCapacity::read_bound` admits a `SubProvider` entry only under an exact binding, which only a managed Qwen launch holds (below). Other sub-provider readings do not enter those capacity gates. A headless judge that has already received a provider-limit error is different: it cannot identify the credential used, so its retry deadline takes the latest spent-window reset across that login's scopes.

Control needs more than a scope: a managed launch must also match the opaque credential account key exactly. Generic launch and schedule code carry one typed state:

| State | Means |
| --- | --- |
| `PendingResolution` | final inputs have not arrived yet |
| `Unsupported` | ordinary kind-wide capacity applies |
| `Unresolved` | exact selection applies but identity is unproven, so no capacity is read |
| `Bound` | only matching exact-account quota and cache reads are permitted |

Qwen is the one `Bound` user, and [adapter_qwen.md](./adapter_qwen.md#managed-launches) owns what resolves each state for it. Its Alibaba windows carry the credential account key from their authoritative read, and a fresh managed Qwen launch or loop fire requires exact scope-and-key equality. An absent reading stays non-blocking. A Qwen selection that is not an Alibaba plan resolves `Unresolved`, so a direct-API managed launch reads no capacity. The binding authorizes only that fresh launch decision. Durable Qwen sessions carry no provider identity, so the scoped windows stay out of displayed session rate-limit status, message resume, and auto-continue.

## Logins

Every probe, claim, cache entry, and panel on this page belongs to a login (`LoginKey`, as in `claude@work` or `codex@default`): the provider home a reading came from. Usage entries and claims also distinguish account scope. Which login a room and each agent run on, and how a named account's home links to the provider's own, are [logins.md](./logins.md). This page reads three sets of logins:

| Set | Members | Read for |
| --- | --- | --- |
| current | the room default per kind (`RoomLoginSet::current_keys`) | one dashboard panel each |
| in use | the room defaults plus the stamps of live root agents (`RoomLoginSet::keys_in_use`) | fusion, account usage, credits, and auto-redeem, dashboard or not |
| idle | every Codex login the machine catalog declares that is not in use | auto-redeem's [expiry rescue](#auto-redeem) alone |

After an account switch, the old login keeps its caches while its agents run, and it has no panel. Spend and the account budget are keyed by the login's history pool (`LoginCatalog::pool`), so `default` and every shared account of a kind share one figure.

RimZ's own credential reads are read-only. It does not refresh tokens, write provider auth files, or import browser cookies. A probe reads the login's home through the environment its caller passes. A catalog-issued `default` drops an exported declared account home through `crates/rimz/src/agents/login.rs::ProviderLogin::env`; `RoomLoginSet::env` also applies the catalog's native-home rule to default logins built outside the catalog. Four probes read the process environment instead: Amp, Copilot, Kimi, and OpenCode. Those kinds have only `default`, whose login environment is the ambient one.

## Two origins

A kind's account and included balance reach the panel two ways, as session context does ([context sources](./adapter.md#context-sources)). A live session's rich context carries account and window readings on its statusline or app-server transport, so any live session fills both at no extra cost. The out-of-band probe reads a logged-in provider directly, so its account stays visible between turns and with no live session.

Where both exist, the live session's account wins because it is current. Windows do not pick a winner: live and authoritative readings fuse ([Window fusion](#window-fusion)).

## The out-of-band probe

[`probe_account`](../../../crates/rimz/src/agents/capabilities.rs) returns an [`AccountProbe`](../../../crates/rimz/src/agents/account.rs), and the arm sets how long the producer trusts it ([`sidebar/timing.rs`](../../../crates/rimz/src/sidebar/timing.rs)):

| Arm | Means | Cached for |
| --- | --- | --- |
| `Found(AgentAccount)` | a logged-in account, possibly still provisional | `ACCOUNTS_TTL` (10 minutes) when settled; `ACCOUNTS_RETRY_TTL` (10 seconds) when provisional; earlier on a stamped input change below |
| `LoggedOut` | the probe ran and found no login | `ACCOUNTS_TTL`, earlier on a stamped input change below |
| `Unavailable` | the probe could not complete: a binary that would not run, a non-zero exit that does not itself report a logout, an unreadable file | `ACCOUNTS_RETRY_TTL` (10 seconds). The provider keeps its last-known facts and retries alone |

An adapter with no out-of-band login surface returns `LoggedOut`. The probe itself is a pure read, and memoization lives in the producer's `accounts.json` publication. Each provider's mechanics (Claude's `claude auth status`, Codex's and Pi's auth-file read, Antigravity's verified loopback service) are in its adapter page.

An adapter can report a deterministic, metadata-only inputs stamp through `AccountCapability::account_inputs_stamp`. When both the stored and current stamps exist and differ, a successful record re-probes after `ACCOUNTS_RETRY_TTL` (10 seconds) from its previous probe. The stored stamp is read before the probe, for every outcome, so an input change during the probe remains due on a later pass. A provisional account also re-probes after that floor, even without a stamp change. Pi and OpenCode leave a multi-credential login provisional while its provider is undecided; Pi can also carry a provisional answer from an older session. Otherwise a missing stored or current stamp keeps the TTL rule: older records do not force a probe, and an older binary that drops the field on rewrite disables this shortcut until the next probe stores it again. Pi stamps auth and session-directory metadata ([coverage and limits](./adapter_pi.md#account-and-balance)); other adapters default to no stamp.

A display-only version probe follows the account probe in the same chain (`crates/rimz/src/sidebar/refresh/accounts.rs::probe_one_account`). It runs for a `Found` account that carries no version, and for a `LoggedOut` or `Unavailable` login whose kind has a live root agent in the room. A re-probe of a provisional account reuses a stored version younger than `ACCOUNTS_TTL`, tracked by `ProviderRecord.version_probed_at_ms` independently of account retries. A cached version with no recorded read time is not reused. It locates the declared executable, runs `<binary> --version`, and hands stdout and stderr to the adapter to normalize. The default parser (`crates/rimz/src/agents/version.rs::conventional_cli_version`) takes the first whitespace-separated token in either stream that parses as a version, so a banner holding a standalone `1.2.3` is accepted. Copilot, Amp, and Cursor recognize their branded build strings. Antigravity disables the probe because invoking `agy --version` is unsafe for idle enrichment, and a manifest plugin uses its configured probe command and parser.

Account and version subprocesses each have a 3-second deadline (`INFORMATIONAL_PROBE_TIMEOUT`). An account timeout is `Unavailable`. A version timeout yields no version and leaves the account outcome as it was. The version shown is the newest among the probe's and every live context's, so a session still running an older binary never replaces a newer one. A successful record without a version re-probes on the retry TTL while its kind has a live root agent and no live context reports a version for the login (`crates/rimz/src/sidebar/refresh/accounts.rs::due_provider_logins`). Otherwise a settled account waits `ACCOUNTS_TTL`, unless its adapter's inputs stamp changes as above.

After a fresh identity-bearing account probe, a profile-capable adapter can read its live plan in the same worker through `AccountCapability::probe_account_profile`. The lane reads the published machine-wide session start and per-login reset stamps once per batch. A read is due when no answer belongs to the fresh account, when its saved-plan basis differs from the fresh auth-status plan after credential refinement, or when the answer predates the latest of that session start, the reset stamp, and 24 hours ago. Attempts must be at least `ACCOUNTS_TTL` apart. An attempt is stamped with its batch's probe time, the instant the account cadence ages from, so the floor has always expired when the login is next account-due. An answer is stamped when its request starts, so a trigger stamped while the request is in flight still makes the next batch due. Equality with a trigger is not due. Profile triggers act at the next account probe; no periodic profile lane exists. `--refresh` bypasses the profile floor, and `RIMZ_OAUTH_USAGE_OFFLINE` suppresses both usage and profile reads without stamping an attempt.

The optional profile state keeps the last attempt time and the last answer's opaque owner, saved-plan basis, recognised plan (or no plan), and request start time. The basis is the account's auth-status plan after credential refinement at read time, and an older answer without a basis reads it as absent. Every probe outcome carries that state. For `Found`, a recognised answer overrides the fresh saved plan only while its owner equals the fresh account's fingerprint and its basis equals that saved plan. Absent identity, an unknown answer, a changed owner, or a changed basis leaves the saved plan in charge. An answered but unrecognised read replaces the old answer, which ends an override. A failed read changes only its attempt time: login outcome, probe error, TTL, metering, entitlement, windows, and credits are untouched, and the last answer stays available. There is no warning or disagreement row.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../../crates/rimz/src/store/snapshot/view/providers.rs) folds sessions, probed accounts, and spend into one [`SidebarProviderPanel`](../../../crates/rimz/src/store/snapshot/view.rs) per login, for the logins its caller names and no others. It runs in the machine-config fold, `fold_machine_config_with` in [`sidebar/enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs), because it needs per-machine config and probe results the pure reducer cannot read. The reducer leaves `providers` empty, and producer and consumers alike fold the producer's published account and spend caches ([state.md](../sidebar/state.md#projections)). The fold takes a `PanelScope`, one per [login set](#logins):

| Scope | Logins | Used by |
| --- | --- | --- |
| `Current` | the room's current login per kind (`RoomLoginSet::current_keys`). A kind whose current login does not resolve gets no panel | the snapshot every renderer paints, and `provider_panels_from_caches`, the `rimz accounts` fold |
| `InUse` | every in-use login (`keys_in_use`), ignoring `provider_list`, `max_provider_blocks`, and `provider_tabs` | only the producer's scoped fold in `refresh_heavy_lanes`, so fusion, account usage, credits, and auto-redeem act on every login in use, including providers hidden from the dashboard |

`rimz accounts` still lists every account, because it folds once per account under a selection naming that account. For `Current`, ordering, `max_provider_blocks`, and the tab decision run over the selected panels, so a hidden login takes no slot. The dashboard tab follows the selected card by kind, since the displayed snapshot carries one top-level panel per kind. In either scope, each other backing provider a Pi root session reports adds an `alternates` panel when the probed account's `credentials` inventory is non-empty, never another tab or stacked block. A missing or older account record has no alternates until a new-build probe supplies that inventory. Each alternate takes its credential facts from the inventory, or the unlabeled, unmetered API-key rule for an unlisted provider; it keeps the top level's stats and spend, and never nests alternates. Window projection, credits, and usage claims visit all of them, regardless of selection.

The renderer chooses a block's credential from the selected card of that kind when its `context.sub_provider` matches the top level or an alternate, then this view's remembered credential while still available, then the top level. Reconciliation remembers a credential only while that first rule matches. The memory survives a card stopping; once its credential leaves the panel, it no longer overrides the top level. A card running an older extension without a provider report uses the same fallback. `rimz accounts show` and `sidebar snapshot --json` use the top-level classification without selection; the snapshot JSON also carries the alternates. `sidebar frame` uses its first-row selection through the same renderer rule.

A login in either scope earns a panel when it has a live session, or when its probed account is metered, has a non-empty `account_id`, or has recorded spend in the past year. Spend alone never creates a panel for a provider with no probed account, and a credentials-only unmetered account stays hidden until its first recorded session. An agent launched through an elevation wrapper as another uid stays out of every panel: its credentials live under that user's home, so the sidebar shows it as a flagged process row.

A panel for a named login titles its block with the account's display label when it is valid, otherwise with its name (`Claude · work`). A `default` panel shows the product name alone. The machine-config fold passes account config to the view, which shares the title rule with the CLI fallback. Labels never enter login resolution or a published schema.

Each panel field has one source:

| Field | Source |
| --- | --- |
| `plan` | the session context with the newest `observed_at`, falling back to the resolved probed account, including a bound profile override. Claude contexts supply no account. `format_plan_label` uses the adapter's declared vocabulary (`max_20x` becomes `Claude Max 20x`, Codex `pro` becomes `ChatGPT Pro 200`), with title-case fallback for unknown kinds |
| `version` | the newest version among the login's session contexts and the probed account, by [`newest_version`](../../../crates/rimz/src/agents/version.rs): a string that parses as a numeric `CliVersion` outranks one that does not, the greater `CliVersion` wins, and the greater string breaks what remains, so the answer does not depend on which session reported last |
| `metered`, `account_scope`, `sub_provider` | the same account. `sub_provider` names the backing credential when known. A missing `metered` is inferred true when live windows exist or a keyed session owns the panel, except that an undecided account is unmetered |
| `undecided` | the account is provisional and has no `sub_provider`. It has no plan label, windows, credits, or budget row, and claims no usage read |
| `alternates` | other reported Pi credentials, each a whole panel with its own credential facts, windows, credits, and entitlement but the same whole-login stats and spend |
| `account_key` | the newest-registered keyed admitted session, by `registered_at` then session id. An alternate takes its probed credential's owner without borrowing another session's |
| `windows` | `fresh_windows` over the admitted sessions, then fused with cache ([Window fusion](#window-fusion)), cleared on an unmetered panel |
| `window_placeholders` | the spec's `expected_windows`, painted as empty tracks before the first reading |
| `extra_credits`, `reset_credits` | the `credits.json` entry, or a local API spend projection |
| `redeem_forecast` | [`project_redeem_forecasts`](../../../crates/rimz/src/harness/auto_redeem.rs) over the cached capacity and burn rate, only on a Codex panel carrying `reset_credits` ([Auto-redeem](#auto-redeem)) |
| `spending` | the [`SpendTally`](./spending.md#what-reads-the-totals) of the login's history pool (`by_login`, keyed by pool) from `provider-spending.json`, so `default` and every shared login of a kind show one figure. `rimz accounts` falls back to the same entry when a login has no panel. Its table prints the default pool once in the heading and standalone pools in the spend column. Its JSON carries explicit pool identity and separates local-day spend from the headline. |
| brand art and color | `AgentSpec` color and name, the embedded emblem catalog, and `[theme.providers.<kind>]` overrides. An unknown kind gets neutral grey ([theme.md](../../guide/theme.md#provider-styling)) |

A color override keeps the catalog's tint runs, while an `ascii_art` override paints its art in the single brand color.

### Which sessions speak for a panel

Pi live windows first join the panel whose `sub_provider` matches the session's report; a session without a report joins only the top level. Within each credential, live windows are partitioned by [birth account key](./model.md#the-rollup) before fusion, so a session registered under another login contributes no usage to the account the panel shows. The panel admits every session carrying its `account_key` plus every keyless session, and excludes sessions with a different key. A keyless panel excludes no admitted session by owner. `plan` and `version` ignore the owner partition: the top-level plan comes from the freshest context or account probe, an alternate's from its probed credential, and the version from the newest version any of the login's sessions reports. The keyed `metered` inference keeps cached authoritative windows attached while a newly registered session has not reported its first window.

### Paid and API rows

A metered panel takes `ExtraCredits` from its `credits.json` entry. An unmetered panel initially synthesizes `ExtraCredits::Known` from the history pool's trailing-month transcript spend, so accounts with shared history share that spend. An undecided panel is unmetered but backs no known credential, so it takes no credits at all (`SidebarProviderPanel::is_api_key`). For a metered or API-key panel, when the provider reported neither a cap nor unlimited credit, the display ceiling from `[accounts.usage_limit_usd]` (a map such as `claude = 50.0`) applies: a capped row drains by the spend and shows remaining dollars, and an uncapped API-billed login paints a full `∞` bar. Reported unlimited credit always paints `∞`, even with a display ceiling. The ceiling is display only, and no provider enforces it.

After credits enrichment, `project_budget_views` resolves the effective account cap for logins in use. A counting `/month` cap on an unmetered panel replaces that initial API projection with the pool's UTC-calendar-month spend and the effective cap, raises included; an undecided panel takes the cap's `month_budget` view, since caps are login-wide, and no API projection. Stale month spend stays unknown on the bar until the next walk; a current publication with no entry for the pool reads as zero spend and shows the full cap left. Day caps, subscription-exempt pools, metered panels, and panels outside the login set keep the credits projection. The sibling `day_budget` and `month_budget` views keep the windows distinct for renderers and account JSON; only a parked view replaces the headline dollars with spend of cap.

A credits entry older than `CREDITS_DISPLAY_MAX_AGE` (24 hours), absent, or scope-mismatched leaves no `ex` row, including when a display ceiling alone is configured. API spend projections are never persisted, since the spending walk and config rebuild them.

### Entitlement

A `ProviderCreditsEntry` without `entitlement` reads as `Ok`. A direct `NotEntitled` completion records `Lapsed { since_ms }` at first observation and keeps that time across repeated rejections. A successful direct `Found` clears it. An account identity change resets it unless that same probe rejects subscription access. Adopting the first identity of a marker-only entry (no account key and no observed reading) does not reset its lapse. No-credentials, failed, unsupported, invalidation, and claim writes otherwise preserve it. A realtime write preserves it only for the same account (equal scope and equal account key, both absent counting as equal): a reading from another account or scope resets it, and only a direct read settles that account's access.

`apply_credits_cache_with` projects entitlement only for subscription (metered) panels with a login in use and a matching scope. Unmetered panels keep `Ok` and their API spend row even when their cache entry is lapsed. A projected lapse never expires with the credits display TTL: it clears the panel's plan, windows, extra credits, and reset credits, including facts from account probes and live context. `mark_entitlement_lapsed` creates a missing entry under the shared credits lock and resets the OAuth attempt, settled-auth state, and claim so the next direct read can confirm or clear it. Entitlement does not gate launch.

### Panel order and the cap

With no explicit `provider_list`, a usage rank orders panels: a live room session first, then trailing-week, month, and year session counts, then a qualifying probed account, then the credential file's mtime, with registry order breaking ties. The mtime comes only from file-based probes. The rank decides both paint order and which panels survive the stacked dashboard's `max_provider_blocks` cap (default 3). A tabbed dashboard is bounded by its active block and shows every provider. Its tab rail keeps the highest-ranked tabs that fit and always keeps the active one. An explicit `provider_list` sets the set and order and bypasses the cap, and `"all"` expands the remaining providers in rank order ([theme.md](../../guide/theme.md#display)). A kind in `provider_list` expands to all its panels, `default` first and then named logins in name order.

### The shared caches

The account, rate-limit, and credits caches live under `~/.rimz/cache/providers/`, with single-flight locks under `$XDG_RUNTIME_DIR/rimz/shared/`.

| Cache | Keyed by | Holds |
| --- | --- | --- |
| `accounts.json` | `logins` | `AgentAccount` per login, including defaulted `provisional` and per-provider `credentials`; the probe's `login` outcome (`logged_in` or `logged_out`, absent after a failed probe), optional version-read time, and optional profile attempt and answer state |
| `rate_limits.json` | `entries` (login, scope) | fused windows, account scope, bound account key and its authoritative copy, pending refills, the unknown-episode marker, and optional `reset_spotted_at_ms`, schema gated by `RATE_LIMITS_CACHE_VERSION` (7); version 6 cold-drops. Optional additive fields such as `lifted` or `amount` keep the version, while a change of meaning bumps it |
| `credits.json` | `logins` (kind-wide), `scoped` (login, sub-provider scope) | paid usage, Codex plan, Codex and Claude reset credits, entitlement with its first-seen time, the usage owner identity, the last direct read's published account hint, and the direct-query claim |

An `accounts.json` record's `login` field says whether the account is logged in. The account alone cannot: a logout probed while the kind has a live agent keeps the CLI version, the same bytes as a login the provider reports no facts about. `ProviderStatus::from_record` is the one reader. A record without the field is inferred from `ok` and the account, and its version-only shape reads as unavailable. Launch gates treat it as unknown until the next probe, and [the accounts command](#the-accounts-command) re-probes it on the spot for Claude, Codex, and Pi, the kinds with named accounts.

The host publishes all three. The shared fast fold and the sidebar's consumers read them without probes, while [`rimz accounts`](#the-accounts-command) probes and refreshes on its own. Usage keys encode kind-wide scopes as `<kind>@<name>` (unchanged for Claude and Codex) and sub-provider scopes as `<kind>@<name>/<provider>/<variant>`. A read skips an entry whose key does not parse and keeps the rest of the file (`AccountUsageKey::deserialize_map`, for `rate_limits.json` entries and the credits `scoped` map). A publication preserves other logins and other scopes under the same login. Incompatible rate-limit versions cold-drop, because every value is rebuildable. The unversioned credits cache grows only by additive, defaulted fields: sub-provider readers prefer `scoped`, then a legacy `logins` entry whose scope matches; writers put sub-provider entries only in `scoped`. Older builds rewriting credits drop `scoped`, so that legacy fallback remains necessary while mixed builds share the cache.

A due account batch runs account-then-version probe chains and any due profile read on up to four workers (`MAX_PARALLEL_ACCOUNT_PROBES`) and joins them into one atomic `accounts.json` publication. A caller that cannot open the coordination lock probes locally without publishing and skips profile reads unless forced, since no attempt stamp could persist. An unforced caller waits up to 300 ms behind a live producer, then serves the current cache for that frame, so a fresh room does not repeat the cold subprocess wave. A forced caller keeps contending, then probes and reads after winning the lock, even if the peer just published. Its wait bound is twice a batch budget: each four-worker wave allows two 3-second informational probes and one 5-second profile request, with at least enough waves for the registered provider kinds. The extra budget covers credential lookup, publication, and scheduling. Only a producer outliving that bound makes the forced caller serve the cache.

## Window fusion

A window is account-scoped, so its displayed value fuses every reading of it: across parallel sessions, across live transports and direct queries, and across time. Each [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs) records its provenance: `observed_at`, and a `source` of `BestEffort` (Claude's statusline) or `Authoritative` (a direct account-usage query: Codex's app-server, a provider OAuth read, Antigravity's verified local service). Fusion runs in two stages.

### Live readings, per frame

[`fresh_windows`](../../../crates/rimz/src/store/snapshot/view/providers.rs) reduces the admitted sessions' readings to one live candidate per window identity.

1. A reading is rejected whole when it is content-stale (`AgentRateLimits::content_stale_at`): its shortest duration window's reset has passed, or, with no dated duration window, its earliest dated scoped reset has. Statusline persistence carries each unchanged `BestEffort` window's prior `observed_at`, matched by identity, usage and reset. An idle replay keeps its content time even if another window changes or disappears. When the prior sidecar has no windows (missing, no rate limits, or an empty set), the first best-effort windows take the earlier of capture and the verified `RIMZ_AGENT_PID` process's start time. This keeps a pre-credit process's first replay after `/clear` or an in-process `/resume` older than the post-credit OAuth anchor. The owner pid must parse above 1 and its process name must match the agent. An unchecked owner or unknown start falls back to capture. A changed window, a new identity in an already populated sidecar or an unstamped prior takes the capture time. An `Authoritative` window always keeps its query time.
2. Within a surviving reading, a window with no `used_percentage` or with a passed reset is dropped.
3. Per identity, find the newest `observed_at`, then keep the highest `used_percentage` among windows stamped no more than 10 seconds before it. This tolerates parallel render lag without letting an old pre-credit high reading beat a new post-credit low one. Unstamped windows count as oldest. If all are unstamped, highest usage wins. The pick is independent of session order, including ties.

### Across sources and time

[`fuse_window`](../../../crates/rimz/src/sidebar/refresh/rate_limits.rs) folds the live candidate into the persisted truth. Trust is decided before direction. The first matching row wins:

| Live candidate | Verdict |
| --- | --- |
| none this frame | keep the prior and its pending refill |
| first reading for this identity | adopt |
| observed more than 6 hours ago (`LIVE_HORIZON_SECS`) | keep the prior |
| `Authoritative`, observed no earlier than the prior | adopt, in either direction |
| `Authoritative`, cannot prove it is no earlier | keep the prior |
| `BestEffort`, not strictly newer than an `Authoritative` prior | keep the prior |
| `BestEffort`, climb or steady | adopt, clear any pending refill |
| `BestEffort` drop whose reset is more than 60 seconds later (`RESET_ADVANCE_SECS`) | adopt: a new window epoch |
| `BestEffort` drop to above 25% used (`REFILL_FLOOR_PCT`) | keep the prior: jitter |
| `BestEffort` drop to 25% or below | park a pending refill, and adopt once it has stood 120 seconds (`REFILL_CONFIRM_SECS`) |

The stamp rows decide only where an `Authoritative` reading is involved. There, an unstamped live reading cannot prove it is current, and an unstamped prior yields to any stamped reading. Between two `BestEffort` readings the stamps do not matter: an unstamped climb is adopted, and a stamped drop above 25% keeps an unstamped prior. Only the producer confirms a refill. A consumer holds the producer's persisted value.

The confirm path exists for the mid-window free reset a provider sometimes grants, which refills usage without moving the reset timer. One garbled statusline sample cannot dip a bar, and a genuine refill still surfaces after two minutes. An unchanged idle replay cannot displace a newer authoritative reading. One disagreement stays unresolved: a live session whose value keeps changing in contradiction to the API can alternate with each direct query, and the authoritative value is never more than one read cadence away.

`RIMZ_RATE_LIMIT_TRACE` is off by default. Its value is trimmed: empty, `1`, or `true` writes `rate_limits_trace.jsonl` under the runtime shared root, and any other value names the path. The trace appends each frame's live candidate (`used_percentage`, `resets_at`, `observed_at`, `source`) and the fused result, for tuning the confirm window against real resets.

Fused account-wide windows for each login feed provider-limit display and [parked-turn](#spent-windows-and-paused-rows) decisions. Per-session `context.rate_limits` feeds fusion and the agent card, never a decision by itself.

### Not-started windows

The heuristic assumes sliding windows: the clock starts on the first token, and until then the provider keeps `resets_at` a full window-length ahead. An unmarked window has not started when its reset sits about a full window out and its usage is at or below the 1% floor (`FRESH_WINDOW_USAGE_FLOOR`). The floor is 1% because a fresh Codex 5h window is taken to report 1% used, which a zero check would miss. Any usage above the floor means the window has started, and its reset is a real countdown. A window marked `fixed_period: true` always reads as started: its clock runs independently of usage. OpenCode Go marks its week and month, and Qwen Alibaba marks its billing month. Their durations stay nominal for stable window identity, even when a billing month lasts 31 days. Pace clamps elapsed time at zero while a fixed period's reset lies beyond its nominal length, so the CLI shows `–` until the early-window floor passes.

The dashboard draws a not-started window as a full bar with no `↻` countdown, which reads as ready to start ([interface](../../interface/sidebar.md#the-provider-dashboard)). The predicate also gates unused-window detection, surplus pacing, and loop `--after-reset` scheduling. Go's scoped readings and Qwen's month stay outside those policy reads.

### Persistence across idle sessions

The producer mirrors each fused window into `rate_limits.json` under its (login, scope) key (atomic write under a shared read-modify-write lock) and reads it back when no live session reports. Switching scope keeps the other credential's reading separate. Read-time projection decides what an entry shows:

| Situation | The panel shows |
| --- | --- |
| before the window's reset | the last fused reading |
| a shorter duration window reset while the longest cached duration window is still future | 0% used, reset set to now plus one window length, until a live reading overwrites it; a `fixed_period` window instead steps from its old reset by whole window lengths, so a week keeps the provider's schedule, and a month steps by its nominal 30 days until the next read corrects it |
| a named window's reset passed | unknown, per window, since named windows never roll forward |
| the freshness ceiling (the longest dated window's reset) passed with no fresh reading | every bar of the provider as an unknown empty track |
| every window undated, and the newest observation older than the shortest reported duration | every bar of the provider as an unknown empty track |

Scoped quota values stay intact after their reset for status decisions. Only the display projection clears them. Projected full and unknown values are never written back.

An all-unknown panel is also a refresh trigger. When a credential's panel holds no usable value (an aged-out ceiling, expired named quotas, or a cold start with no cache), the producer forces that (login, scope)'s direct query instead of waiting out the cadence. The entry's `unknown_since_ms` marker makes the force fire once per unknown episode. The usage claim keeps the fetch single-flight, and completion restamps the read on success and failure alike, so a provider that stays unreachable falls back to ordinary cadence. A usable window clears the marker.

A panel looks up its (login, scope) entry, then meets its bound key under the same rule as [session admission](#which-sessions-speak-for-a-panel): the entry's bound key and the panel's key must be equal, or one must be absent. A keyed mismatch clears the live windows for that frame, so the cached account's truth paints instead of fusing with another account's usage. Only an authoritative account read binds an entry's key, and it fuses onto a prior entry only when scope and key both match, so an account switch writes a fresh entry. A keyed entry also retains its matching authoritative windows separately for `Bound` launch controls. The producer's own publication never binds a key: it republishes fused truth under the prior entry's key, keeping that entry's bound copy. An unmetered panel removes only its own scope's entry. An in-use login with no panel loses its cache entries. Entries outside the room's in-use set are untouched.

Before completing omitted duration windows, the authoritative writer compares the raw response with that (login, scope)'s previous authoritative windows, even across a credential-key change. An unscoped duration window that had started at its observation and was more than 60 seconds from resetting records `reset_spotted_at_ms` when usage drops, or its reset disappears or moves by more than 60 seconds. Scoped, lifted, durationless, best-effort-only, and unstarted windows do not count, nor does an on-schedule reset. The optional Unix-millisecond stamp carries through subsequent merges and producer projections. When a direct read finds a changed credential key, its windows replace that scope's entry through this same merge, whose identity filter keeps the old account's windows out of fusion while the reset check still compares against them. Only a changed key whose reading carries no windows drops that scope, and its stamp with it. Kind-wide stamps remain profile-refresh triggers only for adapters with that capability, not another usage poll.

### The credits cache and usage identity

`credits.json` has the same (login, scope) identity and lock discipline as `rate_limits.json`, with kind-wide entries in `logins` and sub-provider entries in the additive `scoped` map. It persists provider-reported paid usage, Codex plan, and Codex and Claude reset-credit fields, so partial observations survive idle sessions. Reset credits keep every known valid expiry in ascending order, equal deadlines included. The earliest is the compact summary, and the provider count stays authoritative when detail is absent or malformed.

Identity keeps the cache honest across account changes. Every identity-bearing [`AccountUsageProbe`](../../../crates/rimz/src/agents/credits.rs) result (found, no credentials, not entitled, or failed) carries one `AccountUsageIdentity`: the non-secret owner and scope of the credentials read. `AccountUsageSnapshot` holds only the normalized plan, windows, paid credits, and reset credits. Pi and OpenCode select their delegated owner once through [`delegated_account.rs`](../../../crates/rimz/src/agents/delegated_account.rs), preferring an OpenAI account id for the Codex quota scope and otherwise hashing the refresh or access token under an adapter-specific domain.

Completion compares owners symmetrically. `None` to `Some`, `Some` to `None`, two different identified owners, or two different scopes each block carrying the prior plan, paid usage, and reset credits. If a read resolves a different scope, it completes against that scope's prior data and identity fallbacks, preserving its live lease and the originally claimed scope's attempt floor. A changed owner without replacement windows drops only the affected scope's cached windows. A failed read with no known owner change keeps the prior display data. A failed read that proves a new owner drops prior truth without publishing unverified values.

## Refresh cadences

The producer keeps each metered credential current between turns when its spec declares `direct_account_usage`, whether or not a session is live. It spawns one hidden `rimz agents refresh-usage` helper per credential (login and scope), and only after `credits.json` grants that credential a durable direct-query claim. The account fold admits helpers from the panels it just built, so a newly discovered login with no live session starts its first read in the same heavy pass.

| Clock | Value | Governs |
| --- | --- | --- |
| `OAUTH_USAGE_TTL` | 5 minutes | the ordinary refresh |
| `OAUTH_USAGE_SETTLED_TTL` | 1 hour | a (login, scope) whose last read settled as an auth failure (missing or rejected credentials) and whose published account hint is unchanged since that read was claimed |
| `IDLE_OAUTH_USAGE_TTL` | 3 hours | an [idle account](#idle-accounts), after every outcome |
| `ACCOUNT_USAGE_CLAIM_TTL` | 90 seconds | the lease on each helper segment |

With a recorded `oauth_read_hint`, freshness asks whether the published scope or credential stamp changed since the last read was claimed: a change reopens the claim at once, while an unchanged hint waits the cadence even if the reading's own identity differs. An entry with no recorded hint (no published hint, an old cache, or an older build's write) compares the published record with the reading's own identity instead, so it stays due while the two differ; its next completed read under a published hint records it. `RIMZ_OAUTH_USAGE_OFFLINE=1` disables account-usage fetches for the process tree, and `0`, `false`, and an empty value read as off.

### The usage claim

The claim makes the helper safe across rooms. Under the shared credits lock, scheduling derives the claim from that credential's published account scope and credential stamp plus the prior same-scope usage owner. It records a UUID nonce, the claim time, the requested scope, the optional stamp and owner, and the raw published hint before derivation. Completion keeps that hint as `oauth_read_hint` beside `oauth_read_at_ms`, so a hint that moves during the read still makes the next pass due. The helper receives the nonce, `LoginKey`, and optional backing `sub_provider` in its request; a named credential is read exactly, not reselected from a newer session. Only the worker holding the matching nonce resolves credentials, contacts the provider, and publishes. Claims, leases, read hints, and cadence are independent per (login, scope), so rooms admit one fetch per credential without blocking sibling credentials. A failed spawn cancels its claim, and an expired claim becomes retryable. Neither records its hint on the entry.

Before any provider call the helper re-reads the room's defaults and cached agent rollup. It cancels the claim only when the login is neither in use (a default or a resolvable live root stamp) nor an idle account (`crates/rimz/src/sidebar/refresh/usage.rs::refresh_claimed_account_usage_with`).

### The helper's two segments

The first segment is the realtime leg, and it runs only for the room-default login of an adapter that has one (Codex's app-server). That app server belongs to this room and speaks for the room's own login, so every other login, in use or idle, gets the direct probe alone. The leg publishes its reading with the identity the reading carries: for Codex, the `auth.json` account id and auth stamp read before the attempt, and a reading whose stamp moved during the read is dropped. Then it renews the claim.

The credits merge takes that identity whole. The entry's account key and stamp come from the reading, and the prior entry's entitlement and missing display fields carry over only for the same account, so an unbound reading never inherits another account's plan. The direct read's freshness (`oauth_read_at_ms`, `oauth_read_hint`, settled auth) carries over only when the stamp is unchanged too, so a credentials rewrite still reopens the claim.

The second segment runs the direct query and publishes again. A missing, replaced, or lock-contended claim cannot renew, and the direct segment does not start. Completion re-checks the nonce, so a superseded writer is rejected. Both segments publish an `AccountUsageSnapshot`: windows go through `fuse_window`, and plan, paid usage, and reset credits go through one cache conversion. Direct-query windows are authoritative and merge after the realtime fold, so a fresh credential read replaces a stale realtime process. A detached writer waits on the bounded rate-cache lock and publishes before returning. The producer's per-frame path falls back to a non-blocking read under contention.

### Reads that fusion pulls forward

When the producer persists a new reset epoch, parks a new pending refill, or first shows an unknown panel, it clears `oauth_read_at_ms`, `oauth_read_hint`, the settled state, and any stale claim, so scheduling in the same pass re-probes. An already pending refill does not force another read. A successful authoritative response clears it, so a later contradictory low reading can park and request again.

### Idle accounts

An idle Codex account has no panel, and [auto-redeem's expiry rescue](#auto-redeem) is its one reader ([login sets](#logins)). The same lane claims it through `claim_idle_account_usage`: the ordinary claim, held to `IDLE_OAUTH_USAGE_TTL` since the last attempt by any room, whatever that attempt's outcome. A transient failure, a settled auth failure, and a changed credential stamp all wait the floor. A failing or logged-out spare account is therefore not retried on the in-use tiers, and an account another room keeps fresh is never fetched here. An account config that does not load declares no idle account.

### The accounts command

`rimz accounts` (also `accounts list`) and `accounts show` share one query pipeline outside the producer. They probe every catalog login and call `refresh_provider_usage` for logged-in accounts, on the same durable cadence and claim path unless `--refresh` forces the read. `show` limits usage reads to its kind, and still probes every login. For Claude, Codex, and Pi, the kinds with named accounts, the pipeline probes again, on every run, each stored record that is logged out or ambiguous (an `ok` record without the `login` field whose account carries only a version, `ProviderRecord::login_is_ambiguous`), so a new login shows on the next run. Other kinds keep the TTL, except that a provisional answer is re-checked after `ACCOUNTS_RETRY_TTL`, as the probe table above says.

### Direct account-usage HTTP

[`agents/credits.rs`](../../../crates/rimz/src/agents/credits.rs) retries transport failures, body-read failures, and 5xx responses up to three attempts, waiting 300 ms before the second and 600 ms before the third. 429 and every other status return without retry. Redirects are disabled. A plain 401 or 403 counts as authentication rejection, and Claude's organization-access 403 becomes `NotEntitled`. A status error carries a bounded response body only to the adapter's classifier, never to diagnostic formatting or off-box fields. A surfaced transient failure reports off-box under the one `oauth_usage` operation with a `provider` tag, and the error detail names the request host ([diagnostics.md](../diagnostics.md)).

## Spent windows and paused rows

A window is spent when `used_percentage` reaches 100 while its reset is still ahead. A spent window paints the budget bars. It does not park every agent of the kind.

A row becomes `paused` only when that agent stopped mid-turn on a limit or a transient server error. A native turn-error certificate (`TurnErrorClass` `paused_rate_limit`, `paused_spend_limit`, or `paused_overloaded`) parks the running agent it names. A stalled running agent with no certificate parks when the fused login window is spent and unreset. `paused_overloaded` covers provider overload and transient 5xx errors, and has no reset clock.

The stall fallback reads only account-wide windows for the source agent's stamped login. It excludes scoped windows that carry a parent duration, so a model sub-cap (a spent Fable share) never parks another Claude model. Durationless named quotas keep their spent-and-reset behaviour, including promoting a limit marker to `failed` after the quota resets with no spent window left to explain it. Auto-continue likewise looks up capacity by the agent's login, not the room default. Fresh launch availability resolves the room default before reading capacity, account status and applicable account caps. A supervised launch resolves its own login per kind instead (`LaunchAvailability::read_as`).

A `paused_rate_limit` or `paused_spend_limit` pause stays resumable while the fused account has a subscription window with a future reset, including the common spend-limit case where paid usage is disabled or exhausted but the window will refill. After the window recovers, the row stays parked while the [auto-continue](#auto-continue) record has a chance to wake the turn, instead of a frozen 100% reading turning into a spurious `!`.

Calm agents (`idle`, `success`) and progressing turns keep their lifecycle status even when a bar reads empty. The displayed-status ladder is [model.md](./model.md#displayed-status), and the glyphs are [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

### Auto-redeem

Auto-redeem spends a Codex reset credit when the login's spend ladder reaches `reset_credit`. The reset policy is [`harness/auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs), and [`harness/spend_ladder.rs`](../../../crates/rimz/src/harness/spend_ladder.rs) owns the shared reading used by routing, redemption, and CLI tables. The producer evaluates cached windows and provider credit evidence each frame, without waiting for an agent to park.

What a redemption does to the window is a fact on the credit, set by each adapter's normalization (`ResetCredits::effect`). Codex credits are `RestartsWindow`: the window refills and its natural reset restarts a full window from the redemption. Claude credits are `KeepsSchedule`: the window refills and the reset date stays. These values encode user observations of each provider, and no test against a live consume backs them. The verdict branches on the effect and names no provider. A cached credit without the field reads as `RestartsWindow`.

| Step | Verdict |
| --- | --- |
| a credit is within 30 minutes of expiry (always on, either effect, since an unused credit vanishes) | `ExpiryRescue` |
| `[resume] auto_redeem = false` | none |
| no unscoped duration window is spent with a future reset | none |
| `RestartsWindow`, and the spent window's latest natural reset is at least `auto_redeem_min_gain` away (default `12h`) | `BlockedGain` |
| `RestartsWindow`, and the chain deadline below has passed and the near free reset does not defer it | `ScheduledRedeem` |
| `KeepsSchedule`, and the soonest credit would keep less than 24 hours of life after the spent window's latest reset | `DoomedCredit` |
| `KeepsSchedule`, and that reset is at least `auto_redeem_min_gain` away | `BlockedGain` |

The ladder reads `subscription` while the subscription has room or capacity is unknown. Once an account window is spent, `resume.spend_order = "credits_first"` (the default) prefers usable provider-reported paid credit, then a reset this verdict would spend, then `spent`. `resets_first` swaps the paid and reset steps. A model sub-cap remains unavailable regardless of paid or reset credits. Extra credit comes only from a fresh, successful, kind-wide, non-lapsed `credits.json` entry, never from a display-only configured ceiling or synthesized monthly spend. A failed attempt inside the ten-minute cooldown closes the reset step, and so does a reservation still without an outcome two minutes after it was taken, since its helper declined, errored, or died. A younger reservation or a past success does not, so routing stays available while a redeem is in flight. Expiry rescue is independent of the step and spend order.

A missing reset closes the gate. A missing expiry disables only the doomed-credit and rescue clauses. A `RestartsWindow` credit never redeems as doomed: the redemption restarts the reset, so spending it ahead of a near free reset forfeits that reset. A `KeepsSchedule` credit never schedules: the pacing below assumes a window that restarts.

The schedule spaces credits by predicted refill time, and because it sits behind the gate it fires only into a spent window whose reset is nearer than `auto_redeem_min_gain`. The producer samples growth in each in-use login's longest duration window into the user-shared `auto_redeem_rate.codex@<account>.json` every pass, whatever the gate says, and a three-day half-life EWMA predicts how long a fresh window takes to fill. The sorted expiry list yields backward chain deadlines spaced by that refill time. A deadline is never earlier than one predicted refill after the projected longest window began, so a redemption or a natural reset pushes the next attempt out instead of spending more credits into a fresh window. A longest-window reset less than `auto_redeem_min_gain` away defers the deadline when the first credit still outlives it by 24 hours. Missing window timing or a negligible burn rate collapses scheduling to the 30-minute rescue.

The host spawns a detached helper for each in-use Codex login at `reset_credit`, or for an expiry rescue at any step. The helper resolves the login from the room when it is in use (the room's default or a resolvable live root stamp in the cached rollup) and from the machine catalog when it is idle, and cancels its reservation only when the key is neither. It locks by the cached provider account identity when known, falling back to the login key, so two logins sharing an account serialize their fresh reads and consumes. Under the lock it refreshes windows, paid credit, and per-credit details, re-evaluates the ladder, and consumes the soonest-expiring credit with an idempotency key only if a reset or expiry rescue is still due. Fresh usable paid credit under `credits_first` prevents a non-rescue consume. The user-shared `auto_redeem.codex@<account>.json` stamp stays per login and throttles attempts to one per 10 minutes, holding 30 minutes after a success, across every room. Each attempted consume appends its account, evidence, and outcome to the [assist log](../harness/loops.md#the-assist-log). A success returns refreshed usage to the helper's CLI entry, which republishes authoritative usage and reset credits, appends the assist record, and wakes the room, so [auto-continue](#auto-continue) can wake parked turns on the recovered capacity.

The expiry rescue also covers idle accounts, so a credit on an account no room is using does not expire unspent while any room is open. `refresh_heavy_lanes` hands `redeem_credits` each idle account's cached reset credits from `credits.json` (`idle_reset_credits`), dropping an entry that is not `ok` or is older than `CREDITS_DISPLAY_MAX_AGE`, the staleness line a panel's credits are held to. An idle account is evaluated by `idle_rescue` for `ExpiryRescue` only. The helper judges an idle login as idle whatever its request carried, so a login that stopped being in use between spawn and consume continues under the idle rule. The helper, and only the helper, skips the rescue on positive evidence in its fresh read that the window is unused: a kind-wide capacity whose every duration window, projected to now, reads 0% used or has [not started](#not-started-windows), since a fresh Codex window reports 1% (`ProviderCapacity::known_unused`). Absent capacity, an account-scoped entry, or a window with no percentage is unknown, and the rescue fires. The producer never skips on its cached windows, which can be three hours old and predate usage from another machine. It spawns on the cached expiry alone. An idle account samples no burn rate and gets no forecast. Because the cached expiry is a timestamp, the three-hour idle read cadence still reaches the 30-minute lead. Its cost is that a newly granted credit is discovered up to three hours late.

The sidebar's Codex header marker is a forecast of this policy. `project_redeem_forecasts` runs at both display folds, the main enrich and `provider_panels_from_caches` (the `rimz accounts` fold), after the budget views. It reads the same per-login facts as the producer and writes none. It evaluates the ladder with the longest duration window hypothetically spent: a reset step or expiry rescue is `Armed`, otherwise `Holding`, and `auto_redeem = false` is `Manual`. Usable paid credit under `credits_first` therefore reads `Holding`. Where the hypothetical cannot prove a hold, auto mode remains `Armed`. The forecast takes no live agent input. A hold for a near free reset stays firm when the soonest credit outlives that reset by 24 hours. A credit that does not outlive it by a day can turn `Armed` once its chain deadline passes.

Manual [`rimz accounts redeem`](../../reference/cli/accounts.md#redeem) supports Codex and Claude. It prepares one fresh provider action without locking, previews it, then locks only after confirmation and consumes that same action. A provider hold refuses before confirmation or reservation without changing the banked count. Claude alone reads its organization profile during preview and sends a single, non-retried claim on confirmation ([private action](./adapter_claude.md#oauth-usage-probe)), and its preview omits the Codex-only forecast. Automatic Claude redemption remains unarmed. Both kinds use the same machine-wide lock and stamp even outside a room, but never apply `stamp_allows_attempt`: the preview keeps the stamp it read, and the locked tail refuses when the stamp differs from it or is a live reservation (no outcome, younger than ten minutes). A Codex `reset` stamp still holds auto-redeem's thirty-minute post-success cooldown. The shared consume tail reserves before spending and stamps every outcome. Reason `manual` distinguishes its credit-ledger record, whose outcome may include `cooldown`. A claim error leaves its reservation standing. A post-reset refresh failure returns success with a warning rather than inviting a retry. The CLI publishes refreshed usage, appends the record, and wakes the current room. Outside a room, sidebars pick up the shared caches on their next refresh.

### Auto-continue

With `[resume] auto_continue = true` (off by default), the producer resumes a [parked turn](#spent-windows-and-paused-rows) by delivering the configured nudge (`continue` by default) to the agent's pane through the same send path as `message --steer`. The agent's next hook moves the row back to `running`. The trigger and the `rimz agents auto-continue` helper that sends are [`harness/auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), and the pure arm decision is `resume_park`.

A spent window supplies a reset clock only after the turn carries a provider-certified rate-limit marker. Account exhaustion, a stall, or message text never certify why a turn stopped. Antigravity has identity-bearing quota clocks but no certified recoverable stop class, so its quota never arms auto-continue.

#### Arm

Each frame an agent is parked on a resumable certificate, the producer writes a durable `ParkRecord` with the park class and the agent's frozen `last_activity`. A rate-limit record captures the spent window's reset deadline, so the resume survives the session sidecar it was seen through: a 5h or 7d window can outlive the session, and the post-reset reading gives the record one frame to nudge. A backoff record carries the turn-error marker time and retry state. A RimZ [budget park](../harness/budget.md#the-park) with a reset time is checked first and short-circuits the provider classification: it arms a `Budget` or `BudgetMonth` record whose deadline is the park's `resets_at`, with its class chosen by the last binding reset. If the record is lost after the window recovers while the agent still carries a limit marker, the producer re-arms a due-now record before firing.

#### Fire

Once the reset deadline or backoff step is due and `last_activity` has not advanced, the producer spawns the helper, which queues and delivers the nudge. A nudge for a provider park is gated `DeliveryGate::Resume`. Budget-reset nudges (`budget_day_reset` and `budget_month_reset`) are gated `DeliveryGate::Done`; the reason-to-gate mapping belongs to `harness::auto_continue`, not the CLI helper. Overload and transient API-error parks follow `auto_continue_backoff_secs` (default `[180, 300]`), whose last value repeats: the first attempt lands after 3 minutes and later ones every 5. Message events carry the queued, sent, delivered, timed-out, or failed trace, and after delivery the [assist log](../harness/loops.md#the-assist-log) keeps the park time, verdict, handle, and message id.

#### Clear

For a rate-limit or overload park whose activity has advanced, the producer holds the record unchanged while the row is `Running` or `Failed` with no displayed turn error, even when the resume message is not visible yet. Once a pausing marker appears, a matching-card resume message enqueued at or after the park baseline allows a rebase only when there is no root tool timestamp (`last_tool_at`) strictly after resume enqueue and at or after the provider turn start (`turn_started_at`, when known). A tool that itself reopens a resting turn shares the start's ingress timestamp and counts as work, and equality with enqueue does not. Such a bare limit or overload reply within the same park class rebases the activity baseline to `last_activity` while preserving the original attempt anchor and retry state. The tool clock distinguishes work from an errored turn end, which advances `last_activity` even without work. The enqueue boundary excludes old tools when a background wake carries the provider turn stamp forward. An observed tool meeting those boundaries earns a fresh allowance even if the turn then hits another limit. Text-only work without a tool observation still counts as a reply. A settled status other than `Running` or `Failed` with no marker clears the record. A delivered resume message with no turn start still clears it.

#### Exhaust

All provider park classes share `auto_continue_max_retries` (default 12), counted as distinct `DeliveryGate::Resume` prompt message ids enqueued since the attempt anchor (`crates/rimz/src/harness/auto_continue.rs::evidenced_attempts`). A budget-reset nudge is gated `Done`, so it never counts against the allowance. The anchor is `attempts_since`, or `parked_at_activity` before a rebase. The evidence merges the rollup's retained terminal outcomes (every one for seven days) with the live queue. Helper spawns and pre-queue crashes only pace retries. A nudge's bare limit or overload reply within the same park class preserves that count even when it advances the user-turn timestamp. An observed root tool meeting the [Clear](#clear) boundaries ends the episode, so another park after that work gets a fresh allowance without requiring an intervening settled frame. Rate-limit nudges also wait at least 120 seconds after any matching-card resume message, even after the park record is cleared. At the default overload ramp, attempts span about 58 minutes before the row promotes to actionable `failed`.

## Account dollar caps

`[accounts.budget] <kind> = "100/day"` sets one cap per history pool of that kind (`default` and its shared accounts together, each standalone account alone), shared by every room on it. A kind is eligible only when its adapter's `AccountSpend` concern is wired to authoritative account-level dollar history. Identity, a plan label, quota windows, point-in-time prices, and partial transcript estimates do not qualify. Strict config parsing and process compilation reject an ineligible kind with the exact key to remove. Room start warns, and `rimz budget --account KIND` validates only the targeted kind. Fleet reports in `crates/rimz/src/cli/budget.rs::inspect` omit ineligible keys and print one warning per key on stderr after the table, including after fleet writes. The ledger ignores stale unsupported config.

Account config also accepts `/month`, and a standalone named account's own `budget` overrides the kind cap. A shared account's own budget is refused with the standalone-history fix or the shared kind-key alternative. Month caps count API-key billing only; a pool containing any subscription member is exempt, with a room-start warning pointing to Claude's extra-usage limit, Codex prepaid credits, or the provider's own spending limit. The decision input is the local-day or UTC-month spend window the [spending walk](./spending.md#what-reads-the-totals) publishes per history pool. The reader requires the current window's cutoff, not merely a recent timestamp. The ledger, verdict, park, and waiver are [budget.md](../harness/budget.md). On the dashboard, a healthy cap stays quiet. While agents are parked on a crossed account cap, the headline turns alarm-red and appends `$used of $cap/day` or `$used of $cap/month`.

## Per-provider mapping

Each adapter page maps its native surfaces onto these types, and this table is the index.

| Provider | Account identity → `AgentAccount` | Balance → `AgentRateLimits` / `ExtraCredits` |
| --- | --- | --- |
| Claude | [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) → plan, metered; API keys, key helpers, and positively identified Console profiles are unmetered. Same-login credential metadata refines Max subtiers, and a recognised bound live profile overrides the saved plan | metered: statusline 5h/7d windows and the gateway spend quota (`$`), and the OAuth usage query for windows and banked limit resets; unmetered: transcript spend only ([adapter_claude.md](./adapter_claude.md#account-and-balance)) |
| Codex | app-server `planType`, or `$CODEX_HOME/auth.json` | app-server windows and credits, then the OAuth usage query with reset credits ([adapter_codex.md](./adapter_codex.md#account-and-balance)) |
| Antigravity | statusline, or the running `agy` local service → plan, metered | paired status and quota read on one endpoint → hashed kind-wide owner, authoritative 5h and weekly windows. Credits and dollars unknown ([adapter_antigravity.md](./adapter_antigravity.md#account-and-balance)) |
| Copilot | `$COPILOT_HOME/config.json` non-secret login → metered | internal account query → plan, named monthly `cr` or `cht`, and `prm` scopes, and no paid-usage reading ([adapter_copilot.md](./adapter_copilot.md#account-and-balance)) |
| Kimi | `~/.kimi-code/credentials/kimi-code.json` → managed OAuth, kind-wide | managed OAuth usage → weekly and detail windows, optional USD Booster ([adapter_kimi.md](./adapter_kimi.md#account-and-balance)) |
| Pi | `~/.pi/agent/auth.json` (oauth → metered, api_key → unmetered) | extension response headers, and the OAuth usage query over the backing token ([adapter_pi.md](./adapter_pi.md#account-and-balance)) |
| OpenCode | selected OpenCode SQLite database's `credential` table (oauth → metered, key → unmetered, external → unknown) | the OAuth usage query over the backing token ([adapter_opencode.md](./adapter_opencode.md#account-and-balance)) |
| Qwen | effective JSONC model and provider plus credential source → Alibaba scoped, or direct API unmetered | Alibaba API-key usage → scoped 5h, 7d, and mo windows with integer request amounts, display and `Bound` launch only ([adapter_qwen.md](./adapter_qwen.md#account-and-balance)) |
| Qoder | none | no integrated login, quota, or Credits source ([adapter_qoder.md](./adapter_qoder.md#account-cost-and-history)) |
| Cursor | CLI `status --format json` and `about --format json` → email, tier, version | none ([adapter_cursor.md](./adapter_cursor.md#account-and-balance)) |
| Grok | `${GROK_HOME:-~/.grok}/auth.json` metadata → session metered, API key unmetered | none. Spend comes from `turn_completed` ([adapter_grok.md](./adapter_grok.md#account-and-balance)) |
| Amp | `AMP_API_KEY` or `secrets.json` presence → pay-per-use, no identity ([adapter_amp.md](./adapter_amp.md#account-and-balance)) | none |
| OMP | unknown, since the credential SQLite store is not read | extension response-header windows only. Main-session spend reconciles at turn end ([adapter_omp.md](./adapter_omp.md#account-and-balance)) |
| Muse | none integrated ([adapter_muse.md](./adapter_muse.md#context-account-and-cost)) | none |
| MiniMax | none, account schema unverified | none ([adapter_minimax.md](./adapter_minimax.md#context-history-cost-and-account)) |

Claude, Codex, Copilot, Pi, OpenCode, Kimi, and Qwen declare `direct_account_usage` and read provider credentials for it. Antigravity reads no credential and pairs identity and quota through the verified service of an already-running `agy`. Antigravity, Copilot, OpenCode, Kimi, and Qwen have no realtime leg, so the direct query is their only balance source. Pi and OpenCode delegate backing Anthropic and OpenAI tokens to the Claude or Codex usage fetcher; OpenCode also reads Go status with its Console OAuth token. OpenCode's usage probe refuses when its selected credential is non-OAuth, and falls back to no other OAuth credential, so that account shows no bars ([adapter_opencode.md](./adapter_opencode.md#account-and-balance)). Copilot's scopes and Claude's gateway `spend` scope omit a duration on purpose, keeping these allowances out of 5h and 7d policy. The gateway reset does not schedule auto-continue, and a spend-limit park becomes actionable after reset.

Normalization belongs to the adapter. Claude's owns its fixed named durations and clamping. Codex's and OpenAI's own dynamic durations, plan and credit cleanup, ordering, and the lifted empty 5h row on a cold cache. Sidebar fusion completes only durations already present in same-scope persisted truth.

## Adding a provider

A new agent gets an account block and balance bars by filling these types from its own surfaces. Aggregation, fusion, caching, and the dashboard need no change. The steps extend [adapter.md → Adding an agent](./adapter.md#adding-an-agent):

1. Fill `AgentAccount` on the session's `AgentContext` from the transport, override `probe_account` for the idle case, or both.
2. Fill `AgentRateLimits` from the transport: each window with `used_percentage`, a reset instant, and either `duration_mins` or a stable scope id and label.
3. Where a read-only account-usage surface exists, implement `probe_account_usage`, declare `direct_account_usage`, and fill `ExtraCredits`. If the account identifies a sub-provider, override `has_quota_surface` for those with readable quotas (default `false`). Use `Disabled` only when the provider says paid usage is off.
4. Set the spec's color and name, and add curated art to [`emblems.toml`](../../../crates/rimz/src/agents/emblems.toml). Every built-in must have art distinct from the fallback.
5. Implement the transcript spend parser ([spending.md](./spending.md#per-provider-parsing)).
6. Keep every step best-effort: a missing fact is an omitted label, a `v?`, or an unknown track, never an error.

Golden the account mapping from fixture probe and transport payloads, including logged-out and unparseable cases. Each adapter's `account.rs` goldens are the model. Golden the spend parser from a fixture transcript, including dedup and zero or negative cost.

## See also

- [spending.md](./spending.md): transcript spend, the spending caches, and token pricing.
- [model.md](./model.md): the rollup the account facts ride on, and the displayed-status ladder a park feeds.
- [adapter.md](./adapter.md): the capability seam behind `probe_account`, `probe_account_usage`, and `parse_spend`.
- [`rimz accounts`](../../reference/cli/accounts.md): the CLI query for account status, windows, credits, spend, and day or month caps.
- [the interface reference](../../interface/sidebar.md#the-provider-dashboard): bars, the `ex` and `api` rows, and exhausted-window rendering.
- [sidebar.md](../sidebar/sidebar.md#provider-dashboard): where the dashboard sits in the renderer.
- [budget.md](../harness/budget.md): dollar-cap ledgers, verdicts, and waivers.
