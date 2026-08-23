# Pets

A pet is an animated sprite on the sidebar's [provider dashboard](../../interface/sidebar.md#zone-3--the-provider-dashboard) that acts out whatever the selected card is doing. It gives the bottom panel the room's motion while the agent cards above stay steady, and it is opt-in: `[theme.pets] enabled` is off by default.

The subsystem lives in [`crates/rimz/src/sidebar_pane/pets/`](../../../crates/rimz/src/sidebar_pane/pets/) and owns everything expensive: network fetches, disk cache, WebP and PNG decode, frame slicing, cell-art conversion, track selection, and captions. The renderer receives one `PetView` — an optional body, a caption, and a frame interval — and copies it into the ratatui buffer like any other widget. That boundary is the design: no draw path ever blocks on IO.

User-facing setup is the [pets guide](../../guide/pets.md). This page is the mechanics.

## Module map

| module | job |
| --- | --- |
| [`mod.rs`](../../../crates/rimz/src/sidebar_pane/pets/mod.rs) | `PetAssets` and its load state machine, `PetView`, tier resolution, dashboard footprints, and per-frame track selection. |
| [`catalog.rs`](../../../crates/rimz/src/sidebar_pane/pets/catalog.rs) | The built-in pet ids and the fixed sheet geometry every source must match. |
| [`asset.rs`](../../../crates/rimz/src/sidebar_pane/pets/asset.rs) | Selector resolution, HTTPS fetches, the per-machine cache, local and petdex reads, offline mode, and eviction. |
| [`frames.rs`](../../../crates/rimz/src/sidebar_pane/pets/frames.rs) | Sheet decode, geometry validation, and slicing into 72 RGBA frames. |
| [`cellart.rs`](../../../crates/rimz/src/sidebar_pane/pets/cellart.rs) | Sextant downsampling from RGBA into terminal cells, and the cell-aspect probe. |
| [`model.rs`](../../../crates/rimz/src/sidebar_pane/pets/model.rs) | Pet actions, animation tracks, composed tracks, and per-track cadence. |
| [`voice.rs`](../../../crates/rimz/src/sidebar_pane/pets/voice.rs) | Canned captions, keyed by action transition. |
| [`painter.rs`](../../../crates/rimz/src/sidebar_pane/pets/painter.rs) | Pixel-sprite residency: PNG memoization, transmit, and the self-heal cadence. |
| [`preview.rs`](../../../crates/rimz/src/sidebar_pane/pets/preview.rs) | `rimz list-pets` preview loading for both tiers. |
| [`../pixel/`](../../../crates/rimz/src/sidebar_pane/pixel/) | The shared kitty graphics transport: payloads, tmux passthrough, image ids, placeholders, capability probing, and the pixel context meter. |

## One frame

The serve loop drives everything from [`app/paint.rs`](../../../crates/rimz/src/sidebar_pane/app/paint.rs). Per frame:

1. **Project the selection into an action.** `render::selected_pet_action` reduces the selected visible row to one of seven `PetAction` values.
2. **Resolve the tier.** `effective_render_tier` folds the configured mode, the terminal capabilities, and this frame's paintability into `Pixel` or `Cell`.
3. **Note newly unread rows.** `observe_unread_rows` diffs the unread row ids against the previous frame and reports whether any row became unread.
4. **Ask for a view.** `PetAssets::view` polls the loader, starts one if the pet or preparation changed, picks the track, samples a sprite index, and returns a `PetView`.
5. **Transmit, then draw.** For a pixel body the loop transmits the sprite image and its virtual placement *before* the draw that first references its image id; then ratatui draws the frame inside one synchronized-output bracket.

The body tier is `Some` only when pets are enabled, the dashboard is on screen, and the theme allows a body at all (`NO_COLOR` suppresses it and lets the caption carry the state). `frame_interval` tells the loop when to wake next: the animation grid while loading, the active track's frame duration while animating, and nothing when the pet is static.

## From card state to animation

`row_pet_action` in [`render/mod.rs`](../../../crates/rimz/src/sidebar_pane/render/mod.rs) does the projection. Compaction is tested first and wins over every status, so an agent compacting while waiting still reads as reviewing.

