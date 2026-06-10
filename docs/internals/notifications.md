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

The elected sidebar producer is the notification brain. Each fetch cycle folds the latest agent rollup, compares each root agent's current `AgentStatus` with the previous observation, applies the per-machine `[notifications]` policy, and emits notifications only from that elected process.

The first observation after election seeds the baseline without firing. That prevents a renderer restart or producer handoff from replaying every existing `waiting`, `failed`, `paused`, or `success` state as a fresh transition.

The producer applies trigger filtering, per-agent debounce, burst coalescing, and focus suppression. Focus suppression reads the same live pane focus bit the sidebar already folds into snapshots; it is a conservative visibility hint, not ledger truth.

For each notification, the producer spawns `[notifications].command` if configured and broadcasts `SidebarEvent::Notify` to the sidebar socket with the triggering agent pane ids. The command receives `RIMZ_NOTIFY_TITLE`, `RIMZ_NOTIFY_BODY`, `RIMZ_NOTIFY_AGENT`, and `RIMZ_NOTIFY_KIND`, inherits no hook stdout, and is handed to the global child reaper.

The renderer is the terminal mouth. On `SidebarEvent::Notify`, BEL is emitted only by a pane-resident renderer whose tab or window contains one of the triggering agent panes, so mux tab and window bell markers point at the work that needs you. Desktop OSC is a reachability channel: under tmux, pane-resident renderers with their own view emit the DCS-wrapped OSC 777 banner so the active client stream can carry it even when the agent is in a background window. Detached sessions have no attached terminal stream, and inactive-pane passthrough is mux/client-defined, so command delivery is the deterministic off-screen path.

`rimz remote connect` is the notification brain for remote-link loss and recovery. It emits local OSC/BEL and `[notifications].command` directly with `RIMZ_NOTIFY_KIND=link_lost` or `link_restored`; it does not broadcast a sidebar event, because the remote stream may be stalled or gone. Probe blackout emits only local OSC/BEL, so ingest-side failures do not fire the command hook.

Remote-link degraded and recovered notifications ride the normal sidebar producer path while the SSH stream is still alive. The link notification state raises `link_degraded` after a fresh degraded or bad tier holds for ten seconds, raises `link_recovered` after a fresh good tier holds for thirty seconds, and pauses both clocks while link stats are stale. The event targets the renderer's current working panes so BEL and desktop OSC follow the same reachability rules as agent notifications.

## Backend Behavior

tmux forwards OSC notifications when `allow-passthrough` is on; Rimz enables that room option by default. The renderer wraps the OSC payload as `DCS tmux; ... ST` so the local terminal emulator receives it through tmux and SSH. BEL stays targeted to the triggering agent's window; desktop OSC stays broad enough to reach the active client.

Zellij currently drops OSC 9, 777, and 99 notification sequences. `desktop = "auto"` therefore disables desktop OSC under Zellij and leaves `[notifications].command` as the portable route. The targeted BEL still marks only the Zellij tab whose sidebar shares the triggering agent's tab, so the tab bar `[!]` points at the agent that needs you. `desktop = "osc"` forces emission for users testing a future Zellij or terminal path.

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
command = "ntfy publish rimz"
```

The command is per-machine and outside project trust. It is personal routing, often carrying host-specific push credentials, and a cloned repository never inherits it.
