# Pets

## Overview

Pets are renderer-local attention art for the provider dashboard. One animated companion follows the selected agent or process card, while the card rows stay stable and the bottom panel carries the extra motion.

The renderer receives a `PetView`: one optional body, caption text, loading state, current action, and active animation track. The body is either cell art or pixel placement metadata. Network fetches, disk cache reads, WebP/PNG decode, frame slicing, animation selection, and memoized cell-art conversion stay in `src/sidebar_pane/pets/`; the on-screen placement contract lives with [the provider dashboard](../../interface/sidebar.md#zone-3--the-provider-dashboard).

## Module map

| module | job |
| --- | --- |
| `mod.rs` | Public pet surface for the sidebar: `PetAssets`, `PetView`, tier resolution, fixed dashboard footprints, asset lifecycle, and animation-frame selection. |
| `catalog.rs` | Built-in pet ids and the fixed Codex/petdex sheet geometry. |
| `asset.rs` | Selector resolution, HTTPS fetches, per-machine cache installs, local sheet reads, petdex manifest reads, offline mode, and cache eviction. |
| `frames.rs` | WebP/PNG sheet decode, slicing into `RgbaImage` frames, and RGBA-to-PNG encode for kitty transmit. |
| `cellart.rs` | Sextant downsampling from RGBA frames into terminal cells. |
| `painter.rs` | Pet-sprite residency and retransmit lifecycle over the shared pixel transport. |
| `../pixel/` | Shared kitty graphics payloads, tmux passthrough wrapping, image ids, placeholders, content-addressed meter rasters, bounded meter residency, and capability probing. |
| `model.rs` | Pet actions, animation tracks, composed tracks, and per-track timing. |
| `voice.rs` | Canned captions keyed by action transitions. |
| `preview.rs` | `rimz list-pets` preview loading for cell and pixel branches. |

## Data flow

Each frame starts from `[theme.pets] pet`. `asset::resolve_pet_source` turns the selector into a built-in CDN asset, HTTPS URL, local sheet, or petdex install. `PetAssets` owns the load state: `loading` holds the background loader receiver, `loaded` holds decoded frames plus the memoized cell-art cache, and `failed` records an unavailable caption plus a retry cooldown.

The loader path resolves bytes through `asset`, decodes and slices the WebP or PNG sheet through `frames`, and stores frames in `LoadedPet`. A failed fetched cache entry can be removed; local user sheets are read directly. A latched failed load retries after the cooldown so a transient miss heals without a per-frame fetch storm.

The serve loop in `app.rs` projects the selected visible row into a `PetAction`, observes newly unread rows, resolves the effective render tier, passes the optional body tier and image-id base, and calls `PetAssets::view`. `PetAssets` chooses the track in `model`: action changes and newly unread rows play `jumping` once, then the steady action track takes over. The selected sprite becomes a single `PetView` body: either a memoized `PetCellGrid` through `cellart` or a `PetPixelView` carrying the placeholder size, sprite index, and image id.

Rendering consumes only the resulting `PetView`. Cell art is copied into the ratatui buffer. Pixel art writes kitty placeholder cells into the same buffer; the serve loop transmits the sprite image and virtual placement before the draw that first references the image id.

## Tier decision

`PetsGlyphMode` and `PixelRenderCaps` flow through `resolve_render_tier`, the pure mode-and-capability resolver. It returns one typed value: `PetRenderTier::Pixel` or `PetRenderTier::Cell`.

`effective_render_tier` folds in frame-local paintability for the live dashboard. A resolved pixel tier becomes `Cell` when pixels have no provider block to ride beside or the pet body is suppressed. Cell tiers pass through unchanged, so `glyphs = "sextant"` stays sextant when pixels cannot paint.

Pixel capability in the live sidebar is a tmux enrichment: tmux 3.6 or newer and `allow-passthrough` set to `on` or `all` provide the pixel transport fact; an attached rendering client whose terminfo is `xterm-ghostty`, `ghostty`, `xterm-kitty`, or `kitty` provides the kitty-terminal fact. `glyphs = "auto"` requires both facts, while `glyphs = "pixel"` requires only the transport fact. `[theme.display] pixel = "off"` wins over either pet tier choice and also disables the pixel context meter. Zellij resolves to cell art. `rimz list-pets` can use native kitty graphics in standalone Ghostty or kitty, and wraps the same graphics stream through tmux passthrough when run inside tmux.

The downgrade target is sextant because it is the portable cell-art baseline. Capability misses, Zellij, `NO_COLOR`, bodyless frames, and sessions with no provider block all converge on a cell path; a narrow rendered dashboard can still resolve to the cell path when the provider column has no usable room.

Geometry follows the effective tier. Pixel pets reserve `15x9` cells, and cell-art pets reserve `18x9` cells; the dashboard adds one empty row under either body.

## Focused-card action and captions

The renderer projects the selected visible row into one pet action before choosing an animation track.

| selected card state | animation track | default caption |
| --- | --- | --- |
| agent ask / waiting for input | `ask` (`waving` twice, then `waiting` once) | `someone needs you` |
| agent reasoning | `thinking` (`run-left` three loops, then `run-right` three loops) | `thinking it through` |
| agent acting, or a busy process row | `running` | `room is moving` |
| agent parked on background work, rate-limit/overload paused, or waiting on running subagents | `waiting` | `waiting on work` |
| agent compacting context | `review` | `reviewing context` |
| agent failed, or a stuck process row | `failed` | `rough patch - take a look` |
| selected row idle, successful, empty, or an idle process row | `idle` | `all caught up` after work, then `resting` |

Any pet-action change plays `jumping` once before switching to the new steady track. A newly unread row also plays `jumping` once, even when the selected card's action holds. Static role animation overrides skip one-shots and freeze the steady action track on its first frame.

The built-in catalog follows the Codex/petdex sheet rows: row 0 `idle` (6 frames), row 1 `run-right` (8), row 2 `run-left` (8), row 3 `waving` (4), row 4 `jumping` (5), row 5 `failed` (8), row 6 `waiting` (6), row 7 `running` (6), and row 8 `review` (6). The shipped animation tracks are `idle`, `thinking`, `running`, `waiting`, `review`, `ask`, `jumping`, and `failed`; `thinking` composes repeated `run-left` and `run-right` rows, while `ask` composes the `waving` and `waiting` rows.

Captions are canned renderer strings: each action owns a pool of a hundred-plus glanceable lines, and a transition draws one by the frame phase so repeats vary. `pool[0]` is the plain default in the table above. Captions read action transitions only, and `[theme.pets] voice = false` disables them; `voice.rs` tests enforce the pool contract (size, width, uniqueness).

## Cell art

Cell art renders as ordinary terminal cells through ratatui. Each frame is downsampled into a small grid of `char + fg + bg` cells, then copied into the dashboard buffer like any other line.

Sextant cells split a terminal cell into `2x3` subcells. The converter averages source pixels in linear light and chooses the best foreground/background split for each cell.

The converter aspect-fits each frame inside the fixed cell-art footprint at sextant-subcell resolution, bottom-aligning the fitted image so the pet's feet stay planted. It reads cell height/width from the pty's `TIOCGWINSZ` pixel and cell dimensions; zero or implausible pixel reports fall back to `13/6`, the ratio where the historical `36x27` subcell sample preserves a `192x208` frame exactly.

`[theme.pets] cell_aspect` overrides the probe, so the effective precedence is explicit config, pty pixel probe, then the neutral `13/6` fallback. This override supplies the missing fact under Zellij, which reports zero pty pixel dimensions; tmux 3.4 and newer can forward the terminal pixel dimensions.

Cell art stays pane-local across tmux, Zellij, detached sessions, plain terminals, and color-depth modes. Under `NO_COLOR`, the body is suppressed and the caption path carries the pet state.

## Pixel tier

The pixel tier renders the same decoded frames through the kitty graphics protocol and ratatui's normal buffer diff. The renderer paints each placeholder as one styled grapheme cluster: U+10EEEE plus row and column combining marks, with the foreground RGB encoding the kitty image id.

Ghostty's kitty support covers image placement while animation-frame actions (`a=f`/`a=a`) remain unavailable, so the renderer drives frames by cycling image ids. The frame painter owns synchronized output (DECSET 2026) around the image transmit and ratatui draw so each frame lands as one atomic redraw; RimZ applies `*:sync` during tmux room setup, so tmux buffers the bracketed writes and forwards the window to the terminal by default.

RimZ keeps virtual placements resident for the renderer session and re-transmits image data on a bounded cadence so a dropped tmux passthrough or terminal image-store eviction self-heals. Sprite data is transmitted as PNG (`f=100`), encoded once per sprite and memoized, so the self-heal cadence re-sends compressed image bytes instead of raw RGBA. Sprite image ids are stable slots, and a pet change transmits the new sheet through the same ids so terminal image data is replaced in place; rect shifts and frame changes are ordinary ratatui cell diffs. Context meters intern each distinct quantized raster under its own image id, so value changes ride ratatui's cell diff and the self-heal cadence only re-sends identical bytes. The self-heal cadence re-sends every stale visible meter as a per-frame batch under the frame's shared synchronization bracket, while a global spacing window bounds batches across frames. Each meter transmit and placement also carries its own terminal synchronization bracket, including through tmux passthrough, so replacement is atomic at the client. An LRU window bounds meter image residency; allocation protects every id referenced by the displayed frame or the frame being composed and falls back to cell rendering when the window has no safe entry. The meter path keeps every image referenced by a visible placeholder immutable. Deletes happen at renderer teardown. Graphics APCs stay bounded because macOS terminals can re-evaluate the mouse pointer on each image update.

tmux receives every graphics escape through its passthrough DCS wrapper, and placement uses kitty Unicode placeholders so redraws and pane repaints keep ownership in the sidebar pane. The live sidebar and gallery share the ratatui-buffer placeholder path, while `rimz list-pets` keeps its standalone one-shot renderer. All three use the same fixed footprints. Gallery columns paint through separate image-id ranges so one column cannot delete or ghost another column's image.

`rimz list-pets` paces multi-image pixel previews inside tmux by waiting for the terminal's kitty graphics acknowledgement after each pet image transmit. The acknowledgement proves the real terminal consumed the passthrough before the next image starts, so tmux's output-discard repaint path cannot drop later pet image data. A terminal that does not answer within the short timeout falls back to the unpaced best-effort path for the rest of the command.

The probe reads `tmux -V`, the effective pane-to-window-to-global `#{allow-passthrough}` value through `display-message -p -t <pane-or-session>`, session-scoped `list-clients -F '#{client_control_mode} #{client_termname}'`, `tmux display-message -p '#{session_name}'`, and `$TERM` for standalone preview detection. Live tmux re-probes fold failures onto the previous caps: version or passthrough command failures keep the previous transport fact, and command failures or empty rendering-client lists keep the previous kitty-terminal fact. These runtime probes stay read-only. A separate one-shot startup command raises the sidebar's own pane from `allow-passthrough on` to `all`, so graphics transmitted while its window is hidden still reach the terminal; `all` and the user opt-out `off` remain unchanged.

## Assets

The `pet` selector resolves to one of four sources, tried in order: a built-in catalog id served from the public Codex pets CDN, an `https://` URL to a user sheet, a local WebP or PNG spritesheet path, or a petdex pet name. A built-in id wins; an `http(s)://` selector is a URL; a path-like selector (one containing `/` or `.`, or starting with `~`) is a local file or directory; and a bare slug is a petdex pet.

Built-ins are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`; each maps to `<id>-spritesheet-v4.webp` under `https://persistent.oaistatic.com/codex/pets/v1/`. The cache path is `$XDG_CACHE_HOME/rimz/pets/v1/assets/<file>`, falling back to `$HOME/.cache/rimz/pets/v1/assets/<file>` or a temp cache root. Writes use temp-file plus rename.

Remote URLs use HTTPS, the same staged connect, response-header, and body-read timeouts as built-ins, the same 16 MiB byte cap, the same geometry check, and a cache key derived from the URL. A failed fetch retries up to three total attempts, so a slow large sheet can finish while a dead host still fails fast. Plain `http://` is rejected with a clear message.

Petdex pets live under `~/.codex/pets/<name>/` with a `pet.json` manifest beside a `spritesheet.webp` or `spritesheet.png`. RimZ reads `spritesheetPath` and loads that sheet through the same decode pipeline. `rimz list-pets` scans petdex manifests after the built-ins and labels them by selectable slug.

`rimz list-pets` loads previews at most two at a time on a cold cache. A failed pet fetch leaves no cache entry, so a re-run serves successful pets from disk and re-fetches only pets still missing.

Local sheets are read directly, geometry-checked, decoded, and left untouched on decode failure. A local path that points at a directory is treated as a petdex directory.

Geometry is fixed for every source: a `1536x1872` WebP or PNG holding an `8x9` grid of `192x208` pixel frames, 72 frames total. RGBA alpha becomes transparent terminal cells; opaque sheets render filled cells.

`RIMZ_PETS_OFFLINE=1` serves the cache only for built-ins and configured URLs. Petdex and local sheets already read from disk. Pets execute no commands, so `[theme.pets]` stays outside the project trust hash; the visible security surface is asset egress to the Codex CDN or the configured HTTPS host.

## Configuration

User setup lives in the [pets guide](../../guide/pets.md). The config key appears in [configuration.md](../../guide/configuration.md#pets) because it is part of the generated theme template.
