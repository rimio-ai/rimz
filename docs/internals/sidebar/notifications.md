# Notifications

Rimz sends best-effort attention alerts from the same state that drives the sidebar. The ledger remains truth; notifications are latency and reachability.

## Channels

| Channel | Mechanism | Crosses SSH | tmux | Zellij | Notes |
| --- | --- | --- | --- | --- | --- |
| In-band attention | sidebar rows, ranking, and glyphs | n/a | yes | yes | The authoritative user-facing surface. |
| Desktop banner | OSC 777 written by pane renderers; DCS-wrapped under tmux | yes | yes | no | Ghostty, iTerm2, and WezTerm turn the OSC into a native desktop banner. Zellij drops notification OSCs today. |
| Sound | BEL written by the renderer | yes | yes | partial | The terminal owns whether BEL is audible. |
| Notify command | per-machine shell command spawned by the elected producer | command-defined | yes | yes | The portable escape hatch for push services, detached rooms, and Zellij. |
| Remote-link alert | local `rimz remote connect` supervisor writes OSC/BEL and spawns the same notify command for confirmed drops/recoveries | local only | yes | best-effort | Lost and restored edges are emitted locally because a dead SSH link cannot rely on the remote-rendered sidebar. Probe blackout emits terminal-local OSC/BEL only. |

## Producer And Renderer Split

The elected sidebar producer is the notification brain. Each fetch cycle folds the latest snapshot, compares each projected sidebar row's displayed `AgentStatus` with the previous observation, applies the per-machine `[notifications]` policy, and emits notifications only from that elected process.

The first observation after election seeds the baseline without firing. That prevents a renderer restart or producer handoff from replaying every existing `waiting`, `failed`, `paused`, or `success` state as a fresh transition.

The producer applies trigger filtering, per-agent debounce, burst coalescing, and focus suppression. Focus suppression reads the same live pane focus bit the sidebar already folds into snapshots; it is a conservative visibility hint, not ledger truth.

For each notification, the producer spawns `[notifications].command` if configured and broadcasts `SidebarEvent::Notify` to the sidebar socket with the triggering agent pane ids. The command receives `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND`; reminders also receive `RIMZ_NOTIFY_UNREAD` with the unread actionable count. The child inherits no hook stdout and is handed to the global child reaper.