| selected row | action | track | default caption |
| --- | --- | --- | --- |
| agent compacting context | `Review` | `review` | `reviewing context` |
| agent waiting on your answer | `Ask` | `ask` | `someone needs you` |
| agent failed, or a stuck process row | `Failed` | `failed` | `rough patch - take a look` |
| agent paused (rate limit, API overload) | `Waiting` | `waiting` | `waiting on work` |
| agent running but parked, or with a running subagent | `Waiting` | `waiting` | `waiting on work` |
| agent reasoning | `Thinking` | `thinking` | `thinking it through` |
| agent otherwise running, or a busy process row | `Running` | `running` | `room is moving` |
| agent idle or successful, an idle process row, no selection | `Idle` | `idle` | `all caught up`, then `resting` |

Tracks index into the sheet's rows. `model.rs` holds the mapping and the cadence:

| track | sheet rows | frames | fps |
| --- | --- | --- | --- |
| `idle` | row 0 | 6 | 1.6 |
| `thinking` | row 2 ×3, then row 1 ×3 (run-left, run-right) | 48 | 4.0 |
| `running` | row 7 | 6 | 4.0 |
| `waiting` | row 6 | 6 | 3.5 |
| `review` | row 8 | 6 | 3.5 |
| `ask` | row 3 ×2, then row 6 (waving, waiting) | 14 | 3.5 |
| `jumping` | row 4 | 5 | 3.5 |
| `failed` | row 5 | 8 | 3.5 |

Two composed tracks carry meaning that no single sheet row does: `thinking` paces left then right, and `ask` waves twice before settling into waiting.

**The jump is the attention cue.** Any action change plays `jumping` once before the new steady track takes over, and a newly unread row plays it too even when the action holds. Motion in the corner of your eye means the room changed. The one-shot is time-boxed by the jump track's own loop duration, measured in animation phases so it tracks real time at whatever cadence the loop wakes.

A `[theme.animations]` role set to `effect = "static"` quiets the matching pet action through `pet_motion_enabled`: the one-shot is skipped and the steady track freezes on its first frame.

## Captions

`voice::caption` returns a line only on an action transition, so a caption shows once per change and then stands. Each action owns a pool of at least 100 lines and the frame phase selects one, so repeats are rare. Returning to idle draws from `caught_up` after real work and from `resting` on a cold start.

The pools are constrained and tested: ascii, lowercase, at most 26 columns (the sliver beside the sprite), and unique across every pool. `pool[0]` is the plain default listed in the table above. `[theme.pets] voice = false` keeps the animation and drops the captions.

## Assets

The `pet` selector resolves to one of four sources, in this order:

| selector | source | example |
| --- | --- | --- |
| a built-in catalog id | the public Codex pets CDN, cached | `rocky` |
| an `http(s)://` URL | fetched over HTTPS, cached | `https://example.com/pet.webp` |
| path-like (contains `/` or `.`, or starts with `~`) | a local sheet file, or a petdex directory | `~/art/pet.png` |
| a bare slug | a petdex install under `~/.codex/pets/<slug>/` | `wall-e` |

The built-ins are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`, each served as `<id>-spritesheet-v4.webp`.

**Geometry is fixed for every source:** a `1536x1872` WebP or PNG holding an `8x9` grid of `192x208` frames, 72 in total. It is validated before caching and before decoding, so a wrong-shaped sheet fails with a geometry error rather than a garbled pet. Alpha becomes transparent terminal cells.

**Fetched bytes get a cache; user files stay read-only.** The cache lives at `$XDG_CACHE_HOME/rimz/pets/v1/assets/`, falling back to `$HOME/.cache/...` and then a temp root, and writes go through temp-file plus rename. Remote URLs get a stable cache filename derived from a SHA-256 of the URL. Resolution is cache-first: a valid entry is served as-is, a corrupt one is removed and re-fetched, and a decode failure evicts only entries RimZ wrote. Local sheets and petdex installs belong to the user, so a failed decode leaves them exactly where they are.

Fetch policy: HTTPS only (plain `http://` is refused with a clear message rather than a fetch error), staged 5s connect / 10s response / 30s body timeouts, a 16 MiB cap, and up to three attempts with linear backoff. A failed fetch writes no cache entry, so the next run retries it while cached pets still load from disk.

A petdex install is a directory holding `pet.json` beside its sheet; RimZ reads `spritesheetPath` from the manifest and ignores the rest of the metadata. `RIMZ_PETS_OFFLINE=1` serves the cache only for built-ins and URLs; petdex and local sheets already read from disk.

## The load state machine

`PetAssets` holds exactly one of three states per pet, plus the animation bookkeeping. Loads run on a named background thread and report through an `mpsc` channel the serve loop polls without blocking.

