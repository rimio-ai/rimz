# Provider accounts, balances, and spend

> See [DESIGN.md → Triage at a glance](../../../DESIGN.md#triage-at-a-glance) for the account-scoped-budget invariant this doc operationalizes, [agent.md → Rich context](./model.md#rich-context-agentcontext) for how the live-session rich context this doc interprets is stored on the rollup, and [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard) for what the provider dashboard looks like on screen.

A coding agent runs against a **provider account** — a login, on a plan, that may or may not be metered — and that account has a two-tier **balance**: included subscription windows that refill on their clocks, plus paid extra/API usage the provider or the local spend store can name.

This doc owns the account, balance, spend, and pricing model end to end: what the metered/unmetered/plan facts mean, how the producer folds them into the provider dashboard, how full-history spend is totalled, and how a model resolves to a price. It is the **single home for account/balance/spend semantics**, folding onto the internal types [`AgentAccount`](../../../crates/rimz/src/agents/context.rs), [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs), [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs), the [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs) the cost walk produces, and the [`SidebarProviderPanel`](../../../crates/rimz/src/store/snapshot/view.rs) the renderer paints. The shape, end to end:

```text
sources                                    shared user-scoped caches
  a live session's rich context ─────── (rides the rollup; agent.md)
  the out-of-band account probe ──────► accounts.json
  the OAuth usage refresh ────────────► rate_limits.json · credits.json
  the full-history spend walk ────────► spending.json · provider-spending.json
                                                │
                                                ▼
                        producer aggregation + window fusion
                                                │
                                                ▼
                        one SidebarProviderPanel per kind
              plan · version · budget bars · paid usage · spend headline
```

The per-kind surfaces each provider exposes are in the adapter docs ([claude.md](./claude.md#account-and-balance), [codex.md](./codex.md#account-and-balance), [pi.md](./pi.md#account-and-balance), [opencode.md](./opencode.md#account-and-balance)); the raw auth and account-usage surfaces those read are in the per-provider upstream references ([claude-reference.md](../../externals/agent-adapter/claude-reference.md#auth-surface), [codex-reference.md](../../externals/agent-adapter/codex-reference.md#auth-file), [pi-reference.md](../../externals/agent-adapter/pi-reference.md#auth-file), [opencode-reference.md](../../externals/agent-adapter/opencode-reference.md#auth-file)).

Account identity, included balance, and paid-usage display are **enrichment, never correctness** — the no-transcript-correctness rule. A missing binary, a logged-out account, or an unavailable API degrades to an omitted plan label, a `v?` version placeholder, or an unknown budget track. Configured daily dollar caps are the separate rate-limit surface below: their decision input is the durable transcript walk, not a provider probe.

## The model

Three facts, all **account-scoped** — every session of one provider kind shares them, so the dashboard reads them per kind and never paints them per row:

- **Account identity** — [`AgentAccount`](../../../crates/rimz/src/agents/context.rs): the raw `plan` tier the provider reports (`max`, `team`, `pro`), a `metered` flag, and — for a multi-provider client (Pi) — the raw `sub_provider` credential id (`anthropic`, `openai`) naming the subscription the account runs on.
- **Included balance** — [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs): an ordered list of [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs)s (short→long), each a `used_percentage`, a typed `resets_at` instant a renderer formats as a countdown, and a `duration_mins` that names the window. This is the subscription mana bar: it spends first and refills automatically. Both Claude and Codex report a 5-hour and a 7-day window, but no window kind is hard-coded — the duration drives the bar label (`5h`/`7d`) and the reset-to-max roll-forward, so a provider's windows are whatever it reports, and a server-side change in their count or length renders gracefully (a transient Codex bug once widened its window to ~30 days; it painted a labeled `30d` bar rather than misrendering).
- **Paid usage** — [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs): an optional provider-paid balance or local API spend projection beyond the subscription windows. It may name used USD, remaining USD, a limit, or a disabled state; missing fields stay missing, so the renderer can show an unknown or uncapped row without inventing a cap. Disabled or exhausted extra credits do not make a parked turn terminal while the subscription mana bar has a future refill.
- **Reset credits** — [`ResetCredits`](../../../crates/rimz/src/agents/credits.rs): Codex-only redeemable resets for the 100%/7-day usage window. The cache stores the available count and the soonest expiry instant so the dashboard can show a compact header marker and color its glyph by urgency.

Account identity and included balance ride [`AgentContext`](../../../crates/rimz/src/agents/context.rs), the session-scoped rich-context record (see [agent.md → Rich context](./model.md#rich-context-agentcontext)); the producer lifts them to the account scope at aggregation time. Paid usage rides the shared credits cache when a provider reports it, or a read-time local spend projection for API-key accounts.

**Metered vs. unmetered** is the one distinction the dashboard turns on. A subscription or ChatGPT login is *metered*: it draws on the rate-limit windows, drawn as draining "mana" bars. An API-key login is *unmetered by subscription windows*: it has no included-window budget to drain, so the dashboard shows a single `api` paid-usage row sourced from transcript-derived trailing-month spend and an optional display ceiling. `metered: None` is unknown — the dashboard infers metering from whether any rate-limit window was reported.

## Two origins

A kind's account and balance reach the dashboard two ways, mirroring the [two-source split](./model.md#two-sources) of the live context read-path:

1. **A live session's rich context.** The statusline / app-server transport already carries account and included-window balance, so any live session of a kind fills both at no extra cost. The transport is per-kind — Claude's statusline, Codex's app-server, stored on the rollup as described in [agent.md → Rich context](./model.md#rich-context-agentcontext) — and this doc owns only what its fields *mean*.
2. **An out-of-band probe** ([`AgentAdapter::probe_account`](../../../crates/rimz/src/agents/mod.rs), one `account.rs` per adapter behind the shared [`AccountProbe`](../../../crates/rimz/src/agents/account.rs) contract). For a provider that is logged in but has no live session this run, the producer probes the login directly, so the dashboard shows your accounts and budgets between turns — not only mid-turn.

A live session always wins where both exist: its reading is richer and current.

Paid usage reaches the dashboard through a separate shared `credits.json` cache when a provider account-usage surface can be reached from local OAuth credentials or the Codex app-server, and through a read-time local spend projection for API-key accounts. Credential-file probes are read-only: RimZ does not refresh tokens, write provider auth files, or use browser-cookie dashboard strategies. Absence is an unknown `ex`/`api` row, not a synthesized value.

## Daily dollar caps

`[accounts.budget] <kind> = "100/day"` turns on one provider-login cap across every room on the machine. The spending walk computes a local-calendar-day `SpendWindow` per provider independently of the configured headline, publishes it in machine-shared `provider-spending.json`, and version-gates the cache so an upgrade re-aggregates the current cursor without reparsing unchanged transcripts. The producer accepts the cache's normal short TTL staleness as rate-limit latency.

The account ledger `budget.account.<kind>.json` carries a runtime raised cap, disabled state, and current park. Its cap adjustment is inert without the config on-switch. Every room evaluates the shared spend and ledger, but only interrupts its own running panes; raising or clearing from one room nudges the agents that room interrupted, while other rooms remain unparked and at rest until a message or the next local day. A healthy cap stays visually quiet; while agents are parked on a crossed account cap, the dashboard turns the headline alarm-red and appends `$used of $cap/day`.

An agent launched through an elevation wrapper as another real uid stays outside account aggregation. Its hooks and credentials live under that other user's home directory, and the current user's out-of-band probe reads only the current user's account surface, so the sidebar presents it as a flagged process row only and leaves it out of the provider dashboard.

## Per-provider mapping

Each provider maps its native account and balance surfaces onto the internal types in its own adapter doc; this table is the index. A new provider fills the relevant cells in its adapter doc and the rest of RimZ is unchanged.

| Provider | Account identity → [`AgentAccount`](../../../crates/rimz/src/agents/context.rs) | Balance → [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs) / [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs) |
| --- | --- | --- |
| **Claude** | [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) → plan + metered | statusline 5h/7d windows, or the idle OAuth usage probe — [claude.md](./claude.md#account-and-balance) |
| **Codex** | app-server `planType` / `~/.codex/auth.json` | app-server `primary`/`secondary` windows + `credits.balance`, or the OAuth usage probe; reset credits come from the Codex-only OAuth reset endpoint — [codex.md](./codex.md#account-and-balance) |
| **Pi** | `~/.pi/agent/auth.json` (oauth → metered, api_key → unmetered) | extension response headers + the OAuth usage probe, cached under `pi` — [pi.md](./pi.md#account-and-balance) |
| **OpenCode** | `~/.local/share/opencode/auth.json` (oauth → metered, api_key → unmetered) | the OAuth usage probe over the active backing-provider token, cached under `opencode` — [opencode.md](./opencode.md#account-and-balance) |

Every provider meters its account through two channels feeding one per-kind fusion: a **realtime** source where the provider exposes one — Claude's statusline, Codex's app-server, Pi's extension response headers — and a **direct-OAuth API query** over the provider's own token. The producer runs the OAuth query for every metered, logged-in provider on its own cadence, even while a live session is active; OpenCode meters through the OAuth channel alone. The fusion is keyed by kind, so each account paints its own bars.

The realtime leg is the per-kind difference: Codex reads its app-server first because it is the provider's local read-only account surface, Claude uses statusline windows when a session is alive, Pi reads extension response headers, and Pi/OpenCode select their active backing-provider OAuth token from `auth.json` and delegate to the Claude or Codex usage fetcher. The producer drives the OAuth channel on a shared `OAUTH_USAGE_TTL` through one hidden helper ([Refresh cadences](#refresh-cadences)), and its authoritative readings fold into the same cache as realtime readings.

## The out-of-band probe

[`AgentAdapter::probe_account`](../../../crates/rimz/src/agents/mod.rs) returns an [`AccountProbe`](../../../crates/rimz/src/agents/account.rs) with three arms, and the arm — not just the value — drives the producer's cache TTL:

- **`Found(AgentAccount)`** — a resolved login. Authoritative; rides the long success TTL.
- **`LoggedOut`** — the probe ran and confidently found no login. Also authoritative (it changes about never), so it caches like a success.
- **`Unavailable`** — the probe could not complete: a binary that would not run, a non-zero exit, an unreadable file. Transient; retried on the short failure TTL rather than pinning the dashboard empty for the full success window.

The probe is a **pure read**; cross-process memoization lives one layer up in the producer ([Producer aggregation](#producer-aggregation)). Each provider's probe mechanics — Claude's `claude auth status` fork, Codex's and Pi's cheap auth-file read — are in the adapter docs. An adapter with no out-of-band login surface defaults to `LoggedOut`.

Every registered adapter also exposes a display-only version probe by default: run `<kind> --version`, capture stdout, and treat any failure as no version. Rich context transports still win when present — Claude's statusline and Codex's app-server context can report fresher versions for live sessions — while the CLI probe fills idle or older account-cache entries. An absent version bypasses the long success TTL only after the short retry TTL, so a binary that cannot report `--version` does not re-fork on every producer frame; a future adapter overrides the probe only when its binary name differs from its kind or when it has a cheaper or richer idle version source.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../../crates/rimz/src/store/snapshot/view/providers.rs) folds accounts and balances into the dashboard view-model — one [`SidebarProviderPanel`](../../../crates/rimz/src/store/snapshot/view.rs) per kind. It is **producer-only**: it needs per-machine config and the out-of-band probe the pure reducer cannot read, so the reducer leaves `providers` empty and every consumer tab reads the producer's published panel (see [sidebar.md → State access](../sidebar/sidebar.md#state-access)).

Per panel:

- **Aggregate stats** — the per-provider `spending` (configured headline / 7d / 30d / 365d, summed fleet-wide across the kind's transcript history by the spending walk); `plan` and `version` taken from the freshest `context.observed_at`, the version falling back to the display-only binary probe when no session reports one. A kind with no live session still earns a panel when it has a probed login; recorded spend enriches an existing panel but never creates the provider section by itself.
- **Account** — `metered` and the `plan` label come from the kind's account (a live session's, or the probed idle one); a `plan` tier formats into a brand label (`max` → `Claude Max`, `pro` → `ChatGPT Pro`), and a missing account infers `metered` from whether windows were reported.
- **Brand style** — emblem art, color, and product name resolve from `[theme.providers.<kind>]` over the built-in defaults (claude clay, codex blue, pi forest green); an unknown kind gets neutral grey and no emblem. See [theme.md](../../guide/theme.md#provider-styling).
- **Balance windows** — the per-duration set fused by `fresh_windows`/`fuse_window` ([Window fusion](#window-fusion)). A kind that declares a window surface paints its own live and cached readings; a kind that declares no window surface renders the absence deliberately even if a stray sidecar reports windows.
- **Paid/API usage** — `ExtraCredits` from the shared credits cache is folded onto metered panels, then enriched with a display-only `[accounts.usage_limit_usd.<kind>]` ceiling if the provider did not report a cap. Codex `ResetCredits` ride the same cache for the header marker. Unmetered API-key panels synthesize `ExtraCredits::Known` from the provider's trailing-month transcript spend and the same optional ceiling, so an API-key account can show `$used/$limit` without pretending the provider enforces that limit.

Display config plus a spend ranking decide which discovered panels appear. With no explicit `provider_list`, headline stored spend decides which panels survive the *stacked* dashboard's `theme.display.max_provider_blocks` cap (default 3) — a token-only provider ranks on the same transcript-derived footing — while a *tabbed* dashboard is height-bounded by its active block and shows every discovered provider. The shown set always paints in the registry's display order (`claude, codex, pi, opencode`): the panels are the dashboard's tabs ([sidebar.md → Provider dashboard](../sidebar/sidebar.md#provider-dashboard)), so the row never reorders as spend shifts. An explicit `provider_list` overrides the set, the order, and the cap ([theme.md → Display](../../guide/theme.md#display)).

The account, rate-limit, and credits caches are user-scoped, persistent under `$XDG_STATE_HOME/rimz/shared/`, and single-flighted across rooms by locks under `$XDG_RUNTIME_DIR/rimz/shared/`: the elected producer publishes the shared `accounts.json`, `rate_limits.json`, and `credits.json`; consumers read them and never fork.

### Per-provider spend

A panel's headline line is transcript-history burn for the configured `[sidebar] spend_window`: the `◎` session count, then the token breakdown `◇ ↘ ↗ ◌` (integer magnitudes, the fleet-store rows' exact vocabulary, with cache creation folded into `↘` input), with the bold dollar-green `$` pinned right. It reads from `spending`, the per-provider [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs) the spending walk returns ([Cost history](#cost-history)): the producer attaches each kind's entry to its panel before sorting, and the renderer reads `spending.headline` (serialized as `today` in the cache for compatibility) — its `sessions` count, split fields, and `usd`.

There is no live-session aggregate to fall back to, so a token-only provider like Codex (priced from tokens via [token pricing](#token-pricing)) shows its dollars the same as a `costUSD`-logging one. The fleet-wide trailing-week/month `W:`/`M:` store rows (with session counts) are a separate, fleet-level read of `value_tally`, pinned below the panels. The spend is producer-only, like the rest of aggregation; it threads in as a plain map so the reducer stays I/O-free.

### Window fusion

Balance is account-scoped, so one window is fused from every reading of it — across parallel sessions, across the live transport and the out-of-band refresh, and across time. The fused per-kind budget is the single input for provider-limit display and [auto-continue](#auto-continue) decisions; per-session `context.rate_limits` feeds the fusion and the card, not decisions.

Each [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs) carries its provenance: an `observed_at` capture stamp and a `source` — `BestEffort` (Claude's statusline) or `Authoritative` (an official-API query: Codex's app-server, either provider's OAuth refresh). Usage only climbs within a live window, so a reading that *lowers* the bar is a refill that must be earned, and the fusion decides how far to trust it. Two stages:

**Live, per frame** — [`fresh_windows`](../../../crates/rimz/src/store/snapshot/view/providers.rs) reduces every session's readings to one live candidate per `duration_mins`. It first rejects content-stale readings *whole*: a reading whose **shortest window's reset has already passed** predates that reset, so every window in it is stale — even a longer window whose own reset is still future. This is load-bearing, because an idle session re-runs its statusline and re-stamps a fresh `observed_at` over a days-old payload, so `observed_at` alone cannot tell live from stale; the shortest window's reset is the real freshness heartbeat. Among the survivors, the **most-drained** value wins per duration (highest `used_percentage`) — within one live window usage only climbs, so the highest reading is the most current, and the pick is stable against parallel sessions reporting at slightly different instants.

**Across source and time** — [`fuse_window`](../../../crates/rimz/src/sidebar/refresh/rate_limits.rs) folds that live candidate into the persisted truth ([Persistence across idle sessions](#persistence-across-idle-sessions)). A climb is adopted at once. A drop is trusted immediately only with corroboration: an `Authoritative` reading whose capture is no older than the prior's (it queried the official API, so it invalidates older best-effort data, while an out-of-order sidecar with a stale `observed_at` cannot undo a newer reading), or a later `resets_at` (a new window epoch).

A best-effort drop with neither earns the confirm path only when it lands near full (at or below the reset floor) — the signature of the mid-window **free reset** a provider sometimes grants, refilling progress with the reset timer unchanged. The bar holds at the higher prior until the low reading has stood for a short confirm window, so one lagging or garbled sample cannot dip a live budget while a genuine refill still surfaces. A mid-range best-effort drop is jitter, not a reset, and holds the most-drained prior. The same inputs always yield the same bars, regardless of which session reported last.

A debug trace of every reading (`RIMZ_RATE_LIMIT_TRACE`, off by default) records `used_percentage`/`resets_at`/`observed_at`/`source` per frame so the confirm-window timing can be tuned against real reset events.

### Spent windows and paused rows

A window is **spent** at `used_percentage == 100`: it is currently limiting while its reset still sits ahead. The spent window paints the provider dashboard's budget bars; it does not park every agent of that kind.

A row becomes `paused` only when that agent actually stopped mid-turn on a limit or transient server error: a native turn-error certificate (`rate_limit`, `spend_limit`, or `overloaded`) parks the affected running agent, and a stalled running agent uses the fused spent, unreset kind window as the fallback pause predicate. `overloaded` covers provider overload and transient 5xx server errors; neither has a local reset clock.

A `rate_limit` or `spend_limit` pause is resumable while the fused account budget has a subscription window with a future reset — including the common spend-limit case where extra credits are disabled or exhausted but the mana bar will refill. A recovered fused mana bar keeps the row parked while the persisted [auto-continue](#auto-continue) record has a chance to wake the turn, rather than turning a frozen per-agent 100% reading into a spurious `!`.

Calm agents (`idle`/`success`) and actively progressing turns stay in their lifecycle status even when a budget bar reads empty. The rollup keeps each agent's true lifecycle status; the projection and glyph live in [agent.md → Displayed status](./model.md#displayed-status) and [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

### Auto-continue

With `[resume] auto_continue = true` (off by default), the producer resumes a [parked turn](#spent-windows-and-paused-rows) by typing the configured nudge (`continue` by default) into the agent's live pane through the same pane-send path as `message --steer`, so the agent's next hook moves the row back to `running` on its own. The producer-side arm/fire trigger and the `rimz agents auto-continue` helper that performs the send live in [`harness/auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs).

**Arm.** Within seconds of the park, the producer writes a durable per-agent record capturing either the fused spent-window reset deadline (`resume_park`) or the non-clocked turn-error marker. Capturing the deadline up front keeps a clocked resume alive outside the session-sidecar reading it was first seen through: a 5h/7d window can outlive the live session and its cleanup, and a post-reset account reading gives the persisted record one recovery frame to nudge the frozen turn. If the record is lost after the mana bar recovers while the agent still carries a limit marker, the producer re-arms a due-now rate-limit record before firing, because the wait already happened.

**Fire.** Once the recorded deadline or the current backoff step passes and the agent is still idle, the record fires the nudge. Overload and transient API-error parks use a configurable backoff ramp (`auto_continue_backoff_secs`, default `[180, 300]`) whose last value repeats: by default the first attempt lands after 3 minutes and later attempts repeat every 5 minutes. Message events carry the queued, sent, delivered, timed-out, or failed trace.

**Exhaust.** Rate-limit, spend-limit, overload, and transient API-error records share one cap (`auto_continue_max_retries`, default `13`), counted from evidenced hidden `DeliveryGate::Resume` messages since the park — helper spawns and pre-queue crashes only throttle pacing and backoff. At the default ramp, attempts span about 63 minutes before exhaustion promotes the row to actionable `failed`.

### Not-started windows

The included-balance budgets are **sliding** windows: the clock starts on your first token, so until then the provider keeps `resets_at` slid a full window-length ahead. A window whose reset still sits ~a full window out has **not started** — and it is detected by that reset distance, not a `used_percentage` of 0, because a fresh window still reports ~1% used (the live Codex 5h reads `usedPercent: 1` with the reset a full 5h out). That ~1% is the floor: any usage **above** it means the window has clearly started, so only a window at or below the floor (0–1% used) is a not-started candidate — past that, the reset is a real countdown regardless of its distance.

The dashboard omits the countdown for such a window (a near-full bar, no `↻`), so it reads "ready to start" rather than a misleading ticking placeholder. Display only — it touches no parking or correctness — and applies to every provider; the on-screen treatment is [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard).

### Persistence across idle sessions

A session ending or going idle would otherwise empty the dashboard, so the producer mirrors each resolved window into a user-scoped `$XDG_STATE_HOME/rimz/shared/rate_limits.json` cache (atomic write under a shared read-modify-write lock in runtime) and reads it back when no live session reports one:

- Before a window's reset, the last-known fused reading stands.
- Once a shorter window's reset passes while the longest cached window is still in the future, that shorter window has refilled — it shows full with the reset rolled one window-length forward, until a live reading overwrites it.
- Once the longest cached window's reset passes with no fresh reading, RimZ no longer knows the account balance — every cached bar for that provider shows as an unknown empty track until a live or out-of-band reading overwrites it.

Only live ground truth is persisted; the synthesized full or unknown window is a **read-time projection, never written**. The cache tracks login and drops a kind once it logs out.

Paid usage has a sibling `$XDG_STATE_HOME/rimz/shared/credits.json` cache with the same account-scoped shape and runtime-lock discipline. Provider-reported values are persisted when a local account-usage surface returns them; Codex reset credits persist there with the paid-usage entry, preserving the last successful reset read across app-server and extra-credits-only writes. The OAuth refresh records the provider's cheap local account key with the entry; an account-key change makes the credits read due immediately, skips prior-account carry on failure, and drops that kind's cached windows before refetching. API-key spend projections are not persisted because they are already derivable from the transcript spending walk plus config. A stale or absent credits entry leaves the `ex` row unknown (`∞` value over a dim empty track), while a configured display ceiling can still scale the row if usage is known.

### Refresh cadences

The producer keeps every metered account's balance current between turns through one hidden helper, `rimz agents refresh-usage --kind <kind>`, spawned for each metered, logged-in panel whose OAuth-attempt marker is due. The marker uses `OAUTH_USAGE_TTL` and the helper single-flights the actual provider read with a dedicated `credits.json` `oauth_read_at_ms` stamp, so realtime/app-server writes to the display freshness stamp never suppress OAuth. `RIMZ_OAUTH_USAGE_OFFLINE=1` disables account-usage fetches for that process tree, and a provider with no local OAuth credentials records a quiet no-credentials attempt. A settled auth failure, including missing credentials or a 401-rejected token, records a quiet attempt retried on `OAUTH_USAGE_SETTLED_TTL` (1 h) and retried immediately once the provider's auth file changes or the local account key differs.

The helper first folds a provider realtime account reading when the adapter exposes one, then runs the OAuth query on its independent cadence. OAuth windows are authoritative; when the producer asks for them they merge after the realtime fold, so a current OAuth token replaces a stale warm realtime process. The OAuth usage plan tier rides the credits cache and fills a provider panel only when no live session reports a plan. When window fusion persists a new reset epoch, it removes the producer marker and zeroes `oauth_read_at_ms`, forcing the next cache-refresher tick to re-probe before the five-minute cadence elapses. The per-kind account sections own the transport details — Codex's app-server plus OAuth reset-credit endpoint ([codex.md](./codex.md#account-and-balance)); Claude's statusline-stale merge rule ([claude.md](./claude.md#account-and-balance)); and Pi's and OpenCode's `auth.json` token delegation ([pi.md](./pi.md#account-and-balance), [opencode.md](./opencode.md#account-and-balance)).

Transient OAuth HTTP failures — transport, body read, and 5xx responses — retry up to three attempts in-process with short backoff before surfacing. A surfaced failure reports off-box under one shared `oauth_usage` operation with a `provider` tag, grouping every provider into a single issue while the error detail carries the request host.

## Cost history

A read-path walks the *whole* transcript/store history to total spend and token throughput, bucketed into the configured headline window plus trailing 7d / 30d / 365d windows as a [`SpendTally`](../../../crates/rimz/src/agents/spending/aggregate.rs). One read-only parser per provider lives in its adapter's `spend.rs` ([`claude`](../../../crates/rimz/src/agents/claude/spend.rs), [`codex`](../../../crates/rimz/src/agents/codex/spend.rs), [`pi`](../../../crates/rimz/src/agents/pi/spend.rs), [`opencode`](../../../crates/rimz/src/agents/opencode/spend.rs), shared walk helpers in [`transcript_fs`](../../../crates/rimz/src/agents/transcript_fs.rs)), resolved through `AgentAdapter::transcript_files` / `parse_spend`. Each parsed file yields per-entry cost, a four-way token split (`input` / `output` / `cache_write` / `cache_read`), an entry timestamp, a provider-native thread id when the store holds many sessions, and one per-file origin path when the provider exposes one; [`SpendingWalker`](../../../crates/rimz/src/agents/spending/mod.rs) refreshes and aggregates the entries.

The walk discovers every registered spend store fleet-wide — every Claude project dir, every Codex/Pi session file, and every OpenCode database — so each provider counts on the same footing regardless of which project it ran in ([`sidebar::refresh::spending`](../../../crates/rimz/src/sidebar/refresh/spending.rs), or the standalone `rimz stats` refresh path). One aggregation pass returns the fleet total plus a **per-provider breakdown** for global surfaces, publishes UTC-day and per-model rollups for `rimz stats`, and derives the scoped cockpit tally from the same refreshed cache ([Per-provider spend](#per-provider-spend)).

Each window carries the token split — its `↘` input folds in `cache_write`, so the `◇` total is folded input plus output, and `cache_read` rides apart — plus a `sessions` count of the distinct threads that ran in the window. The same parser path feeds [`spending::session_cost_usd`](../../../crates/rimz/src/agents/spending/mod.rs), the turn-end live-card floor for adapters whose provider transport does not push a dollar total.

Three surfaces read the totals. The account-global fleet tally feeds the provider dashboard and the bottom [fleet store](../../interface/sidebar.md#the-fleet-store) (the static trailing-week and trailing-month rows); consumers read stale persistent totals when they exist, and the store reads `$0.00` instead of disappearing before any history exists. `$XDG_STATE_HOME/rimz/shared/provider-spending.json` also publishes UTC-day buckets plus per-model buckets for `rimz stats`. The cockpit's `◎`/`¤`/breakdown summary reads a workspace-scoped walked tally over the same cached files, limited to the room's project root plus grouped worktrees; unknown-origin files are omitted. For the headline `$`, the workspace walk suppresses headline USD for active live-card sessions, publishes the excluded session-key set and headline-window cutoff in `workspace-spending.<scope_hash>.json`, and the renderer adds those cards' current costs back in full. This keeps the headline at least as high as the visible in-scope cards while the W/M/Y store windows, headline tokens, and session counts remain walk-derived. A derive from the shared entry cache that would regress a young matching workspace cache serves the previous cache instead; a real session rollover or day roll ages or changes the cutoff and resets the headline epoch.

The walk is **read-only and sidebar-safe** — no store writes — so it sits apart from the integration adapters. The parsing is mostly shared; two concerns are provider-specific and live in the adapter docs:

- **Dedup.** Claude replays a parent message into each subagent file with an inflated cost, so each parsed chunk dedups retry writes before storage and the spending walk dedups across files; Pi and Codex sessions are single-file and need no cross-file dedup, while OpenCode's SQLite rows carry `session_id` as the native thread key ([claude.md → Cost](./claude.md#cost)).
- **Cost source.** Claude and Codex log token counts, priced through the [token pricing](#token-pricing) table; Pi logs dollars directly, used verbatim; OpenCode uses positive stored `cost` values and prices zero-cost token rows ([claude.md → Cost](./claude.md#cost), [codex.md → Cost](./codex.md#cost), [pi.md → Cost](./pi.md#cost), [opencode.md → Cost](./opencode.md#cost)).

**Unknown prices.** A token-priced turn whose model misses the book still contributes tokens and sessions with zero dollars, and the file cache records the trimmed model name plus its youngest timestamp for the pricing refresh chase; sentinel names such as Claude's `<synthetic>` are filtered out because they are not API model IDs, and unknowns older than the 365-day spend window do not chase. Once an active unknown model resolves, the file cold re-parses from byte zero, so zero-dollar entries recover their spend in the same due walk.

### The incremental cache

The read is incremental and user-scoped: the persistent shared `$XDG_STATE_HOME/rimz/shared/spending.json` cache stores `(mtime, len, cursor, origin)` per file, dedups retry-write rows within each parsed chunk, and compacts finalized rows older than 8 days into per-day/model/thread rollups. Per file class:

- An unchanged file is one stat.
- A grown file parses only its appended suffix from the cursor.
- A cold changed set parses in a bounded worker pool.
- A new file whose mtime predates the widest spend window plus a skew margin is skipped without writing a cache record, and dead records past that boundary are evicted.

Dirty cursor state persists on the first walk, after the five-minute min interval, or after cold-size parse work. The cursor also carries provider-specific resume state: Codex's cumulative-totals fold state and its learned file origin (which survives cold re-parses), Pi's session header cwd, and OpenCode's SQLite `rowid`.

Two version stamps keep the cache honest across schema changes. A shape change to the entry split, store-time dedup, or per-file origin metadata bumps `SPENDING_CACHE_VERSION`, so finalized sessions re-parse cleanly; a semantic change to the published aggregate or guaranteed built-in pricing bumps `PROVIDER_SPENDING_VERSION`, so `provider-spending.json` recomputes once from the current entry cache without forcing a store re-parse. Shared spending cache writers also refuse a schema downgrade after a cheap leading-version probe, so an older long-lived build cannot blank a newer build's published aggregate or force persistent cursor cold-walks; a version bump costs one recompute, then the higher-version cache holds.

### One walk per user

The full store walk runs at most once per user per `SPENDING_TTL`, single-flighted by `$XDG_RUNTIME_DIR/rimz/shared/spending.lock`. Between due walks every room and `rimz stats` serve the shared `provider-spending.json`; a lock-loser grace-serves the last current-version publish for the bounded stale window, and the fallback local walk seeds from the same disk cursor cache without persisting.

During a publishing walk, `WALK_CHECKPOINT_INTERVAL` publishes a current-stamped partial aggregate and checkpoints dirty cursor progress only when the persist gate opens, so the dashboard total climbs during init and a restart resumes from the last checkpoint instead of byte zero. The final parsed-plus-compacted cursor and aggregate remain the authority; cursor and provider write failures log warnings with their paths.

A room with a missing workspace cache derives `workspace-spending.<scope_hash>.json` from the shared entry cache without taking the global walk lock ([performance.md → Per-enrichment cadences](../performance.md#per-enrichment-cadences)).

## Token pricing

Claude and Codex log token counts, so converting their turns to dollars needs a per-model price table; Pi logs `costUSD` directly (as did older Claude transcripts, which still use that figure when present). This section owns that table: where prices come from, how a model resolves to a price, and how the table stays fresh. Pricing is **enrichment, never correctness** — a failed fetch, a missing snapshot, an unknown model each degrades to stale-but-usable prices or zero-dollar token usage, never a hard failure.

The table lives in [`agents/pricing/`](../../../crates/rimz/src/agents/pricing/mod.rs): per-token [`Pricing`](../../../crates/rimz/src/agents/pricing/mod.rs) keyed by model in a [`PriceBook`](../../../crates/rimz/src/agents/pricing/mod.rs). A price row carries input, output, cache-create, cache-read, optional 200k-tier rates for each class, `cache_read_explicit`, and the fast-mode multiplier. Lookups are pure and network-free; the only network is the gated refresh in `load_for_spending`.

### Price table precedence

The book is assembled from four ordered passes. Builtins are an offline fallback under the live LiteLLM refresh, and models.dev only fills rows the other sources still lack:

| Layer | When | Source |
| --- | --- | --- |
| 1. Embedded snapshot | always, at process start | the ignored generated LiteLLM-shaped snapshot, produced from all priced LiteLLM rows plus authoritative models.dev fillers, compacted and gzipped into the binary by [`build.rs`](../../../crates/rimz/build.rs) and `include_bytes!`-ed ([`embedded.rs`](../../../crates/rimz/src/agents/pricing/embedded.rs)); release builds and the published crate ship the refreshed snapshot, forced into the crate via Cargo's `include`, while a fresh clone with no generated snapshot embeds an empty table |
| 2. Builtins | always, before cached refresh rows | hardcoded fallback prices ported from ccusage for Claude, OpenAI/Codex, GLM, Kimi, and Grok families ([`builtins.rs`](../../../crates/rimz/src/agents/pricing/builtins.rs)) |
| 3. LiteLLM refresh | once per TTL, on disk | a fresh full LiteLLM pull once per weekly TTL, overwriting builtins when upstream publishes the row ([`remote.rs`](../../../crates/rimz/src/agents/pricing/remote.rs)) |
| 4. models.dev fill | unknown-model chase only | authoritative Anthropic/OpenAI models.dev entries fetched only when an unknown priceable model persists, inserted only for models LiteLLM and builtins lack |

`gpt-5` is mandatory in the builtins: it is the Codex parser's fallback model, so a Codex event with no resolvable model still prices.

Every read-only price consumer uses the same current book: the embedded snapshot plus builtins and the shared `pricing-cache.json`. This includes local spending fallbacks, live-card transcript costs, and hook-side transcript reconciliation, so an unknown model heals across every USD surface once the chase lands its price without requiring a binary upgrade. Long-lived processes memoize that book by cache path, modification time, and file length; each call still stats the cache so an atomic rewrite invalidates the memo.

### The refresh

`rimz sidebar snapshot` is a one-shot process, so the refresh is disk-cached at `$XDG_STATE_HOME/rimz/shared/pricing-cache.json` rather than held in memory: the spending producer reads the embedded snapshot plus the cache instantly while it holds the shared runtime spending lock, and re-fetches when the cache is older than a week. The pricing cache carries a schema stamp; a stale cache shape is dropped so tierless rows cannot override the embedded snapshot and builtins after a formula upgrade. A failed fetch records its attempt time and backs off an hour, so a persistent outage never re-fetches on every snapshot. `RIMZ_PRICING_OFFLINE` skips every fetch path.

An unknown-model chase rides the same producer walk, with no timer of its own. When the [cost-history pass](#cost-history) records a priceable model name that the assembled book still cannot price, the pricing cache may fetch early on a standalone 30-minute gate, and this chase is the only path that fetches models.dev. While the same unknowns persist, that gate doubles to 1h, 2h, and onward to the 24h cap; a newly seen unknown resets the gate to 30m. The chase also observes a 30-minute floor after any fetch attempt, so a just-refreshed source has time to catch up before RimZ asks again. Failed chase attempts escalate the same way as successful ones; when every recorded unknown resolves, the chase state clears.

`build.rs` never touches the network: it embeds the ignored generated snapshot at `crates/rimz/pricing/litellm-pricing.json` when present (or a `RIMZ_PRICING_JSON_PATH` override), then writes a gzipped `OUT_DIR/litellm-pricing.json.gz`, so ordinary builds stay reproducible and hermetic. `cargo xtask pricing-refresh` is the deliberate update path — it fetches LiteLLM from `https://raw.githubusercontent.com/BerriAI/litellm/refs/heads/main/model_prices_and_context_window.json`, fills missing models from the authoritative Anthropic/OpenAI models.dev catalogues, and rewrites the generated snapshot atomically; `cargo xtask dist` runs that refresh before release builds. The release workflow publishes the crate with that snapshot force-included through Cargo's `include` allowlist and `--allow-dirty`, so `cargo install rimz` embeds it too.

### Resolving a model

`PriceBook::price` resolves a model id to a price by exact match, then a boundary-aware fuzzy scan: the longest stored key that is a word-boundary prefix of the (normalized: trimmed, lowercased, `.`/`@`→`-`) lookup wins. So `claude-sonnet-4-20250514-via-bedrock` resolves to its base model. A purely numeric, non-date version bump is rejected — a new `gpt-5`-family version is never silently priced as the old one; it falls through to its own entry or to no price.

### Computing token-priced cost

The spending walk prices the token-only providers per turn with one shared helper. Input, output, 5-minute cache creation, 1-hour cache creation, and cache read are each tiered at the first 200,000 tokens per class when the model publishes an above-200k rate. The 5-minute cache-creation slice bills at the cache-create rate; the 1-hour cache-creation slice bills at 2x the input rate, including 200k tiers; fast or priority turns multiply the finished cost by the model's fast multiplier. **Codex** passes uncached input, output, and cached input as cache-read. **Claude** splits `message.usage.cache_creation` into 5-minute and 1-hour cache creation when present, falling back to the flat `cache_creation_input_tokens` as 5-minute cache creation otherwise; an older Claude turn that still logs a positive `costUSD` keeps that figure instead. A turn whose model has no known price keeps its tokens and sessions with zero dollars while the pricing chase looks for a price.

## Adding a provider

A new agent earns an account block and balance bars by filling the two internal types from its own surfaces; everything downstream — aggregation, window fusion, caching, the dashboard, the pricing table — is provider-agnostic and comes free. The work mirrors [agent.md → Adding an agent](./model.md#adding-an-agent):

1. **Fill `AgentAccount`** (plan + metered) on the session's `AgentContext` from its rich-context transport, and/or override [`AgentAdapter::probe_account`](../../../crates/rimz/src/agents/mod.rs) for the logged-in-but-idle case.
2. **Fill `AgentRateLimits`** (the windows, each with a `used_percentage`, reset instant, and `duration_mins`) on `AgentContext` from the transport.
3. **Optionally fill `ExtraCredits`** from a read-only provider account-usage surface; use `Disabled` only when the provider explicitly says paid extra usage is off.
4. **Optionally** register `[theme.providers.<kind>]` defaults (emblem, color, name).
5. **Stay best-effort** throughout: a missing fact is an omitted label, a `v?` placeholder, or an unknown budget track, never an error.

Golden the account mapping from a fixture probe payload and a fixture transport payload, including the logged-out and unparseable cases (the inline goldens in each adapter's `account.rs` are the model). Golden the spend parser from a fixture JSONL, including the dedup and zero/negative-cost cases.

## What lives elsewhere

- **The on-screen look** — the mana bars, the `ex`/`api` paid-usage rows, the aligned grid, the `⇅ rc` flag, the exhausted-window and longer-window-gating rendering — is [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard).
- **The renderer's projection** of `providers` and where the dashboard sits in the sidebar is [sidebar.md → Provider dashboard](../sidebar/sidebar.md#provider-dashboard).
- **The per-kind transport** that carries the rich context — the statusline pipe and its wrap/restore, the Codex app-server connection ladder and broker — is the adapter docs ([claude.md](./claude.md#context-and-transcript), [codex.md](./codex.md#context-and-transcript)).
- **Storage** of `AgentContext` on the rollup and the sidecar fold-in is [agent.md → Rich context](./model.md#rich-context-agentcontext).
