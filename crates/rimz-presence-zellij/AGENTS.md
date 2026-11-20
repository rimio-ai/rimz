# Zellij session bridge

Local contract for `crates/rimz-presence-zellij/`, the headless wasm plugin between Zellij's plugin APIs and the RimZ host. Extends the root [AGENTS.md](../../AGENTS.md). What the plugin publishes, its timing, pipes, permissions, and writer fencing are in [multiplexers.md](../../docs/internals/multiplexers.md#the-zellij-presence-plugin): read the subsection you are changing.

## Boundary

- Ship observations, never verdicts. The plugin publishes facts that originate in Zellij's server state, and the host derives every meaning (pane roles, focus repair, the `SidebarEvent` taxonomy, launch-chrome filtering, topology-writer authority, durable cache publication), so a product-policy change never requires a plugin release. A replica reports its own client's facts, never a room verdict.
- A fact derivable from the OS routes host-side through `pane_pid`: the host owns `/proc`.
- A capability lives here only when it needs a plugin-only Zellij API.
- Add a wake shape only for a fact that cannot be derived from an accepted snapshot diff.
- Keep one session plugin. Splitting control features across plugins multiplies lifecycle, permission, and writer-coordination complexity.
- Never retain or forward keys, pasted text, or any other `UserAction` payload. An action yields only own-client identity, its CLI tag, and whether a mouse action is buttonless motion.
- Request the same permissions the host seeds.

## Shape

- `main.rs` only projects Zellij types and executes effects. Decisions stay in `engine.rs`, `policy.rs`, and `wire.rs`, which use no `zellij-tile` type and so test on the host target.
- Topology authority and muting gate topology publication, client queries, and a broadcast palette pipe. Input and mode publication and indexed-key, palette-key, and guard dispatch ignore both: each acts for its own client's replica, which stays live when another client wins topology publication.
- Keepalives, topology dumps, and retained snapshots never manufacture input.
- Host forks are fire-and-forget. Plugin command results are a protocol channel: keep diagnostics off them.
- A source edit here makes the vendored wasm stale, and the pre-commit hook rejects it. Run `cargo xtask plugin-refresh` before committing the source, leave the artifact unstaged for that commit, then commit the artifact and provenance on their own ([build identity](../../docs/internals/multiplexers.md#build-identity-and-embedding)).
