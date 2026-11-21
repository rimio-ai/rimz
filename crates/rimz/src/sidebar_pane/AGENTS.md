# Sidebar renderer

Local contract for `crates/rimz/src/sidebar_pane/`: the pane-resident supervisor and the room host that paints for it. Extends [crates/rimz/AGENTS.md](../../AGENTS.md). Behaviour lives in [state.md](../../../../docs/internals/sidebar/state.md) (the host and its attachments, the attach wire, the fetch cycle, hidden attachments, client views, the paint clock), [sidebar.md](../../../../docs/internals/sidebar/sidebar.md) (cards, frame composition, the serve loop), [docs/interface/sidebar.md](../../../../docs/interface/sidebar.md) (the on-screen spec), [theme.md](../../../../docs/internals/theme.md), and [pets.md](../../../../docs/internals/sidebar/pets.md): read the matching section before changing one.

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

- One [`host.rs`](./host.rs) process per mux session is the only painter.
- Nothing a pane's loop reaches reads the process's own terminal: the host has none, and an ambient read would describe some other pane or block. Geometry, cell aspect, and the cursor come from the pane's [`app/backend.rs`](./app/backend.rs), and pane identity from `ServeConfig::own_pane`.
- Room-owned state (the shared overlay, notification delivery, the room-wide unread reminder timer, reload decisions) stays with the host, never an attachment.
- [`render/`](./render/mod.rs) draws what the snapshot resolved (`SidebarProviderPanel::budget`, `SidebarRow::call_split`) and never re-reads windows, credits, or the usage blob. The renderer's one read of `web` is `web::pixel_ttyd_pids`.
- On tmux a tab switch puts focus on the destination window's active pane, which can be that tab's own sidebar. A rule keyed on focus leaving the sidebar therefore also handles a view arriving at another sidebar pane.
- A view's `animation_phase` can move backwards: raising `refresh_ms` lowers it, and co-viewers stamp one shared view from clock reads taken before the views lock, so a pane can stamp one phase behind. A rule that orders two cyclic phases handles both cases, as `ViewState::advance_head_step` does.
- A change that skips or defers folds lists every per-fold state it stops advancing (spend rolls, width-control stamps, read receipts) and every per-backend emitter it changes.
- View state that must advance with time while content stands still, such as the hook drain rate, is fed from `PaneState::reduce_snapshot` in [`app/loop_state.rs`](./app/loop_state.rs), which every delivery to a view passes, never from `ViewContext::commit_fetch`, which a clock-only delivery bypasses. A `LoopState` rig delivers a fold clock-only only when its content equals the last accepted fold's, so such a test clones every fold from one base snapshot: the `agent_snapshot` fixture stamps each row's `last_activity` with the wall clock, so two calls never compare equal.
- Client views sit under one mutex, and a pane pass reads the published record through `ViewAccess::record` in [`app/loop_state/shared.rs`](./app/loop_state/shared.rs). Reading the views handle directly takes the record mutex on every call and escapes the `ViewsRecord` pass counter the hidden-pass tests bound. Terminal output, mux calls, asset loads, and read-mark I/O never hold the views lock, and the locked path re-reads routing and the latest fold after unlocked preparation.
- A pane pass that requests a frame because a published record field moved requests it on the pass that first sees the new value, never on every pass while its own state lags: paint can be held back, and a repeated request re-arms an immediate frame for the whole hold.
- An input that acts on what the pane drew reads a record captured at draw time (`UiState`, beside `painted_roster`), never `context.current`: the input route replays publications before the handler runs.

## Render rules

- A new mark, line, or chip gets its own `Component` variant even when an existing one already resolves to the tone it wants: borrowing another role's token lets that role's retune recolor the new element.
- Measure the painted width of text the renderer did not format itself (a product name, a theme override, an account label) with `text_width` in [`render/layout.rs`](./render/layout.rs), never `chars().count()`: wide or zero-width characters misplace every cell and click target after them.
- A line reads its width tier from `RowCtx::tier`, measured on `content_width` (pane width minus 2), and never recomputes `Tier::for_width` from the pane width, so every line in a frame degrades at the same column.
- A render change updates the frames drawn from the moved `.snap` files in [docs/interface/sidebar.md](../../../../docs/interface/sidebar.md), the [docs/guide/sidebar.md](../../../../docs/guide/sidebar.md) sentence that lists the changed line's elements in on-screen order, and, for what a click or key does, that input's row in the interface page's input table.
- On a card's identity line, a token (a badge, a mark) yields width before the handle does: the handle draws exactly as on a plain card, and the token drops when it does not fit beside it.

## Tests

- `LoopState` test rigs share views by default, as every attachment does in production. A test runs without views only when that path is its subject.
- Render tests golden a full screen through the `assert_snapshot` helper in [`render/tests/`](./render/tests/mod.rs), never `insta` directly: the helper scrubs live durations and ages, so a frame never fails on elapsed time.
- Before a render change, list every frame `rg -l '<glyph or text>' --glob '*.snap'` returns, with the glyph's Unicode and Nerd Font forms (`GLYPH_CATALOG` in `theme/glyphs.rs`) both in the pattern, and narrow the list by reading each card, since width tiers move elements between lines. Preset cards and the `app::play` filmstrip escape that grep, and `insta` stops at a test's first failing frame, so once the change builds, run `INSTA_FORCE_PASS=1 INSTA_UPDATE=new cargo xtask test 'sidebar::scenario'` and the same over `'sidebar_pane'`, review each `.snap.new`, then `cargo insta accept`.
- A line that admits elements by width is tested at each element's exact drop width and one column either side.
- A change to the order of groups or rows also runs `cargo xtask test -P journey` and `cargo xtask test -P live`: journey tests drive keys over the rendered order and `journey::deep` drives scroll and focus over it.
