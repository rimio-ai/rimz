# Sidebar renderer

Local contract for `crates/rimz/src/sidebar_pane/`: the pane-resident supervisor and the room host that paints for it. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Behaviour lives in [state.md](../../../../docs/internals/sidebar/state.md) (the host and its attachments, the attach wire, the fetch cycle, hidden attachments, client views, the paint clock), [sidebar.md](../../../../docs/internals/sidebar/sidebar.md) (cards, frame composition, the serve loop), [docs/interface/sidebar.md](../../../../docs/interface/sidebar.md) (the on-screen spec), [theme.md](../../../../docs/internals/theme.md), and [pets.md](../../../../docs/internals/sidebar/pets.md): read the matching section before changing one. [app/AGENTS.md](./app/AGENTS.md) extends this one for the pane loop, and [render/AGENTS.md](./render/AGENTS.md) for drawing.

Before handing off any renderer change, follow the [sidebar live check](../../../../docs/contributing/sidebar-live-check.md).

## Invariants

`cargo xtask invariants` enforces each of these.

- **Read-only on the store.** No store-writer or store atomic-publish import anywhere under this tree, tests included: a fixture writes its runtime files with serde and `std::fs::write`. [`store/`](../store/AGENTS.md) writes.
- **Event-log history enters through a `RollupCursor` fold**, the rule that also binds [`sidebar/`](../sidebar/AGENTS.md).
- **No spending walker is constructed here**, tests included. Spend comes from the elected spending service.
- **Rendering reads the snapshot clock.** No `Timestamp::now()` in non-test code under `render/`, so a frame is reproducible from its snapshot alone.
- **The loop reads the driver clock.** `app/`'s `loop_state.rs`, `loop_state/shared.rs`, `view_state.rs`, `views.rs`, `health.rs`, `remind.rs`, and `timing.rs` sample no wall clock and call no `.elapsed()` outside inline tests: loop time is passed in.
- **One mint grants an image permit**, the mint function in [`app/image_permit.rs`](./app/image_permit.rs). The permit type has [`pixel/permit.rs`](./pixel/permit.rs) to itself, so add nothing else to that file. Its production and tuple constructors are rejected everywhere else under `crates/rimz/src` outside test and testkit sources. `LoopState` tests reach images through a client sample or a jump landing, never through the testkit-only constructor.
- **Only the theme pipeline mints a color.** Render code resolves every color through a component token, `theme.component(Component::…)` in [`render/theme.rs`](./render/theme.rs), or a semantic accessor. `Color::Reset` is the one ratatui `Color` a render file may name, indexed and true-color constructors included. The carrier layer is exempt: `render/theme.rs` turns a tone into a ratatui color and [`render/ansi.rs`](./render/ansi.rs) quantizes it for a limited terminal.
- **Every glyph resolves through `theme.glyph(GlyphRole::…)`**, owned by the shared core in [`theme/`](../theme/mod.rs). The invariant carries the banned-literal list.

## The process

- Nothing a pane's loop reaches reads the process's own terminal: the host has none, and an ambient read would describe some other pane or block. Geometry, cell aspect, and the cursor come from the pane's [`app/backend.rs`](./app/backend.rs), and pane identity from `ServeConfig::own_pane`.
- Room-owned state (the shared overlay, notification delivery, the room-wide unread reminder timer, reload decisions) stays with the host, never an attachment.

## Tests

- A change to the order of groups or rows also runs `cargo xtask test -P journey` and `cargo xtask test -P live`: journey tests drive keys over the rendered order and `journey::deep` drives scroll and focus over it.
