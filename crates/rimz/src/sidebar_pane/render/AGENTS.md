# Sidebar drawing

Local contract for `crates/rimz/src/sidebar_pane/render/`. Extends [sidebar_pane/AGENTS.md](../AGENTS.md).

- Draw what the snapshot resolved (`SidebarProviderPanel::budget`, `SidebarRow::call_split`), and never re-read windows, credits, or the usage blob. The one read of `web` here is `web::pixel_ttyd_pids`.
- A new mark, line, or chip gets its own `Component` variant even when an existing one already resolves to the tone it wants: borrowing another role's token lets that role's retune recolor the new element.
- Measure the painted width of text the renderer did not format itself (a product name, a theme override, an account label) with `text_width` in [`render/layout.rs`](./layout.rs), never `chars().count()`: wide or zero-width characters misplace every cell and click target after them.
- A line reads its width tier from `RowCtx::tier`, measured on `content_width` (pane width minus 2), and never recomputes `Tier::for_width` from the pane width, so every line in a frame degrades at the same column.
- A render change updates the frames drawn from the moved `.snap` files in [docs/interface/sidebar.md](../../../../../docs/interface/sidebar.md), the [docs/guide/sidebar.md](../../../../../docs/guide/sidebar.md) sentence that lists the changed line's elements in on-screen order, and, for what a click or key does, that input's row in the interface page's input table.
- On a card's identity line, a token (a badge, a mark) yields width before the handle does: the handle draws exactly as on a plain card, and the token drops when it does not fit beside it.

## Tests

- Render tests golden a full screen through the `assert_snapshot` helper in [`render/tests/`](./tests/mod.rs), never `insta` directly: the helper scrubs live durations and ages, so a frame never fails on elapsed time.
- Before a render change, list every frame `rg -l '<glyph or text>' --glob '*.snap'` returns, with the glyph's Unicode and Nerd Font forms (`GLYPH_CATALOG` in `theme/glyphs.rs`) both in the pattern, and narrow the list by reading each card, since width tiers move elements between lines. Preset cards and the `app::play` filmstrip escape that grep, and `insta` stops at a test's first failing frame, so once the change builds, run `INSTA_FORCE_PASS=1 INSTA_UPDATE=new cargo xtask test 'sidebar::scenario'` and the same over `'sidebar_pane'`, review each `.snap.new`, then `cargo insta accept`.
- A line that admits elements by width is tested at each element's exact drop width and one column either side.
