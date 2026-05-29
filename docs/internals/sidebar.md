# Sidebar

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The sidebar is the product surface. It's a UI client over the workspace ledger; it owns no durable state. Read the ledger through `rimz sidebar snapshot`, write liveness in-process through the `rimz::sidebar::write_heartbeat` helper (a runtime-file write), and never import a ledger-writer module.

## Renderers

The `rimz sidebar snapshot` JSON is the shared view-model. It carries the worktree-grouped row roster — every live pane as a row, agents enriched from the ledger — plus the attention items, per-agent statuses and capability, the ranking keys, and timestamps. Every grouping and ordering decision is made once, here. A renderer is a projection: it maps the view-model's semantics to glyphs and paints them. It never re-derives which worktree a row belongs to, how rows rank, or which pane runs an agent.

The snapshot command runs in the `rimz` binary, which owns mux access, so it enumerates the session's panes and folds them into the roster before serializing. The renderer (and the Zellij plugin rail) stay pure JSON consumers — pane presence reaches them only through the snapshot, never a mux call of their own.

Three renderers project the same snapshot:

- **Native pane (default).** The `rimz-sidebar` binary in an ordinary pane. Identical on Zellij and tmux, across detach/reattach. This is the default and the cross-backend fallback.
- **Zellij plugin rail (optional upgrade).** A docked, persistent left rail for Zellij users who opt in — better placement, same view-model. Described below.
- **CLI listings.** `rimz feed list` and friends, the same data as text.

Because the view-model owns every decision, the per-renderer code is just painting — small enough to keep separate per surface. Visual parity across renderers is therefore a maintained discipline, not shared render code. The one rendering convention renderers share is the semantic→glyph mapping below; keep it aligned so the rail, the pane, and the CLI read the same.

Semantic→glyph conventions:

- agent status (`waiting`/`failed`/`running`/`idle`/`success`) and permission posture (`default`/`auto`/`yolo`/`unknown`) map to the canonical glyph + color table in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape). Two symbols carry attention — `?` waiting, `!` failed-or-stalled — and only active states animate (a working `running` cell fills, plan-mode thinking sparkles, a resolver mid-flight spins braille). The glyph carries the state by shape so it survives `NO_COLOR`; color reinforces it. `default` and `unknown` postures are omitted; `yolo` is warn-colored.
- a resolver mid-flight renders in place of `? waiting` as a braille spinner with `<resolver> <budget>` on the same row; full chain detail stays in `rimz feed list`.
- a per-row, right-aligned age (`12m`) shows time since the agent's last activity on its task; there is no global "updated" timestamp.

## Presence model

Row presence comes from the **live pane list**, not the ledger. `rimz sidebar snapshot` enumerates the workspace session's panes, reads each pane's foreground command and cwd, resolves the cwd to a worktree, and emits **one row per pane**: every live pane is exactly one row, and no pane id is shared by two rows. A pane running `zsh` is a dim process row; a pane an agent stamped is that agent's row. The sidebar's own pane is excluded — it is chrome, not work.

**One pane = one row, bound by the stamped pane id.** A pane's foreground command cannot name the agent: Claude and Codex both run under `node`, so the command reads `node`, not `claude`. Binding is by identity instead — every `agent.lifecycle` event carries the mux pane id the hook ran inside (`TMUX_PANE` / `ZELLIJ_PANE_ID`; see [agent.md](./agent.md)), and the snapshot binds each live pane to the one agent that stamped that exact pane id. Command and cwd never bind a row: two same-kind agents in one worktree are told apart only by their stamped panes, and a pane the agent never stamped — a shell it dropped back to, a `git` it spawned — stays a process row. The agent's ledger identity (status, permission posture, task, model, effort, and context/token enrichments) then enriches that one row.

