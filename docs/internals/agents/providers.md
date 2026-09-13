# Provider accounts and balances

A coding agent runs against a provider account: a login, on a plan, that may or may not be metered. Its balance has two tiers: included subscription windows that refill on their own clocks, and paid usage beyond them. This page owns how RimZ learns those facts, fuses them across sessions and sources, caches them across rooms, and acts on them (parking, auto-redeem, auto-continue). Transcript spend and the token price table are [spending.md](./spending.md).

The facts flow from sources, through shared caches, into one provider panel per kind:

```text
sources                                    user-scoped caches
  a live session's rich context ─────── (rides the rollup; model.md)
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

Everything on this path is enrichment. A missing binary, a logged-out account, or an unreachable API degrades to an omitted plan label, a `v?` version placeholder, or an unknown budget track. Two consumers treat the facts as control inputs: [parked turns](#spent-windows-and-paused-rows) read fused windows, and a fresh managed Qwen launch may gate on an exact account binding ([Account scope](#account-scope)). Neither turns a quota reading into a provider billing statement. [Daily dollar caps](#daily-dollar-caps) decide on transcript spend, never on a provider probe.

The native surfaces each provider reads are in its adapter page ([Per-provider mapping](#per-provider-mapping)); the raw endpoints are in the [upstream references](../../externals/agent-adapter/claude-reference.md#auth-surface). The dashboard on screen is [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard).

## The model

Four account-scoped facts feed a panel. Account identity and included balance ride the session's [`AgentContext`](../../../crates/rimz/src/agents/context.rs) ([model.md](./model.md#rich-context)) or the out-of-band probe; the producer lifts them to the account at aggregation time. Paid usage and reset credits ride the shared `credits.json` cache.

| Fact | Type | Carries |
| --- | --- | --- |
| Account identity | [`AgentAccount`](../../../crates/rimz/src/agents/context.rs) | the non-secret `account_id`, the raw `plan` tier (`max`, `team`, `pro`), `metered`, the typed account scope, an optional `sub_provider` id, the probed version, and the credential file's mtime |
| Included balance | [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs) | ordered [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs)s, each with `used_percentage`, a typed `resets_at`, `observed_at`, a `source`, and either a `duration_mins` or a provider scope (stable id plus compact label) |
| Paid usage | [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs) | used USD, remaining USD, a limit, or a disabled state, each optional |
| Reset credits | [`ResetCredits`](../../../crates/rimz/src/agents/credits.rs) | Codex only: the available count, every known valid expiry, and the earliest expiry |

A window's identity is its duration or its scope id. Duration windows sort short to long (`5h`, `7d`) and support reset roll-forward, not-started detection, pace, and surplus; a window with an unknown duration makes each of those claims fail closed. Named provider windows (Copilot's monthly `AIC`, Qwen's Alibaba windows) fuse by scope id, display their real reset, and never roll forward.

Paid usage keeps missing fields missing, so the renderer can show an unknown or uncapped `ex` row without inventing a cap. Disabled or exhausted paid usage does not make a parked turn terminal while a subscription window still has a future reset. Reset credits color the compact dashboard glyph from the count and earliest expiry, blinking while a spent duration window makes a manual redemption useful; `rimz providers` lists each deadline.

### Metered and unmetered

Metering decides which balance row a panel paints. A subscription or ChatGPT login is metered: it draws on rate-limit windows, painted as draining bars. An API-key login is unmetered: it has no included windows, so the panel paints one `api` row from trailing-month transcript spend against an optional display ceiling. `metered: None` is unknown, and aggregation infers it ([Producer aggregation](#producer-aggregation)).

### Account scope

An account scope ([`ProviderAccountScope`](../../../crates/rimz/src/agents/context.rs)) says which sessions an enrichment belongs to. `KindWide` covers every session of one provider kind. `SubProvider` names a canonical provider plus a variant, such as Alibaba International or China, so a multi-provider client displays only its effective account's enrichment. Qwen sets a user-facing `sub_provider` label for these (for example "Alibaba Coding Plan (International)"); Pi and OpenCode store the raw backing-provider id.

Control needs more than a scope: a managed launch must also match the opaque credential account key exactly. Generic launch and schedule code carry one typed state:

| State | Means |
| --- | --- |
| `PendingResolution` | final inputs have not arrived yet |
| `Unsupported` | ordinary kind-wide capacity applies |
| `Unresolved` | exact selection applies but identity is unproven, so no capacity is read |
| `Bound` | only matching exact-account quota and cache reads are permitted |

Qwen is the one `Bound` user. Its Alibaba windows carry the credential account key from their authoritative read, and a fresh managed Qwen launch or loop fire requires exact scope-and-key equality; an absent reading stays non-blocking. The binding authorizes only that fresh launch decision. Durable Qwen sessions carry no provider identity, so the scoped windows stay out of displayed session rate-limit status, message resume, and auto-continue.

### Logins

A provider home is an account, and every probe, claim, and cache entry on this page is keyed by its `LoginKey` (`claude@work`, `codex@default`) ([`agents/login.rs`](../../../crates/rimz/src/agents/login.rs)). Claude and Codex can declare named accounts (`[accounts.claude.<name>]`), each a standalone home selected by `CLAUDE_CONFIG_DIR` or `CODEX_HOME`; `default` is the native home, and every other kind has only `default`. A room runs each kind under one login fixed at birth, runs its probes under that login's environment, and reads and writes only its own logins' cache entries. Panels stay one per kind, and a non-default login labels its block `Claude · work`. `rimz reset --account` starts a new room on another account; a live room never switches.

Every credential path is read-only. RimZ does not refresh tokens, write provider auth files, or import browser cookies.

## Two origins

A kind's account and included balance reach the panel two ways, mirroring the [context read path](./adapter.md#context-sources):

1. **A live session's rich context.** The statusline or app-server transport carries account and window readings, so any live session fills both at no extra cost.
2. **The out-of-band probe.** A provider that is logged in with no live session this run is probed directly, so its account stays visible between turns.

Where both exist, the live session's account wins because it is current. Windows do not pick a winner: live and authoritative readings fuse ([Window fusion](#window-fusion)).

## The out-of-band probe

[`probe_account`](../../../crates/rimz/src/agents/capabilities.rs) returns an [`AccountProbe`](../../../crates/rimz/src/agents/account.rs), and the arm sets how long the producer trusts it ([`sidebar/timing.rs`](../../../crates/rimz/src/sidebar/timing.rs)):

| Arm | Means | Cached for |
| --- | --- | --- |
| `Found(AgentAccount)` | a resolved login | `ACCOUNTS_TTL` (10 minutes) |
| `LoggedOut` | the probe ran and found no login | `ACCOUNTS_TTL`, since login state rarely changes |
| `Unavailable` | the probe could not complete: a binary that would not run, a non-zero exit, an unreadable file | `ACCOUNTS_RETRY_TTL` (10 seconds); the provider keeps its last-known facts and retries alone |

An adapter with no out-of-band login surface returns `LoggedOut`. The probe itself is a pure read; memoization lives in the producer's `accounts.json` publication. Each provider's mechanics (Claude's `claude auth status`, Codex's and Pi's auth-file read, Antigravity's verified loopback service) are in its adapter page.

Every registered adapter also gets a display-only version probe. It locates the declared executable, runs `<binary> --version`, and hands stdout and stderr to the adapter to normalize. The default parser accepts only a conventional numeric version and abstains on banners; Copilot, Amp, and Cursor recognize their branded build strings. Antigravity disables the probe because invoking `agy --version` is unsafe for idle enrichment, and a manifest plugin uses its configured probe command and parser. Account and version subprocesses share a 3-second deadline (`INFORMATIONAL_PROBE_TIMEOUT`), and a timeout counts as `Unavailable`. A live context's version wins over the probe's. A provider whose version is absent re-probes on the retry TTL, never per frame.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../../crates/rimz/src/store/snapshot/view/providers.rs) folds sessions, probed accounts, and spend into one [`SidebarProviderPanel`](../../../crates/rimz/src/store/snapshot/view.rs) per kind. It runs only in the producer, because it needs per-machine config and probe results the pure reducer cannot read; the reducer leaves `providers` empty, and every consumer reads the producer's published panels ([state.md](../sidebar/state.md#projections)).

A kind earns a panel when it has a live session, or when its probed account is metered, has a non-empty `account_id`, or has recorded spend in the past year. Spend alone never creates a panel for a provider with no probed account, and a credentials-only unmetered account stays hidden until its first recorded session. An agent launched through an elevation wrapper as another uid stays out of every panel: its credentials live under that user's home, so the sidebar shows it as a flagged process row.

Each panel field has one source:

| Field | Source |
| --- | --- |
| `plan`, `version` | the session context with the newest `observed_at`, falling back to the probed account; `format_plan_label` brands the raw tier (`max` becomes `Claude Max`, `pro` becomes `ChatGPT Pro`, other kinds title-case it) |
| `metered`, `account_scope` | the same account; a missing `metered` is inferred true when live windows exist or a keyed session owns the panel |
| `account_key` | the newest-registered keyed session, by `registered_at` then session id |
| `windows` | `fresh_windows` over the admitted sessions, then fused with cache ([Window fusion](#window-fusion)); cleared on an unmetered panel |
| `window_placeholders` | the spec's `expected_windows`, painted as empty tracks before the first reading |
| `extra_credits`, `reset_credits` | the `credits.json` entry, or a local API spend projection |
| `spending` | the kind's [`SpendTally`](./spending.md#what-reads-the-totals) from `provider-spending.json` |
| brand art and color | `AgentSpec` color and name, the embedded emblem catalog, and `[theme.providers.<kind>]` overrides; an unknown kind gets neutral grey ([theme.md](../../guide/theme.md#provider-styling)) |

A color override keeps the catalog's tint runs, while an `ascii_art` override paints its art in the single brand color.

### Which sessions speak for a panel

Live windows are partitioned by [birth account key](./model.md#the-rollup) before fusion, so a session registered under another login contributes no usage to the account the panel shows. The panel admits every session carrying its `account_key` plus every keyless session, and excludes sessions with a different key. A panel whose sessions are all keyless stays keyless and excludes nothing. `plan` and `version` ignore this partition and come from the freshest context. The keyed `metered` inference keeps cached authoritative windows attached while a newly registered session has not reported its first window.

### Paid and API rows

A metered panel takes `ExtraCredits` from its `credits.json` entry. An unmetered panel synthesizes `ExtraCredits::Known` from the kind's trailing-month transcript spend. Either way, when the provider reported no cap, the display ceiling from `[accounts.usage_limit_usd]` (a map such as `claude = 50.0`) applies: a capped row drains by the spend and shows remaining dollars, and an uncapped API key paints a full `∞` bar. The ceiling is display only; no provider enforces it.

A credits entry older than `CREDITS_DISPLAY_MAX_AGE` (24 hours), absent, or scope-mismatched leaves the `ex` row unknown (an `∞` value over a dim empty track). API spend projections are never persisted, since the spending walk and config rebuild them.

### Panel order and the cap

With no explicit `provider_list`, a usage rank orders panels: a live room session first, then trailing-week, month, and year session counts, then a qualifying probed account, then the credential file's mtime, with registry order breaking ties. The mtime comes only from file-based probes. The rank decides both paint order and which panels survive the stacked dashboard's `max_provider_blocks` cap (default 3). A tabbed dashboard is bounded by its active block and shows every provider; its tab rail keeps the highest-ranked tabs that fit and always keeps the active one. An explicit `provider_list` sets the set and order and bypasses the cap, and `"all"` expands the remaining providers in rank order ([theme.md](../../guide/theme.md#display)).

### The shared caches

The account, rate-limit, and credits caches live under `$XDG_STATE_HOME/rimz/shared/`, with single-flight locks under `$XDG_RUNTIME_DIR/rimz/shared/`.

| Cache | Keyed by | Holds |
| --- | --- | --- |
| `accounts.json` | `logins` | probed `AgentAccount` per login |
| `rate_limits.json` | `entries` (login) | fused windows, account scope, bound account key and its authoritative copy, pending refills, the unknown-episode marker; schema gated by `RATE_LIMITS_CACHE_VERSION` |
| `credits.json` | `logins` | paid usage, Codex plan and reset credits, the usage owner identity, and the direct-query claim |

The elected producer publishes all three; consumers read them and never fork. A publication replaces only the probing room's login keys. A cache in an older or kind-keyed shape cold-drops, because every value is rebuildable.

A due account batch runs account-then-version probe chains on up to four workers (`MAX_PARALLEL_ACCOUNT_PROBES`) and joins them into one atomic `accounts.json` publication. A caller that cannot open the coordination lock probes locally without publishing. A caller that times out behind a live producer serves the current cache for that frame, so a fresh room does not repeat the cold subprocess wave.

## Window fusion

A window is account-scoped, so its displayed value fuses every reading of it: across parallel sessions, across live transports and direct queries, and across time. Each [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs) records its provenance: `observed_at`, and a `source` of `BestEffort` (Claude's statusline) or `Authoritative` (a direct account-usage query: Codex's app-server, a provider OAuth read, Antigravity's verified local service). Fusion runs in two stages.

### Live readings, per frame

[`fresh_windows`](../../../crates/rimz/src/store/snapshot/view/providers.rs) reduces the admitted sessions' readings to one live candidate per window identity.

1. A reading is rejected whole when it is content-stale (`AgentRateLimits::content_stale_at`): its shortest duration window's reset has passed, or, with no dated duration window, its earliest dated scoped reset has. An idle session re-runs its statusline and stamps a fresh `observed_at` over a days-old payload, so `observed_at` alone cannot tell live from stale.
2. Within a surviving reading, a window with no `used_percentage` or with a passed reset is dropped.
3. Among the rest, the highest `used_percentage` wins per identity. Usage only climbs within a live window, so the most-drained reading is the most current, and the pick is stable against sessions reporting at slightly different instants.

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
| `BestEffort` drop to 25% or below | park a pending refill; adopt once it has stood 120 seconds (`REFILL_CONFIRM_SECS`) |

An unstamped live reading cannot prove it is current; an unstamped prior yields to any stamped reading. Only the producer confirms a refill: a consumer holds the producer's persisted value.

The confirm path exists for the mid-window free reset a provider sometimes grants, which refills usage without moving the reset timer. One garbled statusline sample cannot dip a bar, and a genuine refill still surfaces after two minutes. One disagreement stays unresolved: a live session that keeps reporting a fresh value contradicting the API alternates with each direct query, and the authoritative value is never more than one read cadence away.

`RIMZ_RATE_LIMIT_TRACE` (off by default; `1` writes `rate_limits_trace.jsonl` under the runtime shared root, any other value names the path) appends each frame's live candidate (`used_percentage`, `resets_at`, `observed_at`, `source`) and the fused result, for tuning the confirm window against real resets.

Fused kind-wide windows feed provider-limit display and [parked-turn](#spent-windows-and-paused-rows) decisions. Per-session `context.rate_limits` feeds fusion and the agent card, never a decision by itself.

### Not-started windows

Included windows are sliding: the clock starts on the first token, and until then the provider keeps `resets_at` a full window-length ahead. A window has not started when its reset sits about a full window out and its usage is at or below the 1% floor (`FRESH_WINDOW_USAGE_FLOOR`); a fresh Codex 5h window reports 1% used, so a zero check would miss it. Any usage above the floor means the window has started, and its reset is a real countdown.

The dashboard draws a not-started window as a full bar with no `↻` countdown, which reads as ready to start ([interface](../../interface/sidebar.md#zone-3--the-provider-dashboard)). This is display only.

### Persistence across idle sessions

The producer mirrors each fused window and its account scope into `rate_limits.json` (atomic write under a shared read-modify-write lock) and reads it back when no live session reports. A scope change reaps the prior entry. Read-time projection decides what an entry shows:

| Situation | The panel shows |
| --- | --- |
| before the window's reset | the last fused reading |
| a shorter duration window reset while the longest cached duration window is still future | 0% used, reset set to now plus one window length, until a live reading overwrites it |
| a named window's reset passed | unknown, per window; named windows never roll forward |
| the freshness ceiling (the longest dated window's reset) passed with no fresh reading | every bar of the provider as an unknown empty track |
| every window undated, and the newest observation older than the shortest reported duration | every bar of the provider as an unknown empty track |

Scoped quota values stay intact after their reset for status decisions; only the display projection clears them. Projected full and unknown values are never written back.

An all-unknown panel is also a refresh trigger. When a kind's panel holds no usable value (an aged-out ceiling, expired named quotas, or a cold start with no cache), the producer forces that kind's direct query instead of waiting out the cadence. The entry's `unknown_since_ms` marker makes the force fire once per unknown episode; the usage claim keeps the fetch single-flight, and completion restamps the read on success and failure alike, so a provider that stays unreachable falls back to ordinary cadence. A usable window clears the marker.

A panel meets its cached entry under the same rule as [session admission](#which-sessions-speak-for-a-panel): the entry's bound key and the panel's key must be equal, or one must be absent. A keyed mismatch clears the live windows for that frame, so the cached account's truth paints instead of fusing with another account's usage. Only an authoritative account read binds an entry's key, and it fuses onto a prior entry only when scope and key both match, so an account switch writes a fresh entry. A keyed entry also retains its matching authoritative windows separately for `Bound` launch controls. The producer's own publication never binds a key: it republishes fused truth under the prior entry's key, keeping that entry's bound copy. When the kind's panel disappears, the room's login entry is dropped; other logins' entries are untouched.

### The credits cache and usage identity

`credits.json` has the same login keying and lock discipline as `rate_limits.json`. It persists provider-reported paid usage, plus Codex plan and reset-credit fields, so partial observations survive idle sessions. Reset credits keep every known valid expiry in ascending order, equal deadlines included; the earliest is the compact summary, and the provider count stays authoritative when detail is absent or malformed.

Identity keeps the cache honest across account changes. Every [`AccountUsageProbe`](../../../crates/rimz/src/agents/credits.rs) result (found, no credentials, or failed) carries one `AccountUsageIdentity`: the non-secret owner and scope of the credentials read. `AccountUsageSnapshot` holds only the normalized plan, windows, paid credits, and reset credits. Pi and OpenCode select their delegated owner once through [`delegated_account.rs`](../../../crates/rimz/src/agents/delegated_account.rs), preferring an OpenAI account id and otherwise hashing the refresh or access token under an adapter-specific domain.

Completion compares owners symmetrically. `None` to `Some`, `Some` to `None`, two different identified owners, or two different scopes each block carrying the prior plan, paid usage, and reset credits, and drop the kind's cached windows. A failed read with no known owner change keeps the prior display data; a failed read that proves a new owner drops prior truth without publishing unverified values.

## Refresh cadences

The producer keeps each metered account current between turns when its spec declares `direct_account_usage`, whether or not a session is live. It spawns one hidden `rimz agents refresh-usage` helper per login, and only after `credits.json` grants that login a durable direct-query claim. The account fold admits helpers from the panels it just built, so a newly discovered idle login starts its first read in the same heavy pass.

The claim is what makes the helper safe across rooms. Under the shared credits lock, scheduling derives the claim from the published account scope and credential stamp plus the prior same-scope usage owner, and records a UUID nonce, claim time, requested scope, and the optional stamp and owner. The helper receives the nonce and the `LoginKey` in its request. Before any provider call it re-reads the room's login selection and cancels the claim if the room no longer runs that account. Only the worker holding the matching nonce resolves credentials, contacts the provider, and publishes. Because the claim and `oauth_read_at_ms` share one lock, rooms admit one fetch; a failed spawn cancels its claim, and an expired claim becomes retryable.

| Clock | Value | Governs |
| --- | --- | --- |
| `OAUTH_USAGE_TTL` | 5 minutes | the ordinary refresh |
| `OAUTH_USAGE_SETTLED_TTL` | 1 hour | a login whose last read settled as an auth failure (missing or rejected credentials) and whose credential stamp is unchanged |
| `ACCOUNT_USAGE_CLAIM_TTL` | 90 seconds | the lease on each helper segment |

A changed published scope, credential stamp, or account key reopens the claim at once. `RIMZ_OAUTH_USAGE_OFFLINE` disables account-usage fetches for the process tree.

The helper runs two segments. First it folds a realtime account reading when the adapter has one (Codex's app-server) and publishes it to the rate-limit cache, then renews the claim. Then it runs the direct query and publishes again. A missing, replaced, or lock-contended claim cannot renew, and the direct segment does not start; completion re-checks the nonce so a superseded writer is rejected. Both segments publish an `AccountUsageSnapshot`: windows go through `fuse_window`, and plan, paid usage, and reset credits go through one cache conversion. Direct-query windows are authoritative and merge after the realtime fold, so a fresh credential read replaces a stale realtime process. A detached writer waits on the bounded rate-cache lock and publishes before returning; the producer's per-frame path falls back to a non-blocking read under contention.

Fusion can pull the next read forward. When the producer persists a new reset epoch, parks a new pending refill, or first shows an unknown panel, it clears `oauth_read_at_ms`, the settled state, and any stale claim, so scheduling in the same pass re-probes. An already pending refill does not force another read; a successful authoritative response clears it, so a later contradictory low reading can park and request again.

Direct account-usage HTTP ([`agents/credits.rs`](../../../crates/rimz/src/agents/credits.rs)) retries transport failures, body-read failures, and 5xx responses up to three attempts with a 300 ms backoff. Redirects are disabled, 401 and 403 both count as authentication rejection, and 429 or any other status returns without retry. Errors never include response bodies or request headers. A surfaced failure reports off-box under the one `oauth_usage` operation with a `provider` tag, and the error detail names the request host ([diagnostics.md](../diagnostics.md)).

## Spent windows and paused rows

A window is spent at `used_percentage == 100` while its reset is still ahead. A spent window paints the budget bars; it does not park every agent of the kind.

A row becomes `paused` only when that agent stopped mid-turn on a limit or a transient server error. A native turn-error certificate (`rate_limit`, `spend_limit`, or `overloaded`) parks the running agent it names. A stalled running agent with no certificate parks when the fused kind window is spent and unreset. `overloaded` covers provider overload and transient 5xx errors, and has no reset clock.

The stall fallback reads only account-wide windows. It excludes scoped windows that carry a parent duration, so a model sub-cap (a spent Fable share) never parks another Claude model. Durationless named quotas keep their spent-and-reset behaviour, including promoting a limit marker to `failed` after the quota resets with no spent window left to explain it.

A `rate_limit` or `spend_limit` pause stays resumable while the fused account has a subscription window with a future reset, including the common spend-limit case where paid usage is disabled or exhausted but the window will refill. After the window recovers, the row stays parked while the [auto-continue](#auto-continue) record has a chance to wake the turn, instead of a frozen 100% reading turning into a spurious `!`.

Calm agents (`idle`, `success`) and progressing turns keep their lifecycle status even when a bar reads empty. The displayed-status ladder is [model.md](./model.md#displayed-status), and the glyphs are [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

Quota readings and locally priced dollars are best-effort control inputs. RimZ parks and resumes local panes against them and enforces user-configured soft caps, but neither is a provider billing statement or a provider-enforced limit.

### Auto-redeem

Auto-redeem spends a Codex reset credit when doing so buys capacity; the policy is [`harness/auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs). The producer evaluates the cached windows and credits each frame and returns the first verdict that matches ([`redeem_verdict`](../../../crates/rimz/src/harness/auto_redeem.rs)):