The renderer is the terminal mouth. The sticky tab and window bell is a marker the renderer cannot retract, so on `SidebarEvent::Notify` it is bound to current unread attention rather than the bare status edge: a pane-resident renderer rings only when a triggering agent pane it owns still maps to an unread row — the same `UnreadTracker` bit it stamps onto each card. Unread is a pending human look: it stays set until you look (focus the pane, or a peer renderer's read receipt) or the row leaves the snapshot, never clearing on the agent's own return to running. The bell and the card therefore clear together when you look, rather than the bell outliving the card. A daemon-only view (whose siblings are infrastructure host panes, never agents that need you) never rings. Link reachability alerts and unread reminders carry that re-check cleared and ring directly on an owned, non-daemon pane. Desktop OSC is a reachability channel: under tmux, pane-resident renderers with their own view emit the DCS-wrapped OSC 777 banner so the active client stream can carry it even when the agent is in a background window. Detached sessions have no attached terminal stream, and inactive-pane passthrough is mux/client-defined, so command delivery is the deterministic off-screen path.

Unread reminders are renderer-local. A renderer starts or refreshes the reminder clock when it relays an initial terminal notification, and also starts it when unread `waiting`/`failed` scope first appears in the renderer's folded snapshot; `success` stays unread visually but sits outside the reminder scope, and `paused` never stamps unread at all because it auto-recovers when the provider window resets. Pane-backed rows ring only in views that own the row's pane. Paneless rows ring through non-focused working panes in the same visible worktree when such panes exist; fully detached paneless asks still rely on the sidebar row and notify-command path. The reminder respects `suppress_focused`, stops when the scope is empty, emits terminal OSC/BEL through the same renderer path, and spawns the notify command with `RIMZ_NOTIFY_KIND=reminder` and `RIMZ_NOTIFY_UNREAD=N`. No reminder is broadcast back through `SidebarEvent::Notify`, so multiple sidebar views dedupe by pane ownership rather than by a global producer lock.

Unread and notifications share the same projected status-edge shape after their first baselines, with one split in the eligible set: a transition into `waiting`, `failed`, or `success` stamps unread, while every trigger in the policy — `paused` included — is eligible to notify. `paused` notifies but never stamps unread, because it parks on a provider limit and resumes on its own with nothing for you to do. They also diverge by design: trigger filtering, debounce, coalescing, and focus suppression can swallow a push while unread still stamps; producer-global notifications are one-per-room while unread is renderer-local with read receipts; reminders narrow the unread set to actionable `waiting`/`failed` rows. The producer decides whether to push; the renderer decides whether the sticky tab bell rings, gating it on the triggering row's live unread bit at emit time. Because unread stays set until you look, the bell rings whenever the producer fires on a still-unread row and clears with the card — so a `paused` push (never unread) reaches desktop and command but leaves no tab `[!]`.

`rimz remote connect` is the notification brain for remote-link loss and recovery. It emits local OSC/BEL and `[notifications].command` directly with `RIMZ_NOTIFY_KIND=link_lost` or `link_restored`; it does not broadcast a sidebar event, because the remote stream may be stalled or gone. Probe blackout emits only local OSC/BEL, so ingest-side failures do not fire the command hook.

Remote-link degraded and recovered notifications ride the normal sidebar producer path while the SSH stream is still alive. The link notification state raises `link_degraded` after a fresh degraded or bad tier holds for ten seconds, raises `link_recovered` after a fresh good tier holds for thirty seconds, and pauses both clocks while link stats are stale. The event targets the renderer's current working panes so BEL and desktop OSC follow the same reachability rules as agent notifications.

## Backend Behavior

tmux forwards OSC notifications when `allow-passthrough` is on; Rimz enables that room option by default. The renderer wraps the OSC payload as `DCS tmux; ... ST` so the local terminal emulator receives it through tmux and SSH. BEL stays targeted to the triggering agent's window; desktop OSC stays broad enough to reach the active client.

Zellij currently drops OSC 9, 777, and 99 notification sequences. `desktop = "auto"` therefore disables desktop OSC under Zellij and leaves `[notifications].command` as the portable route. The targeted BEL marks only the Zellij tab whose sidebar shares an unread triggering agent — never a daemon-only `rimzd` tab — so the tab bar `[!]` points at an agent that still needs you and clears when you visit the tab and look, not when the agent resumes on its own. `desktop = "osc"` forces emission for users testing a future Zellij or terminal path.

## Trace Log

A tab `[!]` with no matching unread card is invisible after the fact, so every notification decision appends to `notify.log.jsonl` in the workspace state directory (`~/.local/state/rimz/workspaces/<id>/`), beside the anomaly `diag.log.jsonl` and rotated at the same 1 MiB cap. The log is diagnostic evidence written through the same `DiagSink` that carries workspace identity to each emission site; correctness never reads it, and nothing rate-limits it, so the full timeline survives.

Three record kinds reconstruct an episode:

- `notification_emitted` — the producer flushed a notification: its kind, the named agents each with the `prev_status → new_status` edge that caused it, the targeted panes, and a reminder's `unread_count`.
- `bell_ring` — a renderer reached a tab-bell decision: whether it `fired`, the panes, the kind, and a `suppressed` reason when it did not (`no_own_view`, `daemon_view`, `pane_not_in_view`, `not_unread`).
- `unread_marked` / `unread_cleared` — a row reached a pending-look status (with that status), or left one with the clear `cause` (`focus`, `receipt`, `row_gone`).

To trace a stray tab marker, grep the agent or pane and read the timeline: a `bell_ring` with `fired: true` whose only later clear is an `unread_cleared` with `cause: row_gone` is the non-retractable marker outliving a row that vanished before you looked, while `suppressed: not_unread` is the gate correctly refusing to ring a row no longer needing a look.

## Configuration

Notification preferences live in `~/.config/rimz/config.toml`, not in `.rimz/config.toml`.

```toml
[notifications]
enabled = true
triggers = ["waiting", "failed", "paused", "success"]
desktop = "auto"          # "auto" | "osc" | "off"
sound = "bell"            # "bell" | "off"
suppress_focused = true
debounce_ms = 5000
coalesce_ms = 1000
remind_secs = 60
command = "ntfy publish rimz"
```

`remind_secs = 0` disables reminders. Desktop badge APIs are terminal- and OS-specific, so Rimz exports the unread count to the command path instead of writing a dock badge escape itself.

The command is per-machine and outside project trust. It is personal routing, often carrying host-specific push credentials, and a cloned repository never inherits it.
