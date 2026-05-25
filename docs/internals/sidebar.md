# Sidebar

> See [DESIGN.md](../../DESIGN.md) for the commitments this doc operationalizes.

The sidebar is the product surface. It's a UI client over the workspace ledger; it owns no durable state. Read the ledger through `rimz sidebar snapshot`, write liveness through `rimz sidebar heartbeat`, and never import a ledger-writer module.

## Launch model

`rimz`, `rimz start`, and cwd-based `rimz attach` ensure the workspace session exists, then launch one sidebar pane best-effort before entering or printing the attach command. `rimz attach <session>` does the same only when a matching `workspace.json` record gives Rimz the workspace ID and cwd; otherwise it warns and leaves the exact session-name attach path alone.

Both backends run the same native renderer through `rimz sidebar serve`:

- Zellij: the session is born from a layout — a left 30% `rimz-sidebar` pane plus a focused terminal — which doubles as the default tab template, so every tab is born with a sidebar. Rimz touches the layout only at creation; an existing session already carries its sidebar (and survives detach/reattach server-side), so launch there is a no-op. One `rimz-sidebar` renderer per tab, each a read-only view of the same room ledger.
- tmux: `tmux split-window -d -h -l <width>% -b -t <session> <rimz-bin> sidebar serve ...` places a left sidebar in the initial window.

Launch is idempotent by heartbeat. Before opening a pane, Rimz scans `runtime/heartbeat/sidebar.*.json` and treats only readable, current-protocol files whose mtime is within the sidebar heartbeat TTL as live. Stale, unreadable, or old-protocol heartbeats are ignored so a crashed sidebar or upgraded protocol does not suppress relaunch.

### Self-close

A sidebar shares its tab with the user's working pane(s) and has no reason to outlive them. Each tick the renderer lists its session's panes via `rimz pane list` (read-only discovery — never `pane capture`/`send`), identifies its own pane from the mux env var (`ZELLIJ_PANE_ID` / `TMUX_PANE`), and counts the other panes in its view. Once it has seen at least one sibling, a later drop to zero means the last working pane exited: the renderer exits, its `close_on_exit` pane closes, and the lone sidebar is gone. The startup latch keeps it from exiting before the terminal pane first appears. This is backend-agnostic — tmux self-closes through the same normalized `rimz pane list`.

## What it looks like

A narrow column (default 30% width, ~24–36 cols), keyed on worktree, showing only
what needs you. Each agent is a two-line cell; the whole row is a jump target.

```
┌ billing-service ───────────┐
│ ◆2  ✗1                     │
│                            │
│ ▌main             2▸ 1◆    │
│ ◆ claude  fix auth flow 12m│
│   Opus · xhigh · plan      │
│ ▸ claude  add tests     8s │
│   Sonnet · high            │
│ ▸ codex   refactor api 30s │
│   GPT-5.5 · high           │
│                            │
│ ▌feature-migration 1✗ 1○   │
│ ✗ claude  db migrate    4m │
│   Opus · xhigh · bypass    │
│ ○ codex   —             1h │
│   GPT-5.5 · low            │
│                            │
│ ▌workspace         1◆      │
│ ◆ deploy  promote?      5m │
│                            │
│ ↵ focus                    │
└────────────────────────────┘
```

