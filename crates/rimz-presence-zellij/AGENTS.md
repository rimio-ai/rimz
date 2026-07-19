# Zellij session bridge

Local contract for `crates/rimz-presence-zellij/` — the headless wasm bridge between Zellij's plugin APIs and the RimZ host. Extends the root [AGENTS.md](../../AGENTS.md); runtime detail lives in [multiplexers.md](../../docs/internals/multiplexers.md#zellij-presence-channel).

## Boundary

- Ship observations, never verdicts. Publish Zellij facts — merged topology snapshots, attached-client observations (including the settled sample after a tab switch), telemetry, and the timing Zellij's event model requires — and let the host turn them into meaning.
- Carry only facts that originate in Zellij's server state. A fact derivable from the OS routes host-side through `pane_pid`: the host owns `/proc`; the plugin owns the event stream.
- Keep capabilities that require plugin-only Zellij APIs here: the runtime focus keybind, mouse reconfiguration, web-session sharing, and hiding or closing the plugin instance.
- Derive meaning on the host. Pane roles (sidebar, card pane), focus-repair decisions, `SidebarEvent` taxonomy, launch-chrome filtering, topology-writer authority, and durable cache publication live in the `rimz` crate, so a product-policy change never requires a plugin release.
- Add a wake shape only for a fact that cannot be derived from an accepted snapshot diff.
- Keep one session plugin. Splitting control features across plugins multiplies lifecycle, permission, and writer-coordination complexity.

## Shape

- `main.rs` projects Zellij types and executes effects; host-testable decisions stay in `engine.rs`, `policy.rs`, and `wire.rs`.
- Keep one canonical pane map. Reducers retain partial manifests, patch event enrichment in place, and publish panes in deterministic tab/key order.
- Treat host forks as fire-and-forget facts. Hook stdout and plugin command results remain protocol channels; diagnostics stay off them.
