# Sidebar renderer

Local contract for `crates/rimz/src/sidebar_pane/` — the pane-resident renderer process. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). The renderer split lives in [docs/internals/sidebar/sidebar.md](../../../../docs/internals/sidebar/sidebar.md), the on-screen spec in [docs/interface/sidebar.md](../../../../docs/interface/sidebar.md), the color pipeline in [theme.md](../../../../docs/internals/theme.md), and the pet in [pets.md](../../../../docs/internals/sidebar/pets.md).

## The process

- [`app.rs`](./app.rs) owns the serve loop; [`render/`](./render/mod.rs) turns a `SidebarSnapshot` into cells. Reducers in [`app/state.rs`](./app/state.rs) stay pure, and [`supervise.rs`](./supervise.rs) owns convergence.
- Snapshots arrive in process on the fetch worker in [`app/fetch.rs`](./app/fetch.rs), which reads through [`sidebar::consumer`](../sidebar/consumer.rs). `rimz sidebar snapshot` is the inspection and scripting delegate over the same library, never this process's data path.
- Rendering reads the snapshot clock. `cargo xtask invariants` rejects `Timestamp::now()` in non-test render code, which is what keeps a frame reproducible from its snapshot alone.
- Renderer-local state — row and group order holds, selection, width control — stays renderer-local and never travels back into the data plane.

## Read-only on the store

- The renderer draws; [`store/`](../store/AGENTS.md) writes. The invariant rejects a store-atomic or store-writer import anywhere under this tree, and it greps text, so state the rule in prose rather than pasting the banned paths.
- Event-log history enters through a `RollupCursor` fold, the same rule that binds [`sidebar/`](../sidebar/AGENTS.md).

## The theme edge

- Resolve every color through a component token, `theme.component(Component::…)` in [`render/theme.rs`](./render/theme.rs), or a semantic accessor. The invariant rejects every named ratatui `Color` variant in render code — including the indexed and true-color constructors — leaving `Color::Reset` as the single literal a render file may name, so only the theme pipeline mints a color.
- Resolve every glyph through `theme.glyph(GlyphRole::…)`, which the shared core in [`theme/`](../theme/mod.rs) owns. The invariant carries the banned-literal list, which is what keeps the legend and the frame in step.
- The carrier layer is exempt: [`render/theme.rs`](./render/theme.rs) and its component tokens turn a tone into a ratatui color, and [`render/ansi.rs`](./render/ansi.rs) quantizes that carrier down for a limited terminal. Add a color or glyph there and name it by role everywhere else.

## Pets

[`pets/`](./pets/mod.rs) owns asset loading, sprite slicing, cell-art conversion, track selection, and captions. The renderer receives a `PetView` and nothing more, so decode, cache, and I/O stay inside the module.

## Tests

Render tests golden a full screen through the `assert_snapshot` helper in [`render/tests/`](./render/tests/mod.rs), which pins `insta` settings and scrubs live durations and ages before comparing — reach for it rather than calling `insta` directly, so a frame never fails on elapsed time. Reducer, selection, and ordering tests stay pure and in-module.