Color legend (ASCII can't show it): `◆` waiting = yellow, `✗` failed = red, `▸`
running = green, `○` idle = dim, `✓` success = green dim; `bypass` mode is
warn-colored. The glyph table is canonical in [DESIGN.md → Sidebar shape](../../DESIGN.md#sidebar-shape).

> Product invariant lives in [DESIGN.md](../../DESIGN.md).

The sidebar **notifies and navigates; it never reproduces the question.** A row
says *who* needs you, *what task* they're on, and is itself the jump to that pane —
you read the actual prompt and answer in the agent's own UI. A script's `feed ask`
is the one surface Rimz can answer directly.

## State access

On load and tick:

```text
rimz sidebar snapshot --workspace-id <id>
rimz sidebar heartbeat --workspace-id <id> --instance-id <id> \
  --mux <zellij|tmux> --session-name <name> --wakeup-socket <path>
```

The heartbeat binds `sock/sidebar.<instance_id>.sock` and writes:

- workspace ID,
- session name,
- mux backend,
- sidebar instance ID,
- protocol version,
- wakeup socket path,
- last-seen timestamp.

On wakeup, the sidebar refetches the snapshot. Missed wakeups are closed by polling (~2s tick).
Ledger wakeups skip sidebar heartbeats whose `protocol_version` does not match the current sidebar protocol; `rimz doctor` reports the mismatch so reload issues are visible after upgrades.

## Reload recovery

The sidebar process keeps the last successful snapshot across iterations. When `rimz sidebar snapshot` or `rimz sidebar heartbeat` fails — the binary is missing, the ledger directory is gone, the JSON is mid-write — the loop:

1. Reuses the last snapshot for the current draw, falling back to an empty placeholder when nothing has loaded yet (sidebar started cold after a workspace move).
2. Promotes the fetch state to `Degraded` and pins the timestamp the loop went unhealthy.
3. Renders a one-line banner at the top of the sidebar — `! Sidebar degraded for 8s: snapshot failed: ledger not found` — so the user sees *why* the UI isn't updating, instead of staring at a stale snapshot.
4. Clears the banner the next iteration that succeeds.

`rimz-sidebar` defaults tracing to `off` so warnings do not corrupt the terminal UI. Set `RUST_LOG` when debugging the renderer.

The decision logic is the pure function `app::compute_next_state`; the loop applies its `RenderState` verbatim.

## Information architecture

Top to bottom, the sidebar is:

1. **Title** — the project display name (workspace-id fallback).
2. **Degraded banner** — only when the refresh loop is unhealthy (see [Reload recovery](#reload-recovery)).
3. **Attention line** — instant triage: `◆2  ✗1` (yellow/red) counts agents waiting or failed; `✓ all clear` (dim) when nothing needs you. It counts even agents hidden by a per-worktree cap, so the aggregate is never lost.
4. **Worktree groups** — the body (below).
5. **Footer** — a dim jump hint (`↵ focus`) on interactive renderers; nothing on the read-only pane. No timestamp; freshness is the degraded banner's job.

There are no feed-group sections: "Recently answered" and "Recent activity" are gone. The sidebar shows only what needs a decision or an action; history lives in `rimz feed list`.

### Worktree groups

A worktree is total isolation — only same-worktree agents collaborate — so it is the spine of the layout. Each group is a bold header with a `▌` isolation marker and a right-aligned status tally (`2▸ 1◆`), then its rows. The `workspace` group holds scripts and CI not tied to a worktree and renders last unless it holds a waiting ask.

### Attention ranking and the per-worktree cap

One principle: the most attention-hungry rises. Within a worktree, agents sort by status bucket (`waiting` → `failed` → `running` → `idle` → `success`), then by age in that bucket — attention-demanding buckets (`waiting`, `failed`) oldest-first (longest overdue rises), calm buckets (`running`, `idle`, `success`) most-recent-first. Worktree groups themselves sort by their top-ranked member.

Each worktree shows at most N agents (default ~6, configurable) with a dim `+K more`. The cap truncates only the calm/done tail; every `waiting`/`failed` agent is exempt and always shown, so the cap can never hide something that needs you.

## Agent rows

Each agent is a two-line cell — line 1 is *what's happening*, line 2 (dim) is *what it is*. Non-agent jobs (scripts, CI) have no model and stay a single line.

```
◆ claude  fix auth flow    12m     line 1 — status · name · task · age
  Opus · xhigh · plan              line 2 — model · effort · mode (dim)
```

Line 1:

- **Status** is the glyph + color (no status word) from the [DESIGN.md table](../../DESIGN.md#sidebar-shape); the glyph's shape carries it under `NO_COLOR`.
- **Name**, clipped with `…`.
- **Task descriptor** — the agent's reported task, or the first ~20 chars of its initial prompt. Display-only enrichment: redactable, never drives a decision (the no-transcript-correctness rule).
- **Age** — right-aligned, dim: time since the agent's last activity on its task. It doubles as the ranking signal (the most-overdue waiting row shows the largest age) and flags a stalled `running` agent.

Line 2 — the capability line, dim `·`-joined tokens: model (`Opus`, `GPT-5.5`), effort/thinking (`xhigh`/`high`/…), and mode (`interactive`/`unknown` omitted, `bypass` warn-colored). When narrow, keep model → effort → mode, except `bypass` which is always kept; with no capability data the line is dropped and the agent renders single-line.

A resolver mid-flight replaces `◆ waiting` on its row with `⟳ <resolver> <budget>`; when the chain exhausts it flips back to `◆ waiting`. Override a slow chain with `rimz feed resolve --override-chain`.

### Jump — the row is the link

You don't read where to go; you go. Selecting a row focuses that agent's pane via the `pane` ref on the snapshot — no view/pane number is ever printed.

- **Zellij plugin rail** — mouse click or `↑/↓` + `↵` calls `focus_pane_with_id(...)`, reconciling `pane_process_start` to refuse a stale pane.
- **Native pane** — read-only output gains a minimal key handler (`↑/↓` select, `↵` focus → mux focus command); where the terminal forwards mouse, a click does the same. The glyph + color stays the at-a-glance signal regardless of input support.

### Token-budget health (future)

An agent can expose remaining context/token budget; the sidebar will reflect it as a small color-graded gauge on line 2 (`▰▰▰▱▱ 38%`), shaded green → amber → red by the value alone so it never competes with the status glyph. Enrich-only and telemetry-gated (like tool telemetry): it never drives a decision. Backed by a future `AgentState.token_budget`.

### View-model fields this assumes

Beyond today's snapshot the rows need `AgentState.task`, `.model`, `.effort`, a last-activity timestamp (for age and ranking), and — future — `.token_budget`; `mode` and the `pane` ref already exist. The sidebar no longer projects `recently_answered` or `recent_activity`; those stay in the ledger for `rimz feed list`.

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
