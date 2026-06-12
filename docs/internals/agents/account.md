# Agent accounts and balances

> See [DESIGN.md → Attention at a glance](../../../DESIGN.md#attention-at-a-glance) for the account-scoped-budget invariant this doc operationalizes, [transcript.md](./transcript.md) for the transport plumbing that carries the rich context this doc interprets, and [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard) for what the provider dashboard looks like on screen.

A coding agent runs against a **provider account** — a login, on a plan, that may or may not be metered — and that account has a **balance**: the included rate-limit windows the plan draws against plus any paid extra/API usage the provider or local spend ledger can name.
This doc owns both: the account/balance model, where each provider's facts come from, how they map onto Rimz's internal types, and how the producer folds them into the provider dashboard.

It is the **single home for account/balance semantics**: what the metered/unmetered/plan facts mean, and how the producer folds them onto the internal types [`AgentAccount`](../../../crates/rimz/src/agents/context.rs), [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs), [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs), and the [`SidebarProviderPanel`](../../../crates/rimz/src/ledger/snapshot/view.rs) the renderer paints.
The raw auth and account-usage surfaces it reads — `claude auth status`, `~/.claude/.credentials.json` plus Claude's OAuth usage endpoint, `~/.codex/auth.json`, Codex's app-server `account/rateLimits/read`, Codex's OAuth usage endpoint, and `~/.pi/agent/auth.json` — are in the per-provider reference: [claude-reference.md](../../externals/agent-adapter/claude-reference.md#auth-surface), [codex-reference.md](../../externals/agent-adapter/codex-reference.md#auth-file), and [pi-reference.md → Auth file](../../externals/agent-adapter/pi-reference.md#auth-file) (which also records Pi's missing balance surface).
[opencode-reference.md → Auth file](../../externals/agent-adapter/opencode-reference.md#auth-file) mirrors OpenCode's auth surface ahead of its adapter — credential types only, with the same missing balance surface.

Account and balance are **enrichment, never correctness** — the no-transcript-correctness rule.
A missing binary, a logged-out account, an unparseable file: each degrades to an omitted plan label, a `v?` version placeholder, or an unknown budget track, never a failed snapshot or a wrong decision.

## The model

Three facts, all **account-scoped** — every session of one provider kind shares them, so the dashboard reads them per kind and never paints them per row:

- **Account identity** — [`AgentAccount`](../../../crates/rimz/src/agents/context.rs): the raw `plan` tier the provider reports (`max`, `team`, `pro`), a `metered` flag, and — for a multi-provider client (Pi) — the raw `sub_provider` credential id (`anthropic`, `openai`) naming the subscription the account runs on.
- **Included balance** — [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs): an ordered list of [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs)s (short→long), each a `used_percentage`, a typed `resets_at` instant a renderer formats as a countdown, and a `duration_mins` that names the window. Both Claude and Codex report a 5-hour and a 7-day window. The duration drives the bar label (`5h`/`7d`) and the reset-to-max roll-forward, so no window kind is hard-coded — a provider's windows are whatever it reports, and a server-side change in their count or length renders gracefully (a transient Codex bug once widened its window to ~30 days; it painted a labeled `30d` bar rather than misrendering).
- **Paid usage** — [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs): an optional provider-paid balance or local API spend projection. It may name used USD, remaining USD, a limit, or a disabled state; missing fields stay missing, so the renderer can show an unknown or uncapped row without inventing a cap.

Account identity and included balance ride [`AgentContext`](../../../crates/rimz/src/agents/context.rs), the session-scoped rich-context record (see [agent.md → Rich context](./agent.md#rich-context-agentcontext)); the producer lifts them to the account scope at aggregation time. Paid usage rides the shared credits cache when a provider reports it, or a read-time local spend projection for API-key accounts.

**Metered vs. unmetered** is the one distinction the dashboard turns on.
A subscription or ChatGPT login is *metered*: it draws on the rate-limit windows, drawn as draining "mana" bars.
An API-key login is *unmetered by subscription windows*: it has no included-window budget to drain, so the dashboard shows a single `api` paid-usage row sourced from transcript-derived trailing-month spend and an optional display ceiling.
`metered: None` is unknown — the dashboard infers metering from whether any rate-limit window was reported.

## Two origins

A kind's account and balance reach the dashboard two ways, mirroring the [transcript two-source split](./transcript.md#two-sources):

1. **A live session's rich context.** The statusline / app-server transport already carries account and included-window balance, so any live session of a kind fills both at no extra cost. The transport plumbing is [transcript.md](./transcript.md)'s concern; this doc owns only what its fields *mean*.
2. **An out-of-band probe** ([`AgentAdapter::probe_account`](../../../crates/rimz/src/agents/mod.rs), one `account.rs` per adapter behind the shared [`AccountProbe`](../../../crates/rimz/src/agents/account.rs) contract). For a provider that is logged in but has no live session this run, the producer probes the login directly, so the dashboard shows your accounts and budgets between turns — not only mid-turn.

A live session always wins where both exist: its reading is richer and current.
Paid usage reaches the dashboard through a separate shared `credits.json` cache when a provider account-usage surface can be reached from local OAuth credentials or the Codex app-server, and through a read-time local spend projection for API-key accounts. Credential-file probes are read-only: Rimz does not refresh tokens, write provider auth files, or use browser-cookie dashboard strategies. Absence is an unknown `ex`/`api` row, not a synthesized value.

An agent launched through an elevation wrapper as another real uid stays outside account aggregation. Its hooks and credentials live under that other user's home directory, and the current user's out-of-band probe reads only the current user's account surface, so the sidebar presents it as a flagged process row only and leaves it out of the provider dashboard.

## Per-provider mapping

Each provider maps its native account and balance surfaces onto the internal types. A new provider fills the relevant cells in this table and the rest of Rimz is unchanged.

| Provider | Account identity → [`AgentAccount`](../../../crates/rimz/src/agents/context.rs) | Balance → [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs) / [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs) |
| --- | --- | --- |
| **Claude** | [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) → `plan` + `metered`; `apiKey` login is unmetered. | Live: statusline [`rate_limits`](../../externals/agent-adapter/claude-reference.md#statusline-json) 5h/7d windows, parsed by [`observe_context`](../../../crates/rimz/src/agents/claude/statusline.rs). Idle/fallback: [`rimz claude refresh-usage`](../../../crates/rimz/src/cli/claude.rs) reads `~/.claude/.credentials.json` and calls Claude's OAuth usage endpoint; `five_hour`/`seven_day` refill `AgentRateLimits`, and `extra_usage` maps cents to `ExtraCredits::Known { used_usd, limit_usd }` or `Disabled`. |
| **Codex** | Live: [`account/rateLimits/read`](../../externals/agent-adapter/codex-reference.md#app-server-api) `planType` → `plan`, riding `AgentContext.account`. Idle: [`~/.codex/auth.json`](../../externals/agent-adapter/codex-reference.md#auth-file) — API key → unmetered by subscription windows, `tokens` → metered ChatGPT (plan tier filled once a session reports it). | Primary: [`account/rateLimits/read`](../../externals/agent-adapter/codex-reference.md#app-server-api) `primary`/`secondary` windows, each with `windowDurationMins` ([`app_server.rs`](../../../crates/rimz/src/agents/codex/app_server.rs)); optional `credits.balance` maps to `ExtraCredits::Known { remaining_usd }` and is cached in `credits.json`. Fallback: [`rimz codex refresh-rate-limits`](../../../crates/rimz/src/cli/codex.rs) reads `~/.codex/auth.json` and calls Codex's OAuth usage endpoint when the app-server returns no account-usage reading. |
| **Pi** | [`pi/account.rs`](../../../crates/rimz/src/agents/pi/account.rs) reads [`auth.json`](../../externals/agent-adapter/pi-reference.md#auth-file): oauth → metered subscription, api_key → unmetered. The plan names the sub the fleet uses (`Anthropic OAuth`, `OpenAI API Key`) — the freshest session's `message.provider` picks among several credentials, else the first OAuth entry — the raw credential key rides `sub_provider`; the shared display-only version probe attaches `pi --version`, the panel header's fallback for a provider whose sessions report none. | Borrowed — pi exposes no rate-limit window surface of its own (declared off via `rate_limit_windows`), but a metered OAuth sub *is* a sibling kind's account, so the producer maps `sub_provider` to the kind metering it ([`kind_for_sub_provider`](../../../crates/rimz/src/agents/registry.rs): each descriptor declares its `sub_providers` — `anthropic` → claude, `openai` → codex) and the Pi panel paints that kind's stable windows. Nothing to borrow (no sibling kind, no readings) renders the unknown-track placeholder for a metered account; an API key gets the `api` paid-usage row. |

Both Claude and Codex can refresh included windows while idle when `[accounts] oauth_usage` is enabled and their local OAuth credentials are present. Codex uses the app-server first because it is the provider's local read-only account surface; the direct OAuth endpoint is its fallback. Claude uses the direct OAuth endpoint because the statusline is live-session only.

## The out-of-band probe

[`AgentAdapter::probe_account`](../../../crates/rimz/src/agents/mod.rs) returns an [`AccountProbe`](../../../crates/rimz/src/agents/account.rs) with three arms, and the arm — not just the value — drives the producer's cache TTL:

- **`Found(AgentAccount)`** — a resolved login. Authoritative; rides the long success TTL.
- **`LoggedOut`** — the probe ran and confidently found no login. Also authoritative (it changes about never), so it caches like a success.
- **`Unavailable`** — the probe could not complete: a binary that would not run, a non-zero exit, an unreadable file. Transient; retried on the short failure TTL rather than pinning the dashboard empty for the full success window.

The probe is a **pure read**; cross-process memoization lives one layer up in the producer (below).
Claude forks `claude auth status`, capturing stdout with stdin and stderr nulled (never inherited, so it stays quiet in a TUI); Codex reads `~/.codex/auth.json` — a cheap file read, no subprocess, so an absent or corrupt file is an authoritative `LoggedOut`, not a retry-worthy `Unavailable`.
Pi reads `~/.pi/agent/auth.json` the same way (absent file → `LoggedOut`, unparseable → `Unavailable`).
Claude, Codex, and Pi expose a separate display-only binary version probe (`<program> --version`) that feeds the same account cache and repairs entries whose login facts are fresh but whose version field is absent; an absent version bypasses the long success TTL only after the short retry TTL, so a binary that cannot report `--version` does not re-fork on every producer frame.
An unknown kind has no probe arm yet and reads as `LoggedOut`.

Every registered adapter exposes a display-only version probe by default: run `<kind> --version`, capture stdout, and treat any failure as no version. Rich context transports still win when present — Claude's statusline and Codex's app-server context can report fresher versions for live sessions — while the CLI probe fills idle or older account-cache entries. A future adapter overrides only when its binary name differs from its kind or when it has a cheaper/richer idle version source.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../../crates/rimz/src/ledger/snapshot/view/providers.rs) folds accounts and balances into the dashboard view-model — one [`SidebarProviderPanel`](../../../crates/rimz/src/ledger/snapshot/view.rs) per kind.
It is **producer-only**: it needs per-machine config and the out-of-band probe the pure reducer cannot read, so the reducer leaves `providers` empty and every consumer tab reads the producer's published panel (see [sidebar.md → State access](../sidebar/sidebar.md#state-access)).

Per panel:

- **Aggregate stats** — the per-provider `spending` (trailing 24h / 7d / 30d / 365d, summed fleet-wide across the kind's transcript history by `compute_spending`); `plan` and `version` taken from the freshest `context.observed_at`, the version falling back to the display-only binary probe when no session reports one. A kind with no live session still earns a panel when it has a probed login; recorded spend enriches an existing panel but never creates the provider section by itself.
- **Account** — `metered` and the `plan` label come from the kind's account (a live session's, or the probed idle one); a `plan` tier formats into a brand label (`max` → `Claude Max`, `pro` → `ChatGPT Pro`), and a missing account infers `metered` from whether windows were reported.
- **Brand style** — emblem art, color, and product name resolve from `[sidebar.providers.<kind>]` over the built-in defaults (claude clay, codex blue, pi forest green); an unknown kind gets neutral grey and no emblem. See [theme.md](../../reference/theme.md#provider-styling).
- **Balance windows** — the per-duration set chosen by `stable_windows` (below). A kind that declares no window surface but runs on a metered sibling subscription (Pi on OAuth, above) borrows the sibling kind's stable windows instead — same account, same bars, so the Pi block and the sibling's block read identically when both show.
- **Paid/API usage** — `ExtraCredits` from the shared credits cache is folded onto metered panels when `[accounts] oauth_usage` is enabled, then enriched with a display-only `[accounts.usage_limit_usd.<kind>]` ceiling if the provider did not report a cap. Unmetered API-key panels synthesize `ExtraCredits::Known` from the provider's trailing-month transcript spend and the same optional ceiling, so an API-key account can show `$used/$limit` without pretending the provider enforces that limit.

Today's JSONL spend decides which discovered panels survive the `[sidebar] max_provider_blocks` cap (default 3), and a token-only provider ranks on the same transcript-derived footing — the retained set then orders stably by kind: the panels are the dashboard's tabs ([sidebar.md → Provider dashboard](../sidebar/sidebar.md#provider-dashboard)), so the row never reorders as spend shifts. The account, rate-limit, and credits caches are user-scoped and single-flighted across rooms — the elected producer publishes shared `accounts.json`, `rate_limits.json`, and `credits.json`; consumers read them and never fork.

### Per-provider spend

A panel's today line — the `◎` session count then the token breakdown `◇ ↘ ↗ ◌` (integer magnitudes, the fleet-ledger rows' exact vocabulary, with cache creation folded into `↘` input) with the bold money-green `$` pinned right — is all today's transcript-history burn, read from `spending` — the per-provider [`SpendTally`](../../../crates/rimz/src/agents/spending.rs) `compute_spending` returns ([transcript.md → Cost history](./transcript.md#cost-history)). The producer attaches each kind's entry to its panel before sorting; the renderer reads `spending.today` (its `sessions` count, split fields, and `usd`), so a token-only provider like Codex (priced from tokens via [pricing.md](./pricing.md)) shows its dollars the same as a `costUSD`-logging one — there is no live-session aggregate to fall back to. The fleet-wide trailing-week/month `W:`/`M:` ledger rows (with session counts) are a separate, fleet-level read of `value_tally`, pinned below the panel. The spend is producer-only, like the rest of aggregation; it threads in as a plain map so the reducer stays I/O-free.

### Stable window selection

Balance is account-scoped, but the *freshest* session is not the truest reading: parallel sessions report the same window at slightly different instants, so "freshest wins" flickers between ticks.
[`stable_windows`](../../../crates/rimz/src/ledger/snapshot/view/providers.rs) instead groups every session's readings by `duration_mins` and picks each duration deterministically: it drops any reading whose reset has already passed (stale), then keeps the **most-drained survivor** (highest `used_percentage`, so the bar never over-promises remaining budget), and returns the set short→long.
Same inputs, same bars, regardless of which session reported last.

### Spent windows and paused rows

A window is **spent** at `used_percentage == 100`; it is currently limiting while its reset still sits ahead. The spent window paints the provider dashboard's budget bars; it does not park every agent of that kind. A row becomes `paused` only when that agent actually stopped mid-turn on a limit: a native turn-error certificate (`rate_limit` or `overloaded`) parks the affected running agent, and a stalled running agent uses a spent, unreset kind window as the fallback pause predicate. A `rate_limit` pause lifts to `failed` only after at least one spent window has reset and no known spent window for that kind remains unreset. Calm agents (`idle`/`success`) and actively progressing turns stay in their lifecycle status even when a budget bar reads empty. The rollup keeps each agent's true lifecycle status, and the projection and glyph live in [agent.md → Displayed status](./agent.md#displayed-status) and [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

### Not-started windows

These budgets are **sliding** windows: the clock starts on your first token, so until then the provider keeps `resets_at` slid a full window-length ahead. A window whose reset still sits ~a full window out has **not started** — and it is detected by that reset distance, not a `used_percentage` of 0, because a fresh window still reports ~1% used (the live Codex 5h reads `usedPercent: 1` with the reset a full 5h out). That ~1% is the floor: any usage **above** it means the window has clearly started, so only a window at or below the floor (0–1% used) is a not-started candidate — past that, the reset is a real countdown regardless of its distance. The dashboard omits the countdown for such a window (a near-full bar, no `↻`), so it reads "ready to start" rather than a misleading ticking placeholder. Display only — it touches no parking or correctness — and applies to every provider; the on-screen treatment is [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard).

### Persistence across idle sessions

A session ending or going idle would otherwise empty the dashboard, so the producer mirrors each resolved window into a user-scoped `rate_limits.json` cache (atomic write under a shared read-modify-write lock) and reads it back when no live session reports one:

- Before a window's reset, the last-known (most-drained) reading stands.
- Once a shorter window's reset passes while the longest cached window is still in the future, that shorter window has refilled — it shows full with the reset rolled one window-length forward, until a live reading overwrites it.
- Once the longest cached window's reset passes with no fresh reading, Rimz no longer knows the account balance — every cached bar for that provider shows as an unknown empty track until a live or out-of-band reading overwrites it.

Only live ground truth is persisted; the synthesized full or unknown window is a **read-time projection, never written**. The cache tracks login and drops a kind once it logs out.

Paid usage has a sibling `credits.json` cache with the same account-scoped shape and lock discipline. Provider-reported values are persisted when a local account-usage surface returns them; API-key spend projections are not persisted because they are already derivable from the transcript spending walk plus config. A stale or absent credits entry leaves the `ex` row unknown (`∞` value over a dim empty track), while a configured display ceiling can still scale the row if usage is known.

### Refresh cadences

Codex keeps its included balance current without a live turn through the read-only app-server path: active root session sidecars refresh at most once per 60 s (so a long turn's windows stay fresh), and a metered account with **no** live session refreshes the shared rate-limit and credits caches on that same cadence via `rimz codex refresh-rate-limits`. When the app-server path returns no account-usage reading and `[accounts] oauth_usage` is enabled, the helper falls back to the direct OAuth usage endpoint and publishes success or failure into `credits.json` so every workspace shares the retry pace.
Claude keeps idle/fallback windows and extra usage current through `rimz claude refresh-usage`, spawned by the producer for a metered Claude account on the same success/failure TTLs. A recent Claude statusline rate-limit reading remains the freshest window source; the helper merges windows only when no root Claude session has fresh statusline windows, and always may refresh the account-scoped `credits.json` entry.

## Adding a provider

A new agent earns an account block and balance bars by filling the two internal types from its own surfaces; everything downstream — aggregation, `stable_windows`, caching, the dashboard — is provider-agnostic and comes free.
The work mirrors [transcript.md → Adding a provider](./transcript.md#adding-a-provider) and [hooks.md → Adding an agent](./hooks.md#adding-an-agent):

1. **Fill `AgentAccount`** (plan + metered) on the session's `AgentContext` from its rich-context transport, and/or override [`AgentAdapter::probe_account`](../../../crates/rimz/src/agents/mod.rs) for the logged-in-but-idle case.
2. **Fill `AgentRateLimits`** (the windows, each with a `used_percentage`, reset instant, and `duration_mins`) on `AgentContext` from the transport.
3. **Optionally fill `ExtraCredits`** from a read-only provider account-usage surface; use `Disabled` only when the provider explicitly says paid extra usage is off.
4. **Optionally** register `[sidebar.providers.<kind>]` defaults (emblem, color, name).
5. **Stay best-effort** throughout: a missing fact is an omitted label, a `v?` placeholder, or an unknown budget track, never an error.

Golden the account mapping from a fixture probe payload and a fixture transport payload, including the logged-out and unparseable cases (the inline goldens in each adapter's `account.rs` are the model).

## What lives elsewhere

- **The on-screen look** — the mana bars, the `ex`/`api` paid-usage rows, the aligned grid, the `⇅ rc` flag, the exhausted-window and longer-window-gating rendering — is [the interface reference](../../interface/sidebar.md#zone-3--the-provider-dashboard).
- **The renderer's projection** of `providers` and where the dashboard sits in the sidebar is [sidebar.md → Provider dashboard](../sidebar/sidebar.md#provider-dashboard).
- **The transport plumbing** that carries the rich context — the statusline pipe and its wrap/restore, the Codex app-server connection ladder and broker — is [transcript.md](./transcript.md).
- **Storage** of `AgentContext` on the rollup and the sidecar fold-in is [agent.md → Rich context](./agent.md#rich-context-agentcontext).
