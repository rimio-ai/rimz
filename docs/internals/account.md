# Agent accounts and balances

> See [DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance) for the account-scoped-budget invariant this doc operationalizes, [transcript.md](./transcript.md) for the transport plumbing that carries the rich context this doc interprets, and [the interface reference](../interface/sidebar.md#zone-3--the-provider-dashboard) for what the provider dashboard looks like on screen.

A coding agent runs against a **provider account** — a login, on a plan, that may or may not be metered — and that account has a **balance**: the rate-limit windows the plan draws against.
This doc owns both: the account/balance model, where each provider's facts come from, how they map onto Rimz's internal types, and how the producer folds them into the provider dashboard.

It is the **single home for account/balance semantics**: what the metered/unmetered/plan facts mean, and how the producer folds them onto the internal types [`AgentAccount`](../../crates/rimz/src/agents/context.rs), [`AgentRateLimits`](../../crates/rimz/src/agents/context.rs), and the [`SidebarProviderPanel`](../../crates/rimz/src/ledger/snapshot/view.rs) the renderer paints.
The raw auth surfaces it reads — `claude auth status`, `~/.codex/auth.json`, `~/.pi/agent/auth.json`, the app-server `account/rateLimits/read` response — are in the per-provider reference: [adapter/claude-reference.md](./adapter/claude-reference.md#auth-surface), [adapter/codex-reference.md](./adapter/codex-reference.md#auth-file), and [adapter/pi-reference.md → Auth file](./adapter/pi-reference.md#auth-file) (which also records Pi's missing balance surface).
[adapter/opencode-reference.md → Auth file](./adapter/opencode-reference.md#auth-file) mirrors OpenCode's auth surface ahead of its adapter — credential types only, with the same missing balance surface.

Account and balance are **enrichment, never correctness** — the no-transcript-correctness rule.
A missing binary, a logged-out account, an unparseable file: each degrades to an omitted plan label or a blank bar, never a failed snapshot or a wrong decision.

## The model

Two facts, both **account-scoped** — every session of one provider kind shares them, so the dashboard reads them per kind and never paints them per row:

- **Account identity** — [`AgentAccount`](../../crates/rimz/src/agents/context.rs): the raw `plan` tier the provider reports (`max`, `team`, `pro`), a `metered` flag, and — for a multi-provider client (Pi) — the raw `sub_provider` credential id (`anthropic`, `openai`) naming the subscription the account runs on.
- **Balance** — [`AgentRateLimits`](../../crates/rimz/src/agents/context.rs): an ordered list of [`RateLimitWindow`](../../crates/rimz/src/agents/context.rs)s (short→long), each a `used_percentage`, a typed `resets_at` instant a renderer formats as a countdown, and a `duration_mins` that names the window. Both Claude and Codex report a 5-hour and a 7-day window. The duration drives the bar label (`5h`/`7d`) and the reset-to-max roll-forward, so no window kind is hard-coded — a provider's windows are whatever it reports, and a server-side change in their count or length renders gracefully (a transient Codex bug once widened its window to ~30 days; it painted a labeled `30d` bar rather than misrendering).

Both ride [`AgentContext`](../../crates/rimz/src/agents/context.rs), the session-scoped rich-context record (see [agent.md → Rich context](./agent.md#rich-context-agentcontext)); the producer lifts them to the account scope at aggregation time.

**Metered vs. unmetered** is the one distinction the dashboard turns on.
A subscription or ChatGPT login is *metered*: it draws on the rate-limit windows, drawn as draining "mana" bars.
An API-key login is *unmetered*: it has no budget to drain, drawn as the `∞` bar.
`metered: None` is unknown — the dashboard infers metering from whether any rate-limit window was reported.

## Two origins

A kind's account and balance reach the dashboard two ways, mirroring the [transcript two-source split](./transcript.md#two-sources):

1. **A live session's rich context.** The statusline / app-server transport already carries account and balance, so any live session of a kind fills both at no extra cost. The transport plumbing is [transcript.md](./transcript.md)'s concern; this doc owns only what its fields *mean*.
2. **An out-of-band probe** ([`AgentAdapter::probe_account`](../../crates/rimz/src/agents/mod.rs), one `account.rs` per adapter behind the shared [`AccountProbe`](../../crates/rimz/src/agents/account.rs) contract). For a provider that is logged in but has no live session this run, the producer probes the login directly, so the dashboard shows your accounts and budgets between turns — not only mid-turn.

A live session always wins where both exist: its reading is richer and current.

## Per-provider mapping

Each provider maps its native account and balance surfaces onto the two internal types. A new provider implements one or both rows of this table and the rest of Rimz is unchanged.

| Provider | Account identity → [`AgentAccount`](../../crates/rimz/src/agents/context.rs) | Balance → [`AgentRateLimits`](../../crates/rimz/src/agents/context.rs) |
| --- | --- | --- |
| **Claude** | [`claude auth status`](./adapter/claude-reference.md#auth-surface) → `plan` + `metered`; `apiKey` login is unmetered. | Statusline [`rate_limits`](./adapter/claude-reference.md#statusline-json) 5h/7d windows, parsed by [`observe_context`](../../crates/rimz/src/agents/claude/statusline.rs). |
| **Codex** | Live: [`account/rateLimits/read`](./adapter/codex-reference.md#app-server-api) `planType` → `plan`, riding `AgentContext.account`. Idle: [`~/.codex/auth.json`](./adapter/codex-reference.md#auth-file) — API key → unmetered, `tokens` → metered ChatGPT (plan tier filled once a session reports it). | [`account/rateLimits/read`](./adapter/codex-reference.md#app-server-api) `primary`/`secondary` windows, each with `windowDurationMins` ([`app_server.rs`](../../crates/rimz/src/agents/codex/app_server.rs)). |
| **Pi** | [`pi/account.rs`](../../crates/rimz/src/agents/pi/account.rs) reads [`auth.json`](./adapter/pi-reference.md#auth-file): oauth → metered subscription, api_key → unmetered. The plan names the sub the fleet uses (`Anthropic OAuth`, `OpenAI API Key`) — the freshest session's `message.provider` picks among several credentials, else the first OAuth entry — the raw credential key rides `sub_provider`, and the probe attaches the `pi -v` binary version, the panel header's fallback for a provider whose sessions report none. | Borrowed — pi exposes no rate-limit window surface of its own (declared off via `rate_limit_windows`), but a metered OAuth sub *is* a sibling kind's account, so the producer maps `sub_provider` to the kind metering it ([`kind_for_sub_provider`](../../crates/rimz/src/agents/registry.rs): each descriptor declares its `sub_providers` — `anthropic` → claude, `openai` → codex) and the Pi panel paints that kind's stable windows. Nothing to borrow (no sibling kind, no readings) renders no bars; an API key the `∞` bar. |

One asymmetry shapes the producer below: **Claude's balance has no source outside a live statusline**, while **Codex's rides a read-only out-of-band app-server call**, so a logged-in idle Codex account can still refresh its windows but an idle Claude account cannot.

## The out-of-band probe

[`AgentAdapter::probe_account`](../../crates/rimz/src/agents/mod.rs) returns an [`AccountProbe`](../../crates/rimz/src/agents/account.rs) with three arms, and the arm — not just the value — drives the producer's cache TTL:

- **`Found(AgentAccount)`** — a resolved login. Authoritative; rides the long success TTL.
- **`LoggedOut`** — the probe ran and confidently found no login. Also authoritative (it changes about never), so it caches like a success.
- **`Unavailable`** — the probe could not complete: a binary that would not run, a non-zero exit, an unreadable file. Transient; retried on the short failure TTL rather than pinning the dashboard empty for the full success window.

The probe is a **pure read**; cross-process memoization lives one layer up in the producer (below).
Claude forks `claude auth status`, capturing stdout with stdin and stderr nulled (never inherited, so it stays quiet in a TUI); Codex reads `~/.codex/auth.json` — a cheap file read, no subprocess, so an absent or corrupt file is an authoritative `LoggedOut`, not a retry-worthy `Unavailable`.
Pi reads `~/.pi/agent/auth.json` the same way (absent file → `LoggedOut`, unparseable → `Unavailable`) and forks `pi -v` for the version — a failed version read leaves the field empty without downgrading the outcome.
An unknown kind has no probe arm yet and reads as `LoggedOut`.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../crates/rimz/src/ledger/snapshot/view.rs) folds accounts and balances into the dashboard view-model — one [`SidebarProviderPanel`](../../crates/rimz/src/ledger/snapshot/view.rs) per kind.
It is **producer-only**: it needs per-machine config and the out-of-band probe the pure reducer cannot read, so the reducer leaves `providers` empty and every consumer tab reads the producer's published panel (see [sidebar.md → State access](./sidebar.md#state-access)).

Per panel:

- **Aggregate stats** — the per-provider `spending` (trailing 24h / 7d / 30d / 365d, summed fleet-wide across the kind's transcript history by `compute_spending`); `plan` and `version` taken from the freshest `context.observed_at`, the version falling back to the probed account's binary read (Pi's `pi -v`) when no session reports one. A kind with no live session still earns a panel when it has recorded spend or a probed login.
- **Account** — `metered` and the `plan` label come from the kind's account (a live session's, or the probed idle one); a `plan` tier formats into a brand label (`max` → `Claude Max`, `pro` → `ChatGPT Pro`), and a missing account infers `metered` from whether windows were reported.
- **Brand style** — emblem art, color, and product name resolve from `[sidebar.providers.<kind>]` over the built-in defaults (claude clay, codex blue, pi forest green); an unknown kind gets neutral grey and no emblem. See [configuration.md](../reference/configuration.md#provider-dashboard).
- **Balance windows** — the per-duration set chosen by `stable_windows` (below). A kind that declares no window surface but runs on a metered sibling subscription (Pi on OAuth, above) borrows the sibling kind's stable windows instead — same account, same bars, so the Pi block and the sibling's block read identically when both show.

Blocks sort by spend and cap at `[sidebar] max_provider_blocks` (default 3) — ranked by a panel's today JSONL spend, so the provider you're actively spending on leads and a token-only provider ranks on the same transcript-derived footing. The account cache and the probe are single-flighted on the elder like the diff stats — the producer publishes `accounts.json`; consumers read it and never fork.

### Per-provider spend

A panel's today line — the token breakdown `◇ ↘ ↗ ◍ ◌` (integer magnitudes) with the bold money-green `$` pinned right — is all today's transcript-history burn, read from `spending` — the per-provider [`SpendTally`](../../crates/rimz/src/agents/spending.rs) `compute_spending` returns ([transcript.md → Cost history](./transcript.md#cost-history)). The producer attaches each kind's entry to its panel before sorting; the renderer reads `spending.today` (its split fields and `usd`), so a token-only provider like Codex (priced from tokens via [pricing.md](./pricing.md)) shows its dollars the same as a `costUSD`-logging one — there is no live-session aggregate to fall back to. The fleet-wide trailing-week/month `W:`/`M:` ledger rows (with session counts) are a separate, fleet-level read of `value_tally`, pinned below all the panels. The spend is producer-only, like the rest of aggregation; it threads in as a plain map so the reducer stays I/O-free.

### Stable window selection

Balance is account-scoped, but the *freshest* session is not the truest reading: parallel sessions report the same window at slightly different instants, so "freshest wins" flickers between ticks.
[`stable_windows`](../../crates/rimz/src/ledger/snapshot/view.rs) instead groups every session's readings by `duration_mins` and picks each duration deterministically: it drops any reading whose reset has already passed (stale), then keeps the **most-drained survivor** (highest `used_percentage`, so the bar never over-promises remaining budget), and returns the set short→long.
Same inputs, same bars, regardless of which session reported last.

### Spent-window verdict → the rate-limited head

A window is **spent** at `used_percentage == 100` with its reset still ahead ([`RateLimitWindow::is_spent`](../../crates/rimz/src/agents/context.rs)). When any window of a kind is spent, the sidebar parks every `idle`, `success`, and `running` agent of that kind to the derived `rate_limited` status — account-scoped, so a session that just launched into a spent account is parked too, and a `running` session whose turn died on the limit parks rather than reading as wedged. This is display, not correctness: the rollup keeps each agent's true lifecycle status, and the projection and its glyph live in [agent.md → The state machine](./agent.md#the-state-machine) and [the interface legend](../interface/sidebar.md#reading-the-glyphs).

### Not-started windows

These budgets are **sliding** windows: the clock starts on your first token, so until then the provider keeps `resets_at` slid a full window-length ahead. A window whose reset still sits ~a full window out has **not started** — and it is detected by that reset distance, not a `used_percentage` of 0, because a fresh window still reports ~1% used (the live Codex 5h reads `usedPercent: 1` with the reset a full 5h out). That ~1% is the floor: any usage **above** it means the window has clearly started, so only a window at or below the floor (0–1% used) is a not-started candidate — past that, the reset is a real countdown regardless of its distance. The dashboard omits the countdown for such a window (a near-full bar, no `↻`), so it reads "ready to start" rather than a misleading ticking placeholder. Display only — it touches no parking or correctness — and applies to every provider; the on-screen treatment is [the interface reference](../interface/sidebar.md#zone-3--the-provider-dashboard).

### Persistence across idle sessions

A session ending or going idle would otherwise empty the dashboard, so the producer mirrors each resolved window into an account-scoped `rate_limits.json` cache (atomic write, single-flighted, reaped with the workspace runtime dir like the other caches) and reads it back when no live session reports one:

- Before a window's reset, the last-known (most-drained) reading stands.
- Once the reset instant passes with no fresh reading, the window has refilled — it shows full with the reset rolled one window-length forward, until a live reading overwrites it.

Only live ground truth is persisted; the synthesized full window is a **read-time projection, never written**. The cache tracks login and drops a kind once it logs out.

### Refresh cadences

Codex has the read-only out-of-band app-server read, so the producer keeps its balance current without a live turn: it refreshes active root Codex session sidecars at most once per 60 s (so a long turn's windows stay fresh), and a metered Codex account with **no** live session refreshes the shared cache on that same cadence via `rimz codex refresh-rate-limits`.
Claude's windows have no source outside a live statusline, so Claude never qualifies for this pull path — an idle Claude account shows its last cached windows until they refill.

## Adding a provider

A new agent earns an account block and balance bars by filling the two internal types from its own surfaces; everything downstream — aggregation, `stable_windows`, caching, the dashboard — is provider-agnostic and comes free.
The work mirrors [transcript.md → Adding a provider](./transcript.md#adding-a-provider) and [hooks.md → Adding an agent](./hooks.md#adding-an-agent):

1. **Fill `AgentAccount`** (plan + metered) on the session's `AgentContext` from its rich-context transport, and/or override [`AgentAdapter::probe_account`](../../crates/rimz/src/agents/mod.rs) for the logged-in-but-idle case.
2. **Fill `AgentRateLimits`** (the windows, each with a `used_percentage`, reset instant, and `duration_mins`) on `AgentContext` from the transport.
3. **Optionally** register `[sidebar.providers.<kind>]` defaults (emblem, color, name).
4. **Stay best-effort** throughout: a missing fact is an omitted label or a blank bar, never an error.

Per [testing.md](../contributing/testing.md), golden the account mapping from a fixture probe payload and a fixture transport payload, including the logged-out and unparseable cases (the inline goldens in each adapter's `account.rs` are the model).

## What lives elsewhere

- **The on-screen look** — the mana bars, the `∞` bar, the aligned grid, the `⇅ rc` flag, the exhausted-window and longer-window-gating rendering — is [the interface reference](../interface/sidebar.md#zone-3--the-provider-dashboard).
- **The renderer's projection** of `providers` and where the dashboard sits in the sidebar is [sidebar.md → Provider dashboard](./sidebar.md#provider-dashboard).
- **The transport plumbing** that carries the rich context — the statusline pipe and its wrap/restore, the Codex app-server connection ladder and broker — is [transcript.md](./transcript.md).
- **Storage** of `AgentContext` on the rollup and the sidecar fold-in is [agent.md → Rich context](./agent.md#rich-context-agentcontext).
</content>
</invoke>