**Paneless agents do not render.** An agent with no live pane — a sub-agent, a ghost a kill left in the rollup, a relaunch-in-place the [rollup reaper](./ledger.md#runtime-projection) has not yet collapsed — is data, not presence. It cannot resurrect a row or latch onto a stranger's pane. So an exited agent never lingers: when it quits, its pane closes (gone from the next snapshot) or reverts to a shell row, and the bug it replaces ("agent quits, row stays") cannot recur. There is no `offline` status. The rollup is kept honest in parallel — dead-pid sessions and stale ghosts are reaped from the derived rollup (best-effort pid liveness plus a ghost TTL; see [agent.md](./agent.md)) — but render presence is the stamped live pane alone.

**Attention folds onto the agent's pane.** A pending agent ask belongs to the agent's pane: the snapshot folds the session's single most-relevant pending ask onto that agent's stamped-pane row (`? waiting`, or a braille resolver spinner with a resolver in front), never as a second row — so a session can never stack more than one row. The snapshot stops folding an ask once the agent records activity past it, so an ask answered in the agent's own UI un-folds and the row returns to `running` (see [agent.md → Liveness](./agent.md#liveness-and-presence)). If the agent's pane is absent (it reverted to `zsh` or closed), the ask leaves the sidebar and default `feed list` but stays in audit (`rimz feed list --audit` / `feed show`). A script's blocking `feed ask` chose Rimz as its surface and has no agent pane to fold onto, so its pending item keeps its own standalone row while the waiter is alive. When pane discovery itself fails, the renderer keeps the last good snapshot and, once the failure persists, raises the sticky health alert rather than inventing an empty room.

**Residual.** Strict pane-keying means a row blinks out if its specific pane is transiently missing from an otherwise non-empty pane list. The zellij transient-empty retry and the snapshot cache cover the common cases (an empty or failed fetch holds the last good frame); a genuine partial list still briefly drops the affected row until the next tick.

## Launch model

`rimz`, `rimz start`, and cwd-based `rimz attach` ensure the workspace session exists, then launch one sidebar pane best-effort before entering or printing the attach command. `rimz attach <session>` does the same only when a matching `workspace.json` record gives Rimz the workspace ID and cwd; otherwise it warns and leaves the exact session-name attach path alone.

Both backends run the same native renderer through `rimz sidebar serve`:

- Zellij: the session is born from a layout — a left 30% `rimz-sidebar` pane plus a focused terminal — which doubles as the default tab template, so every tab is born with a sidebar. Rimz touches the layout only at creation; an existing session already carries its sidebar (and survives detach/reattach server-side), so launch there is a no-op. One `rimz-sidebar` renderer per tab, each a read-only view of the same room ledger.
- tmux: `tmux split-window -d -h -l <width>% -b -t <session> <rimz-bin> sidebar serve ...` places a left sidebar in the initial window, and an `after-new-window` hook re-runs the same split so every window opened later is born with its own sidebar — the tab-template parity Zellij gets from its layout.

Launch is idempotent by heartbeat. Before opening a pane, Rimz scans `runtime/heartbeat/sidebar.*.json` and treats only readable, current-protocol files whose mtime is within the sidebar heartbeat TTL as live. Stale, unreadable, or old-protocol heartbeats are ignored so a crashed sidebar or upgraded protocol does not suppress relaunch.

`rimz reload` recovers in place. Beyond signalling live sidebars to re-exec a freshly-installed binary, it re-adds a sidebar to any tab/window that still has working panes but lost its own — without rebirthing the session, so the user's panes survive. tmux re-runs the same left split (`-b -l <pct>% -d`) against the bare window; Zellij, which docks left only at session birth, reaches a live tab by splitting a `rimz-sidebar` pane to the right, moving it left, and resizing it toward the layout width, then restores the caller's focus. The pass is per-view and run-once: a view that fails to gain a sidebar is logged and left alone, never retried in a loop.

### Self-close

A sidebar shares its tab with the user's working pane(s) and has no reason to outlive them. Each tick the renderer lists its session's panes via `rimz pane list` (read-only discovery — never `pane capture`/`send`), identifies its own pane from the mux env var (`ZELLIJ_PANE_ID` / `TMUX_PANE`), and counts the other panes in its view. Once it has seen at least one sibling, a later drop to zero means the last working pane exited: the renderer exits, its `close_on_exit` pane closes, and the lone sidebar is gone. The startup latch keeps it from exiting before the terminal pane first appears. This is backend-agnostic — tmux self-closes through the same normalized `rimz pane list`. Self-close needs a readable view; when pane discovery itself stays broken the sibling count is unknowable, so a degraded renderer leaves the tab through the give-up exit in [Reload recovery](#reload-recovery) instead.

### Zellij plugin rail (optional upgrade)

Zellij users can opt in to a wasm plugin that presents the same view-model as a docked, persistent left rail (`[layout.zellij]` in [configuration.md](../reference/configuration.md)). The native pane stays the default and the fallback; the rail only changes presentation, never correctness. It lays the view-model out to its own pane geometry, so there is no pre-rendered frame to ship and no resize protocol.

**Reference.** Model the rail on Zellij's bundled `strider` plugin. Strider is a docked side pane that ingests host data asynchronously, keeps `State` separate from per-section view structs, scrolls a bounded list, and handles key + mouse — the same shape carries the worktree-grouped roster and its jump-on-select rows. Mirror its split: state in one module, a render fn per worktree group, pure layout helpers (`calculate_list_bounds`) shared.

**Data ingestion is async through the host.** A wasm plugin cannot block on a subprocess, so it never runs `rimz` inline. It calls `run_command(&["rimz", "sidebar", "snapshot", "--json", "--workspace-id", <id>], ctx)` and receives the bytes back as `Event::RunCommandResult(exit, stdout, stderr, ctx)` — the host bridge strider uses for the filesystem and `about` uses for `xdg-open`. The `ctx` map tags each request so the handler matches its response. Parse stdout into the snapshot view-model; on non-zero exit, keep the last good snapshot and raise the same sticky health alert as the native loop. This is still read-only on the ledger: the rail reaches state only through `rimz sidebar snapshot`, never a ledger-writer import.

**Wakeups arrive as pipes; a timer backstops them.** `zellij pipe --name rimz::feed` lands in `fn pipe()`, which kicks a fresh snapshot `run_command`. A `set_timeout` keepalive tick re-fetches on the slow poll so a missed pipe never strands the rail. Subscribe to `RunCommandResult`, `Key`, `Mouse`, `Timer`, and `PermissionRequestResult` — `run_command` needs the one-time `RunCommands` grant.

**Actions cross the CLI boundary, never an import.** Read-only-on-the-ledger constrains the import graph, not the process tree, so the rail acts by shelling out like every renderer:

- jump (select any row) → `focus_pane_with_id(PaneId::Terminal(raw), …)` after stripping the `zellij:` prefix and reconciling `pane_process_start` (refuse a stale match).
- answer a `script` item → `run_command(&["rimz", "feed", "resolve", <req>, "--decision", <json>, "--method", "sidebar"])`.

**Rendering.** `print_text_with_coordinates` with `Text::color_range`/`.selected()` for the two-line agent rows; `print_nested_list_with_coordinates` for the worktree groups. The semantic→glyph mapping above stays the shared discipline — the rail paints the view-model, it never re-derives the grouping or ranking.

**Lifecycle.** Workspace ID and the `rimz` binary path arrive in the `load()` `configuration` map, mirroring strider's `caller_cwd`. The rail writes no heartbeat: Zellij owns its liveness and an idempotent `launch-or-focus-plugin` dedupes by URL + config, so the heartbeat-scan idempotency above stays the native pane's concern. "Non-killable" is that docked pane plus `launch-or-focus-plugin` — it resists accidental loss and the next `rimz` / attach re-summons it. Left re-placement is the rail's reason to exist: `launch-or-focus-plugin` docks it left into a live session, which a CLI-launched pane cannot reach after birth.

## Phase projection

The same renderer paints every moment of a session — bare shell, first agent, a waiting prompt, a fleet across worktrees, detach and reattach. What changes between phases is the snapshot, never the renderer: the view-model owns every transition and the renderer only projects it. There is no per-phase rendering code, so the mechanics each phase exercises are documented once, in the sections of this doc — the pane→agent overlay and the revert to a shell row in [Presence model](#presence-model), the `?`/`!` rise, bucket-then-age sort, and calm-tail cap in [Attention ranking and the per-worktree cap](#attention-ranking-and-the-per-worktree-cap), the two-line cell and the resolver braille-spinner swap in [Agent rows](#agent-rows), the dim `· zsh` row in [Process rows](#process-rows), and the notify-and-route discipline in [Notifications](#notifications). Detach and reattach reconstruct from the ledger ([Reload recovery](#reload-recovery)); hook installs are gated by the consent screen the [experience walkthrough](../guide/experience.md#phase-1--the-first-keystroke-rimz-and-the-consent-gate) frames.

For the felt, phase-by-phase walk-through — what the developer does, sees, and thinks from first keystroke to a ten-agent fleet — see [the experience walkthrough](../guide/experience.md).

### Empty-room hint

With nothing waiting or failed the attention line is omitted and the body is never blank: each pane is still a [process row](#process-rows), and a dim first-run hint points at the real next step. The renderer keys the hint on the snapshot's `agent_hooks_ready` flag — an unwired room reads `install hooks: rimz hooks install claude`, a wired room reads `run claude or codex` — and clears it the instant the first agent or feed item appears, including a supported agent visible as a plain process row before hook enrichment lands. The hint is for a *healthy* empty room: under an active health alert the alert takes over and the hint is suppressed, because an empty body under a failed fetch is a missing snapshot, not an empty room (see [Reload recovery](#reload-recovery)). `rimz doctor` reports the same per-agent install status the flag reflects.

## State access

On load and tick, the renderer fetches the snapshot through the CLI:

```text
rimz sidebar snapshot --workspace-id <id> --exclude-pane-id <own>
```

It refreshes its own liveness heartbeat **in process** — no `rimz` fork per tick — through the `rimz::sidebar::write_heartbeat` liveness helper (a runtime-file write, never a ledger-writer import). The renderer binds `sock/sidebar.<instance_id>.sock` and the heartbeat carries:

- workspace ID,
- session name,
- mux backend,
- sidebar instance ID,
- protocol version,
- wakeup socket path,
- last-seen timestamp.

On wakeup, the sidebar refetches the snapshot. Missed wakeups are closed by polling (~2s tick). A terminal resize is also a wakeup: a watcher thread turns Zellij's resize (`SIGWINCH`) into a socket nudge so the loop repaints at the new size at once — without it the first usable frame on attach waits for the next tick, reading as a blank pane. The blocking wait treats a signal-interrupted receive as that same "redraw now", never an error. Ledger wakeups skip sidebar heartbeats whose `protocol_version` does not match the current sidebar protocol; `rimz doctor` reports the mismatch so reload issues are visible after upgrades.

The snapshot CLI also folds each session's rich statusline context onto its `agents[]` entry as `AgentState.context` (read-only; the feed process is the writer — see [agent.md → The statusline as a context datasource](./agent.md#the-statusline-as-a-context-datasource)). This is captured-and-exposed only: `snapshot --json` carries the full `context` blob (cost, token breakdown, rate-limit windows, PR) for detail views and tooling, but the compact rows do not render it yet.

## Reload recovery

The sidebar process keeps the last successful snapshot across iterations. When the `rimz sidebar snapshot` fetch or the in-process heartbeat write fails — the binary is missing, the ledger directory is gone, pane discovery hit a transient mux hiccup — the loop:

1. Reuses the last snapshot for the current draw, falling back to an empty placeholder when nothing has loaded yet (sidebar started cold after a workspace move).
2. Counts the consecutive failures. A single flaky fetch is absorbed silently — the last good frame already covers it, so one blip never flashes a banner.
3. Once a failure persists past the debounce threshold, raises a sticky **health alert** pinned to the bottom of the sidebar — `! Sidebar degraded for 8s: snapshot failed: ledger not found` — and pins the timestamp the episode began, so "for Ns" grows monotonically.
4. On recovery the alert is not erased: it lingers as a dim `⚠ last alert 8s ago: … · x dismiss` notice so a failure that flickered past is still visible after the fact. Pressing `x` dismisses it; a fresh failure re-arms it. While the alert is *active* the body is a stale or empty fetch, so the first-run hint, footer, and help overlay step aside and let the alert speak alone.
5. Gives up if the failure never clears. The common deleted-binary snapshot failure already self-heals in place — the snapshot fork re-resolves to the installed `rimz` on `PATH` each tick when its launch-path binary vanishes — so the alert clears without a restart. Give-up is the backstop for what that cannot cure: a failing heartbeat write, a vanished ledger, or no `rimz` on `PATH` at all. A renderer continuously degraded for ~30s is non-functional and — once its heartbeat has gone stale — invisible to `rimz reload` and ledger wakeups, so it exits rather than freezing forever on a stale frame. Its `close_on_exit` pane closes; reload/attach recovery then rebuilds a current-build sidebar against the live panes, and a lone orphan with no working pane simply disappears. This is the degraded twin of [self-close](#self-close): self-close fires when the view empties, give-up fires when the view can no longer be read at all.

The alert is reserved space at the bottom edge of the viewport (status-bar style): the body is truncated before the alert is ever clipped, so the sticky notice can never scroll off a full sidebar.

`rimz-sidebar` defaults tracing to `off` so warnings do not corrupt the terminal UI. Set `RUST_LOG` when debugging the renderer.

The decision logic is the pure function `app::compute_next_state`, which folds each fetch outcome into a debounced, sticky `Health` (`failure_streak` plus an optional `Alert`); the loop applies its `RenderState` verbatim.

## Information architecture

Top to bottom, the sidebar is:

1. **Title** — the project display name (workspace-id fallback).
2. **Attention line** — instant triage: `?2  !1` (yellow/red) counts agents waiting or failed/stalled; omitted when nothing needs you. It counts even agents hidden by a per-worktree cap, so the aggregate is never lost.
3. **Worktree groups** — the body (below).
4. **Footer** — a dim hint for the interactive keys: `↵ jump`, plus `␣ next ?!` and `? keys` when more than one row needs you; with a single waiting item it names the target (`↵ jump to claude`). No timestamp; freshness is the health alert's job.
5. **Health alert** — the sticky, dismissable bottom line, present only when the refresh loop is or recently was unhealthy (see [Reload recovery](#reload-recovery)).

There are no feed-group sections: "Recently answered" and "Recent activity" are gone. The sidebar shows only what needs a decision or an action; full history lives in `rimz feed list --audit`.

### Worktree groups

A worktree is total isolation — only same-worktree agents collaborate — so it is the spine of the layout. Each worktree group is a bold header with a `▌` isolation marker, optional worktree diff stats (`+127 -43`), and a right-aligned status tally (`2◕ 1?`), then its rows. Diff stats are read by `rimz sidebar snapshot` from `git diff --numstat HEAD --`, cached briefly in runtime state, and live on the worktree header rather than a row so shared-worktree changes never pretend to belong to one agent. A pane's group is decided by a path-prefix test against the project root: a cwd that is the root (the main checkout) or nested under it — including `<root>/.claude/worktrees/*` — belongs to that worktree. Anything outside (a home shell, `/tmp`) folds into the catch-all, which also holds scripts and CI not tied to a worktree. The catch-all renders not as a `▌` pod header but as a dim `┄ external ┄┄┄` divider, so it reads as "outside the project," and it sorts last unless it holds a waiting ask. The test is lexical on the reported cwd: a git worktree parked outside the root would classify as `external` (the canonical `<root>/.claude/worktrees/` layout avoids this), and a snapshot with no known root keeps each cwd in its own group.

### Attention ranking and the per-worktree cap

One principle: the most attention-hungry rises. Within a worktree, agents sort by status bucket (`waiting` → `failed` → `running` → `idle` → `success`), then by age in that bucket — attention-demanding buckets (`waiting`, `failed`) oldest-first (longest overdue rises), calm buckets (`running`, `idle`, `success`) most-recent-first. Bare process rows have no status, so they sort below every agent row, most-recent-first. Worktree groups themselves sort by their top-ranked member.

Each worktree shows at most N rows (default ~6, configurable) with a dim `+K more`. The cap truncates only the calm tail — calm agents and process rows; every `waiting`/`failed` agent is exempt and always shown, so the cap can never hide something that needs you.

## Agent rows

Each agent is a small stacked cell — line 1 is *what's happening*, the dim capability line below is *what it is*, and a thin full-width bar underlines them with the context meter. Non-agent jobs (scripts, CI) and bare process rows (below) have no model or meter and stay a single line.

```
◕ claude  fix auth flow  12m       line 1 — status (working cell animates) · name · task · age
  Opus · xhigh · yolo              line 2 — model · effort · posture when non-default
  ━━━━━━━━━━──────────────         context bar — full-width, underlines the model name
```

Line 1:

- **Status** is the glyph + color (no status word) from the [DESIGN.md table](../../DESIGN.md#sidebar-shape); the glyph's shape carries it under `NO_COLOR`.
- **Name**, clipped with `…`.
- **Task descriptor** — the agent's reported task, or the first ~20 chars of its initial prompt. Display-only enrichment: redactable, never drives a decision (the no-transcript-correctness rule).
- **Age** — right-aligned, dim: time since the agent's last activity on its task. It doubles as the ranking signal (the most-overdue waiting row shows the largest age) and grows toward the stall window that escalates a silent `running` agent to `!`.
- **Animated cell** — the leading glyph is the row's one animated cell, driven by a wall-clock tick: a `running` agent fills a circle (`○ ◔ ◑ ◕ ●`) while working, sparkles (`· ✢ ✳ ✶ ✻ ✽`) while thinking in plan mode, and a resolver mid-flight spins braille. Activity is the per-tool heartbeat folded into `last_activity` (see [agent.md → Liveness](./agent.md#liveness-and-presence)), so the motion tracks real work; when an agent goes silent past the stall window it does not freeze — it escalates to the static `!` attention state. Every static state (`?`/`!`/`◌`/`✓`) holds still; attention cues do not jitter.

Line 2 — the capability line: model (`Opus`, `GPT-5.5`), effort/thinking (`xhigh`/`high`/…), and non-default permission posture (`auto` dim, `yolo` warn-colored); the default posture is omitted. At wider widths it also inlines todo progress (`●●●○○ 3/5`). The context bar (next section) renders on its own thin line directly below, underlining the model name. With no capability data the capability line is dropped, but the agent still shows its identity line and bar. Selection never reshapes a row: line 2 and the bar are identical to their unselected selves — selection only *adds* lines for data not already shown (today, total tokens `12.4k tok`) and marks the row with a left accent bar `▎` in a reserved one-cell gutter. A calm agent with nothing extra to show keeps its exact shape when selected.

A resolver mid-flight replaces `? waiting` on its row with a braille spinner and `<resolver> <budget>` (the agent name stays; the budget fills the task slot), and still counts in the attention tally — the item is pending, just being handled. When the chain exhausts it flips back to `? waiting`. Override a slow chain with `rimz feed resolve --override-chain`.

### Process rows

A pane no agent has stamped renders as a single dim line: a `·` marker, the command name (`zsh`, `vim`, `node`), and the right-aligned age since the pane was last seen. No capability line, no status glyph, no attention count — it is presence, not a cue. It is still a jump target: selecting it focuses that pane like any other row. A process row carries no ledger identity; once an agent's hook stamps that pane id, the same pane renders as that agent's row (Phase 1).

### Jump — the row is the link

You don't read where to go; you go. Selecting a row focuses that agent's pane via the `pane` ref on the snapshot — no mux pane number is ever printed. Both renderers share one key model:

- `↑/↓` select a row; `↵` jumps to the selected pane.
- `1`–`9` jump by the row's visible ordinal (its position in the column, not a mux pane id).
- `␣` jumps to the *next item that needs you* — the next `waiting`/`failed` row in ranking order — without first selecting it. This is the fleet-scale triage key (Phase 4): one keystroke to the oldest blocked pane, again for the next. It is bound only inside the Rimz session, so it never touches the user's global mux config.
- `x` dismisses the sticky [health alert](#reload-recovery) once it has recovered; an active failure re-arms it.
- `r` re-execs the renderer in place, picking up a freshly-installed build without leaving the pane — the keypress scope of `rimz reload`, which the renderer reaches by riding the same `reload` wakeup word the CLI posts.
- `?` toggles a legend-and-keys overlay, so the glyph vocabulary and the key model are learnable in place without leaving the room.

Per renderer this is the same model over different input plumbing:

- **Zellij plugin rail** — mouse click or the keys above call `focus_pane_with_id(...)`, reconciling `pane_process_start` to refuse a stale pane.
- **Native pane** — the renderer's key handler maps the same keys to mux focus commands; where the terminal forwards mouse, a click does the same. The glyph + color stays the at-a-glance signal regardless of input support.

Either way the jump reconciles pane id *and* `pane_process_start`, so a reused pane id never silently focuses a stranger (see [Action rules](#action-rules)).

On every refresh, the native pane mirrors selection to the focused working pane in its own mux view. If focus is on the sidebar itself or focus cannot be discovered, the current manual selection stays in place.

### Live enrichments and density

Every enrichment follows the same grammar: line 1 is the cue, and the lines below are meters. Context-window percent renders as a thin full-width rule on its own line — a heavy `━` filled run that ramps green → amber → red over a light `─` track, with no label (`━━━━━━━━━━──────────────`); todo progress renders as a bounded dot shape (`●●●○○ 3/5`), total tokens render as a compact selected-row token (`12.4k tok`), and worktree diff stats render on the group header (`+127 -43`). All are display-only and privacy-gated; a missing field means "agent did not report it", never zero, and none drives attention counts or routing.

Width controls ambient density: narrow rows keep identity and the bar, the default width adds model/effort, and wider rows inline todo progress. Because the bar spans the full width and starts at the left edge, every agent's bar shares one column — the bars line up across worktrees with no alignment work. Selection is orthogonal and light: a reserved one-cell gutter carries the accent bar `▎` on the focused row, and selection adds detail lines (total tokens today) only when there is data not already on screen. Color is the muted 256-color palette by default; `NO_COLOR` suppresses color only, and the bar's heavy/light shape still carries the meaning.

### View-model fields the rows use

The rows read `SidebarRow.{row_kind, name, status, permission_posture, plan_mode, pane, task, model, effort, context_pct, total_tokens, todo_done, todo_total, worktree_path, worktree_branch, last_activity, resolver, options}` plus the per-group `status_counts` and `diff_added`/`diff_removed`. `row_kind` is `agent` or `process`; a process row carries its command in `name` and leaves `status` unset (it never enters `status_counts`). A standalone script/bridge ask with no agent pane is an `agent` row too — it carries no model or meter, so it renders as a single line. Agent rows always carry `context_pct`, defaulting to `0` until transcript usage is known, so every observed agent paints the same context bar from its first frame. Pane presence reaches the reducer through `PaneRef.{command, cwd}`, resolved to `worktree_path`/`worktree_branch` before grouping. Age, ranking, the stall escalation, and the ask-recovery guard all come from `last_activity`, which the per-tool activity heartbeat advances between turn boundaries.

`SidebarSnapshot` still carries `recently_answered` and `recent_activity`, but the sidebar renderer ignores them — it paints only `worktree_groups` and the attention line. Full history surfaces through `rimz feed list --audit`. If no renderer ever consumes those two fields, drop them from the sidebar view-model; they restate ledger queries.

## Action rules

- A row never shows approve/deny and never shows the question text — Rimz cannot answer the agent's own UI, and the prompt belongs in the agent's pane. The row's job is to route you there.
- A script's `feed ask` is the exception: it chose Rimz as its surface, so its declared options are answerable (clickable on the plugin rail, or via `rimz feed resolve`).
- Jump reconciles pane ID *and* process start time, so a reused pane ID never silently focuses a stranger.

## Notifications

Native notifications are best-effort polish; the ledger remains authoritative. Opt-in per workspace via `[notifications]` in project or per-machine config.

Notify on:

- agent enters `waiting`,
- resolver picks up or hands off an item,
- bridge falls back to native prompt,
- item is answered,
- agent resumes after waiting,
- agent stays `waiting` past a configured threshold.

**Coalesce, then escalate.** Several agents entering `waiting` together produce one notification (*"3 agents need you · query-engine"*), not one each. An agent that stays `waiting` past the threshold earns a single nudge, not a stream.

**A notification routes; it never answers.** Its text names *who* needs you and *what task* — never the agent's prompt. Activating one focuses the terminal best-effort and pre-selects that row, so even when the OS cannot focus an exact pane the sidebar already has it highlighted for the jump. A missed notification loses nothing; the ledger and the attention line stay authoritative.
