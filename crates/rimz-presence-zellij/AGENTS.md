# Zellij session bridge

Local contract for `crates/rimz-presence-zellij/` — the headless wasm bridge between Zellij's plugin APIs and the RimZ host. Extends the root [AGENTS.md](../../AGENTS.md); runtime detail lives in [multiplexers.md](../../docs/internals/multiplexers.md#zellij-presence-channel).

## Boundary

- Publish Zellij facts: merged topology snapshots, attached-client observations, focus-repair requests, telemetry, and timing required by Zellij's event model.
- Keep capabilities that require plugin-only Zellij APIs here: the runtime focus keybind, mouse reconfiguration, web-session sharing, and hiding or closing the plugin instance.
- Derive meaning on the host. `SidebarEvent` taxonomy, launch-chrome filtering, topology-writer authority, and durable cache publication live in the `rimz` crate.
- Add event taxonomy host-side. Add a wake shape only for a fact that cannot be derived from an accepted snapshot diff.
- Keep one session plugin. Splitting control features across plugins multiplies lifecycle, permission, and writer-coordination complexity.

## Shape

- `main.rs` projects Zellij types and executes effects; host-testable decisions stay in `engine.rs`, `policy.rs`, and `wire.rs`.
- Keep one canonical pane map. Reducers retain partial manifests, patch event enrichment in place, and publish panes in deterministic tab/key order.
- Treat host forks as fire-and-forget facts. Hook stdout and plugin command results remain protocol channels; diagnostics stay off them.
