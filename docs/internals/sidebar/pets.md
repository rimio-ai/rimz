# Pets

A pet is an animated sprite on the sidebar's [provider dashboard](../../interface/sidebar.md#zone-3--the-provider-dashboard) that acts out what the selected card is doing. It is opt-in: `[theme.pets] enabled` is off by default, and the default `pet` is `rocky`.

The pets module owns everything expensive, and the renderer owns nothing but a copy. [`sidebar_pane/pets/`](../../../crates/rimz/src/sidebar_pane/pets/) does the network fetches, disk cache, WebP and PNG decode, frame slicing, cell-art conversion, track selection, and captions. The renderer receives one `PetView` (an optional body, an optional caption, and an optional frame interval) and copies it into the ratatui buffer like any other widget, so no draw path blocks on IO.

User-facing setup is the [pets guide](../../guide/pets.md). This page is the mechanics.

## Module map

| module | job |
| --- | --- |
| [`pets/mod.rs`](../../../crates/rimz/src/sidebar_pane/pets/mod.rs) | `PetAssets` and its load state machine, `PetView`, tier resolution, dashboard footprints, and per-frame track selection |
| [`pets/catalog.rs`](../../../crates/rimz/src/sidebar_pane/pets/catalog.rs) | the built-in pet ids and the sheet geometry every source must match |
| [`pets/asset.rs`](../../../crates/rimz/src/sidebar_pane/pets/asset.rs) | selector resolution, HTTPS fetches, the per-machine cache, local and petdex reads, offline mode, and eviction |
| [`pets/frames.rs`](../../../crates/rimz/src/sidebar_pane/pets/frames.rs) | sheet decode, geometry validation, and slicing into 72 RGBA frames |
| [`pets/cellart.rs`](../../../crates/rimz/src/sidebar_pane/pets/cellart.rs) | sextant downsampling from RGBA into terminal cells, and the cell-aspect probe |
| [`pets/model.rs`](../../../crates/rimz/src/sidebar_pane/pets/model.rs) | pet actions, animation tracks, composed tracks, and per-track cadence |
| [`pets/voice.rs`](../../../crates/rimz/src/sidebar_pane/pets/voice.rs) | canned captions, keyed by action transition |
| [`pets/painter.rs`](../../../crates/rimz/src/sidebar_pane/pets/painter.rs) | pixel-sprite residency: PNG memoization, transmit, and re-send |
| [`pets/preview.rs`](../../../crates/rimz/src/sidebar_pane/pets/preview.rs) | `rimz list-pets` preview loading for both tiers |
| [`pixel/`](../../../crates/rimz/src/sidebar_pane/pixel/) | the shared kitty graphics transport: payloads, tmux passthrough, image ids, placeholders, residency, and the pixel context meter |
| [`pixel/probe.rs`](../../../crates/rimz/src/sidebar_pane/pixel/probe.rs) | terminal and tmux capability probes |
| [`pixel/pacing.rs`](../../../crates/rimz/src/sidebar_pane/pixel/pacing.rs) | the acknowledgement pacer for multi-image output through tmux |
| [`render/sections/pets.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/pets.rs) | the renderer side: draws a `PetView` body and caption into the dashboard |

## One frame

Each frame runs two calls in [`app/paint.rs`](../../../crates/rimz/src/sidebar_pane/app/paint.rs): `FramePainter::refresh_view` builds the pet's view, and `draw_and_paint` puts it on screen.

1. `render::selected_pet_action` reduces the selected row to one of seven `PetAction` values ([From card state to animation](#from-card-state-to-animation)).
2. `effective_render_tier` folds the configured mode, the probed capabilities, and this frame's paintability into `Pixel` or `Cell` ([Render tiers](#render-tiers)).
3. The body tier is kept only when pets are enabled, the dashboard is on screen, and the theme allows a body; `NO_COLOR` suppresses the body, and the caption carries the state alone.
4. `PetAssets::observe_unread_rows` diffs the unread row ids against the previous frame and reports whether any row became unread.
5. `PetAssets::view` polls the loader, starts a load if the pet or its preparation changed, picks the track, samples a sprite index, and returns the `PetView`.
6. `draw_and_paint` opens one synchronized-output bracket (DECSET 2026), transmits any pixel sprite the frame needs, draws the ratatui frame, transmits the context meter's images, and closes the bracket. The sprite goes out before the draw that first references its image id.

`frame_interval` tells the serve loop when to wake next. While an asset loads it is the animation grid (`sidebar::timing::animation_frame`); while a body animates it is the active track's frame duration; it is `None` when the pet is static or has no body.

## From card state to animation

`row_pet_action` in [`render/mod.rs`](../../../crates/rimz/src/sidebar_pane/render/mod.rs) projects the selected row onto an action, testing in table order. Compaction comes first, so an agent compacting while it waits on you still shows `Review`. The status column uses the sidebar's [displayed status](../agents/model.md#displayed-status); a parked turn displays as `success`, so it lands on `Idle`.

| selected row | action | track | default caption |
| --- | --- | --- | --- |
| agent compacting context | `Review` | `review` | `reviewing context` |
| agent `waiting` on your answer | `Ask` | `ask` | `someone needs you` |
| agent `failed`, or a stuck process row | `Failed` | `failed` | `rough patch - take a look` |
| agent `paused` (rate limit, API overload) | `Waiting` | `waiting` | `waiting on work` |
| agent `running` with a running subagent | `Waiting` | `waiting` | `waiting on work` |
| agent `running` in the reasoning phase | `Thinking` | `thinking` | `thinking it through` |
| agent otherwise `running`, or a busy process row | `Running` | `running` | `room is moving` |
| agent `idle`, `success`, or `sleeping`; an idle process row; no selection | `Idle` | `idle` | `all caught up` after another action, `resting` on a cold start |

Tracks index into the sheet's rows. The `ANIMATIONS` table in `model.rs` holds the mapping and the cadence, and `default_catalog_matches_petdex_rows` pins it:

| track | sheet frames | frames | fps | animation role |
| --- | --- | --- | --- | --- |
| `idle` | row 0, columns 0 to 5 | 6 | 1.6 | `idle` |
| `thinking` | row 2 three times, then row 1 three times (run left, run right) | 48 | 4.0 | `thinking` |
| `running` | row 7, columns 0 to 5 | 6 | 4.0 | `working` |
| `waiting` | row 6, columns 0 to 5 | 6 | 3.5 | `delegating` |
| `review` | row 8, columns 0 to 5 | 6 | 3.5 | `compacting` |
| `ask` | row 3 columns 0 to 3 twice, then the `waiting` frames (wave, wait) | 14 | 3.5 | `waiting` |
| `jumping` | row 4, columns 0 to 4 | 5 | 3.5 | none (one-shot) |
| `failed` | row 5 | 8 | 3.5 | `failed` |

A track's frame duration is `1000 / fps` milliseconds, never shorter than the display refresh interval. `thinking` and `ask` are composed from more than one row, because no single sheet row paces back and forth or waves before waiting.

The jump is the attention cue. An action change plays `jumping` once before the new steady track, and so does a newly unread row while the action holds. The first action a fresh `PetAssets` sees is not a change, and nothing jumps until the asset has loaded. `selected_track` bounds the one-shot by the jump track's own loop duration, measured in animation phases, so it lasts the same wall time at any wake cadence.

Motion follows the [animation roles](../../guide/theme.md#animations) in `[theme.animations]`. `pet_motion_enabled` maps each action to the role in the table above, and a role quiets the pet when it sets `effect = "static"` explicitly without a multi-frame `frames` override (`motion_quieted` in `render/animation.rs`). A quieted pet skips the jump and freezes on its track's first frame.

## Captions

`voice::caption` returns a line only when the action differs from the previous frame's, including the first frame, so a caption shows once per change and then stands. Each action owns a pool of at least 100 lines, and the frame's animation phase picks one. `Idle` has two pools: `caught_up` after any other action, and `resting` on a cold start. `pool[0]` of each pool is the default caption in the action table.

The tests in `voice.rs` hold every line to ascii, lowercase, at most 26 columns (the space beside the sprite), no edge whitespace, and uniqueness across all pools. `[theme.pets] voice = false` keeps the animation and drops these captions.

`PetAssets::view` puts status captions ahead of the voice line, and they show whether or not `voice` is on:

| caption | when |
| --- | --- |
| `no pet selected` | the `pet` selector is empty; there is no body |
| `pet unavailable` | the latest load for this pet failed, including while a retry runs |
| `fetching pet...` | a load is in flight and no voice caption is set |

## Assets

`resolve_pet_source` in `asset.rs` trims the `pet` selector and resolves it to one of four `PetSource` variants, testing in this order:

| selector | source | example |
| --- | --- | --- |
| a built-in catalog id | the public Codex pets CDN, cached | `rocky` |
| starts with `https://` or `http://` | fetched over HTTPS, cached | `https://example.com/pet.webp` |
| path-like: contains `/` or `.`, or starts with `~` | a local sheet file, or a petdex directory when the path is a directory | `~/art/pet.png` |
| a bare slug | a petdex install under `$HOME/.codex/pets/<slug>/` | `wall-e` |

The built-ins (`BUILTIN_PETS` in `catalog.rs`) are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`. Each is fetched from `https://persistent.oaistatic.com/codex/pets/v1/<id>-spritesheet-v4.webp`.

Every source must match one geometry: a `1536x1872` WebP or PNG holding an `8x9` grid of `192x208` frames, 72 in total. `frames::validate_sheet_geometry` reads the dimensions before a fetched sheet is cached and before any sheet is decoded, so a wrong-shaped sheet fails with a geometry error. Alpha becomes transparent terminal cells.

RimZ caches only bytes it fetched, and never deletes the user's files. The cache is `pets/v1/assets/` under the home's cache directory, `~/.rimz/cache/pets/v1/assets/` by default and wherever `RIMZ_HOME` relocates the home. A built-in caches under its sheet filename, and a URL under `remote-<hex>.webp`, where `<hex>` is the first 16 bytes of the URL's SHA-256. Writes go to a temp sibling and are renamed into place. `resolve_cached` serves a valid entry as-is and removes a wrong-shaped one before fetching again. A decode failure after resolution evicts the entry only when RimZ wrote it; a local sheet or petdex install stays where it is.

Fetches follow one policy (`fetch_url`):

| rule | value |
| --- | --- |
| scheme | HTTPS only; an `http://` URL fails with `pet URL must be https` before any request |
| timeouts | 5 s connect, 10 s response, 30 s body |
| size cap | 16 MiB |
| attempts | 3, sleeping 250 ms times the attempt number between them |
| status | anything but 200 is a failure |
| on failure | no cache entry is written, so the next load retries while cached pets still load from disk |

A petdex install is a directory holding `pet.json` beside its sheet. RimZ reads only `spritesheetPath` from the manifest, relative to the directory or absolute, and reads that sheet like a local one.

Setting `RIMZ_PETS_OFFLINE`, to any value, makes built-ins and URLs cache-only: a missing entry fails as offline, and a wrong-shaped one is removed and fails. Petdex installs and local sheets already read from disk and are unaffected.

## The load state machine

`PetAssets` holds one `PetLoadState` for the current pet, plus the previous action, the jump start, the unread row set, and the caption. A load runs on a thread named `rimz-pet-assets` and reports through an `mpsc` channel that the serve loop polls with `try_recv`, so it never blocks.

```text
            ┌─────────┐  spawn   ┌─────────┐   Ok    ┌────────┐
   (none) ─►│  Empty  │─────────►│ Loading │────────►│ Loaded │
            └─────────┘          └────┬────┘         └────────┘
                 ▲                    │ Err                ▲
                 │                    ▼                    │ retry Ok
     pet or key  │               ┌─────────┐               │
     changed     └───────────────│ Failed  │───────────────┘
     (from any state)            └─────────┘
                                   │     ▲
                     cooldown over │     │ retry Err
                     spawn retry   └─────┘
```

A load is keyed by the pet id and a `PreparationKey` of `(tier, footprint, cell aspect)`. `clear_mismatched_pet` drops any state whose pet id or key differs from this frame's, so changing the pet, flipping tiers, or a probe that changes the cell aspect starts a fresh load. The aspect is part of the key only for the cell tier; the pixel tier pins it to neutral. Disabling pets clears the load state, action, jump, caption, and unread rows in one step, and `PixelPainter::release_process_payload` drops the memoized PNGs.

A failed load retries after `RETRY_COOLDOWN_MS` (20 seconds), measured in animation phases. A first fetch can fail transiently (a cold network the moment pets are switched on, a CDN blip), and latching forever would leave `pet unavailable` up for the session. The retry thread runs while the state stays `Failed`, so the caption stays `pet unavailable` while the frame interval switches to the loading grid. A retry that fails restarts the cooldown.

The two tiers keep different things in memory. The cell tier converts all 72 frames on the loader thread and keeps only the `PetCellGrid`s (character, foreground, and background per cell); the decoded RGBA is dropped before the result is sent. The pixel tier keeps the 72 RGBA frames, because the painter encodes each to PNG when it first transmits it.

## Render tiers

`resolve_render_tier` is the pure resolver over `[theme.pets] glyphs` and the probed `PixelRenderCaps`:

| `glyphs` | requires | result |
| --- | --- | --- |
| `sextant` | nothing | `Cell` |
| `pixel` | pixel transport | `Pixel`, else `Cell` |
| `auto` (default) | pixel transport and kitty clients | `Pixel`, else `Cell` |

`effective_render_tier` applies this frame's conditions on top. `[theme.display] pixel = "off"` forces `Cell`. A resolved `Pixel` drops to `Cell` when the dashboard has no provider block to sit beside or `NO_COLOR` suppresses the body. `Cell` always passes through, so `sextant` stays sextant. Sextant cell art is the portable baseline every miss falls back to.

The tier sets the footprint (`dashboard_pet_size`): a pixel pet reserves `15x9` cells and a cell-art pet `18x9`, with one empty row under either body.

`pixel/probe.rs` reports two independent capabilities:

| capability | inside tmux | inside Zellij | standalone |
| --- | --- | --- | --- |
| `pixel_transport` | tmux 3.6 or newer, and `allow-passthrough` is `on` or `all` on `$TMUX_PANE` (the session when that is unset) | off | on |
| `kitty_clients` | every non-control-mode client's `client_termname` is `xterm-ghostty`, `ghostty`, `xterm-kitty`, or `kitty`, or the client descends (within four parent hops) from a live `ttyd` daemon speaking the current `TTYD_PIXEL_PROTOCOL` | off | `$TERM` is one of the same four names |

Both bits stay off under Zellij because Zellij 0.45 does not accept the Unicode-placeholder placement the pixel tier uses. `rimz doctor` still probes Zellij's kitty graphics support separately (`probe_zellij_kitty`, 0.45 minimum), but the renderer does not consult that result.

The sidebar probes at startup, again on every resize (`on_resize`), and on a tmux-only backstop: a tick re-probes once `CAPS_REFRESH_INTERVAL` (10 seconds) has passed, which catches a browser attach or detach that does not change the pane size. Each tmux command has a 500 ms timeout. A command that fails keeps that capability's previous reading, and so does an empty client list for `kitty_clients`, so the tier does not flap.

The probes only read, with one exception at startup. Inside tmux, `escalate_own_pane_passthrough` raises the sidebar's own pane from `allow-passthrough on` to `all`, so graphics sent while the window is hidden still reach the terminal. A pane already at `all`, or set to `off`, is left alone.

## Cell art

Cell art turns each frame into a grid of cells, each a character with a foreground and background color, that ratatui copies like any other line. It needs no graphics protocol, so it works under tmux, Zellij, detached sessions, plain terminals, and every color depth.

`cellart::render_frame` samples each frame at sextant resolution: every terminal cell is a `2x3` subcell grid. It averages source pixels per subcell in linear light and marks subcells with coverage at or above `INK_THRESHOLD` (0.5) as ink; the rest render as the terminal background. A fully opaque cell picks the best two-color split of its subcells, a cell on the sprite's edge paints its ink subcells in one color over the background, and a fully transparent cell is blank.

Terminal cells are taller than they are wide, so `fitted_sample_rect` fits each frame inside the footprint at subcell resolution using the cell aspect (height over width), centers it horizontally, and aligns it to the bottom so the pet's feet stay on the floor. The aspect resolves in this order:

1. `[theme.pets] cell_aspect`, a ratio from 1.0 to 4.0, stored in 1/120 steps.
2. `probe_cell_aspect`, which reads the pty's pixel and cell dimensions through crossterm's `window_size` and returns nothing when any of them is zero or the ratio falls outside 1.0 to 4.0.
3. `CellAspect::NEUTRAL`, `13/6`, the ratio at which a `36x27` subcell grid holds a `192x208` frame undistorted.

`cell_aspect` exists for ptys that report zero pixel dimensions. tmux 3.4 and newer passes the host terminal's pixel dimensions through; whether Zellij 0.45 does has not been verified against a real host terminal.

## Pixel tier

The pixel tier sends the decoded frames through the kitty graphics protocol and still lets ratatui's buffer diff place them. `render/sections/pets.rs` fills the footprint with placeholder cells: each is one grapheme cluster of U+10EEEE plus a row and a column combining mark, and its foreground RGB encodes the kitty image id, which is why ids are masked to 24 bits. Moving the pet or changing its frame is an ordinary cell diff.

Animation cycles image ids instead of using kitty's animation-frame actions (`a=f`, `a=a`), which Ghostty does not implement; the transport sends only transmit (`a=t`), place (`a=p`), and delete (`a=d`). Each renderer process picks an id base (`runtime_image_id_base`, mixed from the pid and the clock), and sprite `n` uses `base + n`. A pet change re-transmits the new sheet under the same ids, so the terminal replaces image data in place without a delete.

Every graphics escape is wrapped in tmux's passthrough DCS when the sidebar runs inside tmux. The transmit writes a virtual placement (`U=1`) that the placeholder cells refer to, so tmux redraws and pane repaints keep the image inside the sidebar pane.

`ImageResidency` in `pixel/mod.rs` decides when to send:

- A sprite is transmitted the first time a frame shows it, or when its id or pet changes. `PixelPainter` encodes the PNG once per sprite and keeps it.
- A sprite whose last send is `RESIDENT_REFRESH_MS` (2 seconds) old is re-sent the next time a frame shows it, so a dropped tmux passthrough or a terminal image-store eviction recovers on its own. Re-sends that fall on different frames are spaced at least `MIN_RESEND_SPACING_MS` (250 ms) apart.
- Deletes are sent only at teardown (`PixelPainter::clear`), inside a synchronized-output bracket.

The bracket in `draw_and_paint` makes each frame one redraw. tmux forwards synchronized output to the terminal because RimZ sets `terminal-features` `*:sync` during room setup (`apply_room_options` in `mux/tmux.rs`).

Graphics traffic stays bounded because some macOS terminals re-evaluate the mouse pointer shape on every image update. `glyphs = "sextant"` removes that traffic entirely.

The pixel context meter shares the transport and the id base: it interns each distinct raster from `base + 0x4000`, up to 512 ids, with least-recently-used eviction. [`pixel/meter.rs`](../../../crates/rimz/src/sidebar_pane/pixel/meter.rs) owns its mechanics.

## rimz list-pets

`rimz list-pets` ([`cli/list_pets/`](../../../crates/rimz/src/cli/list_pets/mod.rs)) previews every listable pet with the live dashboard's catalog, cache, tier resolver, and footprints. It lists built-ins first, then installed petdex pets sorted by slug. With `--json` it prints the ids as a JSON array, and when stdout is not a terminal it prints one id per line and loads nothing.

On a terminal it resolves the tier with `resolve_render_tier` from the machine config and the environment probe (`detect_env`), then loads previews two at a time (`PREVIEW_FETCH_CONCURRENCY`) and prints them in rows that fit the terminal width. Each preview shows the pet's idle sprite. A pet that fails to load leaves a blank slot and adds one closing note that points at the network and `RIMZ_PETS_OFFLINE`; because a failed fetch writes no cache entry, a re-run loads the rest from disk and retries only the missing pets.

The command writes its own output without ratatui. The pixel path gives each pet its own image id, counting from 1, transmits the idle sprite and a virtual placement, and then writes the placeholder rows directly (`inline_placeholder_row`) inside a synchronized-output bracket.

Pixel previews inside tmux are paced by `LiveGraphicsPacer` in [`pixel/pacing.rs`](../../../crates/rimz/src/sidebar_pane/pixel/pacing.rs), wrapped by `cli/list_pets/pacing.rs`. Without pacing, tmux can discard later image data while it repaints. After each image the command places it with replies enabled and waits for the terminal's kitty graphics acknowledgement before sending the next. If no acknowledgement arrives within `ACK_TIMEOUT` (500 ms), pacing turns off for the rest of the run and later placements are sent quiet. On exit the pacer waits once more for an acknowledgement it is still owed and drains the tty, so the reply does not appear in the shell as typed input.

The contributor gallery (the hidden `rimz sidebar gallery`, `serve_gallery` in `app/demo.rs`, built only with the `testkit` feature) is a different path: it runs the live `FramePainter` per column and offsets each column's id base by `index << 12`, so one column cannot overwrite another's images.

## Security surface

Pets execute no commands, so `[theme.pets]` is outside the [project trust hash](../harness/trust.md). The visible surface is asset egress: a GET to the Codex CDN for a built-in, or to the HTTPS host you configured. Prompts, transcripts, pane text, workspace paths, and provider credentials never travel on this path, and `RIMZ_PETS_OFFLINE` removes the egress. The user-facing statement is one bullet in [security.md](../../guide/security.md#what-leaves-your-machine).

## Where to make a change

| you want to | change |
| --- | --- |
| map a card state to a different pet action | `row_pet_action` in `render/mod.rs` |
| retime or recompose an animation track | the `ANIMATIONS` table in `model.rs`, and `default_catalog_matches_petdex_rows` |
| tie a pet action to a different animation role | `pet_motion_enabled` in `render/mod.rs` |
| add caption lines | the matching pool in `voice.rs`; the tests enforce width, ascii, and uniqueness |
| add a built-in pet | `BUILTIN_PETS` in `catalog.rs` |
| accept a new selector form | `resolve_pet_source` and `PetSource` in `asset.rs` |
| change when pixels are allowed | `resolve_render_tier` and `effective_render_tier` for policy, `pixel/probe.rs` for the facts |
| change the dashboard footprint | `DASHBOARD_PIXEL_PET` and `DASHBOARD_CELL_PET` in `pets/mod.rs`, and the dashboard layout beside them |