```text
                  ┌──────────┐  pet or preparation changed  ┌─────────┐
   (none) ───────►│ loading  │◄─────────────────────────────│ loaded  │
                  └────┬─────┘                              └─────────┘
                       │ Ok                                      ▲
                       ├─────────────────────────────────────────┘
                       │ Err
                       ▼
                  ┌──────────┐   20s cooldown elapsed
                  │  failed  │──────────────────────────► loading
                  └──────────┘
```

A `PreparationKey` of `(tier, footprint, cell aspect)` keys the loaded state alongside the pet id. Changing the pet, flipping tiers, or resizing invalidates it and starts a fresh load; cell aspect participates only for the cell tier, since pixel frames ignore it. Disabling pets releases loaded, pending, failed, action, caption, and unread state in one step.

The failure latch is deliberate. A first fetch can fail transiently — a cold network the moment pets are switched on, a CDN blip — and latching forever would strand the pet on `pet unavailable` for the session. A 20-second cooldown lets it self-heal without spawning a loader thread every frame.

The two tiers keep different things in memory. Cell tier renders all 72 grids on the background thread and drops the decoded RGBA sheet before publishing, so the renderer holds only compact `char + fg + bg` grids. Pixel tier retains the RGBA frames, because the painter re-encodes them to PNG on demand.

## Render tiers

`resolve_render_tier` is the pure resolver over the configured mode and the probed capabilities:

| `[theme.pets] glyphs` | needs | result |
| --- | --- | --- |
| `sextant` | nothing | `Cell` |
| `pixel` | pixel transport | `Pixel`, else `Cell` |
| `auto` (default) | pixel transport **and** kitty-capable rendering clients | `Pixel`, else `Cell` |

`effective_render_tier` then folds in this frame's reality: `[theme.display] pixel = "off"` forces `Cell` outright, and a resolved pixel tier downgrades to `Cell` when there is no provider block to ride beside or the body is suppressed. A cell tier always passes through unchanged, so `sextant` stays sextant.

Capabilities come from [`pixel/probe.rs`](../../../crates/rimz/src/sidebar_pane/pixel/probe.rs) as two independent facts:

- **Pixel transport.** Inside tmux: version 3.6 or newer *and* `allow-passthrough` set to `on` or `all` on the pane (falling back to the session). Standalone: always available. Zellij 0.45 can terminate kitty graphics and `rimz doctor` queries that capability, but its server rejects RimZ's unicode-placeholder placement; the renderer therefore keeps both capability bits off until it has a cursor-placement path.
- **Kitty clients.** Inside tmux: every rendering client either advertises `xterm-ghostty`, `ghostty`, `xterm-kitty`, or `kitty`, or descends from the live ttyd daemon serving RimZ's current pixel compatibility protocol; control-mode clients are excluded. Standalone: `$TERM` matches the native-terminal list.

Live re-probes fold failures onto the previous reading — a command that fails keeps the last known fact rather than flapping the tier — and every runtime probe only reads. Resize re-probes immediately; a ten-second tmux backstop covers equal-size browser attaches and detaches. One startup command writes: it raises the sidebar's own pane from `allow-passthrough on` to `all`, so graphics transmitted while the window is hidden still reach the terminal. A pane already at `all`, and a user's explicit `off`, stay as they are.

Sextant is the downgrade target because it is the portable baseline. Capability misses, Zellij's unsupported placement mode, `NO_COLOR`, a suppressed body, and a dashboard with no provider column all converge on it.

**Geometry follows the tier.** Pixel pets reserve `15x9` cells and cell-art pets reserve `18x9`, with one empty row under either body.

## Cell art

Each frame downsamples into a grid of `char + fg + bg` cells that ratatui copies like any other line, so cell art survives tmux, Zellij, detached sessions, plain terminals, and every color depth.

Sextants split one terminal cell into a `2x3` subcell grid. The converter averages source pixels in linear light, then picks the best foreground/background split per cell; coverage at or above the ink threshold is sprite, the rest is terminal background.

Terminal cells are taller than they are wide, so a naive fit distorts the sprite. The converter aspect-fits each frame inside the footprint at subcell resolution and bottom-aligns it, keeping the pet's feet planted. The ratio resolves in a fixed precedence:

1. `[theme.pets] cell_aspect`, when set.
2. The pty probe, reading pixel and cell dimensions from `TIOCGWINSZ`.
3. `13/6`, the neutral fallback where the historical `36x27` subcell sample preserves a `192x208` frame exactly.

