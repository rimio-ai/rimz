# Provider accounts, balances, and spend

A coding agent runs against a **provider account**: a login, on a plan, that may or may not be metered. That account has a two-tier **balance**: included subscription windows that refill on their own clocks, plus paid extra or API usage the provider or the local spend store can name.

This doc owns the account, balance, spend, and pricing model end to end. What the metered, unmetered, and plan facts mean; how the producer folds them into the provider dashboard; how full-history spend is totalled; and how a model resolves to a price. It is the single home for account, balance, and spend semantics, folding onto [`AgentAccount`](../../../crates/rimz/src/agents/context.rs), [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs), [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs), the [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs) the cost walk produces, and the [`SidebarProviderPanel`](../../../crates/rimz/src/store/snapshot/view.rs) the renderer paints.

The shape, end to end:

```text
sources                                    shared user-scoped caches
  a live session's rich context ─────── (rides the rollup; model.md)
  the out-of-band account probe ──────► accounts.json
  the account-usage refresh ─────────► rate_limits.json · credits.json
  the full-history spend walk ────────► spending.json · provider-spending.json
                                                │
                                                ▼
                        producer aggregation + window fusion
                                                │
                                                ▼
                        one SidebarProviderPanel per kind
              plan · version · budget bars · paid usage · spend headline
```

