# Provider accounts and balances

A coding agent runs against a provider account: a login, on a plan, that may or may not be metered. Its balance has two tiers: included subscription windows that refill on their own clocks, and paid usage beyond them. This page owns how RimZ learns those facts, fuses them across sessions and sources, caches them across rooms, and acts on them (parking, auto-redeem, auto-continue). Transcript spend and the token price table are [spending.md](./spending.md).

The facts flow from sources, through shared caches, into one provider panel per login; the dashboard shows the room's current login per kind:

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

The native surfaces each provider reads are in its adapter page ([Per-provider mapping](#per-provider-mapping)); the raw endpoints are in the [upstream references](../../externals/agent-adapter/claude-reference.md#auth-surface). The dashboard on screen is [the interface reference](../../interface/sidebar.md#the-provider-dashboard).

## The model

Four account-scoped facts feed a panel. Account identity and included balance ride the session's [`AgentContext`](../../../crates/rimz/src/agents/context.rs) ([model.md](./model.md#rich-context)) or the out-of-band probe; the producer lifts them to the account at aggregation time. Paid usage and reset credits ride the shared `credits.json` cache.

| Fact | Type | Carries |
| --- | --- | --- |
| Account identity | [`AgentAccount`](../../../crates/rimz/src/agents/context.rs) | the non-secret `account_id`, the raw `plan` tier (`max`, `team`, `pro`), `metered`, the typed account scope, an optional `sub_provider` id, the probed version, and the credential file's mtime |
| Included balance | [`AgentRateLimits`](../../../crates/rimz/src/agents/context.rs) | ordered [`RateLimitWindow`](../../../crates/rimz/src/agents/context.rs)s, each with `used_percentage`, a typed `resets_at`, `observed_at`, a `source`, and either a `duration_mins` or a provider scope (stable id plus compact label) |
| Paid usage | [`ExtraCredits`](../../../crates/rimz/src/agents/credits.rs) | used USD, remaining USD, a limit, or a disabled state, each optional |
| Reset credits | [`ResetCredits`](../../../crates/rimz/src/agents/credits.rs) | Codex and Claude: the banked count, every known valid expiry, and the earliest expiry. Claude's claimability is separate from its banked count ([source and mapping](./adapter_claude.md#oauth-usage-probe)) |

A window's identity is its duration or its scope id. Duration windows sort short to long (`5h`, `7d`) and support reset roll-forward, not-started detection, pace, and surplus; a window with an unknown duration makes each of those claims fail closed. Named provider windows (Copilot's monthly `AIC`, Qwen's Alibaba windows) fuse by scope id, display their real reset, and never roll forward.

Paid usage keeps missing fields missing, so the renderer can show an unknown or uncapped `ex` row without inventing a cap. Disabled or exhausted paid usage does not make a parked turn terminal while a subscription window still has a future reset. Reset credits draw the compact dashboard marker, which shows the earliest expiry within a week and never blinks; for Codex its glyph also forecasts what [auto-redeem](#auto-redeem) would do. `rimz providers` lists the deadlines when credits are banked.

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

`crates/rimz/src/agents/login.rs::room_accounts` reads the room's `workspace.json` pins and delegates to `resolve_room_accounts`: per kind, the pin wins, else the trusted project's `[accounts]`, else the machine's live `[accounts.use]`, else `default`. `RoomAccounts::account`, `name`, `source`, and `login` return that kind's result; `LoginSource` names `pinned`, `project default`, `machine default`, or `provider default`. Only an unreadable record fails the whole resolution. Blocked project trust refuses inheriting kinds; undeclared names and unreadable machine core refuse only kinds needing them. A pin to `default` reads neither project nor machine core. Display readers omit a failed kind's active account and preserve healthy siblings; `names()` projects successful kinds only and must never be read as failed-kind-to-`default` fallback. Birth writes only requested pins; `accounts use` pins, `--reset` unpins, and `--global` preflights before changing the live machine selection. Legacy named `logins` become pins and legacy `default` entries inherit ([workspace record](../store.md#the-workspace-record)).

A provider home is an account, and every probe, claim, and cache entry on this page is keyed by its `LoginKey` (`claude@work`, `codex@default`) ([`agents/login.rs`](../../../crates/rimz/src/agents/login.rs)). Claude and Codex can declare named accounts (`[accounts.claude.<name>]`), each with its own credentials, selected by `CLAUDE_CONFIG_DIR` or `CODEX_HOME`. One reconciler, [`agents::account_links`](../../../crates/rimz/src/agents/account_links.rs), brings the account home to its link set at `accounts add` and in `run_exec` before the launch plan compiles; `compile` and `agents explain` never write. A `shared` account links every top-level entry of either home that is not unshared to the native home: a name is unshared when the adapter's `private_home_entries` hold it or when it is a lock or temp name, a dotted `.lock` or `.tmp` component past the start of the name, which no adapter restates, so its sessions and transcripts live there; a `standalone` one links the adapter's `shared_home_entries` alone and keeps its own. The adapter's `history_home_entries` are linked for a shared account even when neither home holds them yet, created in the native home first, so a first session never writes history into the account home. The reconciler holds one machine-wide lock per kind (`disk::paths::account_lock`), because every shared account of a kind moves entries into the same native home. A link into the native home at an unshared name is removed in either mode without consulting the live-agent count, since the provider must own that name per home, and is reported as a `ShareReport::warnings` line, which both `accounts add` and a launch print. It removes nothing but a link, sets a conflicting entry aside under `<home>/.rimz-aside/<UTC timestamp>/`, and refuses to set aside a directory, or a link to one, or to remove a stale link to one, while another agent stamped with the account has a pane attached and its recorded process is not dead (`room::other_live_agents_on`): a seat of the same batch that has not started and a row a rebirth recovered have no pane, and a launch that failed or was killed after its pane was bound stops counting once its process is gone, before the room records its end. Once that count is zero, the same gate asks the adapter for the live sessions a provider daemon holds under the account home (`RuntimeControlCapability::runtime_control_writes_history`), which answers one of three ways (`DaemonSessions`): clear proceeds, and a session count or an unknown answer refuses before any entry changes (`ShareErr::LiveDaemon`, `ShareErr::LiveDaemonUnlink`). A running daemon that holds no session does not block and is left running. Codex answers in two steps. Its own record, `<home>/app-server-daemon/app-server.pid`, decides whether a daemon runs: a missing or malformed record, or one that does not name a process that is alive, not a zombie, and started at the recorded time, is clear, and no socket is opened. For a confirmed process it reads `thread/loaded/list` over `<home>/app-server-control/app-server-control.sock`: an empty list is clear, ids are that many sessions, and no socket, a failed handshake or request, or an unrecognized reply is unknown, which refuses because the directory would move under a possible writer. Both directories are private entries, so the record and the socket belong to the account in both history modes, and the gate ignores `RIMZ_CODEX_APP_SERVER_SOCK`, which could name another account's daemon. A thread stays loaded for the unload delay after its last subscriber leaves ([upstream](../../externals/agent-adapter/codex-reference.md#read-methods)), so the gate keeps refusing for that long after a conversation closes. The loaded set is the evidence rather than the room's session rows: a daemon-served row has no pane, a closed remote conversation's row can outlive it, and a row's stamp is the room's default at creation, not the home of the daemon that served it. Three limits remain. The lock serializes reconcilers only, so an idle daemon can load a thread between its reply and the rename, and a conversation opened in that window writes into the directory that was set aside or behind the link that was removed. And only sessions block: an idle daemon may still write a log through a link the switch removes. And an idle daemon outlives the switch with the environment it started under: `forward_login_env` gives it `CODEX_SQLITE_HOME` only when the login sets it, which a shared login does and a standalone one does not, so after a switch to standalone its next conversation writes rollouts into the account home and, if the app-server keeps the variable it inherited (provider behaviour, not observed), database rows into the native home; after a switch to shared it runs without the variable. A `CODEX_SQLITE_HOME` the user exported reaches the daemon in either mode and does not change with the switch. Turning remote control off and on restarts the daemon under the new mode, which the old refusal's remedy did as a side effect. Every other kind answers clear. A converged home and a file-only change run neither check. A failure after a set-aside carries those lines on the error (`ShareErr::AfterSetAside`). Spend discovery resolves each pool base under `LoginCatalog::native_ambient`, as the reconciler does, so a walker started from an account pane still walks the native home as `default`. A shared login whose adapter names a database-home key (`CODEX_SQLITE_HOME`) launches with it set to the native home unless the ambient environment sets it, and the Codex app-server, daemon-control, and login-status children forward it with `CODEX_HOME`. `default` is the native home, and every other kind has only `default`. A room records explicit account pins; unpinned kinds follow project and machine defaults live, and changes leave running agents on their stamps. Panels are keyed by login: a non-default login labels its block `Claude · work`. Defaults and live root stamps select the account probes and caches, but the dashboard shows only the room's current login per kind: after a switch, the old login keeps its caches while its agents run and has no panel ([Producer aggregation](#producer-aggregation)). The existing substantive-account/session admission rule still excludes empty provider blocks. A kind in `provider_list` expands to all its panels, default first and then named logins in name order. Usage, carry episodes and aggregate diagnostics keep this same login identity, while spend and the account budget are keyed by the login's history pool (`LoginCatalog::pool`); the dashboard tab follows the selected card by kind, since the displayed snapshot carries one panel per kind.

The durable agent stamp is the login of the agent's latest process. The launch batch resolves `LaunchLogin::RoomDefault` as pin-or-live under the workspace lock or preserves `LaunchLogin::Pinned`; `session_login` resolves the allocated stamp at exec, so a switch between allocation and exec cannot change the process home. `None` means the native `default`, not today's room selection. Every fresh RimZ launch uses the room default for its provider, including peers and team members, unless its `SupervisedRunRequest::login` pins one. A `rimz subagents` child of its parent's kind launches `Pinned` on the parent's stamp, an unstamped parent counting as `default`; a child of another kind takes the room default for its own kind. Restart, resume, fork, subagent follow-up, and lane, team, and crash recovery reopen a session only when its stamp and the room's current default for its kind share a history pool (`LoginCatalog::pool`), then launch on that default: [`harness::resume::relaunch_login`](../../../crates/rimz/src/harness/resume.rs) answers the login or the mismatch, and every one of those paths builds its launch from the answer. Every one of them but crash recovery, and every fresh interactive launch, then runs `ProviderLogin::health` on that login before any write ([fleet.md](../harness/fleet.md#every-path-that-builds-a-launch-layout)). The wrapper's `agent.attached` carries the process login and moves the stamp to it, so a session resumed under another account of its pool is billed, parked, and preflighted as that account. An explicit reopen refuses with the `rimz accounts use` fix (a lane or team resume refuses the whole command); rebirth skips the session as `different account`. Supervised retries pin the preflighted login. Native in-process provider children inherit known parents only for the same provider at ingress, and late hooks adopting a provisional launch preserve its batch stamp. A session discovered in a provider home has no durable row until a hook or an attach creates one: its [local session observation](./instances.md#local-session-observations) carries the account it was discovered under, and the transient row a snapshot seeds for it takes that account, so restart and the in-use login set read it as the room's account rather than `default`. A prior row the observation binds to keeps its own stamp.

`RoomLoginSet::default_login` and `default_key` address new launches, and `current_keys` collects `default_key` over every kind for the dashboard. `with_agents` adds stamps of live roots (no `ended_at`, excluding provider-native subagents) to `in_use` and `keys_in_use`, together with the defaults. Unresolvable named stamps are omitted from that resolved set, fail-closed: they do not become native-home reads. Session-specific readers use the agent's own stamp instead of the room default. The Codex broker re-resolves the room default before each request and respawns its child when the login or credentials move; a context refresh for a Codex agent stamped with any other login bypasses the broker and connects under its own login, so no reading lands under another login's key. An operation resolves its login once, where it is first checked, and carries it as `LaunchLogin::Pinned` to every later gate, batch, and connection: supervised retries, preflight, and tier routing never re-read the room default mid-operation.

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
| `Unavailable` | the probe could not complete: a binary that would not run, a non-zero exit that does not itself report a logout, an unreadable file | `ACCOUNTS_RETRY_TTL` (10 seconds); the provider keeps its last-known facts and retries alone |

An adapter with no out-of-band login surface returns `LoggedOut`. The probe itself is a pure read; memoization lives in the producer's `accounts.json` publication. Each provider's mechanics (Claude's `claude auth status`, Codex's and Pi's auth-file read, Antigravity's verified loopback service) are in its adapter page.

Every registered adapter also gets a display-only version probe. It locates the declared executable, runs `<binary> --version`, and hands stdout and stderr to the adapter to normalize. The default parser accepts only a conventional numeric version and abstains on banners; Copilot, Amp, and Cursor recognize their branded build strings. Antigravity disables the probe because invoking `agy --version` is unsafe for idle enrichment, and a manifest plugin uses its configured probe command and parser. Account and version subprocesses share a 3-second deadline (`INFORMATIONAL_PROBE_TIMEOUT`), and a timeout counts as `Unavailable`. The version shown is the newest among the probe's and every live context's, so a session still running an older binary never replaces a newer one. A provider whose version is absent re-probes on the retry TTL, never per frame.

## Producer aggregation

[`SidebarSnapshot::with_provider_aggregates`](../../../crates/rimz/src/store/snapshot/view/providers.rs) folds sessions, probed accounts, and spend into one [`SidebarProviderPanel`](../../../crates/rimz/src/store/snapshot/view.rs) per login, for the logins its caller names and no others. It runs in the machine-config fold, `fold_machine_config_with` in [`sidebar/enrich.rs`](../../../crates/rimz/src/sidebar/enrich.rs), because it needs per-machine config and probe results the pure reducer cannot read; the reducer leaves `providers` empty, and producer and consumers alike fold the producer's published account and spend caches ([state.md](../sidebar/state.md#projections)). The fold takes a `PanelScope`:

- `Current`: the room's current login per kind (`RoomLoginSet::current_keys`). The snapshot every renderer paints and `provider_panels_from_caches`, the `rimz providers` fold, use it. A kind whose current login does not resolve gets no panel. `rimz providers` still lists every account, because it folds once per account under a selection naming that account.
- `InUse`: every in-use login (`keys_in_use`). Only the producer's scoped fold in `refresh_heavy_lanes` uses it, so rate-limit fusion, account usage, credits, and auto-redeem keep acting on every login a live agent runs on.

Ordering, `max_provider_blocks`, and the tab decision run over the panels in scope, so a hidden login takes no slot. A login in scope earns a panel when it has a live session, or when its probed account is metered, has a non-empty `account_id`, or has recorded spend in the past year. Spend alone never creates a panel for a provider with no probed account, and a credentials-only unmetered account stays hidden until its first recorded session. An agent launched through an elevation wrapper as another uid stays out of every panel: its credentials live under that user's home, so the sidebar shows it as a flagged process row.

Each panel field has one source:

| Field | Source |
| --- | --- |
| `plan` | the session context with the newest `observed_at`, falling back to the probed account; `format_plan_label` brands the raw tier (`max` becomes `Claude Max`, `pro` becomes `ChatGPT Pro`, other kinds title-case it) |
| `version` | the newest version among the login's session contexts and the probed account, by [`newest_version`](../../../crates/rimz/src/agents/version.rs): a string that parses as a numeric `CliVersion` outranks one that does not, the greater `CliVersion` wins, and the greater string breaks what remains, so the answer does not depend on which session reported last |
| `metered`, `account_scope` | the same account; a missing `metered` is inferred true when live windows exist or a keyed session owns the panel |
| `account_key` | the newest-registered keyed session, by `registered_at` then session id |
| `windows` | `fresh_windows` over the admitted sessions, then fused with cache ([Window fusion](#window-fusion)); cleared on an unmetered panel |
| `window_placeholders` | the spec's `expected_windows`, painted as empty tracks before the first reading |
| `extra_credits`, `reset_credits` | the `credits.json` entry, or a local API spend projection |
| `redeem_forecast` | [`project_redeem_forecasts`](../../../crates/rimz/src/harness/auto_redeem.rs) over the cached capacity and burn rate, only on a Codex panel carrying `reset_credits` ([Auto-redeem](#auto-redeem)) |
| `spending` | the [`SpendTally`](./spending.md#what-reads-the-totals) of the login's history pool (`by_login`, keyed by pool) from `provider-spending.json`, so `default` and every shared login of a kind show one figure; `rimz providers` falls back to the same entry when a login has no panel |
| brand art and color | `AgentSpec` color and name, the embedded emblem catalog, and `[theme.providers.<kind>]` overrides; an unknown kind gets neutral grey ([theme.md](../../guide/theme.md#provider-styling)) |

A color override keeps the catalog's tint runs, while an `ascii_art` override paints its art in the single brand color.

### Which sessions speak for a panel

Live windows are partitioned by [birth account key](./model.md#the-rollup) before fusion, so a session registered under another login contributes no usage to the account the panel shows. The panel admits every session carrying its `account_key` plus every keyless session, and excludes sessions with a different key. A panel whose sessions are all keyless stays keyless and excludes nothing. `plan` and `version` ignore this partition: `plan` comes from the freshest context, `version` from the newest version any of the login's sessions reports. The keyed `metered` inference keeps cached authoritative windows attached while a newly registered session has not reported its first window.

### Paid and API rows

A metered panel takes `ExtraCredits` from its `credits.json` entry. An unmetered panel synthesizes `ExtraCredits::Known` from the login's trailing-month transcript spend. Either way, when the provider reported no cap, the display ceiling from `[accounts.usage_limit_usd]` (a map such as `claude = 50.0`) applies: a capped row drains by the spend and shows remaining dollars, and an uncapped API key paints a full `∞` bar. The ceiling is display only; no provider enforces it.

A credits entry older than `CREDITS_DISPLAY_MAX_AGE` (24 hours), absent, or scope-mismatched leaves the `ex` row unknown (an `∞` value over a dim empty track). API spend projections are never persisted, since the spending walk and config rebuild them.

### Panel order and the cap

With no explicit `provider_list`, a usage rank orders panels: a live room session first, then trailing-week, month, and year session counts, then a qualifying probed account, then the credential file's mtime, with registry order breaking ties. The mtime comes only from file-based probes. The rank decides both paint order and which panels survive the stacked dashboard's `max_provider_blocks` cap (default 3). A tabbed dashboard is bounded by its active block and shows every provider; its tab rail keeps the highest-ranked tabs that fit and always keeps the active one. An explicit `provider_list` sets the set and order and bypasses the cap, and `"all"` expands the remaining providers in rank order ([theme.md](../../guide/theme.md#display)).

### The shared caches

The account, rate-limit, and credits caches live under `~/.rimz/cache/providers/`, with single-flight locks under `$XDG_RUNTIME_DIR/rimz/shared/`.

| Cache | Keyed by | Holds |
| --- | --- | --- |
| `accounts.json` | `logins` | probed `AgentAccount` per login, and the probe's `login` outcome (`logged_in`, `logged_out`; absent after a failed probe) |
| `rate_limits.json` | `entries` (login) | fused windows, account scope, bound account key and its authoritative copy, pending refills, the unknown-episode marker; schema gated by `RATE_LIMITS_CACHE_VERSION` |
| `credits.json` | `logins` | paid usage, Codex plan, Codex and Claude reset credits, the usage owner identity, and the direct-query claim |

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

Fused account-wide windows for each login feed provider-limit display and [parked-turn](#spent-windows-and-paused-rows) decisions. Per-session `context.rate_limits` feeds fusion and the agent card, never a decision by itself.

### Not-started windows

Included windows are sliding: the clock starts on the first token, and until then the provider keeps `resets_at` a full window-length ahead. A window has not started when its reset sits about a full window out and its usage is at or below the 1% floor (`FRESH_WINDOW_USAGE_FLOOR`); a fresh Codex 5h window reports 1% used, so a zero check would miss it. Any usage above the floor means the window has started, and its reset is a real countdown.

The dashboard draws a not-started window as a full bar with no `↻` countdown, which reads as ready to start ([interface](../../interface/sidebar.md#the-provider-dashboard)). This is display only.

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

An all-unknown panel is also a refresh trigger. When a login's panel holds no usable value (an aged-out ceiling, expired named quotas, or a cold start with no cache), the producer forces that login's direct query instead of waiting out the cadence. The entry's `unknown_since_ms` marker makes the force fire once per unknown episode; the usage claim keeps the fetch single-flight, and completion restamps the read on success and failure alike, so a provider that stays unreachable falls back to ordinary cadence. A usable window clears the marker.

A panel meets its cached entry under the same rule as [session admission](#which-sessions-speak-for-a-panel): the entry's bound key and the panel's key must be equal, or one must be absent. A keyed mismatch clears the live windows for that frame, so the cached account's truth paints instead of fusing with another account's usage. Only an authoritative account read binds an entry's key, and it fuses onto a prior entry only when scope and key both match, so an account switch writes a fresh entry. A keyed entry also retains its matching authoritative windows separately for `Bound` launch controls. The producer's own publication never binds a key: it republishes fused truth under the prior entry's key, keeping that entry's bound copy. An in-use login whose panel disappears loses its cache entry; entries outside the room's in-use set are untouched.

### The credits cache and usage identity

`credits.json` has the same login keying and lock discipline as `rate_limits.json`. It persists provider-reported paid usage, Codex plan, and Codex and Claude reset-credit fields, so partial observations survive idle sessions. Reset credits keep every known valid expiry in ascending order, equal deadlines included; the earliest is the compact summary, and the provider count stays authoritative when detail is absent or malformed.

Identity keeps the cache honest across account changes. Every [`AccountUsageProbe`](../../../crates/rimz/src/agents/credits.rs) result (found, no credentials, or failed) carries one `AccountUsageIdentity`: the non-secret owner and scope of the credentials read. `AccountUsageSnapshot` holds only the normalized plan, windows, paid credits, and reset credits. Pi and OpenCode select their delegated owner once through [`delegated_account.rs`](../../../crates/rimz/src/agents/delegated_account.rs), preferring an OpenAI account id and otherwise hashing the refresh or access token under an adapter-specific domain.

Completion compares owners symmetrically. `None` to `Some`, `Some` to `None`, two different identified owners, or two different scopes each block carrying the prior plan, paid usage, and reset credits, and drop the kind's cached windows. A failed read with no known owner change keeps the prior display data; a failed read that proves a new owner drops prior truth without publishing unverified values.

## Refresh cadences

The producer keeps each metered account current between turns when its spec declares `direct_account_usage`, whether or not a session is live. It spawns one hidden `rimz agents refresh-usage` helper per login, and only after `credits.json` grants that login a durable direct-query claim. The account fold admits helpers from the panels it just built, so a newly discovered login with no live session starts its first read in the same heavy pass.

An idle Codex account (one the machine catalog declares, `codex@default` included, that is neither a room default nor a live root stamp) has no panel, and [auto-redeem's expiry rescue](#auto-redeem) is its one reader. The same lane claims it through `claim_idle_account_usage`: the ordinary claim, held to `IDLE_OAUTH_USAGE_TTL` since the last attempt by any room, whatever that attempt's outcome. A transient failure, a settled auth failure, and a changed credential stamp all wait the floor, so a failing or logged-out spare account is not retried on the in-use tiers, and an account another room keeps fresh is never fetched here. An account config that does not load declares no idle account.

Two CLI commands are readers outside the producer: `rimz providers` and `rimz accounts list` probe every listed login and call `refresh_provider_usage` for each logged-in one, on the same durable cadence and claim path unless `rimz providers --refresh` forces the read. The list also probes again each login whose stored record says logged out or has the ambiguous legacy shape below, so a login made since the last probe shows on the next run.

A record's `login` field is what says whether the account is logged in. The account alone cannot: a logout probed while the kind has a live agent keeps the CLI version, the same bytes as a login the provider reports no facts about. `ProviderStatus::from_record` is the one reader. For a record an older build wrote without the field it infers from `ok` and the account, and reads that version-only shape as unavailable; the launch gates and `rimz providers` treat it as unknown until the next probe, and `rimz accounts list` probes it again on the spot.

The claim is what makes the helper safe across rooms. Under the shared credits lock, scheduling derives the claim from the published account scope and credential stamp plus the prior same-scope usage owner, and records a UUID nonce, claim time, requested scope, and the optional stamp and owner. The helper receives the nonce and the `LoginKey` in its request. Before any provider call it re-reads the room's defaults and cached agent rollup, and cancels the claim only when the login is neither in use (a default or a resolvable live root stamp) nor an idle account. For an idle account it skips the realtime leg, which asks this room's Codex app server and so speaks for the room's own login, and runs the direct probe alone. Only the worker holding the matching nonce resolves credentials, contacts the provider, and publishes. Because the claim and `oauth_read_at_ms` share one lock, rooms admit one fetch; a failed spawn cancels its claim, and an expired claim becomes retryable.

| Clock | Value | Governs |
| --- | --- | --- |
| `OAUTH_USAGE_TTL` | 5 minutes | the ordinary refresh |
| `OAUTH_USAGE_SETTLED_TTL` | 1 hour | a login whose last read settled as an auth failure (missing or rejected credentials) and whose credential stamp is unchanged |
| `IDLE_OAUTH_USAGE_TTL` | 3 hours | an idle account, after every outcome |
| `ACCOUNT_USAGE_CLAIM_TTL` | 90 seconds | the lease on each helper segment |

A changed published scope, credential stamp, or account key reopens the claim at once. `RIMZ_OAUTH_USAGE_OFFLINE=1` disables account-usage fetches for the process tree; `0`, `false`, and an empty value read as off.

The helper runs two segments. First it folds a realtime account reading when the adapter has one (Codex's app-server) and publishes it to the rate-limit cache, then renews the claim. Then it runs the direct query and publishes again. A missing, replaced, or lock-contended claim cannot renew, and the direct segment does not start; completion re-checks the nonce so a superseded writer is rejected. Both segments publish an `AccountUsageSnapshot`: windows go through `fuse_window`, and plan, paid usage, and reset credits go through one cache conversion. Direct-query windows are authoritative and merge after the realtime fold, so a fresh credential read replaces a stale realtime process. A detached writer waits on the bounded rate-cache lock and publishes before returning; the producer's per-frame path falls back to a non-blocking read under contention.

Fusion can pull the next read forward. When the producer persists a new reset epoch, parks a new pending refill, or first shows an unknown panel, it clears `oauth_read_at_ms`, the settled state, and any stale claim, so scheduling in the same pass re-probes. An already pending refill does not force another read; a successful authoritative response clears it, so a later contradictory low reading can park and request again.

Direct account-usage HTTP ([`agents/credits.rs`](../../../crates/rimz/src/agents/credits.rs)) retries transport failures, body-read failures, and 5xx responses up to three attempts with a 300 ms backoff. Redirects are disabled, 401 and 403 both count as authentication rejection, and 429 or any other status returns without retry. Errors never include response bodies or request headers. A surfaced failure reports off-box under the one `oauth_usage` operation with a `provider` tag, and the error detail names the request host ([diagnostics.md](../diagnostics.md)).

## Spent windows and paused rows

A window is spent at `used_percentage == 100` while its reset is still ahead. A spent window paints the budget bars; it does not park every agent of the kind.

A row becomes `paused` only when that agent stopped mid-turn on a limit or a transient server error. A native turn-error certificate (`rate_limit`, `spend_limit`, or `overloaded`) parks the running agent it names. A stalled running agent with no certificate parks when the fused login window is spent and unreset. `overloaded` covers provider overload and transient 5xx errors, and has no reset clock.

The stall fallback reads only account-wide windows for the source agent's stamped login. It excludes scoped windows that carry a parent duration, so a model sub-cap (a spent Fable share) never parks another Claude model. Durationless named quotas keep their spent-and-reset behaviour, including promoting a limit marker to `failed` after the quota resets with no spent window left to explain it. Auto-continue likewise looks up capacity by the agent's login, not the room default. Fresh launch availability resolves the room default before reading capacity, account status and daily caps; a supervised launch resolves its own login per kind instead (`LaunchAvailability::read_as`).

A `rate_limit` or `spend_limit` pause stays resumable while the fused account has a subscription window with a future reset, including the common spend-limit case where paid usage is disabled or exhausted but the window will refill. After the window recovers, the row stays parked while the [auto-continue](#auto-continue) record has a chance to wake the turn, instead of a frozen 100% reading turning into a spurious `!`.

Calm agents (`idle`, `success`) and progressing turns keep their lifecycle status even when a bar reads empty. The displayed-status ladder is [model.md](./model.md#displayed-status), and the glyphs are [the interface legend](../../interface/sidebar.md#reading-the-glyphs).

Quota readings and locally priced dollars are best-effort control inputs. RimZ parks and resumes local panes against them and enforces user-configured soft caps, but neither is a provider billing statement or a provider-enforced limit.

### Auto-redeem

Auto-redeem spends a Codex reset credit when doing so buys capacity for an agent that is waiting on it; the policy is [`harness/auto_redeem.rs`](../../../crates/rimz/src/harness/auto_redeem.rs). The producer evaluates the cached windows and credits each frame, together with whether an agent of this room is stopped on that login's limit, and returns the first verdict that matches ([`redeem_verdict`](../../../crates/rimz/src/harness/auto_redeem.rs)).

What a redemption does to the window is a fact on the credit, set by each adapter's normalization (`ResetCredits::effect`). As observed by the user: redeeming a Codex credit refills the window and restarts its natural reset a full window from the redemption (`RestartsWindow`); redeeming a Claude credit refills the window and leaves the reset date where it was (`KeepsSchedule`). The verdict branches on that fact and names no provider. A credit cached before the field existed reads as `RestartsWindow`.

| Step | Verdict |
| --- | --- |
| a credit is within 30 minutes of expiry (always on, either effect, since an unused credit vanishes) | `ExpiryRescue` |
| `[resume] auto_redeem = false` | none |
| the gate is closed: no limit-paused agent, or no unscoped duration window spent with a future reset | none |
| `RestartsWindow`, and the spent window's latest natural reset is at least `auto_redeem_min_gain` away (default `12h`) | `BlockedGain` |
| `RestartsWindow`, and the chain deadline below has passed and the near free reset does not defer it | `ScheduledRedeem` |
| `KeepsSchedule`, and the soonest credit would keep less than 24 hours of life after the spent window's latest reset | `DoomedCredit` |
| `KeepsSchedule`, and that reset is at least `auto_redeem_min_gain` away | `BlockedGain` |

A row counts as limit-paused for a login when it is a live root row (not ended, not a provider subagent), its login is that login, it carries no RimZ [budget park](../harness/budget.md#the-park), and its displayed turn error is a provider limit (`TurnErrorClass::is_limit`). That last test is `auto_continue::limit_marker_active`, the same predicate auto-continue arms on, so a credit is spent only where a refill gives auto-continue (or the user) a turn to wake. An agent parked on the same login in another room does not open the gate here, and an agent parked only on a model sub-cap leaves the account windows unspent, so it does not either.

A missing reset closes the gate; a missing expiry disables only the doomed-credit and rescue clauses. A `RestartsWindow` credit never redeems as doomed: the redemption restarts the reset, so spending it ahead of a near free reset forfeits that reset. A `KeepsSchedule` credit never schedules: the pacing below assumes a window that restarts.

The schedule spaces credits by predicted refill time, and because it sits behind the gate it fires only into a spent window whose reset is nearer than `auto_redeem_min_gain`. The producer samples growth in each in-use login's longest duration window into the user-shared `auto_redeem_rate.codex@<account>.json` every pass, whatever the gate says, and a three-day half-life EWMA predicts how long a fresh window takes to fill. The sorted expiry list yields backward chain deadlines spaced by that refill time. A deadline is never earlier than one predicted refill after the projected longest window began, so a redemption or a natural reset pushes the next attempt out instead of spending more credits into a fresh window. A longest-window reset less than `auto_redeem_min_gain` away defers the deadline when the first credit still outlives it by 24 hours. Missing window timing or a negligible burn rate collapses scheduling to the 30-minute rescue.

For any verdict, the elected producer spawns a detached helper under each in-use Codex login whose panel yields a verdict, and the request carries the producer's limit-paused evidence (`AutoRedeemRequest::limit_paused`; a payload without it reads as not paused, so only a rescue can fire). The helper resolves the login from the room when it is in use (the room's default or a resolvable live root stamp in the cached rollup) and from the machine catalog when it is idle, cancels its reservation only when the key is neither, takes the user-shared `auto_redeem.codex@<account>.lock`, refreshes windows and per-credit details, re-evaluates the verdict with the producer's evidence unchanged, and consumes the soonest-expiring credit with an idempotency key. It trusts the producer for the agent half of the gate and re-checks the window half on fresh capacity: a window that recovered between spawn and consume yields no verdict. The user-shared `auto_redeem.codex@<account>.json` stamp throttles attempts to one per 10 minutes and holds 30 minutes after a success, across every room. Each attempted consume appends its account, evidence, and outcome to the [assist log](../harness/loops.md#the-assist-log). A success returns the refreshed usage to the helper's CLI entry, which republishes authoritative usage and reset credits at once through the sidebar's one account-usage publication entry, then appends the assist record and wakes the room, so [auto-continue](#auto-continue) can wake parked turns on the recovered capacity.

The expiry rescue also covers idle accounts, so a credit on an account no room is using does not expire unspent while any room is open. `refresh_heavy_lanes` hands `redeem_credits` each idle account's cached reset credits from `credits.json` (`idle_reset_credits`), dropping an entry that is not `ok` or is older than `CREDITS_DISPLAY_MAX_AGE`, the staleness line a panel's credits are held to. An idle account is evaluated by `idle_rescue` for `ExpiryRescue` only: the producer always sends `limit_paused = false`, and the helper judges an idle login as idle whatever its request carried, so a login that stopped being in use between spawn and consume continues under the idle rule. The helper, and only the helper, skips the rescue on positive evidence in its fresh read that the window is unused: a kind-wide capacity whose every duration window, projected to now, reads 0% used or has [not started](#not-started-windows), since a fresh Codex window reports 1% (`ProviderCapacity::known_unused`). Absent capacity, an account-scoped entry, or a window with no percentage is unknown, and the rescue fires. The producer never skips on its cached windows, which can be three hours old and predate usage from another machine; it spawns on the cached expiry alone. An idle account samples no burn rate and gets no forecast. Because the cached expiry is a timestamp, the three-hour idle read cadence still reaches the 30-minute lead; its cost is that a newly granted credit is discovered up to three hours late.

The sidebar's Codex header marker is a forecast of this policy. `project_redeem_forecasts` runs at both display folds, the main enrich and `provider_panels_from_caches` (the `rimz providers` fold), after the budget views. It reads the same per-login capacity and burn-rate caches the producer evaluates and writes neither. It runs `redeem_verdict` on a hypothetical in which the longest duration window is spent at its projected reset and has parked an agent: any verdict is `Armed`, none is `Holding`, `auto_redeem = false` is `Manual`, and a capacity without a dated longest window is `Armed`, since no hold can be proven. The forecast takes no agent input: the `rimz providers` fold has no agents, and a forecast gated on a live park would read `Holding` at every moment except the frame before a redeem. One hold is firm: when the hypothetical natural reset is under `auto_redeem_min_gain` away and the soonest credit outlives it by 24 hours, the scheduling tail's defer check holds, advancing time only brings that reset closer, and the forecast stays `Holding` until the reset while the verdict on the real capacity stays silent. A hold for a credit that does not outlive the reset by a day can turn `Armed` once its chain deadline passes.

Manual [`rimz accounts redeem`](../../reference/cli/accounts.md#redeem) supports Codex and Claude. It prepares one fresh provider action without locking, previews it, then locks only after confirmation and consumes that same action. A provider hold refuses before confirmation or reservation without changing the banked count. Claude alone reads its organization profile during preview and sends a single, non-retried claim on confirmation ([private action](./adapter_claude.md#oauth-usage-probe)); its preview omits the Codex-only forecast. Automatic Claude redemption remains unarmed. Both kinds use the same machine-wide lock and stamp even outside a room, but never apply `stamp_allows_attempt`: the preview keeps the stamp it read, and the locked tail refuses when the stamp differs from it or is a live reservation (no outcome, younger than ten minutes). A Codex `reset` stamp still holds auto-redeem's thirty-minute post-success cooldown. The shared consume tail reserves before spending and stamps every outcome; reason `manual` distinguishes its credit-ledger record, whose outcome may include `cooldown`. A claim error leaves its reservation standing. A post-reset refresh failure returns success with a warning rather than inviting a retry. The CLI publishes refreshed usage, appends the record, and wakes the current room; outside a room, sidebars pick up the shared caches on their next refresh. A manual stamp is unreadable to older binaries, which treat it as absent and may skip their cooldown once during an upgrade window.

### Auto-continue

With `[resume] auto_continue = true` (off by default), the producer resumes a [parked turn](#spent-windows-and-paused-rows) by delivering the configured nudge (`continue` by default) to the agent's pane through the same send path as `message --steer`; the agent's next hook moves the row back to `running`. The trigger and the `rimz agents auto-continue` helper that sends are [`harness/auto_continue.rs`](../../../crates/rimz/src/harness/auto_continue.rs), and the pure arm decision is `resume_park`.

A spent window supplies a reset clock only after the turn carries a provider-certified rate-limit marker; account exhaustion, a stall, or message text never certify why a turn stopped. Antigravity has identity-bearing quota clocks but no certified recoverable stop class, so its quota never arms auto-continue.

**Arm.** Each frame an agent is parked on a resumable certificate, the producer writes a durable `ParkRecord` with the park class and the agent's frozen `last_activity`. A rate-limit record captures the spent window's reset deadline, so the resume survives the session sidecar it was seen through: a 5h or 7d window can outlive the session, and the post-reset reading gives the record one frame to nudge. A backoff record carries the turn-error marker time and retry state. A RimZ [budget park](../harness/budget.md#the-park) with a reset time is checked first and short-circuits the provider classification: it arms a `Budget` record whose deadline is the park's `resets_at`. If the record is lost after the window recovers while the agent still carries a limit marker, the producer re-arms a due-now record before firing.

**Fire.** Once the reset deadline or backoff step is due and `last_activity` has not advanced, the producer spawns the helper, which queues and delivers a resume-gated message. Overload and transient API-error parks follow `auto_continue_backoff_secs` (default `[180, 300]`), whose last value repeats: the first attempt lands after 3 minutes and later ones every 5. Message events carry the queued, sent, delivered, timed-out, or failed trace, and after delivery the [assist log](../harness/loops.md#the-assist-log) keeps the park time, verdict, handle, and message id.

**Clear.** Activity since the park or a delivered resume message removes the record. When activity advances but a provider limit still holds and a matching-card resume message was enqueued at or after the park baseline, the producer instead carries the park forward: it rebases `last_activity` while preserving the original attempt anchor and retry state. Real progress that clears the limit marker clears the park as before.

**Exhaust.** All park classes share `auto_continue_max_retries` (default 12), counted as distinct `DeliveryGate::Resume` prompt message ids enqueued since the attempt anchor (`attempts_since`, or `parked_at_activity` before a rebase). The evidence merges the rollup's retained terminal outcomes (every one for seven days) with the live queue; helper spawns and pre-queue crashes only pace retries. A nudge's limit reply preserves that count even when it advances the user-turn timestamp. Real progress ends the episode, so a later park gets a fresh allowance. Rate-limit nudges also wait at least 120 seconds after any matching-card resume message, even after the park record is cleared. At the default overload ramp, attempts span about 58 minutes before the row promotes to actionable `failed`.

## Daily dollar caps

`[accounts.budget] <kind> = "100/day"` sets one cap per history pool of that kind (`default` and its shared accounts together, each standalone account alone), shared by every room on it. A kind is eligible only when its adapter's `AccountSpend` concern is wired to authoritative account-level dollar history. Identity, a plan label, quota windows, point-in-time prices, and partial transcript estimates do not qualify. Strict config parsing and room start reject an ineligible kind with the exact key to remove; `rimz budget --account KIND` validates only the targeted kind. Fleet reports in `cli/budget.rs::inspect` omit ineligible keys and print one warning per key on stderr after the table, including after fleet writes; the ledger ignores stale unsupported config.

The decision input is the local-day spend window the [spending walk](./spending.md#what-reads-the-totals) publishes per history pool, which the producer accepts at the cache's normal staleness. The ledger, verdict, park, and waiver are [budget.md](../harness/budget.md). On the dashboard, a healthy cap stays quiet; while agents are parked on a crossed account cap, the headline turns alarm-red and appends `$used of $cap/day`.

## Per-provider mapping

Each adapter page maps its native surfaces onto these types; this table is the index.

| Provider | Account identity → `AgentAccount` | Balance → `AgentRateLimits` / `ExtraCredits` |
| --- | --- | --- |
| Claude | [`claude auth status`](../../externals/agent-adapter/claude-reference.md#auth-surface) → plan, metered | statusline 5h/7d windows, and the OAuth usage query for windows and banked limit resets ([adapter_claude.md](./adapter_claude.md#account-and-balance)) |
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
- [the interface reference](../../interface/sidebar.md#the-provider-dashboard): bars, the `ex` and `api` rows, and exhausted-window rendering.
- [sidebar.md](../sidebar/sidebar.md#provider-dashboard): where the dashboard sits in the renderer.
- [budget.md](../harness/budget.md): dollar-cap ledgers, verdicts, and waivers.