| Verdict | Fires when |
| --- | --- |
| `ExpiryRescue` | a credit is within 30 minutes of expiry (always on, since an unused credit vanishes) |
| `DoomedCredit` | `[resume] auto_redeem = true`, a duration window is spent, and the soonest credit would keep less than 24 hours of life after that window's latest reset |
| `BlockedGain` | `auto_redeem = true`, a duration window is spent, and its latest natural reset is at least `auto_redeem_min_gain` away (default `12h`) |
| `ScheduledRedeem` | `auto_redeem = true` and the chain deadline below has passed, even at low usage |

A missing reset disables the spent-window clauses; a missing expiry disables only the doomed-credit and rescue clauses.

The schedule spaces credits by predicted refill time. The producer samples growth in the longest duration window into the user-shared `auto_redeem_rate.codex@<account>.json`, and a three-day half-life EWMA predicts how long a fresh window takes to fill. The sorted expiry list yields backward chain deadlines spaced by that refill time. A deadline is never earlier than one predicted refill after the projected longest window began, so a redemption or a natural reset pushes the next attempt out instead of spending more credits into a fresh window. A natural reset less than `auto_redeem_min_gain` away defers the deadline when the first credit still outlives it by 24 hours. Missing window timing or a negligible burn rate collapses scheduling to the 30-minute rescue.