Account identity, included balance, and paid-usage display are enrichment by default. A missing binary, a logged-out account, or an unavailable API degrades to an omitted plan label, a `v?` version placeholder, or an unknown budget track. The one exception is a fresh managed Qwen launch, where an exact local credential binding may use its matching authoritative Alibaba windows as a launch-time quota gate; absence there stays non-blocking. [Daily dollar caps](#daily-dollar-caps) are the separate correctness surface: their decision input is the durable transcript walk, not a provider probe.

Where the per-kind surfaces live: each provider maps its native account and balance surfaces in its own [adapter doc](#per-provider-mapping), and the raw auth and usage endpoints those read are in the [upstream references](../../externals/agent-adapter/claude-reference.md#auth-surface). See [DESIGN.md](../../../DESIGN.md#triage-at-a-glance) for the account-scoped-budget invariant this operationalizes, and [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard) for what the dashboard looks like on screen.

## The model

Four facts, all account-scoped rather than session-scoped.

**Account identity** ([`AgentAccount`](../../../crates/rimz/src/agents/context.rs)): the non-secret `account_id` a provider exposes, the raw `plan` tier it reports (`max`, `team`, `pro`), a `metered` flag, the typed account scope, and, for a multi-provider client such as Pi or Qwen, a user-facing `sub_provider` label naming the backing account.

**Included balance** ([`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs)): an ordered list of [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs)s, each with a `used_percentage`, a typed `resets_at` instant, and either a `duration_mins` temporal identity or a provider scope with a stable id and compact label. Duration windows keep the short-to-long `5h` and `7d` display along with reset-to-max roll-forward, not-started, pace, and surplus semantics. Provider- or plugin-defined named windows fuse independently by scope id and display their real reset, while an unknown duration makes every duration-derived claim fail closed.

**Paid usage** ([`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs)): an optional provider-paid balance or local API spend projection beyond the subscription windows. It may name used USD, remaining USD, a limit, or a disabled state, and missing fields stay missing, so the renderer can show an unknown or uncapped row without inventing a cap. Disabled or exhausted extra credits do not make a parked turn terminal while the subscription mana bar still has a future refill.

**Reset credits** ([`ResetCredits`](../../../crates/rimz/src/agents/credits.rs)): Codex-only redeemable rate-limit resets. The cache stores the provider-reported available count, every known valid available-credit expiry, and the earliest expiry summary. The compact dashboard uses the count and earliest instant to color its glyph and blink while a spent duration window makes a manual redemption useful; the standalone provider report can list individual deadlines.

Account identity and included balance ride [`AgentContext`](../../../crates/rimz/src/agents/context.rs), the session-scoped rich-context record ([model.md](./model.md#rich-context-agentcontext)); the producer lifts them to the account scope at aggregation time. Paid usage rides the shared credits cache when a provider reports it, or a read-time local spend projection for API-key accounts.

### Account scope

A `KindWide` scope belongs to every session of one provider kind. A `SubProvider` scope carries a canonical provider plus a variant, such as Alibaba International or China, so a multi-provider client displays only the effective account's enrichment without claiming every session shares it. Control additionally requires the opaque credential account key to match exactly.

Managed launches carry one typed state across generic launch and schedule code:

| State | Means |
| --- | --- |
| `PendingResolution` | final inputs have not arrived yet |
| `Unsupported` | ordinary kind-wide capacity applies |
| `Unresolved` | exact selection applies but identity is unproven, so no capacity is read |
| `Bound` | only matching exact-account quota and cache reads are permitted |

### Metered vs unmetered

This is the one distinction the dashboard turns on. A subscription or ChatGPT login is *metered*: it draws on the rate-limit windows, drawn as draining mana bars. An API-key login is *unmetered by subscription windows*: it has no included-window budget to drain, so the dashboard shows a single `api` budget row derived from trailing-month transcript spend against an optional display ceiling, with remaining dollars beside the bar. A `metered: None` is unknown, and the dashboard infers metering from whether any rate-limit window was reported.

## Two origins

A kind's account and balance reach the dashboard two ways, mirroring the [context read path's](./adapter.md#context-sources) own split:

1. **A live session's rich context.** The statusline or app-server transport already carries account and included-window balance, so any live session of a kind fills both at no extra cost. The transport is per-kind and stored on the rollup; this doc owns only what its fields mean.
2. **An out-of-band probe.** For a provider that is logged in but has no live session this run, the producer probes the login directly. A metered login, a non-empty account identity, or existing spend history then keeps that provider visible between turns.

A live session always wins where both exist: its reading is richer and current.

Paid usage reaches the dashboard through a separate shared `credits.json` cache when a provider account-usage surface can be reached from local credentials or the Codex app-server, and through a read-time local spend projection for API-key accounts. Absence is an unknown `ex` row or an unlimited full `api` bar, never a synthesized value.

Every credential-file path stays read-only. RimZ does not refresh tokens, write provider auth files, import browser cookies, or implement managed multi-account switching.

## The out-of-band probe

[`probe_account`](../../../crates/rimz/src/agents/capabilities.rs) returns an [`AccountProbe`](../../../crates/rimz/src/agents/account.rs) with three arms, and the arm, not just the value, drives that provider's cache TTL:

| Arm | Means | Caching |
| --- | --- | --- |
| `Found(AgentAccount)` | a resolved login | authoritative; rides the long success TTL |
| `LoggedOut` | the probe ran and confidently found no login | also authoritative (it changes about never), so it caches like a success |
| `Unavailable` | the probe could not complete: a binary that would not run, a non-zero exit, an unreadable file | transient; the provider keeps its last-known-good facts and retries alone on the short failure TTL |

The probe is a pure read; cross-process memoization lives one layer up, in the producer. Each provider's probe mechanics (Claude's `claude auth status` fork, Codex's and Pi's cheap auth-file read, Antigravity's verified same-uid process-owned loopback service) are in the adapter docs. An adapter with no out-of-band login surface defaults to `LoggedOut`.

Every registered adapter also exposes a display-only version probe by default: locate its declared executable, run `<binary> --version`, capture stdout and stderr separately, and hand those streams back to the adapter for normalization. The default parser accepts only a conventional numeric CLI version and abstains on banners or unknown prose; Copilot, Amp, and Cursor recognize their branded build shapes and publish only the validated version token. Raw account and version subprocesses have a three-second wall deadline, and a timeout is the same transient outcome as a spawn or exit failure. Rich context transports still win when present, since Claude's and Antigravity's statuslines and Codex's app-server context report fresher versions for live sessions; the CLI probe fills idle or older account-cache entries. Antigravity keeps its disabled override because invoking `agy --version` is unsafe for idle enrichment, and manifest plugins keep their configured probe command and parser. A provider's absent version bypasses its long success TTL only after its short retry TTL, so a binary that cannot report a recognized shape does not re-fork on every producer frame or disturb another provider's clock.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../../crates/rimz/src/store/snapshot/view/providers.rs) folds accounts and balances into the dashboard view-model — one [`SidebarProviderPanel`](../../../crates/rimz/src/store/snapshot/view.rs) per kind. It is **producer-only**: it needs per-machine config and the out-of-band probe the pure reducer cannot read, so the reducer leaves `providers` empty and every consumer tab reads the producer's published panel (see [state.md → Projections](../sidebar/state.md#projections)).

What fills each part of a panel:

- **Aggregate stats.** The per-provider `spending` (configured headline, 7d, 30d, 365d, summed fleet-wide across the kind's transcript history by the spending walk); the session headline uses the machine-global burst shared by every provider kind. `plan` and `version` come from the freshest `context.observed_at`, the version falling back to the binary probe when no session reports one. A kind with no live session earns a panel when its probed account is metered, exposes a non-empty account identity, or has recorded spend. A credentials-only unmetered account stays hidden until its first recorded session, and spend without a probed login never creates a provider section by itself.
- **Account.** `metered`, the account scope, and the `plan` label come from the kind's account (a live session's, or the probed idle one). A `plan` tier formats into a brand label (`max` becomes `Claude Max`, `pro` becomes `ChatGPT Pro`), and a missing account infers `metered` from whether windows were reported.
- **Brand style.** `[theme.providers.<kind>]` overrides the built-in defaults. Art resolves from the embedded emblem catalog, using a curated kind entry when present and the shared fallback otherwise; the catalog also resolves built-in tint masks into color runs. A color override keeps those tints, while an `ascii_art` override paints its replacement in the single brand color. Color and product name come from `AgentSpec`, and an unknown kind gets neutral grey ([theme.md](../../guide/theme.md#provider-styling)).
- **Balance windows.** The per-identity set fused by `fresh_windows` and `fuse_window` ([Window fusion](#window-fusion)): scoped lanes use their stable provider id, unscoped lanes use their duration. A kind that declares a window surface paints its own live and cached readings; a kind that declares none renders the absence deliberately even if a stray sidecar reports windows.
- **Paid and API usage.** `ExtraCredits` from the shared credits cache folds onto metered panels, then takes a display-only `[accounts.usage_limit_usd.<kind>]` ceiling if the provider did not report a cap. Codex `ResetCredits` ride the same cache for the header marker. Unmetered API-key panels synthesize `ExtraCredits::Known` from the provider's trailing-month transcript spend plus the same optional ceiling: a configured ceiling drains by that spend and displays the remaining dollars, while an uncapped key paints a full `∞` bar without pretending the provider enforces a limit.

**Which panels appear, and in what order.** With no explicit `provider_list`, a usage rank decides: a live room session leads, then trailing-week, month, and year distinct-session counts, then a qualifying probed account and the credential file's mtime, with registry order breaking a full tie. The mtime is a best-effort login-recency signal from file-based probes; subprocess-only and probe-less adapters leave it absent. This rank decides both paint order and survival under the *stacked* dashboard's `theme.display.max_provider_blocks` cap (default 3), while a *tabbed* dashboard is height-bounded by its active block and shows every discovered provider. The tab rail greedily keeps the highest-ranked tabs that fit and always reserves the active tab, so a narrow pane drops the least relevant tail without hiding the selected account. An explicit `provider_list` overrides the set, order, and cap; `"all"` expands the remaining providers in usage-rank order ([theme.md](../../guide/theme.md#display)).

**Where the caches live.** The account, rate-limit, and credits caches are user-scoped and persistent under `$XDG_STATE_HOME/rimz/shared/`, single-flighted across rooms by locks under `$XDG_RUNTIME_DIR/rimz/shared/`. The elected producer publishes `accounts.json`, `rate_limits.json`, and `credits.json`; consumers read them and never fork. A due account batch runs independent account-then-version chains through four scoped workers and joins them before one deterministic atomic `accounts.json` publication. A caller that cannot open the coordination lock may probe locally without publishing, and a caller that times out behind a live producer serves the current cache for that frame and lets the next tick observe the winner, so fresh rooms do not duplicate the cold subprocess wave.

### Per-provider spend

A panel's headline line is transcript-history burn for the configured `[sidebar] spend_window`: the `◎` session count, then the token breakdown `◇ ↘ ↗ ◌` (integer magnitudes, the fleet-store rows' exact vocabulary, with cache creation folded into `↘` input), with the bold dollar-green `$` pinned right. It reads from `spending`, the per-provider [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs) the walk returns ([Cost history](#cost-history)): the producer attaches each kind's entry to its panel before sorting, and the renderer reads `spending.headline` (serialized as `today` in the cache for compatibility).

There is no live-session aggregate to fall back to, so a token-only provider like Codex, priced through [token pricing](#token-pricing), shows its dollars the same as a `costUSD`-logging one. The fleet-wide trailing-week and trailing-month `W:` and `M:` store rows are a separate, fleet-level read pinned below the panels. The spend is producer-only, like the rest of aggregation, and threads in as a plain map so the reducer stays free of IO.

## Window fusion

Balance is account-scoped, so one window is fused from every matching reading of it: across parallel sessions, across the live transport and the out-of-band refresh, and across time.

Each [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs) carries its provenance: an `observed_at` capture stamp, and a `source` that is either `BestEffort` (Claude's statusline) or `Authoritative` (a direct account-usage query: Codex's app-server, a provider OAuth refresh, or Antigravity's verified private local service). Usage only climbs within a live window, so a reading that *lowers* the bar is a refill that must be earned, and the fusion decides how far to trust it. Two stages:

**Live, per frame.** [`fresh_windows`](../../../crates/rimz/src/store/snapshot/view/providers.rs) reduces every session's readings to one live candidate per stable window identity. It first rejects content-stale readings *whole*: a reading whose shortest duration window's reset has already passed predates that reset, so every window in it is stale, even a longer window whose own reset is still future. If the payload has no dated duration window, its earliest dated scoped reset supplies this backstop. This is load-bearing, because an idle session re-runs its statusline and re-stamps a fresh `observed_at` over a days-old payload, so `observed_at` alone cannot tell live from stale. Among the survivors, the most-drained value wins independently per scope id or duration: within one live window usage only climbs, so the highest reading is the most current, and the pick is stable against parallel sessions reporting at slightly different instants.

**Across source and time.** [`fuse_window`](../../../crates/rimz/src/sidebar/refresh/rate_limits.rs) folds that live candidate into the persisted truth ([Persistence across idle sessions](#persistence-across-idle-sessions)). A climb is adopted at once. A drop is trusted immediately only with corroboration:

| Evidence | Verdict |
| --- | --- |
| an `Authoritative` reading no older than the prior | adopt: it queried the official API, so it invalidates older best-effort data |
| a later `resets_at` | adopt: a new window epoch |
| a `BestEffort` drop landing at or below the reset floor | hold the prior until the low reading stands for a short confirm window |
| a mid-range `BestEffort` drop | hold the most-drained prior: this is jitter, not a reset |

The confirm path exists for the mid-window free reset a provider sometimes grants, refilling progress with the reset timer unchanged, so one lagging or garbled sample cannot dip a live budget while a genuine refill still surfaces. An out-of-order sidecar with a stale `observed_at` cannot undo a newer reading. The same inputs always yield the same bars, whichever session reported last.

A debug trace of every reading (`RIMZ_RATE_LIMIT_TRACE`, off by default) records `used_percentage`, `resets_at`, `observed_at`, and `source` per frame, so the confirm-window timing can be tuned against real reset events.

Kind-wide budgets feed provider-limit display and [auto-continue](#auto-continue) decisions. Alibaba sub-provider windows additionally carry the opaque credential account key from their authoritative read, and fresh managed Qwen launch and loop controls require exact scope-and-key equality; those scoped windows still stay out of displayed session rate-limit status, message resume, and auto-continue, because durable Qwen sessions carry no provider identity. Per-session `context.rate_limits` feeds matching fusion and the card, never a decision by itself.

### Not-started windows

The included-balance budgets are **sliding** windows: the clock starts on your first token, so until then the provider keeps `resets_at` slid a full window-length ahead. A window whose reset still sits about a full window out has **not started**, and it is detected by that reset distance rather than by a `used_percentage` of 0, because a fresh window still reports about 1% used (a live Codex 5h reads `usedPercent: 1` with the reset a full 5h out). That 1% is the floor: any usage above it means the window has clearly started, so only a window at or below the floor is a not-started candidate. Past that, the reset is a real countdown regardless of its distance.

The dashboard omits the countdown for such a window (a near-full bar, no `↻`), so it reads "ready to start" rather than a misleading ticking placeholder. Display only, applying to every provider; the on-screen treatment is [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard).

### Persistence across idle sessions

A session ending or going idle would otherwise empty the dashboard, so the producer mirrors each resolved window and its account scope into a versioned user-scoped `rate_limits.json` (atomic write under a shared read-modify-write lock in runtime) and reads it back when no live session reports one. A scope change reaps the prior kind entry, and the cache cold-drops incompatible schemas because the data is rebuildable.

| Situation | The cache shows |
| --- | --- |
| before a window's reset | the last-known fused reading |
| a shorter duration window's reset passes while the longest cached duration window is still future | full, with the reset rolled one window-length forward, until a live reading overwrites it |
| a named durationless window's last reported reset passes | unknown, independently: named windows never roll forward |
| the account freshness ceiling (the longest dated window) passes with no fresh reading | every bar for that provider as an unknown empty track |
| every cached window is undated and the newest observation ages past the shortest reported duration | every bar for that provider as an unknown empty track |

An undated reading therefore remains bounded by its own window shape instead of claiming a confident budget forever. An all-unknown dashboard is a refresh trigger, not only a paint state. Once a kind's panel holds no usable value, from an aged-out freshness ceiling, expired named quotas, or a cold start whose cache was never written, the producer forces that kind's authoritative probe on the spot rather than waiting out the read cadence. An `unknown_since_ms` marker carries the open episode, so the force fires on the transition into unknown and once per episode; the durable claim keeps the fetch single-flight, and completion restamps the read on success and failure alike, so a provider that stays unreachable falls back to ordinary throttling. A usable window painting again clears the marker and re-arms the next episode.

Fused ground truth is persisted, including authoritative lifted rows and the open unknown episode's marker; reset-to-full and unknown windows stay read-time projections, never written. The cache tracks login and drops a kind once it logs out.

Paid usage has a sibling `credits.json` cache with the same account-scoped shape and lock discipline. Provider-reported values persist when a local account-usage surface returns them, and Codex plan and reset-credit fields persist there with the paid-usage entry, preserving partial observations across idle sessions. Reset-credit persistence retains all known valid available expiries in ascending order without merging equal deadlines, while the earliest expiry stays the compact summary and the provider count stays authoritative when detail is absent or malformed.

Identity is what keeps the credits cache honest. [`AccountUsageProbe`](../../../crates/rimz/src/agents/credits.rs) carries one `AccountUsageIdentity` on every found, no-credentials, or failed result, while `AccountUsageSnapshot` holds only normalized plan, windows, paid credits, and reset credits, so a successful observation names the exact non-secret owner and scope of the credentials that produced it without a second preflight read. Delegated Pi and OpenCode reads select once through [`delegated_account.rs`](../../../crates/rimz/src/agents/delegated_account.rs), preferring an OpenAI account id and otherwise hashing the refresh token or access-token fallback under an adapter-specific domain. Completion then compares owners symmetrically: `None` to `Some`, `Some` to `None`, unequal identified owners, and unequal scopes each block prior plan, paid, and reset carry and drop that kind's cached windows, while failed reads retain prior display data when no known owner change occurred. API-key spend projections are not persisted, since they are already derivable from the spending walk plus config. A stale, absent, or scope-mismatched credits entry leaves the `ex` row unknown (`∞` value over a dim empty track), while a configured display ceiling can still scale the row if usage is known.

### Refresh cadences

The producer keeps every metered account whose spec declares `direct_account_usage` current between turns through one hidden helper, `rimz agents refresh-usage --kind <kind>`, spawned only after `credits.json` grants a durable per-kind direct-query claim. The account fold admits helpers from the panels it just built, before PR, policy, and git enrichment, so a newly discovered idle login starts its first usage read in the same heavy pass.

The claim is what makes this safe across rooms. Under the shared credits lock, scheduling derives the claim identity only from the published account-cache scope and credential stamp plus the prior same-scope credits owner, and records a UUID nonce, claim time, requested scope, optional credential-file stamp, and optional cached owner fallback. The helper receives the nonce through a hidden `--claim-id`, and only the matching worker may resolve credentials, contact the provider, and publish. The claim and `oauth_read_at_ms` live under the same lock, so workspaces admit one fetch, a failed spawn cancels its claim, and an expired worker claim becomes retryable without a marker file.

The cadences:

| Clock | Value | Governs |
| --- | --- | --- |
| direct account usage | 5 minutes | the ordinary between-turns refresh |
| settled auth | 1 hour | an owner-only change with no cheap published signal |
| claim lease | 90 seconds | each bounded helper segment, renewed between them |

A published scope or credential-stamp change reopens the claim immediately. `RIMZ_OAUTH_USAGE_OFFLINE=1` disables account-usage fetches for that process tree.

The helper first folds a provider realtime account reading when the adapter exposes one, renews the matching claim, then runs the direct account-usage query on its own cadence where one exists. The lease covers each segment independently: realtime plus one rate-cache publication before renewal, then direct provider work plus publication after. A missing, replaced, or lock-contended claim cannot renew, and direct work stops; completion keeps the nonce check that rejects a superseded writer. Both paths publish the same `AccountUsageSnapshot`; every authoritative window reading is published, while per-window timestamp-aware fusion decides display precedence and plan, paid credits, and reset credits follow one cache conversion. Direct-query windows merge after the realtime fold, so a current credential reading can replace a stale warm realtime process. Identity-bearing completion invalidates the prior account's windows before publishing replacements; a known new-owner failure drops prior truth without publishing unverified values, while ownerless and same-owner failures retain it. Detached authoritative writers wait on the bounded rate-cache advisory lock and publish their read-modify-write result before returning, while the high-frequency producer keeps its non-blocking read-only fallback under contention.

When window fusion persists a new reset epoch, or a kind's display first goes unknown, it clears `oauth_read_at_ms`, settled state, and any stale claim, forcing the next cache-refresher tick to re-probe before the five-minute cadence elapses, and before the one-hour settled cadence for the unknown transition.

Transient account-usage HTTP failures (transport, body read, and 5xx responses) retry up to three attempts in-process with short backoff before surfacing. Redirects are disabled, both 401 and 403 count as authentication rejection, response bodies and request headers stay out of errors, and 429 or other statuses return without an immediate retry. A surfaced failure reports off-box under one shared `oauth_usage` operation with a `provider` tag, grouping every provider into a single issue while the error detail carries the request host.

## Spent windows and paused rows

A window is **spent** at `used_percentage == 100`: it is currently limiting while its reset still sits ahead. The spent window paints the dashboard's budget bars; it does not park every agent of that kind.

A row becomes `paused` only when that agent actually stopped mid-turn on a limit or transient server error. A native turn-error certificate (`rate_limit`, `spend_limit`, or `overloaded`) parks the affected running agent, and a stalled running agent uses the fused spent, unreset kind window as the fallback pause predicate. `overloaded` covers provider overload and transient 5xx server errors, and neither has a local reset clock.

A `rate_limit` or `spend_limit` pause is resumable while the fused account budget has a subscription window with a future reset, including the common spend-limit case where extra credits are disabled or exhausted but the mana bar will refill. A recovered mana bar keeps the row parked while the persisted [auto-continue](#auto-continue) record has a chance to wake the turn, rather than turning a frozen per-agent 100% reading into a spurious `!`.

Calm agents (`idle` and `success`) and actively progressing turns stay in their lifecycle status even when a budget bar reads empty. The rollup keeps each agent's true lifecycle status; the projection ladder is [model.md](./model.md#displayed-status) and the glyph is [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

Provider quota readings and locally price-booked dollars are best-effort operational control inputs. RimZ can park or resume local panes against them and enforce a user-configured soft cap, but neither value is a provider billing statement or a provider-enforced spending limit.

### Auto-redeem

Reset-credit policy lives in [`harness/auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs). Expiry rescue always attempts a credit within 30 minutes of expiry, because an unused credit would otherwise vanish. With `[resume] auto_redeem = true`, a spent duration window also redeems when its latest natural reset is at least `auto_redeem_min_gain` away (default `12h`), or when the credit would retain less than 24 hours of useful life after that reset. A missing reset cannot arm the spent-window path, while a missing expiry disables only the doomed-credit and rescue clauses.

The producer samples growth in the longest duration window into the user-shared `auto_redeem_rate.codex.json` cache. A three-day half-life EWMA predicts how long a fresh window takes to fill, and the full sorted credit-expiry list produces backward chain deadlines spaced by that refill time. The effective deadline is no earlier than one predicted refill after the projected longest window began, so either a credit redemption or a natural reset moves the next scheduled attempt forward instead of draining more credits into a fresh window. `ScheduledRedeem` starts the chain even at low current usage so later credits can each capture a refill; a natural reset less than `auto_redeem_min_gain` away defers that deadline when the first credit still survives it by at least 24 hours. Missing window timing or negligible burn rate collapses scheduling back to the 30-minute rescue deadline.

The elected producer spawns a detached helper for `ExpiryRescue`, `BlockedGain`, `DoomedCredit`, or `ScheduledRedeem`. The helper takes the user-shared `auto_redeem.codex.lock`, refreshes windows and per-credit details, re-evaluates the verdict, and consumes the soonest-expiring available credit with an idempotency key; it reads the burn-rate cache without updating it. The user-shared `auto_redeem.codex.json` stamp throttles attempts for 10 minutes and successful resets for 30 minutes across every room. Once a consume is attempted, the helper appends its decision evidence and outcome to the account-global [assist log](../harness/loops.md#the-assist-log). A successful consume immediately republishes authoritative usage and reset-credit readings, letting [auto-continue](#auto-continue) wake certified parked turns on the recovered capacity.

### Auto-continue

With `[resume] auto_continue = true` (off by default), the producer resumes a [parked turn](#spent-windows-and-paused-rows) by typing the configured nudge (`continue` by default) into the agent's live pane, through the same pane-send path as `message --steer`, so the agent's next hook moves the row back to `running` on its own. A spent account window supplies a reset clock only after that turn carries a provider-certified rate-limit marker: account exhaustion, stalled state, and message text never certify why a turn stopped. Antigravity currently has identity-bearing clocks but no certified recoverable Stop class, so its quota supports display, fusion, and surplus without arming auto-continue. The producer-side trigger and the `rimz agents auto-continue` helper that performs the send live in [`harness/auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs).

**Arm.** Within seconds of the park, the producer writes a durable per-agent record capturing either the fused spent-window reset deadline or the non-clocked turn-error marker. Capturing the deadline up front keeps a clocked resume alive outside the session sidecar it was first seen through: a 5h or 7d window can outlive the live session and its cleanup, and a post-reset account reading gives the persisted record one recovery frame to nudge the frozen turn. If the record is lost after the mana bar recovers while the agent still carries a limit marker, the producer re-arms a due-now rate-limit record before firing, because the wait already happened. Qwen's exact Alibaba launch binding does not arm this state machine: it authorizes only a fresh launch decision, and provider-mutable interactive panes, resumed sessions, and `--wake` deliveries have no session-bound identity that could prove they still use the cached account.

**Fire.** Once the recorded deadline or the current backoff step passes and the agent is still idle, the record fires the nudge. Overload and transient API-error parks use a configurable backoff ramp (`auto_continue_backoff_secs`, default `[180, 300]`) whose last value repeats, so by default the first attempt lands after 3 minutes and later attempts repeat every 5 minutes. Message events carry the queued, sent, delivered, timed-out, or failed trace; after the helper's delivery attempt, the account-global [assist log](../harness/loops.md#the-assist-log) preserves the park timestamp, delivery verdict, handle, and message id even after the transient park record clears.

**Exhaust.** Rate-limit, spend-limit, overload, and transient API-error records share one cap (`auto_continue_max_retries`, default `12`), counted from evidenced hidden `DeliveryGate::Resume` messages since the park; helper spawns and pre-queue crashes only throttle pacing and backoff. At the default ramp, attempts span about 58 minutes before exhaustion promotes the row to actionable `failed`.

## Daily dollar caps

`[accounts.budget] <kind> = "100/day"` turns on one provider-login cap across every room on the machine, and only when that adapter's `AccountSpend` concern is wired to authoritative account-level dollar history. Identity, a plan label, subscription quota windows, display-only or point-in-time prices, and partial transcript estimates do not establish a provider-day decision input. Strict config parsing, room start, and `rimz budget --account` reject unknown or ineligible kinds with the exact key to remove or fix, while the ledger defensively ignores unsupported stale config.

For an eligible kind, the spending walk computes a local-calendar-day `SpendWindow` independently of the configured headline, publishes it in machine-shared `provider-spending.json`, and version-gates the cache so an upgrade re-aggregates the current cursor without reparsing unchanged transcripts. The producer accepts the cache's normal short TTL staleness as rate-limit latency.

What the engine does with that spend, from the machine-shared `budget.account.<kind>.json` ledger through the park and its waiver, is [budget.md](../harness/budget.md). The display consequence is here: a healthy cap stays visually quiet, and while agents are parked on a crossed account cap the dashboard turns the headline alarm-red and appends `$used of $cap/day`.

An agent launched through an elevation wrapper as another real uid stays outside account aggregation. Its hooks and credentials live under that other user's home directory, and the current user's probe reads only the current user's account surface, so the sidebar presents it as a flagged process row and leaves it out of the provider dashboard.

## Live cost coverage

Every card cost carries temporal coverage, and the coverage decides where the number may travel.

| Coverage | Shape | May enter |
| --- | --- | --- |
| `Session` | cumulative | cockpit spend, live agent and room budgets |
| `CurrentUsage` | replace-style, point-in-time | the card only: adding it over time would double-count |

Cursor's hook accumulator and Droid's cumulative settings counters both have session coverage, whether a provider supplied dollars or RimZ priced provider-owned token counters. Antigravity uses `CurrentUsage`. Both render as ordinary dollars, and coverage never creates provider history, provider or account totals, or account-day budget eligibility.

Live bound sessions are a separate provider-panel input. The pane projection counts distinct identity-bearing root panes per kind after binding, so two durable conversation rows sharing one pane count once and an identity-less pre-session pane counts zero. This room-local count never enters `SpendTally`, budgets, account spend, or `rimz stats`.

## Cost history

A read path walks the *whole* transcript and store history to total spend, token throughput, and supported named tool calls, bucketed into the configured headline window plus trailing 7d, 30d, and 365d windows as a [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs). One read-only parser per provider lives in its adapter's `spend.rs`, resolved through `spending_sources` and `parse_spend`. Each parsed file yields per-entry cost, a four-way token split (`input`, `output`, `cache_write`, `cache_read`), a per-name tool-call map where the provider exposes one, an entry timestamp, a provider-native thread id when the store holds many sessions, and one per-file origin path when the provider exposes one.

The walk discovers every registered spend store fleet-wide (every Claude project directory, every Codex, Pi, and Copilot session file, every OpenCode database), so each provider counts on the same footing regardless of which project it ran in. Each adapter declares its store topology once: complete transcript lookup walks the declaration on demand, while indexed warm discovery prunes retired history and repairs its frontier on the complete reconciliation cadence. Transcript-only providers such as Kiro keep session lookup without entering fleet aggregation, and plugins enter only when their manifest declares both transcript globs and a spend probe.

Each window carries the token split (its `↘` input folds in `cache_write`, so the `◇` total is folded input plus output, and `cache_read` rides apart), named tool-call totals, and a `sessions` count of the distinct threads that ran in the window. The same parser path feeds [`spending::session_cost_usd`](../../../crates/rimz/src/agents/spending/mod.rs), the turn-end live-card floor for adapters whose provider transport does not push a dollar total: it sums one session's entries and returns session coverage whether each entry carried provider USD or used the `PriceBook`. Droid uses that boundary for its cumulative exact-table value and keeps `spending_sources()` empty, so its settings snapshot cannot reach historical aggregation.

Per-agent and per-team lifetime figures use [`spending::slot_effort`](../../../crates/rimz/src/agents/spending/effort.rs), the sanctioned transcript read for one durable agent seat. For every continuation, the adapter's session-spend-transcripts capability selects every file whose bytes carry that session's effort; Claude returns the main transcript plus its `subagents/*.jsonl` companions. The fold selects the continuation's rows, runs one `SidechainDedup` across every file and session so resumed transcripts and sidechain copies cannot replay prior work into the total, and prices every retained entry through the shared book. Attribution, `rimz agents show`, `rimz teams show`, and the sidebar's finished-cohort receipt all consume that fold, so their dollars and token split name the same work.

Three surfaces read the totals. When a provider has a historical tally, the provider dashboard paints its full headline session, token, and dollar row, and the bottom [fleet store](../../interface/sidebar.md#the-fleet-store) paints the static trailing-week and trailing-month rows; consumers read stale persistent totals when they exist, and the fleet store reads `$0.00` before any supported history is recorded. A ledgerless provider panel keeps the full headline template with its separately derived active-session count live and unavailable token and dollar positions painted as dim `–` placeholders, which is what Antigravity uses because its statusline current usage is replace-style rather than cumulative and `$0.00` would be dishonest. `provider-spending.json` also publishes UTC-day buckets plus per-model buckets for `rimz stats`.

The cockpit's `◎`, `¤`, and breakdown summary reads a producer-published workspace-scoped tally limited to the room's project root plus grouped worktrees, omitting unknown-origin files. The default session headline opens on a recorded user prompt and stays open while priced activity keeps each gap below five hours; autonomous activity never opens a burst by itself. For the headline `$` and the local-day budget figure, the producer publishes recent per-session cumulative baselines in `workspace-spending.<scope_hash>.json`, and each consumer adds only the positive difference between a live card's cumulative cost and its walked baseline. That baseline-delta overlay counts spend that has not yet flushed into the walk without importing pre-window process cost or double-counting flushed spend, while the W, M, and Y store windows, headline tokens, and session counts stay walk-derived.

The walk is read-only and sidebar-safe (no store writes), so it sits apart from the integration adapters. The parsing is mostly shared; two concerns are provider-specific and live in the adapter docs:

- **Dedup.** Claude replays parent messages into subagent files, so exact duplicates keep the richest main-thread record by token count and speed metadata before sidechain suppression. Codex copied rollouts dedup by a provider-namespaced fingerprint over the native timestamp, model, and token split. Copilot keys each shutdown or model delta by session plus native record id, with a timestamp-and-counters fallback. Pi sessions are single-file, while OpenCode's SQLite rows carry `session_id` as the native thread key.
- **Cost source.** Claude, Codex, and Copilot log token counts priced through [token pricing](#token-pricing); Copilot's shutdown output already contains reasoning, and its AI Credits stay outside USD. Pi prefers its present non-negative direct cost and prices token-bearing rows where that field is absent; OpenCode uses positive stored `cost` values and prices zero-cost token rows.

**Unknown prices.** A token-priced turn whose model misses the book still contributes tokens and sessions with zero dollars, and the file cache records the trimmed model name plus its youngest timestamp for the pricing chase. Sentinel names such as Claude's `<synthetic>` are filtered out because they are not API model ids, and unknowns older than the 365-day spend window do not chase. Once an active unknown model resolves, the file cold re-parses from byte zero, so zero-dollar entries recover their spend in the same due walk.

### The incremental cache

The read is user-scoped: the persistent shared `spending.json` cache stores a high-resolution logical source stamp beside each cursor and origin, dedups retry-write rows within each parsed chunk, and compacts finalized rows older than 8 days into per-day, per-model, per-thread rollups. The stamp is `(primary mtime seconds and nanos, primary length, optional provider companion mtime seconds and nanos and length)`. Append-only stores advance incrementally; rewind-prone stores return an authoritative replacement fold.

| Source state | Work |
| --- | --- |
| exact logical-stamp hit | served from cache |
| companion-free primary growth | parse only the appended suffix from the cursor |
| a companion appears, changes, or is removed | cold parse |
| a cold changed set | parse in a bounded worker pool |
| a new source whose newest mtime predates the widest spend window plus a skew margin | skipped without writing a cache record; dead records past that boundary are evicted |

Dirty cursor state persists on the first walk, after the five-minute minimum interval, or after cold-size parse work. The cursor also carries provider-specific resume state: Codex's cumulative-totals fold state and its learned file origin (which survives cold re-parses), Copilot's per-model shutdown baselines plus session-start directory, and Pi's session header directory. OpenCode joins the rewind-prone stores: every changed SQLite database ignores resume state, cold-folds the whole mutable table, and authoritatively replaces that file's cached entries, so in-place row completion cannot lose spend.

The long-lived walker memo retains only `(file index, entry index)` locations for dedup winners into the generation- and signature-keyed cache. Aggregation borrows paths and native thread ids from that cache, allocating legacy session strings only when materializing the published live-baseline and session-key outputs; an append, truncate, compaction, or file-set change rebuilds the locations once and stable walks reuse them.

The same walker owns a disposable directory index for discovery. Ordinary due passes stat roots plus retained active-frontier directories, reuse unchanged nodes without `read_dir`, and stop returning or restatting files after their observed mtime crosses the 365-day-plus-skew cutoff. Successful enumeration authoritatively reconciles additions and deletions, while a transient metadata or directory-read failure retains the prior subtree and leaves it due. Every 15 minutes a complete reconciliation bypasses frontier and Codex date-partition pruning, repairing coarse directory mtimes, scan races, and an in-place write below a pruned historical branch. The index rebuilds after process restart and never enters `spending.json` or the service protocol.

Two version stamps keep the cache honest across schema changes. A shape or parsed-value change to the entry split, store-time dedup, per-file origin metadata, pricing fallback, or logical source stamp bumps `SPENDING_CACHE_VERSION` (currently 22, which removes locally fabricated pricing fallbacks and cold-reprices every v21 cursor once), so finalized sessions re-parse cleanly. A semantic change to the published aggregate or guaranteed pricing that does not require a source re-parse bumps `PROVIDER_SPENDING_VERSION` (currently 12, which publishes the new per-tool window and breakdown maps), so `provider-spending.json` recomputes once from the current entry cache. Writers also refuse a schema downgrade after a cheap leading-version probe, so an older long-lived build cannot blank a newer build's published aggregate or force persistent cursor cold-walks: a version bump costs one recompute, then the higher-version cache holds.

### One walk per persistent and discovery namespace

The first host-eligible long-lived client that finds no service takes the versioned owner lock, removes any stale socket, binds mode `0600`, and hosts the service for that process lifetime. Socket and lock names include the cursor, provider, and workspace schema versions plus a digest of the persistent state root and the provider-discovery environment, and each request repeats that namespace identity for boundary validation.

The owner's single `SpendingWalker` stays warm across workspace producer churn and rehydrates once from `spending.json` after owner exit or reload. Connections are accepted concurrently: fully fresh durable publications return without the walker, one stale request takes it, and another stale request receives a busy response immediately and serves durable publications rather than queueing the caller's refresh tick. Every publishing refresh still takes the runtime `spending.lock` for mixed-build and one-shot-fallback safety, and the no-downgrade guard stays authoritative.

The full store walk runs at most once per namespace per `SPENDING_TTL`. Between due walks every room and `rimz stats` serve the shared `provider-spending.json`, and a sidebar service failure grace-serves compatible publications and retries on the next cache tick. One-shot producers never take the lifetime lock: a command that must answer uses the bounded direct walker fallback seeded from the same disk cursor cache when no service exists, while held stats uses the service and retains no walker of its own.

The cache model is unchanged by service ownership: raw rows stay exact for eight days, which covers the trailing seven-day window and cross-file retry and sidechain dedup, while days 8 through 365 stay compact day, model, and thread rollups.

During a publishing walk, `WALK_CHECKPOINT_INTERVAL` publishes a current-stamped partial aggregate and checkpoints dirty cursor progress only when the persist gate opens, so the dashboard total climbs during init and a restart resumes from the last checkpoint instead of byte zero. The final parsed-plus-compacted cursor and aggregate remain the authority, and cursor and provider write failures log warnings with their paths. A room with a missing workspace cache derives `workspace-spending.<scope_hash>.json` from the shared entry cache without taking the global walk lock ([performance.md](../performance.md#per-enrichment-cadences)).

## Token pricing

Claude and Codex log token counts, so converting their turns to dollars needs a per-model price table. Pi normally logs `costUSD` directly and uses the table only when a token-bearing record omits it, and older Claude transcripts likewise keep a present direct figure. This section owns that table: where prices come from, how a model resolves to a price, and how the table stays fresh. Pricing is enrichment, never correctness: a failed fetch, a missing snapshot, or an unknown model each degrades to stale prices when available and otherwise to zero-dollar token usage, never a hard failure.

The table lives in [`agents/pricing/`](../../../crates/rimz/src/agents/pricing/mod.rs): per-token [`Pricing`](../../../crates/rimz/src/agents/pricing/mod.rs) keyed by model in a [`PriceBook`](../../../crates/rimz/src/agents/pricing/mod.rs). A price row carries input, output, cache-create, and cache-read rates, optional long-context rates for each class, an optional request-selected tier threshold, `cache_read_explicit`, the fast-mode multiplier, and optional positive integer `max_input_tokens` capacity from compacted source metadata. Lookups are pure and network-free; the only network is the gated refresh in `load_for_spending`.

### Price table precedence

The book is assembled from two ordered passes. Each generated or runtime table is one projection: LiteLLM supplies the base catalogue, and the explicitly allowlisted official models.dev catalogues fill missing models and source fields before the table becomes visible.

| Layer | When | Source |
| --- | --- | --- |
| 1. Embedded snapshot | always, at process start | the generated compact LiteLLM-shaped snapshot, gzipped into the binary by [`build.rs`](../../../crates/rimz/build.rs). Release builds and the published crate ship the refreshed snapshot; a fresh clone with no generated snapshot embeds an empty table. |
| 2. Projected refresh | once per weekly TTL, or early during an unknown-model chase | fresh LiteLLM and models.dev documents projected together by [`source.rs`](../../../crates/rimz/src/agents/pricing/source.rs), then cached as one table that overwrites embedded rows. Provider-namespaced keys gain bare aliases; regional and gateway prices retain only their full upstream keys. The authoritative models.dev catalogues are `anthropic`, `openai`, `google`, `xai`, `zai`, `zhipuai`, `alibaba`, and `moonshotai`, in that precedence order; they fill missing models and fields, including context capacities and request-selected context tiers. |

#### Aliasing a namespaced key

A LiteLLM key under a provider namespace also lands under the bare model id, so a lookup that names the model alone still resolves: `anthropic.claude-3-5-haiku-20241022-v1:0` supplies `claude-3-5-haiku-20241022` and `claude-3-5-haiku`. Three rules keep an alias from carrying a price the model does not charge.

- An exact upstream row always beats an alias, and a date-stripped alias reads the direct dated row whenever one exists. LiteLLM files Bedrock's Anthropic catalogue under `anthropic.`, and some Bedrock rows resell at a markup — `claude-3-7-sonnet` takes Anthropic's 3.00/15.00 rather than Bedrock's 3.60/18.00.
- An alias keeps a version or date token of its own. `anthropic.claude-v2:1` spends its only version on the Bedrock revision suffix, and the bare `claude` it would leave behind is a word-boundary prefix of every Claude id, so it would price the whole family from one 2023 row and hide new models from the chase.
- Regional (`eu.`, `au.`, `apac.`, `us.`, `global.`) and gateway (`vertex_ai/`, `azure_ai/`, `openrouter/`, `bedrock/`, `baseten/`, `deepinfra/`, `vercel_ai_gateway/`) prefixes never alias, so their markups stay addressable only by the full upstream key.

#### Rates the sources leave unpublished

A missing cache rate falls back to the shared ccusage defaults — 1.25× input for a cache write, 0.1× input for a cache read — which state Anthropic's ratios. Where a provider bills differently and neither source publishes the rate, [`cache-rate-ratios.json`](../../../crates/rimz/src/agents/pricing/cache-rate-ratios.json) declares the model's own ratio against input, alongside [`fast-multiplier-overrides.json`](../../../crates/rimz/src/agents/pricing/fast-multiplier-overrides.json) for the priority multiplier. Both files hold ratios rather than prices, so upstream stays the only source of absolute numbers, and a row applies only where the source document is silent — an upstream that starts publishing the value retires the entry on its own. Two families need one today: OpenAI bills a GPT-5 through GPT-5.5 cache write as plain input (GPT-5.6 onward carries the 1.25× premium upstream), and Alibaba discounts a cached Qwen 3 coder token to a fifth of input rather than a tenth.

Every read-only price consumer uses the same current book (the embedded snapshot plus the shared `pricing-cache.json`), including local spending fallbacks, live-card transcript costs, and hook-side transcript reconciliation, so an unknown model heals across every USD surface once the chase lands its price, with no binary upgrade. Long-lived processes memoize that book by cache path, modification time, and file length, and each call still stats the cache so an atomic rewrite invalidates the memo.

### The refresh

`rimz sidebar snapshot` is a one-shot process, so the refresh is disk-cached at `$XDG_STATE_HOME/rimz/shared/pricing-cache.json` rather than held in memory. The spending producer reads the embedded snapshot plus the cache instantly while it holds the shared runtime spending lock, and re-fetches when the cache is older than a week. The cache carries a schema stamp and one projected model map, and a stale shape is dropped after a formula upgrade. Each attempt fetches both sources and replaces that map only after both documents project successfully; a partial outage retains the last complete table. A failed fetch records its attempt time and backs off an hour, so a persistent outage never re-fetches on every snapshot. `RIMZ_PRICING_OFFLINE` skips every fetch path. A source build without the generated snapshot therefore prices at zero until its first successful runtime refresh; with `RIMZ_PRICING_OFFLINE` set, that zero-dollar state is permanent.

An unknown-model chase rides the same producer walk, with no timer of its own. When the [cost-history pass](#cost-history) records a priceable model name the assembled book cannot price, the pricing cache may run the same two-source projection early on a standalone 30-minute gate. While the same unknowns persist, that gate doubles to 1h, 2h, and onward to a 24h cap; a newly seen unknown resets it to 30m. The chase also observes a 30-minute floor after any fetch attempt, so a just-refreshed source has time to catch up before RimZ asks again. Failed chase attempts escalate the same way as successful ones, and the chase state clears once every recorded unknown resolves.

`build.rs` never touches the network or projects source data: it embeds the already compacted snapshot at `crates/rimz/pricing/litellm-pricing.json` when present (or a `RIMZ_PRICING_JSON_PATH` override), then writes a gzipped copy into `OUT_DIR`, so ordinary builds stay reproducible and hermetic. The hidden `rimz pricing-refresh` helper owns fetch and projection in the runtime crate, and `cargo xtask pricing-refresh` delegates to it. `--out` names the snapshot destination; `cargo xtask pricing-refresh --check` fetches without writing instead, and fails when the LiteLLM catalogue is implausibly small, an authoritative models.dev provider disappears, a declared built-in default model loses its price, or a long-context canary loses its tier. `RIMZ_PRICING_JSON_PATH` and `RIMZ_PRICING_MODELS_DEV_JSON_PATH` substitute a local document for either fetch, and `--check` requires both so a partial override reads as the missing document rather than eight provider renames. `cargo xtask dist` runs the refresh before release builds, and the release workflow publishes the crate with the snapshot force-included through Cargo's `include` allowlist, so `cargo install rimz` embeds it too.

### Resolving a model

`PriceBook::price` resolves a model id by exact match, then a boundary-aware fuzzy scan: the longest stored key that is a word-boundary prefix of the normalized lookup wins, where normalization trims, lowercases, and maps `.` and `@` to `-`. So `claude-sonnet-4-20250514-via-bedrock` resolves to its base model. A purely numeric, non-date version bump is rejected, so a new `gpt-5`-family version is never silently priced as the old one; it falls through to its own entry or to no price. `PriceBook::exact_price` is the stricter identity boundary for Droid custom mappings, where only the trimmed canonical key can supply capacity or cost.

### Computing token-priced cost

The spending walk prices the token-only providers per turn with one shared helper. `Pricing::cost` is the per-request entry point, used by the history parsers, Codex's resumable live rollout fold, and Cursor's generation-id turn pricing; Antigravity also uses it for its replace-style current-context estimate, which stays outside live-spend aggregation. Copilot and Qwen statuslines expose only session-cumulative totals with no request boundaries, so `Pricing::session_cost` estimates them at linear base rates rather than inventing long-context tier choices from a sum.

The tier semantics differ by publisher, and the book keeps both:

- LiteLLM `*_above_200k_tokens` rows are **marginal**: input, output, 5-minute cache creation, 1-hour cache creation, and cache read each tier at the first 200,000 tokens in that class.
- OpenAI's published long-context rows are **request-selected**: when total request input exceeds that model's threshold, currently 272,000 tokens for the covered GPT flagship families, every input, cached-input, and output token in that request uses the long-context rate.

On top of that: the 5-minute cache-creation slice bills at the cache-create rate; the 1-hour slice bills at twice the input rate, including long-context tiers; and fast or priority turns multiply the finished cost by the model's fast multiplier.

Two providers need a note. **Codex** passes uncached input and output, and bills cached input at the model's cache-read rate when that rate is explicit, otherwise at the input rate, so a Codex model without a discounted cache-read rate does not discount cached tokens. **Claude** splits `message.usage.cache_creation` into 5-minute and 1-hour cache creation when present, falling back to the flat `cache_creation_input_tokens` as 5-minute creation otherwise; an older Claude turn that still logs a positive `costUSD` keeps that figure instead.

A turn whose model has no known price keeps its tokens and sessions with zero dollars while the pricing chase looks for a price.

## Per-provider mapping

Each provider maps its native account and balance surfaces onto the internal types in its own adapter doc; this table is the index.

| Provider | Account identity → [`AgentAccount`](../../../crates/rimz/src/agents/context.rs) | Balance → [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs) / [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs) |
| --- | --- | --- |
| **Claude** | [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) → plan + metered | statusline 5h/7d windows, or the idle OAuth usage probe ([claude.md](./claude.md#account-and-balance)) |
| **Codex** | app-server `planType` or `~/.codex/auth.json` | app-server `primary`/`secondary` windows + `credits.balance`, or the OAuth usage probe; reset credits come from the Codex-only OAuth reset endpoint ([codex.md](./codex.md#account-and-balance)) |
| **Antigravity** | statusline or running `agy` private local service → plan + metered | paired same-endpoint status and quota read → hashed kind-wide owner + authoritative 5h/weekly windows; credits and dollars remain unknown ([antigravity.md](./antigravity.md#account-and-balance)) |
| **Copilot** | `$COPILOT_HOME/config.json` host-safe non-secret login identity → metered | bounded internal account query → plan + named monthly `AIC`/`cht` and genuine `prm` scopes; completions, extra credits, dollars, and spend remain unsupported ([copilot.md](./copilot.md#account-and-balance)) |
| **Kimi** | typed `~/.kimi-code/credentials/kimi-code.json` shape → managed OAuth, kind-wide | fixed-host managed OAuth usage → weekly and detail windows + optional USD Booster ([kimi.md](./kimi.md#account-and-balance)) |
| **Pi** | `~/.pi/agent/auth.json` (oauth → metered, api_key → unmetered) | extension response headers + the OAuth usage probe, cached under `pi` ([pi.md](./pi.md#account-and-balance)) |
| **OpenCode** | `~/.local/share/opencode/auth.json` (oauth → metered, api_key → unmetered) | the OAuth usage probe over the active backing-provider token, cached under `opencode` ([opencode.md](./opencode.md#account-and-balance)) |
| **Qwen** | effective JSONC model/provider + credential source → Alibaba scoped, or recognized direct API unmetered | experimental fixed-host Alibaba API-key usage → scoped 5h/7d/30d display only; local transcript dollars stay separate ([qwen.md](./qwen.md#account-and-balance)) |
| **Cursor** | resolved CLI `status --format json` + `about --format json` → email, raw tier, version | no quota, paid-usage, or durable spend surface ([cursor.md](./cursor.md#account-and-balance)) |
| **Grok** | `${GROK_HOME:-~/.grok}/auth.json` non-secret metadata → session metered or API key unmetered | no quota-window query; active-branch `turn_completed` supplies native or locally priced durable USD spend ([grok.md](./grok.md#account-and-balance)) |
| **Amp** | thread and account surface ([amp.md](./amp.md#account-and-balance)) | no included-window surface |

Provider account balances reach one fusion through two channels: a **realtime** source where the provider exposes one, and a **direct account-usage query** where the provider exposes one. Claude, Codex, Copilot, Pi, OpenCode, Kimi, and Qwen Alibaba use provider credentials for the direct query; Antigravity pairs identity and quota through only the verified loopback service of an already-running `agy` and reads no credential. The producer runs a supported query for each metered, logged-in provider on its own cadence, even while a live session is active, and Antigravity, Copilot, OpenCode, Kimi, and Qwen Alibaba meter through that channel alone. Fusion is keyed by kind, account scope, and stable window identity, so a scoped reading paints only its matching account and each named or temporal quota paints its own bar. Copilot's authoritative scopes deliberately omit duration, keeping its monthly allowance out of 5h/7d policy.

The realtime leg is the per-kind difference. Codex reads its app-server first because it is the provider's local read-only account surface; Claude uses statusline windows when a session is alive; Pi reads extension response headers; Pi and OpenCode select their active backing-provider OAuth token from `auth.json` and delegate to the Claude or Codex usage fetcher; Copilot selects its native environment or config credential and fixed host-derived account endpoint; Antigravity's direct probe discovers the verified running service once and binds status and quota to one endpoint. Claude account normalization owns both fixed named durations and shared clamping, while Codex and OpenAI normalization own dynamic durations, plan and credit cleanup, ordering, and cold-cache lifted 5h completion. Sidebar fusion completes only durations already present in same-scope persisted truth.

## Adding a provider

A new agent earns an account block and balance bars by filling the internal types from its own surfaces. Everything downstream (aggregation, window fusion, caching, the dashboard, the pricing table) is provider-agnostic and comes free. The work mirrors [adapter.md → Adding an agent](./adapter.md#adding-an-agent):

1. **Fill `AgentAccount`** (plan plus metered) on the session's `AgentContext` from its rich-context transport, and/or override `probe_account` for the logged-in-but-idle case.
2. **Fill `AgentRateLimits`** on `AgentContext` from the transport: each window with a `used_percentage`, a reset instant, and either `duration_mins` or a stable scope id plus compact label.
3. **Optionally fill `ExtraCredits`** from a read-only provider account-usage surface. Use `Disabled` only when the provider explicitly says paid extra usage is off.
4. **Set** the spec's color and name, and optionally add curated art to the embedded [`emblems.toml`](../../../crates/rimz/src/agents/emblems.toml) catalog; kinds without catalog art use the shared fallback, and `[theme.providers.<kind>]` remains a per-machine override.
5. **Stay best-effort** throughout: a missing fact is an omitted label, a `v?` placeholder, or an unknown budget track, never an error.

Golden the account mapping from a fixture probe payload and a fixture transport payload, including the logged-out and unparseable cases; the inline goldens in each adapter's `account.rs` are the model. Golden the spend parser from a fixture JSONL, including the dedup and zero or negative cost cases.

## See also

- [model.md](./model.md) — the agent model the account facts ride on, and the displayed-status ladder a park feeds.
- [adapter.md](./adapter.md) — the capability seam behind `probe_account`, `probe_account_usage`, and `parse_spend`.
- [`rimz providers`](../../reference/cli/providers.md) — the standalone CLI query for account status, windows, credits, published spend, and daily caps.
- [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard) — the mana bars, the `ex` and `api` rows, the aligned grid, and the exhausted-window rendering.
- [sidebar.md](../sidebar/sidebar.md#provider-dashboard) — the renderer's projection of `providers` and where the dashboard sits.
- [budget.md](../../guide/budget.md) — the user-facing four budget scopes and what a park means.
