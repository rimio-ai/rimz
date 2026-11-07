# Zellij session bridge

Local contract for `crates/rimz-presence-zellij/` — the headless wasm bridge between Zellij's plugin APIs and the RimZ host. Extends the root [AGENTS.md](../../AGENTS.md); runtime detail lives in [multiplexers.md](../../docs/internals/multiplexers.md#the-zellij-presence-plugin).

## Boundary

- Ship observations, never verdicts. Publish Zellij facts — merged topology snapshots, attached-client observations (including the settled sample after a tab switch), human-input timestamps, telemetry, and the timing Zellij's event model requires — and let the host turn them into meaning.
- Carry only facts that originate in Zellij's server state. A fact derivable from the OS routes host-side through `pane_pid`: the host owns `/proc`; the plugin owns the event stream.
- Keep capabilities that require plugin-only Zellij APIs here: the runtime room keymap, pressing-client focus queries for the palette, mouse reconfiguration, web-session sharing, and hiding or closing the plugin instance.
- Derive meaning on the host. Pane roles (sidebar, card pane), focus-repair decisions, `SidebarEvent` taxonomy, launch-chrome filtering, topology-writer authority, and durable cache publication live in the `rimz` crate, so a product-policy change never requires a plugin release.
- Add a wake shape only for a fact that cannot be derived from an accepted snapshot diff.
- Keep one session plugin. Splitting control features across plugins multiplies lifecycle, permission, and writer-coordination complexity.

## Shape

- `main.rs` projects Zellij types and executes effects; host-testable decisions stay in `engine.rs`, `policy.rs`, and `wire.rs`.
- Request the same five permissions the host seeds, including unconditional `InterceptInput`. Project `UserAction` to own-client identity, its CLI tag, and whether a mouse action is buttonless motion; never retain or forward keys, pasted text, or another action payload. CLI actions, other clients, and buttonless motion supply no input stamp.
- Publish input independently of topology authority and muting: leading edge immediately, then at most once per second with the latest action-time stamp and a trailing wake. Keepalives, topology dumps, and retained snapshots never manufacture input.
- Dispatch granted indexed key commands and palette key presses independently of topology authority and muting: `MessagePluginId` targets the pressing client's replica, whose keys remain live when another client wins topology publication. A broadcast palette pipe stays gated on muting, so it opens one palette.
- Subscribe to `ModeUpdate` only when the keymap requests `report_modes`. Each replica publishes its own client's lowercase mode word, not a room-mode verdict, through `sidebar wake --reason mode --session-name S --client-id N --mode WORD --at-ms T`. Publish changes without throttling or topology, independently of muting; the first grant publishes `normal` to clear a reused client's lane. Per-client stamps increase even when changes share one millisecond, so reordered host children cannot restore an older mode. Native keymaps request no mode reporting and fork no mode wakes.
- Keep one canonical pane map. Reducers retain partial manifests, patch event enrichment in place, and publish panes in deterministic tab/key order.
- Publish the viewed-tab observation as `panes[].tab_viewed`, true only, uniformly across each tab's panes. Latch stable tab ids from `active` or `other_focused_clients`, prune ids absent from `TabUpdate`, and clear all on a current-session zero-client `SessionUpdate`. Every latch change schedules a publish even without a changed manifest. The clear excludes the transient detached-birth viewer; the host owns viewport proof and width policy.
- Treat host forks as fire-and-forget facts. Hook stdout and plugin command results remain protocol channels; diagnostics stay off them.
