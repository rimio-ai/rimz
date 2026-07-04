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
| `frames.rs` | WebP/PNG sheet decode and slicing into `RgbaImage` frames. |
| `cellart.rs` | Sextant downsampling from RGBA frames into terminal cells. |
| `pixel.rs` | Kitty graphics payloads, tmux passthrough wrapping, image ids, placeholders, and stale-image cleanup. |
| `pixel/probe.rs` | Runtime capability probe for tmux passthrough and standalone kitty-style preview terminals. |
| `model.rs` | Pet actions, animation tracks, composed tracks, and per-track timing. |
| `voice.rs` | Canned captions keyed by action transitions. |
| `preview.rs` | `rimz list-pets` preview loading for cell and pixel branches. |

## Data flow

Each frame starts from `[theme.pets] pet`. `asset::resolve_pet_source` turns the selector into a built-in CDN asset, HTTPS URL, local sheet, or petdex install. `PetAssets` owns the load state: `loading` holds the background loader receiver, `loaded` holds decoded frames plus the memoized cell-art cache, and `failed` records an unavailable caption plus a retry cooldown.

The loader path resolves bytes through `asset`, decodes and slices the WebP or PNG sheet through `frames`, and stores frames in `LoadedPet`. A failed fetched cache entry can be removed; local user sheets are read directly. A latched failed load retries after the cooldown so a transient miss heals without a per-frame fetch storm.

The serve loop in `app.rs` projects the selected visible row into a `PetAction`, observes newly unread rows, resolves the effective render tier, passes the optional body tier, and calls `PetAssets::view`. `PetAssets` chooses the track in `model`: action changes and newly unread rows play `jumping` once, then the steady action track takes over. The selected sprite becomes a single `PetView` body: either a memoized `PetCellGrid` through `cellart` or a `PetPixelView` for the post-ratatui kitty graphics paint path.

Rendering consumes only the resulting `PetView`. Cell art is copied into the ratatui buffer. Pixel art reserves blank placeholder cells in the dashboard and the serve loop writes the kitty graphics placement after ratatui flushes stdout.

## Tier decision

`PetsGlyphMode` and `PetRenderCaps` flow through `resolve_render_tier`, the pure mode-and-capability resolver. It returns one typed value: `PetRenderTier::Pixel` or `PetRenderTier::Cell`.

`effective_render_tier` folds in frame-local paintability for the live dashboard. A resolved pixel tier becomes `Cell` when pixels have no provider block to ride beside or the pet body is suppressed. Cell tiers pass through unchanged, so `glyphs = "sextant"` stays sextant when pixels cannot paint.

Pixel capability in the live sidebar is a tmux enrichment: tmux 3.6 or newer and `allow-passthrough` set to `on` or `all` provide the pixel transport fact; an attached rendering client whose terminfo is `xterm-ghostty`, `ghostty`, `xterm-kitty`, or `kitty` provides the kitty-terminal fact. `glyphs = "auto"` requires both facts, while `glyphs = "pixel"` requires only the transport fact. Zellij resolves to cell art. `rimz list-pets` can use native kitty graphics in standalone Ghostty or kitty, and wraps the same graphics stream through tmux passthrough when run inside tmux.

The downgrade target is sextant because it is the portable cell-art baseline. Capability misses, Zellij, `NO_COLOR`, bodyless frames, and sessions with no provider block all converge on a cell path; a narrow rendered dashboard can still clear a pixel placement when the placeholder rect has no usable room.

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

Captions are canned renderer strings. They read action transitions only, and `[theme.pets] voice = false` disables them.

## Cell art

Cell art renders as ordinary terminal cells through ratatui. Each frame is downsampled into a small grid of `char + fg + bg` cells, then copied into the dashboard buffer like any other line.

Sextant cells split a terminal cell into `2x3` subcells. The converter averages source pixels in linear light and chooses the best foreground/background split for each cell.

Cell art stays pane-local across tmux, Zellij, detached sessions, plain terminals, and color-depth modes. Under `NO_COLOR`, the body is suppressed and the caption path carries the pet state.

## Pixel tier

The pixel tier renders the same decoded frames through the kitty graphics protocol after ratatui draws the dashboard. The renderer reserves blank cells and records the absolute placeholder rect; the serve loop owns stdout and writes placement escapes for that rect.

Ghostty's kitty support covers image placement while animation-frame actions (`a=f`/`a=a`) remain unavailable, so the renderer drives frames by cycling image ids and rewriting placeholder cells. Each frame is emitted under synchronized output (DECSET 2026) so the swap lands as one atomic redraw and skips partial or blank intermediate frames; Rimz applies `*:sync` during tmux room setup, so tmux buffers the bracketed writes and forwards the window to the terminal by default.

Rimz transmits each sprite image and emits its virtual placement once per sprite id for the current pet and rect; frame changes then rewrite only placeholder cells. Graphics APCs stay one-shot because macOS terminals can re-evaluate the mouse pointer on each image update.

tmux receives every graphics escape through its passthrough DCS wrapper, and placement uses kitty Unicode placeholders so redraws and pane repaints keep ownership in the sidebar pane. The live sidebar, gallery, and `rimz list-pets` share the same fixed footprints. Gallery columns paint through separate image-id ranges so one column cannot delete or ghost another column's image.

The probe reads `tmux -V`, `allow-passthrough`, session-scoped `list-clients -F '#{client_control_mode} #{client_termname}'`, `tmux display-message -p '#{session_name}'`, and `$TERM` for standalone preview detection. These are runtime probes, not command-executing config.

## Assets

The `pet` selector resolves to one of four sources, tried in order: a built-in catalog id served from the public Codex pets CDN, an `https://` URL to a user sheet, a local WebP or PNG spritesheet path, or a petdex pet name. A built-in id wins; an `http(s)://` selector is a URL; a path-like selector (one containing `/` or `.`, or starting with `~`) is a local file or directory; and a bare slug is a petdex pet.

Built-ins are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`; each maps to `<id>-spritesheet-v4.webp` under `https://persistent.oaistatic.com/codex/pets/v1/`. The cache path is `$XDG_CACHE_HOME/rimz/pets/v1/assets/<file>`, falling back to `$HOME/.cache/rimz/pets/v1/assets/<file>` or a temp cache root. Writes use temp-file plus rename.

Remote URLs use HTTPS, the same timeout, the same 16 MiB byte cap, the same geometry check, and a cache key derived from the URL. Plain `http://` is rejected with a clear message.

Petdex pets live under `~/.codex/pets/<name>/` with a `pet.json` manifest beside a `spritesheet.webp` or `spritesheet.png`. Rimz reads `spritesheetPath` and loads that sheet through the same decode pipeline. `rimz list-pets` scans petdex manifests after the built-ins and labels them by selectable slug.

Local sheets are read directly, geometry-checked, decoded, and left untouched on decode failure. A local path that points at a directory is treated as a petdex directory.

Geometry is fixed for every source: a `1536x1872` WebP or PNG holding an `8x9` grid of `192x208` pixel frames, 72 frames total. RGBA alpha becomes transparent terminal cells; opaque sheets render filled cells.

`RIMZ_PETS_OFFLINE=1` serves the cache only for built-ins and configured URLs. Petdex and local sheets already read from disk. Pets execute no commands, so `[theme.pets]` stays outside the project trust hash; the visible security surface is asset egress to the Codex CDN or the configured HTTPS host.

## Configuration

User setup lives in [theme.md Pets](../../reference/theme.md#pets). The config key appears in [configuration.md](../../reference/configuration.md#pets) because it is part of the generated theme template.
