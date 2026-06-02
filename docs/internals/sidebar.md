# Sidebar

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes, and [the interface reference](../interface/sidebar.md) for what the sidebar looks like on screen — the cockpit, the cards, and the provider dashboard, frame by frame. This doc is the mechanics: how presence, attention, layout, and recovery are computed.

The sidebar is a UI client over the workspace ledger that owns no durable state. It reads through `rimz sidebar snapshot`, writes only its own liveness heartbeat in-process (`rimz::sidebar::write_heartbeat`, a runtime-file write), and never imports a ledger-writer module.

One renderer paints every moment of a session — a bare shell, the first agent, a waiting prompt, a fleet across worktrees, detach and reattach. What changes between those moments is the snapshot, never the renderer. For the felt, phase-by-phase walk-through, see [the experience guide](../guide/experience.md).

## The view-model and its renderers

**The snapshot is the shared view-model, and every decision is made once, here.** The `rimz sidebar snapshot` JSON carries the worktree-grouped row roster (every live pane as a row, agents enriched from the ledger), the attention items, per-agent status and capability, the ranking keys, and timestamps. A renderer is a *projection*: it maps those semantics to glyphs and paints them. It never re-derives which worktree a row belongs to, how rows rank, or which pane runs an agent.

The snapshot command runs in the `rimz` binary, which owns mux access, so it enumerates the session's panes and folds them into the roster before serializing. Every renderer is then a pure JSON consumer — pane presence reaches the screen through the snapshot, never through a mux call of the renderer's own. (The lone exception is metadata-only: the native renderer may list panes on resize to count its own siblings for [self-close](#self-close), and that count never updates the rendered snapshot.)

Three renderers project the same snapshot:

- **Native pane (default).** The `rimz-sidebar` binary in an ordinary pane — identical on Zellij and tmux, across detach/reattach. The default and the cross-backend fallback.
- **Zellij plugin rail (optional upgrade).** A docked, persistent left rail for Zellij users who opt in. See [Zellij plugin rail](#zellij-plugin-rail-optional-upgrade).
- **CLI listings.** `rimz feed list` and friends — the same data as text.

The per-renderer code is just painting, so visual parity is a maintained discipline rather than shared render code. The one convention they all share is the semantic→glyph mapping — the table drawn in full in [the interface legend](../interface/sidebar.md#reading-the-glyphs), with its design rationale in [DESIGN.md → Attention at a glance](../../DESIGN.md#attention-at-a-glance). The mechanic this doc owns: the glyph carries state by **shape** so it survives `NO_COLOR`, and color only reinforces it. Keep the rail, the pane, and the CLI aligned to that table.

## Presence model

**Row presence comes from the live pane list, not the ledger.** The snapshot enumerates the session's panes, reads each pane's foreground command and cwd, resolves the cwd to a worktree, and emits **one row per pane** — no pane id is ever shared by two rows. A pane running `zsh` is a dim [process row](#process-rows); a pane an agent stamped is that agent's row. The sidebar's own pane is excluded — it is chrome, not work.

**One pane, one row, bound by the stamped pane id.** A pane's foreground command cannot name the agent — Claude runs under `node`, and two same-kind agents in one worktree read identically. Binding is by identity instead: every `agent.lifecycle` event carries the mux pane id the hook ran inside (`TMUX_PANE` / `ZELLIJ_PANE_ID`), and the snapshot binds each live pane to the one agent that stamped that exact id. Command and cwd never bind a row — a shell the agent dropped back to, or a `git` it spawned, stays a process row. The agent's ledger identity (status, posture, task, model, effort, context enrichments) then enriches that one row.

**The Codex daemon exception.** Codex under remote control fires its hooks from the pane-less app-server daemon (why it is pane-less: [transcript.md → Appendix Codex](./transcript.md#appendix--codex)), so it stamps no pane. As a last resort — only after the stamped-id and host checks miss — a pane-less Codex agent binds the live `codex` pane whose cwd equals its worktree (`codex_for_pane`); two in one worktree resolve most-recently-active, and the [rollup reaper](./agent.md#liveness-and-presence) collapses the stale one. The match is exact-worktree and Codex-only. A pane-less *Claude* agent is always stamped while live, so a missing pane means it is genuinely gone — it is never rescued. A wired-but-unprompted Codex (it registers its session lazily) shows a synthesized idle `○ codex` row until the first turn binds the real session; an *unwired* Codex stays a process row.

**Paneless agents do not render.** An agent with no live pane — a subagent, a ghost a kill left in the rollup, a relaunch the [reaper](./agent.md#liveness-and-presence) hasn't collapsed — is data, not presence. It cannot resurrect a row or latch onto a stranger's pane (the Codex daemon bind above is the lone, same-worktree exception). So an exited agent never lingers: its pane closes (gone next snapshot) or reverts to a shell row. There is no `offline` status.

**Attention folds onto the agent's pane.** A pending agent ask belongs to the agent's pane, so the snapshot folds the session's single most-relevant pending ask onto that agent's row (`? waiting`, or a braille resolver spinner) rather than adding a second row — a session never stacks more than one row. It un-folds once the agent records activity past the ask, so an ask answered in the agent's own UI returns the row to `running` (see [agent.md → Liveness](./agent.md#liveness-and-presence)). If the agent's pane is absent the ask leaves the sidebar but stays in audit (`rimz feed list --audit`). A script's blocking `feed ask` has no agent pane to fold onto, so it keeps its own standalone row while the waiter is alive.

### Honest reads across a mux hiccup

`list-panes` is a round-trip the mux occasionally answers with an empty body, or with a live pane missing its command or cwd. Two read-side guards keep a momentary glitch from reaching the screen as a lie:

- **Degraded reads carry pane fields forward.** Rather than relabel a known pane as an anonymous row that blinks out next tick, the snapshot backfills any dropped field (command, cwd, process-start) from the last good read of that *exact* pane id — so a reused id reports its own fresh fields. The repair is surgical, and unbounded while the pane persists.
- **The renderer never paints a regression over a stable pane set.** If a transient race the carry-forward missed — a momentarily agentless rollup, a half-written frame — would demote an agent's stamped-pane row to a bare `process`, the renderer holds the last good frame rather than commit the regression, as long as the live pane set is unchanged. A count- and time-bounded escape hatch releases the hold so a *genuine* demotion still surfaces promptly. This is the read-side twin of the ledger's lock-serialized rollup ([Runtime projection](./ledger.md#runtime-projection)): the lock keeps an agent from vanishing at the source, the gate keeps a one-frame slip from reaching the screen.

When pane discovery itself fails, the renderer keeps the last good snapshot and, once the failure persists, raises the sticky health alert rather than inventing an empty room (see [Reload recovery](#reload-recovery)).

## Ranking and grouping

### Worktree groups

A worktree is total isolation — only same-worktree agents collaborate — so each group is one bounded block under a bold-teal header (the spines and seal are drawn in [the interface reference](../interface/sidebar.md#worktree-groups-and-the-selection-lane)). Two rules the renderer enforces:

- **The header is a jump target** — clicking the name lands on the group's first row — and carries **no status tally**: the cockpit owns the fleet make-up, and each row carries its own glyph.
- **Diff stats live on the header, not a row**, so shared-worktree changes never pretend to belong to one agent. They are the worktree's total change against trunk — committed, staged, and unstaged — from `git diff --numstat <merge-base with main> --`, so the number is what the branch added on top of its fork point.

A pane's group is decided by membership in the project's worktrees: a cwd at or under the project root (including `<root>/.claude/worktrees/*`), or inside any checkout `git worktree list` reports (even one parked outside the root), belongs to that worktree. The snapshot enumerates the worktree roots (cached under the diff-stats TTL) and the reducer runs a lexical containment test against the project root and each worktree root. A cwd that is neither — a home shell, `/tmp`, CI not tied to a worktree — folds into a catch-all that renders as a dim `┄ external ┄┄┄` divider, sorts last unless it holds a waiting/failed agent, and keeps an attention-only tally (`? n` / `! n`). When the git probe finds no worktrees the reducer falls back to the project-root prefix alone.

### Attention ranking and the per-worktree cap

**One principle: the most attention-hungry rises, and nothing else moves.** The status comparator is `status_rank` in [`snapshot.rs`](../../crates/rimz/src/ledger/snapshot.rs); the order it encodes:

- Within a worktree, rows sort by status bucket: `waiting` → `failed` → `idle` → `success` → `running`. A working agent is the least attention-hungry, so it settles to the bottom.
- The attention buckets (`waiting`, `failed`) sort **oldest-first**: a blocked agent's `last_activity` is frozen, so the longest-overdue rises and `␣` always lands on the oldest blocked pane.
- The calm buckets and bare process rows hold a **stable spawn order** keyed on `pane_process_start` — untouched by the activity heartbeat, so a working agent never jumps just because it finished a tool, and a new agent appends at the bottom of its bucket.
- Worktree groups sort by their most-urgent member; same-tier groups keep a stable order — project worktrees before the `external` catch-all, then earliest pane start, then label.

**The cap protects the calm tail only.** Each worktree shows at most `WORKTREE_ROW_CAP` rows with a dim `+K more`. The cap truncates only calm rows; every `waiting`/`failed` agent is exempt and always shown, so the cap can never hide something that needs you.

## Composing the frame

The sidebar is **borderless** — it already lives inside a framed mux pane, so a second border would double-frame it and cost two columns. A title line, faint `─` hairline rules, and a one-cell left gutter (which doubles as the selection lane) carry the structure instead. The on-screen layout is drawn frame by frame in [the interface reference](../interface/sidebar.md); this section owns the invariants behind it.

Two structural rules the layout depends on:

- **Fixed height where it counts.** The repo dashboard (identity, the `✦`/`✧` head-count, and fleet spend) and the cockpit make-up reserve their rows whether or not the room has agents, so the body never shifts vertically as agents change *state*. The footer and health alert are bottom-pinned chrome, reserved before the body so they can never scroll off.
- **Counts span the cap, totals span the fleet.** The cockpit buckets sum each group's `status_counts` — counting even agents the cap hides — and split `running` into thinking/working by the visible rows' posture; the `◷`/`◇`/`◆` totals sum the full agent list.

There are no feed-group sections: "Recently answered" and "Recent activity" do not exist here. The sidebar shows only what needs a decision or an action; full history lives in `rimz feed list --audit`. With nothing waiting or failed, the attention line is omitted and a dim first-run hint points at the real next step, keyed on the snapshot's `agent_hooks_ready` flag (an unwired room reads `install hooks: rimz hooks install claude`, a wired room reads `run claude or codex`). The hint clears the instant the first agent or feed item appears, and steps aside under an active health alert — an empty body under a failed fetch is a missing snapshot, not an empty room.

### Agent rows

Each agent is a small stacked card — identity and capability, then the description, the context meter, and (deeper) the token and work stats. The card anatomy and meter grammar are drawn in [the interface reference → the card](../interface/sidebar.md#the-card); the invariants behind it:

- **Selection only appends, never reshapes.** Every line of the resting card renders identically whether selected or not; selection paints the bold `▌` spine over the gutter and *appends* the deeper lines, so the card never reflows on expand. `[sidebar] density` (per-machine, default `compact`) sets the resting height — `compact` is identity + description + the `▣` context meter, `full` adds the token and work lines — and selecting any row always reaches the full card, so the deepest data is one keystroke away.
- **Capability is preferred, never synthesized.** Model display name and effort come from the session's rich [context](./agent.md#rich-context-agentcontext) when present, falling back to the coarse [transcript scalars](./transcript.md); a missing field means "the agent did not report it," never a zero. Model names shorten for the row (`Opus 4.8 (1M context)` → `Opus 4.8 (1M)`). The account-scoped 5h/7d budgets are **not** on the row — they live in the [provider dashboard](#provider-dashboard).
- **Line 1 carries no timestamp.** A blocked `?`/`!` reddens past the neglect window and a working agent's animated head signals liveness, so a stale row escalates by color in place rather than by a clock; the one coarse last-activity age lives on the expanded work line. Width tiers degrade line 1 — a narrow row keeps just the name, wider rows add the model, then effort and an inline todo — while the context meter keeps its label and value at every width.
- **Enrichments are display-only and privacy-gated**, and none drives attention or routing. Context severity is the worse of a fill-percentage ramp and an absolute-token overlay, so a large-window model green by percentage still warns by volume; while calm-green and the per-message breakdown is known, the bar splits into composition segments (cache writes, cache reads, fresh input), and once it warns it goes a single solid run. The `▣` glyph is decoupled from the bar — it tracks *total* window usage on its own ramp — so the bar shows where the tokens went while the glyph shows how full the window is. `NO_COLOR` suppresses color only; every bar's heavy/light and filled/hollow shape still carries the meter.

The rows read the fields on [`SidebarRow`](../../crates/rimz/src/ledger/snapshot.rs) plus the per-group `status_counts`, `diff_added`/`diff_removed`, and `commits_ahead`; that struct is the field catalog. Three projection rules are not evident from it: the renderer prefers the rich `context` blob over the coarse `model`/`effort`/`context_pct` scalars (and falls back when it is absent); an unnamed session's line 2 falls `session name → task → prompt → em dash`, so it keeps a label once the turn ends; and density rides `SidebarSnapshot.sidebar`, filled by the CLI like `project_root`.

#### Sub-agent lists

The expanded card lists the subagents the agent spawned this turn — each a status glyph plus its type (`⢿ Explore`, `○ review`). A subagent is paneless, so it never renders as its own row ([Presence model](#presence-model)); instead the rollup carries each child's `parent_agent_id` and the snapshot nests it under its parent at projection time. The list is **expanded-only**, so selection still only appends and the card never reflows. Retention is **turn-scoped**: a still-running child is always shown, but a *finished* child clears once the parent starts its next turn (its work predates the parent's `turn_started_at`). A child whose parent row is absent is an orphan and never renders.

### Process rows

A pane no agent has stamped renders dim: a lead glyph then the program it runs (`zsh`, `vim`, `cargo`). The program is read past a `sudo` wrapper and through a `node`/`npx` launcher to the script it runs, so `sudo npm install -g @openai/codex` reads as an `npm` install (never a codex agent) while `node …/codex` reads as the pre-enrichment codex host. The lead is a hollow `○` (the same idle glyph an agent shows, in the dim process tone) for an idle pane — a shell, an editor, or the pre-enrichment agent host (`node`/`claude`/`codex`) — and a dim braille spinner for a pane doing genuine work like a build or a test, so live work reads at a glance while staying secondary to an agent. An **active** pane anchors its primary line on the shell that owns it (its root process, read from `/proc/<pane_pid>/comm`) so the line stays put as commands come and go, and carries the live command in full on a dim second line. No capability line, no attention count; a process row never enters the cockpit tallies. It is still a jump target, and once an agent's hook stamps that pane id the same pane becomes that agent's row.

### Jump — the row is the link

You don't read where to go; you go. Selecting a row focuses that agent's pane via the `pane` ref on the snapshot — no mux pane number is ever printed. Both renderers share one key model:

- `↑/↓` select a row; `↵` jumps to the selected pane.
- `1`–`9` jump by the row's visible ordinal.
- `␣` jumps to the *next item that needs you* — the next `waiting`/`failed` row in ranking order — without first selecting it. The fleet-scale triage key, bound only inside the Rimz session so it never touches the user's global mux config.
- `x` dismisses the sticky [health alert](#reload-recovery) once it has recovered; an active failure re-arms it.
- `r` reloads the tab: when a freshly-installed build is on disk it re-execs the renderer in place (the keypress scope of `rimz reload`); otherwise it forces an immediate refetch, so `r` always pulls live data and un-sticks a tab whose producer has stalled.
- `?` toggles a legend-and-keys overlay, so the glyph vocabulary is learnable in place.

The native renderer focuses the bound pane directly, in process (`rimz::mux::backend_for(..).focus_pane`) on a detached thread; a click anywhere in a row's multi-line block jumps via a hit-test line map the renderer emits in lockstep with the body. **Every jump reconciles pane id *and* `pane_process_start`**, so a reused id never silently focuses a stranger — it self-corrects on the next snapshot rather than blocking the jump. The row routes you to the pane; the prompt and its approve/deny live in the agent's pane, where the full context is. A script's `feed ask` is the exception: it chose Rimz as its surface, so its declared options are answerable on the row.

#### How the highlight stays on the right pane

**Selection is keyed by pane identity, not row position** (`UiState::selected_pane`), and re-derived every fold (`app::reconcile_selection`), so a status-churn reorder re-anchors the highlight to the same pane. Which pane is selected is a contest between two timestamped values:

- `local_selection` — the pane and instant of the last local click or key.
- `external_focus` — the last *valid* external mux focus and the instant the producer sampled it. A focus report is valid only when the sidebar pane is not itself focused and the focused pane is an agent row; it refreshes only on a genuine move (a different pane, a newer stamp). It reads the per-client `PaneRef.client_focused`, not the per-view active pane ([multiplexers.md → Two kinds of focus](./multiplexers.md#two-kinds-of-focus)).

**The newer timestamp wins.** A worked example: you click pane A at t=0, so `local_selection = (A, 0)`, but the producer is mid-fetch and the highlight hasn't repainted yet. At t=0.1 the mux reports focus moving to pane B (a keybind); the report is valid, so `external_focus = (B, 0.1)` — and the newer stamp wins, so the highlight follows the real focus to B. Reverse the order — a focus report stamped before your click arrives after it — and your click holds A, because the stale report loses. This is what lets a click hold its row through the briefly-stale focus window a [click-through](./multiplexers.md#zellij-backend) jump opens, while a genuine external focus move still moves the highlight.

### Provider dashboard

The 5-hour and 7-day budgets are **account-scoped, not session-scoped** — every session of a provider shares one account's budget — so they lift off the rows into a pinned per-provider dashboard at the bottom, bracketed by hairline rules above the footer: one block per agent kind, plus a block for any provider logged in but idle this run. How each panel's account, plan, metered flag, and stable windows are sourced and aggregated is [account.md](./account.md); this section owns only the painting.

- **Mana bars and the shared grid.** Budget bars use a light segmented style (`▰` filled = remaining, `▱` track) and drain green→yellow→red by remaining budget; each `5h`/`7d` label wears its bar's color. The label slot and the `↻ reset` column are fixed-width, so every provider bar shares one front and one end column and the dashboard reads as one aligned grid. A fully-spent window turns its whole track red rather than faint, and the weekly cap gates the short window — an exhausted `7d` forces the `5h` row to the same red, no countdown. Below a narrow width the emblem drops and the bars run full-width.
- **Brand styling is config-driven**, resolved producer-side from `[sidebar.providers.<kind>]` over built-in defaults — see [configuration.md](../reference/configuration.md#sidebar-provider-dashboard).
- **Remote control is a flag, not a row.** `claude remote-control` is host infrastructure, so the snapshot filters its pane out of the room and the Claude block pins a violet `⇅ rc` flag when `[remote_control] claude` is on. Codex has no host pane.

The panel is inert in the hit-test map — it is a dashboard, not a row list.

## The runtime loop

### Launch model

`rimz`, `rimz start`, and cwd-based `rimz attach` ensure the workspace session exists, then launch one sidebar pane best-effort before entering or printing the attach command. Both backends run the same native renderer through `rimz sidebar serve`:

- **Zellij** is born from a layout — a left 30% `rimz-sidebar` pane plus a focused terminal — which doubles as the default tab template, so every tab is born with a sidebar. Rimz touches the layout only at creation; an existing session is a no-op.
- **tmux** splits a left sidebar into the initial window, and an `after-new-window` hook re-runs the same split so every later window is born with its own sidebar — the tab-template parity Zellij gets from its layout.

Launch is **idempotent by heartbeat**: before opening a pane Rimz scans `runtime/heartbeat/sidebar.*.json` and treats only readable, current-protocol, fresh files as live, so a crashed or upgraded sidebar does not suppress relaunch. A launch lock serializes the check-then-spawn so concurrent attaches to one session don't each spawn a daemon, and the sweep removes orphaned heartbeats and sockets a SIGKilled sidebar left behind.

`rimz reload` recovers in place: beyond re-execing live sidebars onto a freshly-installed binary, it re-adds a sidebar to any tab that lost its own — without rebirthing the session, so the user's panes survive. The pass is per-view and run-once; a view that fails to gain a sidebar is logged and left alone.

### State access

**Data access splits by role** (see [performance.md → Principles](./performance.md#principles)). One **producer** per workspace — the eldest live renderer (UUIDv7 ids sort by birth) — forks the one `list-panes` + git round-trip and publishes the coalesced pane list:

```text
rimz sidebar snapshot --workspace-id <id> --exclude-pane-id <own>
```

Every other per-tab renderer is a **consumer**: it reads that published frame **in process** (`read_published_snapshot`), folding only its own-pane exclusion — no subprocess, no `list-panes`/git, no ledger lock. The **rollup** is read separately and event-fresh from `latest.json` (`consumer_rollup`, the lock-free read-only twin of `Ledger::snapshot_cached`) and folded over those panes.

This decoupling is the freshness fix: a `ledger_delta` repaints a status change or a new agent in an existing pane within one wakeup, independent of the slower `list-panes` cadence that governs only genuine pane open/close. A consumer trusts the producer's frame only while it stays fresh — once the elder's `snapshot.json` stops advancing past `PUBLISHED_FRAME_STALE_AFTER`, the consumer produces locally through the same single-flight lock, so a stalled producer can never freeze a tab. Because only the elder produces, even a duplicate that slips the launch lock costs one cache read, not a second mux round-trip.

Each renderer refreshes its own heartbeat **in process** (`rimz::sidebar::write_heartbeat`, never a `rimz` fork per tick) and binds `sock/sidebar.<instance_id>.sock`; the heartbeat carries the workspace and session ids, the mux backend, the instance id, the protocol version, the wakeup socket path, and the last-seen timestamp.

Data fetch is **push-primary**: a `ledger_delta` wakeup (any ledger write, plus the statusline context-sidecar push) drives the refetch, and a slow backstop closes missed wakeups and catches pane/git drift the mux never signals. The spinner animates on a separate, faster cadence (`ANIMATION_FRAME`) from the cached snapshot and never forks a fetch, so the render layer stays smooth regardless of fetch latency. A terminal resize is also a wakeup — a watcher turns `SIGWINCH` into a socket nudge — so the loop repaints at the new size at once instead of reading as a blank pane until the next tick. Ledger wakeups skip heartbeats whose `protocol_version` mismatches the current sidebar protocol; `rimz doctor` reports the mismatch so reload issues are visible after upgrades.

### Self-close

A sidebar shares its tab with the user's working pane(s) and has no reason to outlive them. Each tick counts the panes in its view from the read-only pane list the renderer already needs, and resize events run a metadata-only fast probe against that list so a sidebar that expands after the last sibling exits can close before the next data tick. Both paths identify the sidebar's own pane from the mux env var and never use `pane capture`/`send`. Once it has seen at least one sibling, a later drop to zero means the last working pane exited: the renderer exits and its `close_on_exit` pane closes. A startup latch keeps it from exiting before the terminal pane first appears. This is backend-agnostic. When pane discovery itself stays broken the sibling count is unknowable, so a degraded renderer leaves through the give-up exit in [Reload recovery](#reload-recovery) instead.

### Reload recovery

The sidebar keeps the last successful snapshot across iterations. When the snapshot fetch or the in-process heartbeat write fails — a missing binary, a vanished ledger directory, a transient mux hiccup — the loop:

1. Reuses the last snapshot for the current draw, falling back to an empty placeholder only when nothing has loaded yet (a cold start after a workspace move).
2. Absorbs a single flaky fetch silently — the last good frame already covers it, so one blip never flashes a banner.
3. Once the failure persists past the debounce, raises a sticky **health alert** pinned to the bottom edge (status-bar style) — `! Sidebar degraded for 8s: snapshot failed: ledger not found` — with a monotonic "for Ns". The body is truncated before the alert is ever clipped, so the notice can never scroll off.
4. On recovery, lingers as a dim `⚠ last alert 8s ago … · x dismiss` notice rather than erasing, so a failure that flickered past is still visible; `x` dismisses it, a fresh failure re-arms it. While the alert is active the body — first-run hint, footer, help overlay — steps aside and lets the alert speak alone.
5. Gives up if the failure never clears. The common deleted-binary case self-heals (the snapshot fork re-resolves to the installed `rimz` on `PATH` each tick); give-up is the backstop for a failing heartbeat write, a vanished ledger, or no `rimz` on `PATH` at all. A renderer continuously degraded past `GIVE_UP_AFTER_DEGRADED` is non-functional and — once its heartbeat goes stale — invisible to `rimz reload` and ledger wakeups, so it exits rather than freezing on a stale frame; reload/attach then rebuilds a current-build sidebar against the live panes, and a lone orphan with no working pane simply disappears.

This is the degraded twin of [self-close](#self-close): self-close fires when the view empties, give-up when the view can no longer be read at all. The decision logic is the pure function `app::compute_next_state`, which folds each fetch outcome into a debounced, sticky `Health` (`failure_streak` plus an optional `Alert`); the loop applies its `RenderState` verbatim. `rimz-sidebar` defaults tracing to `off` so warnings do not corrupt the terminal UI — set `RUST_LOG` when debugging the renderer.

## Notifications

Native notifications are best-effort polish; the ledger remains authoritative. Opt-in per workspace via `[notifications]` in project or per-machine config. Notify on: an agent entering `waiting`, a resolver picking up or handing off, a bridge falling back to native prompt, an item answered, an agent resuming after waiting, or an agent staying `waiting` past a threshold.

**Coalesce, then escalate.** Several agents entering `waiting` together produce one notification (*"3 agents need you · query-engine"*), not one each; an agent that stays `waiting` past the threshold earns a single nudge, not a stream. **A notification routes you to the pane.** Its text names *who* needs you and *what task* — the prompt itself stays in the agent's pane. Activating one focuses the terminal best-effort and pre-selects that row, so even when the OS cannot focus an exact pane the sidebar already has it highlighted for the jump. A missed notification loses nothing; the ledger and the attention line stay authoritative.

## Zellij plugin rail (optional upgrade)

Zellij users can opt in to a wasm plugin that presents the same view-model as a docked, persistent left rail (`[layout.zellij]` in [configuration.md](../reference/configuration.md)). The native pane stays the default and fallback; the rail only changes presentation, never correctness, and lays the view-model out to its own pane geometry — so there is no pre-rendered frame to ship and no resize protocol.

- **Reference.** Model it on Zellij's bundled `strider` plugin: a docked side pane that ingests host data asynchronously, keeps `State` separate from per-section view structs, scrolls a bounded list, and handles key + mouse. Mirror its split — state in one module, a render fn per worktree group, pure layout helpers shared.
- **Async ingestion through the host.** A wasm plugin cannot block on a subprocess, so it never runs `rimz` inline: it calls `run_command(&["rimz", "sidebar", "snapshot", "--json", "--workspace-id", <id>], ctx)` and receives `Event::RunCommandResult`, with the `ctx` map tagging each request so the handler matches its response. Parse stdout into the view-model; on non-zero exit, keep the last good snapshot and raise the same sticky health alert as the native loop. Stay read-only on the ledger — never a ledger-writer import.
- **Wakeups arrive as pipes; a timer backstops them.** `zellij pipe --name rimz::feed` lands in `fn pipe()` and kicks a fresh snapshot `run_command`; a `set_timeout` keepalive re-fetches on the slow poll so a missed pipe never strands the rail. Subscribe to `RunCommandResult`, `Key`, `Mouse`, `Timer`, and `PermissionRequestResult` — `run_command` needs the one-time `RunCommands` grant.
- **Actions cross the CLI boundary, never an import** (read-only constrains the import graph, not the process tree): jump → `focus_pane_with_id(...)` after stripping the `zellij:` prefix and reconciling `pane_process_start`; answer a `script` item → `run_command(&["rimz", "feed", "resolve", …])`.
- **Lifecycle.** Workspace id and the `rimz` binary path arrive in the `load()` `configuration` map, mirroring strider's `caller_cwd`. The rail writes no heartbeat — Zellij owns its liveness, and an idempotent `launch-or-focus-plugin` dedupes by URL + config and docks it left into a live session, which a CLI-launched pane cannot reach after birth. That docked pane plus `launch-or-focus-plugin` is what makes it "non-killable": it resists accidental loss and the next `rimz` / attach re-summons it.
