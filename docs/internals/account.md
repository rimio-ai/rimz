# Agent accounts and balances

> See [DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance) for the account-scoped-budget invariant this doc operationalizes, [transcript.md](./transcript.md) for the transport plumbing that carries the rich context this doc interprets, and [the interface reference](../interface/sidebar.md#zone-3--the-provider-dashboard) for what the provider dashboard looks like on screen.

A coding agent runs against a **provider account** — a login, on a plan, that may or may not be metered — and that account has a **balance**: the rate-limit windows the plan draws against.
This doc owns both: the account/balance model, where each provider's facts come from, how they map onto Rimz's internal types, and how the producer folds them into the provider dashboard.

It is the **single home for account/balance enrichment**.
Provider auth surfaces (`claude auth status`, `~/.codex/auth.json`, the app-server `account/rateLimits/read` method) and the metered/unmetered/plan semantics appear here and nowhere else — every other doc speaks only the internal types: [`AgentAccount`](../../crates/rimz/src/agents/context.rs) and [`AgentRateLimits`](../../crates/rimz/src/agents/context.rs), and the [`SidebarProviderPanel`](../../crates/rimz/src/ledger/snapshot.rs) the renderer paints.

Account and balance are **enrichment, never correctness** — the no-transcript-correctness rule.
A missing binary, a logged-out account, an unparseable file: each degrades to an omitted plan label or a blank bar, never a failed snapshot or a wrong decision.

## The model

Two facts, both **account-scoped** — every session of one provider kind shares them, so the dashboard reads them per kind and never paints them per row:

- **Account identity** — [`AgentAccount`](../../crates/rimz/src/agents/context.rs): the raw `plan` tier the provider reports (`max`, `team`, `pro`) and a `metered` flag.
- **Balance** — [`AgentRateLimits`](../../crates/rimz/src/agents/context.rs): a `five_hour` and a `seven_day` `RateLimitWindow`, each a `used_percentage` plus a typed `resets_at` instant a renderer formats as a countdown.

Both ride [`AgentContext`](../../crates/rimz/src/agents/context.rs), the session-scoped rich-context record (see [agent.md → Rich context](./agent.md#rich-context-agentcontext)); the producer lifts them to the account scope at aggregation time.

**Metered vs. unmetered** is the one distinction the dashboard turns on.
A subscription or ChatGPT login is *metered*: it draws on the rate-limit windows, drawn as draining "mana" bars.
An API-key login is *unmetered*: it has no budget to drain, drawn as the `∞` bar.
`metered: None` is unknown — the dashboard infers metering from whether any rate-limit window was reported.

## Two origins

A kind's account and balance reach the dashboard two ways, mirroring the [transcript two-source split](./transcript.md#two-sources):

1. **A live session's rich context.** The statusline / app-server transport already carries account and balance, so any live session of a kind fills both at no extra cost. The transport plumbing is [transcript.md](./transcript.md)'s concern; this doc owns only what its fields *mean*.
2. **An out-of-band probe** ([`account.rs`](../../crates/rimz/src/agents/account.rs)). For a provider that is logged in but has no live session this run, the producer probes the login directly, so the dashboard shows your accounts and budgets between turns — not only mid-turn.

A live session always wins where both exist: its reading is richer and current.

## Per-provider mapping

Each provider maps its native account and balance surfaces onto the two internal types. A new provider implements one or both rows of this table and the rest of Rimz is unchanged.

| Provider | Account identity → [`AgentAccount`](../../crates/rimz/src/agents/context.rs) | Balance → [`AgentRateLimits`](../../crates/rimz/src/agents/context.rs) |
| --- | --- | --- |
| **Claude** | `claude auth status` JSON: `subscriptionType` → `plan`, `authMethod` → `metered` (an `apiKey` login is unmetered). | The statusline blob's rate-limit windows, parsed by [`observe_context`](../../crates/rimz/src/agents/statusline.rs). |
| **Codex** | Live: the app-server `account/rateLimits/read` `planType` → `plan`, riding `AgentContext.account` with no extra spawn. Idle: the *shape* of `~/.codex/auth.json` — an `OPENAI_API_KEY` is unmetered (`∞`), a `tokens` block is a metered ChatGPT login (its plan tier filled once a session reports it). | The app-server `account/rateLimits/read` 5h/7d windows ([`codex_app_server.rs`](../../crates/rimz/src/agents/codex_app_server.rs)). |

One asymmetry shapes the producer below: **Claude's balance has no source outside a live statusline**, while **Codex's rides a read-only out-of-band app-server call**, so a logged-in idle Codex account can still refresh its windows but an idle Claude account cannot.

## The out-of-band probe

[`account::probe(kind)`](../../crates/rimz/src/agents/account.rs) returns an `AccountProbe` with three arms, and the arm — not just the value — drives the producer's cache TTL:

- **`Found(AgentAccount)`** — a resolved login. Authoritative; rides the long success TTL.
- **`LoggedOut`** — the probe ran and confidently found no login. Also authoritative (it changes about never), so it caches like a success.
- **`Unavailable`** — the probe could not complete: a binary that would not run, a non-zero exit, an unreadable file. Transient; retried on the short failure TTL rather than pinning the dashboard empty for the full success window.

The probe is a **pure read**; cross-process memoization lives one layer up in the producer (below).
Claude forks `claude auth status`, capturing stdout with stdin and stderr nulled (never inherited, so it stays quiet in a TUI); Codex reads `~/.codex/auth.json` — a cheap file read, no subprocess, so an absent or corrupt file is an authoritative `LoggedOut`, not a retry-worthy `Unavailable`.
An unknown kind has no probe arm yet and reads as `LoggedOut`.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../crates/rimz/src/ledger/snapshot.rs) folds accounts and balances into the dashboard view-model — one [`SidebarProviderPanel`](../../crates/rimz/src/ledger/snapshot.rs) per kind.
It is **producer-only**: it needs per-machine config and the out-of-band probe the pure reducer cannot read, so the reducer leaves `providers` empty and every consumer tab reads the producer's published panel (see [sidebar.md → State access](./sidebar.md#state-access)).

Per panel:

- **Aggregate stats** — spend, tokens, and lines summed across the kind's sessions (zero for an idle, session-less kind); `plan` and `version` taken from the freshest `context.observed_at`.
- **Account** — `metered` and the `plan` label come from the kind's account (a live session's, or the probed idle one); a `plan` tier formats into a brand label (`max` → `Claude Max`, `pro` → `ChatGPT Pro`), and a missing account infers `metered` from whether windows were reported.
- **Brand style** — emblem art, color, and product name resolve from `[sidebar.providers.<kind>]` over the built-in defaults (claude clay, codex blue, pi forest green); an unknown kind gets neutral grey and no emblem. See [configuration.md](../reference/configuration.md#sidebar-provider-dashboard).
- **Balance windows** — each `5h`/`7d` window chosen by `stable_window` (below).

Blocks sort by spend and cap at `[sidebar] max_provider_blocks` (default 3). The account cache and the probe are single-flighted on the elder like the diff stats — the producer publishes `accounts.json`; consumers read it and never fork.

### Stable window selection

Balance is account-scoped, but the *freshest* session is not the truest reading: parallel sessions report the same window at slightly different instants, so "freshest wins" flickers between ticks.
[`stable_window`](../../crates/rimz/src/ledger/snapshot.rs) instead picks each window deterministically across every session of the kind: it drops any reading whose reset has already passed (stale), then keeps the **most-drained survivor** (highest `used_percentage`, so the bar never over-promises remaining budget).
Same inputs, same bar, regardless of which session reported last.

### Persistence across idle sessions

A session ending or going idle would otherwise empty the dashboard, so the producer mirrors each resolved window into an account-scoped `rate_limits.json` cache (atomic write, single-flighted, reaped with the workspace runtime dir like the other caches) and reads it back when no live session reports one:

- Before a window's reset, the last-known (most-drained) reading stands.
- Once the reset instant passes with no fresh reading, the window has refilled — it shows full with the reset rolled one window-length forward, until a live reading overwrites it.

Only live ground truth is persisted; the synthesized full window is a **read-time projection, never written**. The cache tracks login and drops a kind once it logs out.

### Refresh cadences

Codex has the read-only out-of-band app-server read, so the producer keeps its balance current without a live turn: it refreshes active root Codex session sidecars at most once per 60 s (so a long turn's windows stay fresh), and a metered Codex account with **no** live session refreshes the shared cache on that same cadence via `rimz codex refresh-rate-limits`.
Claude's windows have no source outside a live statusline, so Claude never qualifies for this pull path — an idle Claude account shows its last cached windows until they refill.

## Adding a provider

A new agent earns an account block and balance bars by filling the two internal types from its own surfaces; everything downstream — aggregation, `stable_window`, caching, the dashboard — is provider-agnostic and comes free.
The work mirrors [transcript.md → Adding a provider](./transcript.md#adding-a-provider) and [hooks.md → Adding an agent](./hooks.md#adding-an-agent):

1. **Fill `AgentAccount`** (plan + metered) on the session's `AgentContext` from its rich-context transport, and/or add an out-of-band arm to [`account::probe`](../../crates/rimz/src/agents/account.rs) for the logged-in-but-idle case.
2. **Fill `AgentRateLimits`** (the 5h/7d windows with reset instants) on `AgentContext` from the transport.
3. **Optionally** register `[sidebar.providers.<kind>]` defaults (emblem, color, name).
4. **Stay best-effort** throughout: a missing fact is an omitted label or a blank bar, never an error.

Per [testing.md](../contributing/testing.md), golden the account mapping from a fixture probe payload and a fixture transport payload, including the logged-out and unparseable cases (the inline goldens in [`account.rs`](../../crates/rimz/src/agents/account.rs) are the model).

## What lives elsewhere

- **The on-screen look** — the mana bars, the `∞` bar, the aligned grid, the `⇅ rc` flag, the exhausted-window and weekly-cap rendering — is [the interface reference](../interface/sidebar.md#zone-3--the-provider-dashboard).
- **The renderer's projection** of `providers` and where the dashboard sits in the sidebar is [sidebar.md → Provider dashboard](./sidebar.md#provider-dashboard).
- **The transport plumbing** that carries the rich context — the statusline pipe and its wrap/restore, the Codex app-server connection ladder and broker — is [transcript.md](./transcript.md).
- **Storage** of `AgentContext` on the rollup and the sidecar fold-in is [agent.md → Rich context](./agent.md#rich-context-agentcontext).
</content>
</invoke>