For any verdict, the elected producer spawns a detached helper under the room's Codex login. The helper refuses if the room no longer runs that account, takes the user-shared `auto_redeem.codex@<account>.lock`, refreshes windows and per-credit details, re-evaluates the verdict, and consumes the soonest-expiring credit with an idempotency key. The user-shared `auto_redeem.codex@<account>.json` stamp throttles attempts to one per 10 minutes and holds 30 minutes after a success, across every room. Each attempted consume appends its evidence and outcome to the [assist log](../harness/loops.md#the-assist-log). A success republishes authoritative usage and reset credits at once, so [auto-continue](#auto-continue) can wake parked turns on the recovered capacity.

### Auto-continue

With `[resume] auto_continue = true` (off by default), the producer resumes a [parked turn](#spent-windows-and-paused-rows) by delivering the configured nudge (`continue` by default) to the agent's pane through the same send path as `message --steer`; the agent's next hook moves the row back to `running`. The trigger and the `rimz agents auto-continue` helper that sends are [`harness/auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), and the pure arm decision is `resume_park`.

A spent window supplies a reset clock only after the turn carries a provider-certified rate-limit marker; account exhaustion, a stall, or message text never certify why a turn stopped. Antigravity has identity-bearing quota clocks but no certified recoverable stop class, so its quota never arms auto-continue.

**Arm.** Each frame an agent is parked on a resumable certificate, the producer writes a durable `ParkRecord` with the park class and the agent's frozen `last_activity`. A rate-limit record captures the spent window's reset deadline, so the resume survives the session sidecar it was seen through: a 5h or 7d window can outlive the session, and the post-reset reading gives the record one frame to nudge. A backoff record carries the turn-error marker time and retry state. A RimZ [budget park](../harness/budget.md#the-park) with a reset time is checked first and short-circuits the provider classification: it arms a `Budget` record whose deadline is the park's `resets_at`. If the record is lost after the window recovers while the agent still carries a limit marker, the producer re-arms a due-now record before firing.

**Fire.** Once the reset deadline or backoff step is due and `last_activity` has not advanced, the producer spawns the helper, which queues and delivers a resume-gated message. Overload and transient API-error parks follow `auto_continue_backoff_secs` (default `[180, 300]`), whose last value repeats: the first attempt lands after 3 minutes and later ones every 5. Message events carry the queued, sent, delivered, timed-out, or failed trace, and after delivery the [assist log](../harness/loops.md#the-assist-log) keeps the park time, verdict, handle, and message id.

**Clear.** Any activity since the park advances `last_activity` and removes the record, as does a delivered resume message.

**Exhaust.** All park classes share `auto_continue_max_retries` (default 12), counted from evidenced `DeliveryGate::Resume` messages since the park; helper spawns and pre-queue crashes only pace retries. At the default ramp, attempts span about 58 minutes before the row promotes to actionable `failed`.

## Daily dollar caps

`[accounts.budget] <kind> = "100/day"` sets one cap per account of that kind, shared by every room on that account. A kind is eligible only when its adapter's `AccountSpend` concern is wired to authoritative account-level dollar history. Identity, a plan label, quota windows, point-in-time prices, and partial transcript estimates do not qualify. Config parsing, room start, and `rimz budget --account` reject an ineligible kind with the exact key to remove; the ledger ignores stale unsupported config.

The decision input is the local-day spend window the [spending walk](./spending.md#what-reads-the-totals) publishes per account, which the producer accepts at the cache's normal staleness. The ledger, verdict, park, and waiver are [budget.md](../harness/budget.md). On the dashboard, a healthy cap stays quiet; while agents are parked on a crossed account cap, the headline turns alarm-red and appends `$used of $cap/day`.

## Per-provider mapping

Each adapter page maps its native surfaces onto these types; this table is the index.

| Provider | Account identity → `AgentAccount` | Balance → `AgentRateLimits` / `ExtraCredits` |
| --- | --- | --- |
| Claude | [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) → plan, metered | statusline 5h/7d windows, and the OAuth usage query ([adapter_claude.md](./adapter_claude.md#account-and-balance)) |
| Codex | app-server `planType`, or `$CODEX_HOME/auth.json` | app-server windows and credits, then the OAuth usage query with reset credits ([adapter_codex.md](./adapter_codex.md#account-and-balance)) |
| Antigravity | statusline, or the running `agy` local service → plan, metered | paired status and quota read on one endpoint → hashed kind-wide owner, authoritative 5h and weekly windows; credits and dollars unknown ([adapter_antigravity.md](./adapter_antigravity.md#account-and-balance)) |
| Copilot | `$COPILOT_HOME/config.json` non-secret login → metered | internal account query → plan, named monthly `AIC`, `cht`, and `prm` scopes; no paid usage or spend ([adapter_copilot.md](./adapter_copilot.md#account-and-balance)) |
| Kimi | `~/.kimi-code/credentials/kimi-code.json` → managed OAuth, kind-wide | managed OAuth usage → weekly and detail windows, optional USD Booster ([adapter_kimi.md](./adapter_kimi.md#account-and-balance)) |
| Pi | `~/.pi/agent/auth.json` (oauth → metered, api_key → unmetered) | extension response headers, and the OAuth usage query over the backing token ([adapter_pi.md](./adapter_pi.md#account-and-balance)) |
| OpenCode | `~/.local/share/opencode/auth.json` (oauth → metered, api_key → unmetered) | the OAuth usage query over the backing token ([adapter_opencode.md](./adapter_opencode.md#account-and-balance)) |
| Qwen | effective JSONC model and provider plus credential source → Alibaba scoped, or direct API unmetered | Alibaba API-key usage → scoped 5h, 7d, and 30d windows, display and `Bound` launch only ([adapter_qwen.md](./adapter_qwen.md#account-and-balance)) |
| Cursor | CLI `status --format json` and `about --format json` → email, tier, version | none ([adapter_cursor.md](./adapter_cursor.md#account-and-balance)) |
| Grok | `${GROK_HOME:-~/.grok}/auth.json` metadata → session metered, API key unmetered | none; spend comes from `turn_completed` ([adapter_grok.md](./adapter_grok.md#account-and-balance)) |
| Amp | thread and account surface ([adapter_amp.md](./adapter_amp.md#account-and-balance)) | none |

Claude, Codex, Copilot, Pi, OpenCode, Kimi, and Qwen declare `direct_account_usage` and read provider credentials for it; Antigravity reads no credential and pairs identity and quota through the verified service of an already-running `agy`. Antigravity, Copilot, OpenCode, Kimi, and Qwen have no realtime leg, so the direct query is their only balance source. Pi and OpenCode delegate to the Claude or Codex usage fetcher for their backing token. Copilot's scopes omit a duration on purpose, keeping its monthly allowance out of 5h and 7d policy.

Normalization belongs to the adapter. Claude's owns its fixed named durations and clamping; Codex's and OpenAI's own dynamic durations, plan and credit cleanup, ordering, and the lifted empty 5h row on a cold cache. Sidebar fusion completes only durations already present in same-scope persisted truth.

## Adding a provider

A new agent gets an account block and balance bars by filling these types from its own surfaces; aggregation, fusion, caching, and the dashboard need no change. The steps extend [adapter.md → Adding an agent](./adapter.md#adding-an-agent):

1. Fill `AgentAccount` on the session's `AgentContext` from the transport, override `probe_account` for the idle case, or both.
2. Fill `AgentRateLimits` from the transport: each window with `used_percentage`, a reset instant, and either `duration_mins` or a stable scope id and label.
3. Where a read-only account-usage surface exists, implement `probe_account_usage`, declare `direct_account_usage`, and fill `ExtraCredits`. Use `Disabled` only when the provider says paid usage is off.
4. Set the spec's color and name, and optionally add art to [`emblems.toml`](../../../crates/rimz/src/agents/emblems.toml).
5. Implement the transcript spend parser ([spending.md](./spending.md#per-provider-parsing)).
6. Keep every step best-effort: a missing fact is an omitted label, a `v?`, or an unknown track, never an error.

Golden the account mapping from fixture probe and transport payloads, including logged-out and unparseable cases; each adapter's `account.rs` goldens are the model. Golden the spend parser from a fixture transcript, including dedup and zero or negative cost.

## See also

- [spending.md](./spending.md): transcript spend, the spending caches, and token pricing.
- [model.md](./model.md): the rollup the account facts ride on, and the displayed-status ladder a park feeds.
- [adapter.md](./adapter.md): the capability seam behind `probe_account`, `probe_account_usage`, and `parse_spend`.
- [`rimz providers`](../../reference/cli/providers.md): the CLI query for account status, windows, credits, spend, and daily caps.
- [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard): bars, the `ex` and `api` rows, and exhausted-window rendering.
- [sidebar.md](../sidebar/sidebar.md#provider-dashboard): where the dashboard sits in the renderer.
- [budget.md](../harness/budget.md): dollar-cap ledgers, verdicts, and waivers.
