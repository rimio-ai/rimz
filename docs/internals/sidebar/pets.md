# Pets

A pet is an animated sprite on the sidebar's [provider dashboard](../../interface/sidebar.md#the-provider-dashboard) that acts out what the selected card is doing. It is opt-in: `[theme.pets] enabled` is off by default, and the default `pet` is `rocky` (`config/pets.rs::PetsConfig`).

The pets module owns everything expensive, and the renderer owns nothing but a copy. [`sidebar_pane/pets/`](../../../crates/rimz/src/sidebar_pane/pets/) does the network fetches, disk cache, WebP and PNG decode, frame slicing, cell-art conversion, track selection, and captions. The renderer receives one `PetView` (an optional body, an optional caption, and an optional frame interval) and copies it into the ratatui buffer like any other widget, so no draw path blocks on I/O.

The kitty graphics transport the pixel tier uses lives beside it in [`sidebar_pane/pixel/`](../../../crates/rimz/src/sidebar_pane/pixel/mod.rs), and the pixel context meter shares it. This page owns both. User-facing setup is the [pets guide](../../guide/pets.md).

## Module map

| module | job |
| --- | --- |
| [`pets/mod.rs`](../../../crates/rimz/src/sidebar_pane/pets/mod.rs) | `PetAssets` and its load state machine, `PetView`, tier resolution, dashboard footprints, and per-frame track selection |
| [`pets/catalog.rs`](../../../crates/rimz/src/sidebar_pane/pets/catalog.rs) | the built-in pet ids and the sheet geometry every source must match |
| [`pets/asset.rs`](../../../crates/rimz/src/sidebar_pane/pets/asset.rs) | selector resolution, HTTPS fetches, the per-machine cache, local and petdex reads, offline mode, and eviction |
| [`pets/frames.rs`](../../../crates/rimz/src/sidebar_pane/pets/frames.rs) | sheet decode, geometry validation, and slicing into 72 RGBA frames |
| [`pets/cellart.rs`](../../../crates/rimz/src/sidebar_pane/pets/cellart.rs) | sextant downsampling from RGBA into terminal cells, and the cell-aspect readers |
| [`pets/model.rs`](../../../crates/rimz/src/sidebar_pane/pets/model.rs) | pet actions, animation tracks, composed tracks, and per-track cadence |
| [`pets/voice.rs`](../../../crates/rimz/src/sidebar_pane/pets/voice.rs) | canned captions, keyed by action transition |
| [`pets/painter.rs`](../../../crates/rimz/src/sidebar_pane/pets/painter.rs) | `PixelPainter`: one pane's sprite residency over the shared sheet's PNGs |
| [`pets/preview.rs`](../../../crates/rimz/src/sidebar_pane/pets/preview.rs) | idle-sprite previews in both tiers, for `rimz list-pets` and the setup pet question |
| [`pixel/mod.rs`](../../../crates/rimz/src/sidebar_pane/pixel/mod.rs) | the shared kitty graphics transport: escape encoding, tmux passthrough, slot leases and image ids, placeholders, and `ImageResidency` |
| [`pixel/probe.rs`](../../../crates/rimz/src/sidebar_pane/pixel/probe.rs) | terminal and tmux capability probes, and the passthrough escalation |
| [`pixel/meter.rs`](../../../crates/rimz/src/sidebar_pane/pixel/meter.rs) | the pixel context meter: rasters, id interning, and its own residency |
| [`pixel/pacing.rs`](../../../crates/rimz/src/sidebar_pane/pixel/pacing.rs), [`pixel/tty.rs`](../../../crates/rimz/src/sidebar_pane/pixel/tty.rs) | the acknowledgement pacer for multi-image output through tmux, over a raw read of the controlling terminal's kitty graphics replies |
| [`render/sections/pets.rs`](../../../crates/rimz/src/sidebar_pane/render/sections/pets.rs) | the renderer side: draws a `PetView` body and caption into the dashboard |
| [`app/paint.rs`](../../../crates/rimz/src/sidebar_pane/app/paint.rs) | `FramePainter`: one pane's view refresh, pixel session, and frame composition |

