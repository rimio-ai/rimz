# Sidebar renderer

Local contract for `crates/rimz-sidebar/` — the renderer. Extends the root [AGENTS.md](../../AGENTS.md); it never restates parent rules. The mechanics — presence, ranking, the runtime loop, recovery — are [docs/internals/sidebar.md](../../docs/internals/sidebar.md); the on-screen look is [docs/interface/sidebar.md](../../docs/interface/sidebar.md); the glyph/status table is [docs/interface/sidebar.md → reading the glyphs](../../docs/interface/sidebar.md#reading-the-glyphs); the agent-state rollup it projects is [docs/internals/agent.md](../../docs/internals/agent.md).

## The boundary

This crate is a **pure projection** over the `SidebarSnapshot` view-model. It owns two layers only — composing the frame (`render/`) and the runtime loop (`app.rs`) — and makes no product decisions.

- **The view-model owns every decision.** Grouping, ranking, and pane→row binding are resolved once in the producer ([`ledger/snapshot.rs`](../rimz/src/ledger/snapshot.rs) in the `rimz` crate). Never re-derive them here — a renderer maps view-model fields to glyphs and nothing more. The view-model types (`SidebarSnapshot`, `SidebarRow`, …) live in `rimz`; this crate consumes them.
- **Read-only on the ledger.** Reach state only through `rimz sidebar snapshot` or the in-process consumer read (`rimz::sidebar::snapshot`); never import a ledger-writer module (CI grep). The only write is this renderer's own liveness heartbeat, via `rimz::sidebar::write_heartbeat`.
- **Panes are opaque.** Pane presence arrives in the snapshot; never call `pane capture` / `pane send`. The renderer's one mux call is geometry-only — the resize-time sibling count for self-close — and never updates the rendered snapshot. Pane width is the launch path's concern: every pane is born at the launch's width verdict (30% capped at `sidebar.max_cols`, resolved once at start), so the renderer never resizes its own pane.
- **The semantic→glyph mapping is the one cross-renderer discipline.** It lives in [`render/labels.rs`](./src/render/labels.rs) and must track the canonical table in [docs/interface/sidebar.md](../../docs/interface/sidebar.md#reading-the-glyphs) so the native pane, the Zellij rail, and the CLI read the same.

## The loop

- **The render thread never blocks** ([performance.md](../../docs/internals/performance.md)). The data fetch runs off-thread; the spinner animates from the cached snapshot on its own cadence. The pure reducer is `app::compute_next_state` — fold each fetch outcome into it and apply the returned `RenderState` verbatim.
- **Tracing defaults to `off`** so warnings never corrupt the terminal UI; gate renderer logging behind `RUST_LOG`.

## Tests

Golden the composed frames with `insta` snapshots (the `render_*` tests) and keep `compute_next_state` covered by pure unit tests — health debounce, the regression gate, the self-close latch, and the selection model (the derived baseline plus browse resolution in `reconcile_selection`). Run through `cargo xtask test`.