The explicit config exists because Zellij reports zero pty pixel dimensions; tmux 3.4 and newer forwards them. Under `NO_COLOR` the body is suppressed entirely and the caption carries the pet's state.

## Pixel tier

The pixel tier sends the same decoded frames through the kitty graphics protocol while staying inside ratatui's normal buffer diff. Each placeholder cell is one styled grapheme cluster — U+10EEEE plus row and column combining marks — with the foreground RGB encoding the kitty image id. Rect shifts and frame changes are then ordinary cell diffs.

Ghostty's kitty support covers placement but not the animation-frame actions (`a=f`/`a=a`), so the renderer drives animation by cycling image ids instead. Sprite ids are stable slots directly above the painter's id base; a pet change re-transmits the new sheet through the same ids so terminal image data is replaced in place.

Three properties keep it robust:

- **Atomic frames.** The painter brackets the image transmit and the ratatui draw in synchronized output (DECSET 2026) so each frame lands as one redraw. RimZ applies `*:sync` during tmux room setup, so tmux forwards the bracket to the terminal.
- **Self-healing residency.** Virtual placements stay resident for the renderer session, and image data re-transmits on a bounded cadence (2s staleness, 250ms minimum spacing between batches) so a dropped tmux passthrough or a terminal image-store eviction recovers on its own. Sprites are encoded to PNG once and memoized, so a re-send costs compressed bytes rather than raw RGBA.
- **Bounded traffic.** Deletes happen at teardown only, and graphics APCs stay bounded because macOS terminals can re-evaluate the mouse pointer on every image update. `glyphs = "sextant"` is the escape hatch for anyone who wants that traffic gone entirely.

tmux receives every graphics escape through its passthrough DCS wrapper, and placement uses kitty Unicode placeholders, so redraws and pane repaints keep ownership in the sidebar pane.

The context meter shares this transport and this id space, interning each distinct quantized raster under its own id from a reserved offset with an LRU residency window. Its mechanics live in [`pixel/meter.rs`](../../../crates/rimz/src/sidebar_pane/pixel/meter.rs).

## rimz list-pets

The preview command shares the catalog, cache, tier resolver, and footprints with the live dashboard, and loads at most two pets at a time on a cold cache. Built-ins come first, then installed petdex pets labeled by selectable slug. Because a failed fetch leaves no cache entry, a re-run serves the pets that worked from disk and retries only the ones still missing.

Multi-image pixel previews inside tmux are paced ([`cli/list_pets/pacing.rs`](../../../crates/rimz/src/cli/list_pets/pacing.rs)): after each pet image the command waits for the terminal's kitty graphics acknowledgement, proving the real terminal consumed the passthrough before the next image starts, so tmux's output-discard repaint path cannot drop later image data. A terminal that stays silent past a short timeout drops to the unpaced best-effort path for the rest of the command. On exit the pacer gives any owed acknowledgement a grace period, so the terminal's reply is consumed instead of leaking into the shell as typed input.

The live sidebar and the gallery share the ratatui placeholder path; `rimz list-pets` keeps a standalone one-shot renderer and paints gallery columns through separate image-id ranges, so one column cannot delete or ghost another's image.

## Security surface

Pets execute no commands, which is why `[theme.pets]` stays outside the project trust hash. The visible surface is asset egress: a request to the Codex CDN for a built-in, or to the HTTPS host you configured. Prompts, transcripts, pane text, workspace paths, and provider credentials never leave the box on this path, and `RIMZ_PETS_OFFLINE=1` removes the egress entirely. The user-facing statement is one bullet in [security.md](../../guide/security.md#what-leaves-your-machine).

## Where to make a change

| you want to | change |
| --- | --- |
| map a card state to a different pet action | `row_pet_action` in `render/mod.rs` |
| retime or recompose an animation track | the `ANIMATIONS` table in `model.rs` |
| add caption lines | the matching pool in `voice.rs`; the tests enforce width, ascii, and uniqueness |
| add a built-in pet | `BUILTIN_PETS` in `catalog.rs` |
| accept a new selector form | `resolve_pet_source` and `PetSource` in `asset.rs` |
| change when pixels are allowed | `resolve_render_tier` for policy, `pixel/probe.rs` for the facts |
| change the dashboard footprint | `DASHBOARD_PIXEL_PET` / `DASHBOARD_CELL_PET` in `mod.rs`, and the dashboard layout beside it |