## One frame

A pane paints a pet in two calls on `FramePainter` in [`app/paint.rs`](../../../crates/rimz/src/sidebar_pane/app/paint.rs). `refresh_view` builds the `PetView` from clones of the snapshot and of the pane's `ViewState` (selection and animation phase), taken under the view lock and used outside it. `compose_frame` then composes the frame under the lock, and the caller writes the returned `BufferedFrame` after it releases the lock.

`refresh_view` runs these steps:

1. `render::selected_pet_action` reduces the selected row to one of seven `PetAction` values ([From card state to animation](#from-card-state-to-animation)).
2. `effective_render_tier` folds the configured mode, the probed capabilities, and this frame's paintability into `Pixel` or `Cell` ([Render tiers](#render-tiers)).
3. The tier becomes the body tier only when `render::pets_on_dashboard` holds (pets enabled, no alert covering the dashboard) and the theme allows a body. `NO_COLOR` suppresses the body (`Theme::pet_body_enabled`), and the caption then carries the state alone.
4. `PetAssets::observe_unread_rows` diffs the unread row ids against the previous frame and reports whether any row became unread.
5. `PetAssets::view` polls the loader, starts a load if the pet or its preparation changed, picks the track, samples a sprite index, and returns the `PetView`.
6. When pets are off or the tier is not `Pixel`, `PixelPainter::release_process_payload` forgets what the pane transmitted, so the next composition deletes its resident images.

`compose_frame` stages the frame in `PaneBackend` in a fixed order. It transmits any pet sprite the frame needs, then lets ratatui write its cell diff, then transmits the context meter's images, whose rasters are known only after that draw. A sprite therefore always precedes the first cell that references its image id. The frame carries a synchronized-output bracket only when it wrote kitty graphics ([Synchronized output](#synchronized-output)).

`PetView::frame_interval` tells the pane when to wake next. While an asset loads it is the animation grid (`sidebar::timing::animation_frame`). While a body animates it is the active track's frame duration. It is `None` when the pet is static or has no body. `render::animation_interval` takes it whenever the pet is on the dashboard, and takes the faster money grid instead while a cost figure is rolling.

## From card state to animation

`row_pet_action` in [`render/mod.rs`](../../../crates/rimz/src/sidebar_pane/render/mod.rs) projects the selected row onto an action, testing in table order. Compaction comes first, so an agent compacting while it waits on you still shows `Review`. The status column uses the sidebar's [displayed status](../agents/model.md#displayed-status). A parked turn displays as `success`, so it lands on `Idle`.

| selected row | action | track | default caption |
| --- | --- | --- | --- |
| agent compacting context | `Review` | `review` | `reviewing context` |
| agent `waiting` on your answer | `Ask` | `ask` | `someone needs you` |
| agent `failed`, or a stuck process row | `Failed` | `failed` | `rough patch - take a look` |
| agent `paused` (rate limit, API overload) | `Waiting` | `waiting` | `waiting on work` |
| agent `running` with a `running` subagent (`SidebarSubAgent::holds_parent_turn`) | `Waiting` | `waiting` | `waiting on work` |
| agent `running` in the reasoning phase | `Thinking` | `thinking` | `thinking it through` |
| agent otherwise `running`, or a busy process row | `Running` | `running` | `room is moving` |
| agent `idle`, `success`, or `sleeping`; an idle process row; no selection | `Idle` | `idle` | `all caught up` after another action, `resting` on a cold start |

Tracks index into the sheet's rows. The `ANIMATIONS` table in `model.rs` holds the mapping and the cadence, and the test `default_catalog_matches_petdex_rows` pins it:

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

A track's frame duration is `1000 / fps` milliseconds, rounded, and never shorter than the display refresh interval. `thinking` and `ask` are composed from more than one row, because no single sheet row paces back and forth or waves before waiting.

The jump is the attention cue. An action change plays `jumping` once before the new steady track, and so does a newly unread row while the action holds. The first action a fresh `PetAssets` sees is not a change, and nothing jumps until the asset has loaded. `selected_track` bounds the one-shot by the jump track's own loop duration, measured in animation phases, so it lasts the same wall time at any wake cadence.

Motion follows the [animation roles](../../guide/theme.md#animations) in `[theme.animations]`. `pet_motion_enabled` in `render/mod.rs` maps each action to the role in the track table. A role quiets the pet when it sets `effect = "static"` explicitly without a multi-frame `frames` override (`motion_quieted` in `render/animation.rs`). A quieted pet skips the jump and freezes on its track's first frame.

## Captions

`voice::caption` returns a line only when the action differs from the previous frame's, including the first frame, so a caption shows once per change and then stands. Each action owns a pool of at least 100 lines, and the frame's animation phase picks one. `Idle` has two pools: `caught_up` after any other action, and `resting` on a cold start. `pool[0]` of each pool is the default caption in the action table.

The tests in `voice.rs` hold every line to ASCII, lowercase, single spaces, no edge whitespace, at most 26 columns (the space beside the sprite), and uniqueness across all pools. `[theme.pets] voice = false` keeps the animation and drops these captions.

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

RimZ caches only bytes it fetched, and never deletes the user's files. The cache is `pets/v1/assets/` under the home's cache directory: `~/.rimz/cache/pets/v1/assets/` by default, and wherever `RIMZ_HOME` relocates the home. A built-in caches under its sheet filename, and a URL under `remote-<hex>.webp`, where `<hex>` is the first 16 bytes of the URL's SHA-256. Writes go to a temp sibling and are renamed into place. `resolve_cached` serves a valid entry as-is and removes a wrong-shaped one before fetching again. A decode failure after resolution evicts the entry only when RimZ wrote it. A local sheet or petdex install stays where it is.

A URL selector must be HTTPS: `require_https` fails an `http://` URL with `pet URL must be https` before the cache or the network is touched. Every fetch then follows one policy (`fetch_url`):

| rule | value |
| --- | --- |
| timeouts | 5 s connect, 10 s response, 30 s body |
| size cap | 16 MiB |
| attempts | 3, sleeping 250 ms times the attempt number between them |
| status | anything but 200 is a failure |
| on failure | no cache entry is written, so the next load retries while cached pets still load from disk |

A petdex install is a directory holding `pet.json` beside its sheet. RimZ reads only `spritesheetPath` from the manifest, relative to the directory or absolute, and reads that sheet like a local one.

`RIMZ_PETS_OFFLINE=1` makes built-ins and URLs cache-only. The value is read by `utils::env::flag_enabled`, so `0`, `false`, and an empty value mean off. Offline, a missing entry fails as offline, and a wrong-shaped one is removed and fails. Petdex installs and local sheets read from disk anyway and are unaffected.

## The load state machine

`PetAssets` holds one `PetLoadState` for the current pet, plus the previous action, the jump start, the unread row set, and the caption. A load runs on a thread named `rimz-pet-assets` and reports through an `mpsc` channel that the pane polls with `try_recv`, so a frame never waits on it.

```text
                 ┌────────────── cache hit ──────────────┐
                 │                                       ▼
            ┌────┴────┐ new/join ┌─────────┐   Ok    ┌────────┐
   (none) ─►│  Empty  │─────────►│ Loading │────────►│ Loaded │
            └─────────┘          └────┬────┘         └────────┘
                 ▲                    │ Err                ▲
                 │                    ▼                    │ retry Ok
     pet or key  │               ┌─────────┐               │
     changed     └───────────────│ Failed  │───────────────┘
     (from any state)            └─────────┘
                                   │     ▲
                     cooldown over │     │ retry Err
                    new/join retry └─────┘
```

A load is keyed by the pet id and a `PreparationKey` of `(tier, footprint, cell aspect)`. The aspect is part of the key only for the cell tier, and the pixel tier pins it to `CellAspect::NEUTRAL`. `clear_mismatched_pet` drops any state whose pet id or key differs from this frame's.

Prepared sheets are shared across all the host's panes through a process-wide table (`prepared_pets`): in-flight loads hold waiters, and completed sheets are held only weakly. A live cache hit enters `Loaded` at once without a loader thread, from `Empty` or from a `Failed` state whose retry is due. Concurrent misses for one key join one loader, and every waiter receives the same result. A failed load fans its error out to every waiter and leaves no entry; an unwind or failed spawn disconnects the waiters, who read `pet loader stopped`. Dead weak entries are pruned on insertion, while in-flight entries stay. A sheet lives while any pane holds it: changing pets, tiers, or cell aspect releases the pane's hold, and so does closing the pane. An edited local or petdex sheet is therefore read again only after every holder releases it, not when another pane attaches while it is still held. Disabling pets clears the load state, action, jump, caption, and unread rows.

The two tiers keep different things in a prepared sheet. The cell tier converts all 72 frames on the loader thread and keeps only the `PetCellGrid`s (character, foreground, and background per cell). The pixel tier encodes all 72 frames to PNG on that thread and keeps the byte arrays, which every pane's painter transmits without a copy of its own. Both slice frames lazily, keeping the decoded sheet plus one RGBA slice live instead of the sheet plus all 72 slices: an estimated slicing footprint of about 11 MiB instead of 22 MiB per load, computed from the geometry, excluding compressed input and decoder scratch, not measured. Both drop the decoded RGBA before the result crosses the channel.

A failed load retries after `RETRY_COOLDOWN_MS` (20 seconds), measured in animation phases. A first fetch can fail transiently (a cold network the moment pets are switched on, a CDN blip), and latching forever would leave `pet unavailable` up for the session. The retry thread runs while the state stays `Failed`, so the caption stays `pet unavailable` while the frame interval switches to the loading grid. A retry that fails restarts the cooldown.

## Render tiers

`resolve_render_tier` is the pure resolver over `[theme.pets] glyphs` and the probed `PixelRenderCaps`:

| `glyphs` | requires | result |
| --- | --- | --- |
| `sextant` | nothing | `Cell` |
| `pixel` | pixel transport | `Pixel`, else `Cell` |
| `auto` (default) | pixel transport and kitty clients | `Pixel`, else `Cell` |

`effective_render_tier` applies this frame's conditions on top. `[theme.display] pixel = "off"` forces `Cell`. A resolved `Pixel` drops to `Cell` when the pane holds no [pixel slot](#image-ids-and-slots), the dashboard has no provider block to sit beside, or `NO_COLOR` suppresses the body. `Cell` always passes through, so `sextant` stays sextant. Sextant cell art is the portable baseline every miss falls back to.

The tier sets the footprint (`dashboard_pet_size`): a pixel pet reserves `15x9` cells and a cell-art pet `18x9`, with one empty row under either body.

### Capability probes

`PixelRenderCaps` carries two independent bits, `pixel_transport` and `kitty_clients`. The sidebar reads them through the host's shared `RoomCaps` in `pixel/probe.rs`, answered for the pane's own tmux pane id:

| capability | in a tmux room | in a Zellij room |
| --- | --- | --- |
| `pixel_transport` | tmux 3.6 or newer, and `allow-passthrough` is `on` or `all` on the pane (the session when no pane id is known) | off |
| `kitty_clients` | every non-control-mode client's `client_termname` is `xterm-ghostty`, `ghostty`, `xterm-kitty`, or `kitty`, or the client descends (within four parent hops) from a live `ttyd` whose daemon record carries the current `TTYD_PIXEL_PROTOCOL` (`web::pixel_ttyd_pids`) | off |

Both bits stay off under Zellij because Zellij 0.45 does not accept the Unicode-placeholder placement the pixel tier uses ([Zellij reference](../../externals/mux-adapter/zellij-reference.md#kitty-graphics-protocol)). `rimz doctor` still probes Zellij's kitty graphics support separately (`probe_zellij_kitty`, 0.45 minimum), but the renderer never consults that result. Its `zellij kitty graphics` row is informational: `cli/doctor/render.rs::push_capabilities` reports a supported probe as "supported by the host terminal; Zellij rooms still draw cell art (Zellij rejects the placement the pixel tier uses)".

Outside a room, `detect_env` reads the same tmux facts when `TMUX` is set, reports nothing under `ZELLIJ`, and otherwise reports `pixel_transport` on and `kitty_clients` on when `$TERM` is one of the four names. `rimz list-pets`, the setup pet preview, and the contributor player use it.

A tmux room re-reads its facts on three triggers: when an attachment starts, 300 ms after a resize settles (`RESIZE_CAPS_SETTLE`), and on a backstop tick once `CAPS_REFRESH_INTERVAL` (10 seconds) has passed. The backstop catches a browser attach or detach that does not change the pane size. One tmux sample (the version, the client list, and every pane's passthrough from one `list-panes -s`) serves all the host's panes for 10 seconds, so a resize inside that window re-reads only the pane's cell aspect. Each tmux command has a 500 ms timeout. A failed command keeps that capability's previous reading, and so does an empty client list for `kitty_clients`, so the tier does not flap.

The probes only read, with one exception. When an attachment starts in a tmux room, `escalate_own_pane_passthrough` raises the sidebar's own pane from `allow-passthrough on` to `all`, so graphics sent while the window is hidden still reach the terminal. A pane already at `all`, or set to `off`, is left alone, and a failed escalation only warns.

## Cell art

Cell art turns each frame into a grid of cells, each a character with a foreground and background color, that ratatui copies like any other line. It needs no graphics protocol, so it works under tmux, Zellij, detached sessions, plain terminals, and every color depth.

`cellart::render_frame` samples each frame at sextant resolution: every terminal cell is a `2x3` subcell grid. It averages source pixels per subcell in linear light and marks subcells with coverage at or above `INK_THRESHOLD` (0.5) as ink; the rest render as the terminal background. A fully opaque cell picks the best two-color split of its subcells, a cell on the sprite's edge paints its ink subcells in one color over the background, and a fully transparent cell is blank.

Terminal cells are taller than they are wide, so `fitted_sample_rect` fits each frame inside the footprint at subcell resolution using the cell aspect (height over width). It centers the frame horizontally and aligns it to the bottom, so the pet's feet stay on the floor. The aspect resolves in this order:

1. `[theme.pets] cell_aspect`, a ratio from 1.0 to 4.0, stored in 1/120 steps (`config::CellAspect`).
2. The pane's own pixel and cell dimensions, read from its fd by `PaneBackend::cell_aspect` when the attachment starts and after each resize. `rimz list-pets` and the setup preview read the controlling terminal instead (`probe_cell_aspect`). Both give nothing when a dimension is zero or the ratio falls outside 1.0 to 4.0.
3. `CellAspect::NEUTRAL`, `13/6`, the ratio at which a `36x27` subcell grid holds a `192x208` frame undistorted.

`cell_aspect` exists for ptys that report zero pixel dimensions. tmux 3.4 and newer passes the host terminal's pixel dimensions through. Whether Zellij 0.45 does has not been verified against a real host terminal.

## Pixel tier

The pixel tier sends the decoded frames through the kitty graphics protocol and still lets ratatui's buffer diff place them. `render/sections/pets.rs` fills the footprint with placeholder cells. Each is one grapheme cluster of U+10EEEE plus a row and a column combining mark (the leaf `pixel_wire` module holds the encoding the browser page shares), and its foreground RGB encodes the kitty image id. That is why ids are masked to 24 bits. Moving the pet or changing its frame is an ordinary cell diff.

Animation cycles image ids instead of using kitty's animation-frame actions (`a=f`, `a=a`), which Ghostty does not implement. The transport sends only transmit (`a=t`), place (`a=p`), and delete (`a=d`).

### Image ids and slots

Each pane's attachment leases one slot when it starts (`PixelLease::acquire`): the lowest free of 256 flock guards, `pixel-slot-<n>.lock` in the user's shared runtime directory (`RuntimePaths::shared_pixel_slot_lock`), so every room of the user draws from one pool. Slot `n` owns ids `0x520000 + n * 0x200` up to the next slot's base. Sprite `s` uses `base + s`, and the context meter uses `base + 0x100` onward. This layout is a cross-version contract (`PixelSlot`): moving it would orphan the images an older renderer left behind.

The first frame that draws pixels sweeps every id in the slot with individual `d=I` deletes before its first transmit. A successor thus frees images a dead holder left, without touching another live sidebar's slot. Lease exhaustion or a lock I/O error warns once and selects cell rendering for the attachment's lifetime.

The testkit scenario player (the hidden `rimz sidebar play`, built only with the `testkit` feature) leases slots like live attachments, one per column. Each column keeps its `PixelLease` outside its rebuilt loop state, so a seek or wrap keeps its slot and never sweeps a live sidebar's slot. Headless exports use cell art, and live playback detects capabilities from the environment alone, without mux probes.

### Sending images

`ImageResidency` in `pixel/mod.rs` decides when to send, for the pet (`PixelPainter`) and the meter alike:

- A sprite is transmitted the first time a frame shows it, or when its id or pet changes. The PNG bytes come from the shared prepared sheet.
- A sprite whose last send is `RESIDENT_REFRESH_MS` (2 seconds) old is re-sent the next time a frame shows it, so a dropped tmux passthrough or a terminal image-store eviction recovers on its own. Re-sends that fall on different frames are spaced at least `MIN_RESEND_SPACING_MS` (250 ms) apart.
- A pet change re-transmits the new sheet under the same ids, so the terminal replaces image data in place without a delete.
- Deletes free both image data and placements (`d=I`), at teardown and on the next paint after pixels are disabled. Every attachment loop exit, errors included, attempts cleanup (`LoopState::clear_pixel`). A lost pty cannot deliver it, and the next holder's sweep bounds those leftovers. Range deletes are never used, because Ghostty 1.3.1 deletes unrelated images for that operation.

Every transmit carries a virtual placement (`U=1,p=1`) that the placeholder cells refer to. A re-send replaces that placement instead of adding one, and restores it on terminals that drop placements when replacing image data.

Inside tmux, one image's complete chunked transmit and its placement share one passthrough DCS envelope (`wrap_pixel_payload`). tmux interleaves panes only between DCS envelopes, while the terminal holds one pending chunked load, so one envelope per image keeps another pane's graphics commands from corrupting that load. Each kitty chunk still carries at most 4096 base64 bytes. Deletes go in envelopes of their own.

Graphics traffic is kept to these sends because some macOS terminals re-evaluate the mouse pointer shape on every image update. `glyphs = "sextant"` removes the pet's share, and `[theme.display] pixel = "off"` removes the meter's too.

### Synchronized output

A frame that writes kitty graphics is one synchronized update (DECSET 2026): `BufferedFrame::write` brackets it, and only it. Mode 2026 does not nest, so the slot sweep, in-frame deletes, and meter transmits add no bracket of their own. Teardown (`FramePainter::clear`) brackets its resident deletes and writes nothing when none remain.

A cell-only frame, including animation over images already resident, carries no bracket. tmux 3.7b and later repaints the whole pane at the end of every synchronized update instead of forwarding only the changed cells ([tmux reference](../../externals/mux-adapter/tmux-reference.md#releases-and-version-floors)), so a bracket on every frame would multiply the client's bytes. tmux forwards the bracket to the terminal because room setup writes `terminal-features[240]` `*:sync` (`mux/tmux/options.rs::tmux_terminal_features_commands`, applied by `apply_room_options`). The client-byte budget and its live guard are in [performance](../performance.md#live-client-byte-guard).

### The context meter

The pixel context meter shares the transport and the slot. It paints only when the pane holds a slot, both capability bits are on, and `[theme.display] pixel` is `auto`. `MeterPixels` interns each distinct raster to an id from `base + 0x100`, up to 256 ids, with least-recently-used eviction that protects the ids visible on screen and those composed in the current frame. When every id is protected, the meter falls back to the cell bar. [`pixel/meter.rs`](../../../crates/rimz/src/sidebar_pane/pixel/meter.rs) owns the rest.

The terminal's image memory depends on peak concurrent attachments, not on uptime or restart count: each slot holds at most 72 pet sprites and 256 meter rasters.

## rimz list-pets

`rimz list-pets` ([`cli/list_pets/`](../../../crates/rimz/src/cli/list_pets/mod.rs)) previews every listable pet with the live dashboard's catalog, cache, tier resolver, and footprints. It lists built-ins first, then installed petdex pets (directories under `$HOME/.codex/pets/` holding a `pet.json`) sorted by slug. With `--json` it prints the ids as a JSON array, and when stdout is not a terminal it prints one id per line. Neither form loads a sheet.

On a terminal it resolves the tier with `resolve_render_tier` from the machine config and `detect_env`. It loads previews two at a time (`PREVIEW_FETCH_CONCURRENCY`) and prints them in rows that fit the terminal width. Each preview shows the pet's idle sprite. A pet that fails to load leaves a blank slot and adds one closing note that points at the network and `RIMZ_PETS_OFFLINE`. Because a failed fetch writes no cache entry, a re-run loads the rest from disk and retries only the missing pets. The setup pet question draws its one preview through the same row writers (`cli/first_run.rs::build_pet_art`).

The command writes its own output without ratatui. The pixel path gives each pet its own image id, counting from 1, transmits the idle sprite and a virtual placement, and then writes the placeholder rows directly (`inline_placeholder_row`) inside a synchronized-output bracket.

Pixel previews inside tmux are paced by `LiveGraphicsPacer` in [`pixel/pacing.rs`](../../../crates/rimz/src/sidebar_pane/pixel/pacing.rs), wrapped by `cli/list_pets/pacing.rs`. Without pacing, tmux can discard later image data while it repaints. After each image the command places it with replies enabled and waits for the terminal's kitty graphics acknowledgement before it sends the next. If no acknowledgement arrives within `ACK_TIMEOUT` (500 ms), pacing turns off for the rest of the run and later placements are sent quiet. On exit the pacer waits once more for an acknowledgement it is still owed and drains the tty, so the reply does not appear in the shell as typed input.

## Security surface

Pets execute no commands, so `[theme.pets]` is outside the [project trust hash](../harness/trust.md). The visible surface is asset egress: a GET to the Codex CDN for a built-in, or to the HTTPS host you configured. Prompts, transcripts, pane text, workspace paths, and provider credentials never travel on this path, and `RIMZ_PETS_OFFLINE` removes the egress. The user-facing statement is one bullet in [security.md](../../guide/security.md#what-leaves-your-machine).

## Where to make a change

| you want to | change |
| --- | --- |
| map a card state to a different pet action | `row_pet_action` in `render/mod.rs` |
| retime or recompose an animation track | the `ANIMATIONS` table in `model.rs`, and `default_catalog_matches_petdex_rows` |
| tie a pet action to a different animation role | `pet_motion_enabled` in `render/mod.rs` |
| add caption lines | the matching pool in `voice.rs`; the tests enforce width, ASCII, and uniqueness |
| add a built-in pet | `BUILTIN_PETS` in `catalog.rs` |
| accept a new selector form | `resolve_pet_source` and `PetSource` in `asset.rs` |
| change when pixels are allowed | `resolve_render_tier` and `effective_render_tier` for policy, `pixel/probe.rs` for the facts |
| change the dashboard footprint | `DASHBOARD_PIXEL_PET` and `DASHBOARD_CELL_PET` in `pets/mod.rs`, and the dashboard layout beside them |
| add another pixel surface | an id range inside the slot (`METER_ID_OFFSET` shows the pattern) and an `ImageResidency` of its own; the slot layout itself is a cross-version contract |
