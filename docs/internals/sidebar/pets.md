# Pets

> Status: implemented. `[theme.pets] enabled = true` adds a pet overlay to the provider dashboard.

Pets are renderer-local attention art. The dashboard shows one animated companion for the selected agent or process card. The pet lives in the dashboard rather than the cards, so row layout stays stable while the bottom panel mirrors the card under the highlight.

## Dashboard Placement

The pet shares the dashboard area with the single active provider block. The rail stays provider-tabs-only, and the active tab follows the selected pane's provider unless `Left`/`Right` or mouse picks another provider. With no provider blocks, the pet renders alone.

`size = "medium"` keeps the fixed pet footprint. `size = "small"` fits the sprite body to the active provider block's height.

The sprite drops out under `NO_COLOR`, and on panes too narrow or short to hold it, leaving the provider block the full width. The rendered look and provider-block layout tiers live in [interface/sidebar.md Zone 3](../../interface/sidebar.md#zone-3--the-provider-dashboard).

## Focused-Card Action

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

Any pet-action change plays `jumping` once before switching to the new steady track. A newly unread row also plays `jumping` once, even when the selected card's action did not change. Static role animation overrides skip one-shots and freeze the steady action track on its first frame.

The built-in catalog follows the petdex/Codex sheet rows: row 0 `idle` (6 frames), row 1 `run-right` (8), row 2 `run-left` (8), row 3 `waving` (4), row 4 `jumping` (5), row 5 `failed` (8), row 6 `waiting` (6), row 7 `running` (6), and row 8 `review` (6). The default mapping uses `idle`, `thinking`, `running`, `waiting`, `review`, `ask`, `jumping`, and `failed`; `moving`, `run-right`, `run-left`, and `waving` remain defined catalog tracks for compatibility and composed tracks. Movement, waving, jumping, failed, waiting, running, and review tracks run at a calmer cadence, roughly half the Codex default speed.

Captions are canned strings in the renderer. They read action transitions only; they do not read transcripts, prompts, terminal scrollback, or provider conversations. `[theme.pets] voice = false` disables those captions.

## Cell Art

The pet renders as ordinary terminal cells through ratatui. Each WebP frame is downsampled into a small grid of `char + fg + bg` cells, then copied into the buffer like every other dashboard line. That keeps output pane-local across tmux, Zellij, detached sessions, and plain terminals.

`[theme.pets] glyphs = "auto"` uses sextants as the default quality tier. `half`, `sextant`, and `octant` pin the block-glyph tier explicitly. The converter averages source pixels in linear light and chooses the best split for each cell's foreground/background pair.

The render module receives only `PetView` data: a cell grid, an optional caption, loading state, current action, and active animation track. Network fetch, disk cache, WebP decode, frame slicing, animation selection, and memoized cell-art conversion stay in `src/sidebar_pane/pets/`.

## Assets

The `pet` selector resolves to one of four sources, tried in order: a built-in catalog id served from the public Codex pets CDN, an `https://` URL to your own sheet, a local WebP spritesheet path, or a petdex pet name. A built-in id wins; an `http(s)://` selector is a URL; a path-like selector (one containing `/` or `.`, or starting with `~`) is a local file or directory; and a bare slug is a petdex pet. This is the same "bundled name or a file path" shape the theme `scheme` field uses, extended with URL and petdex sources.

**Built-ins** resolve from the CDN on first use and install into a per-machine cache.

- **Catalog.** The built-in ids are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`; each maps to `<id>-spritesheet-v4.webp`. `rimz list-pets` previews them.
- **Source.** `https://persistent.oaistatic.com/codex/pets/v1/<file>` over HTTPS, with a request timeout and a 16 MiB byte cap.
- **Install.** The cache path is `$XDG_CACHE_HOME/rimz/pets/v1/assets/<file>`, falling back to `$HOME/.cache/rimz/pets/v1/assets/<file>` or a temp cache root. Writes use temp-file-plus-rename so a partial download never presents as a valid sheet.

**Remote URLs** bring your own pet from a host you name. The URL is fetched and cached exactly like a built-in — the same request timeout, 16 MiB cap, geometry check, and atomic install — but keyed in the cache by a SHA-256 of the URL so distinct URLs never collide and the same URL reuses its cache across runs. The scheme must be `https`: an `http://` URL is rejected with a clear message rather than fetched in plaintext. Widening egress to an arbitrary host is the cost of the convenience, so it is opt-in through the configured URL.

**Petdex pets** are the pets the Codex `petdex` tool installs under `~/.codex/pets/<name>/` — a `pet.json` manifest beside a `spritesheet.webp`. A bare slug (`pet = "wall-e"`) is looked up by name under that root; a path that points at a petdex directory works too. Either way Rimz reads the manifest's `spritesheetPath` (relative to the directory, or absolute) and loads that sheet with no network, never deleting it. The petdex sheets are the same geometry as the built-ins, so they decode through the identical pipeline. Only `spritesheetPath` is read from the manifest; the `id`/`displayName`/`description` are metadata Rimz does not render.

**Local sheets** bring your own pet with no network at all. The configured path is read directly, geometry-checked, and decoded the same way a built-in is — but Rimz only reads it: a local sheet that fails to decode is never deleted, since eviction is for cache entries, not the user's files. A local path that points at a directory is treated as a petdex pet (its `pet.json` is read for the sheet).

**Geometry** is the shared contract for both sources, and the spec a bring-your-own sheet must match: a `1536×1872` WebP holding an `8×9` grid of `192×208`-pixel frames (72 frames total). An RGBA sheet renders its alpha as transparent terminal cells; an opaque sheet renders fully filled.

`RIMZ_PETS_OFFLINE=1` disables fetches for that process tree — both the CDN and a configured URL — and uses the cache only (it has no effect on a local sheet, which never fetches). An offline host with no cached sheet shows a text loading or unavailable state while the rest of the sidebar renders normally. Cached fetched sheets are geometry-checked before use and unusable cache entries are removed; a failed load settles as unavailable for that pet and re-attempts on a fixed cooldown rather than retrying on every frame, so a transient first-fetch miss self-heals without a per-frame retry storm. Pets execute no commands, so the config stays out of the project trust hash; the visible security surface is the opt-in asset fetch — a built-in reaches the Codex CDN, a URL reaches the host you name, and petdex or local sheets remove egress entirely.

## Configuration

```toml
[theme.pets]
enabled = false
pet = "rocky"
size = "medium"
glyphs = "auto"
voice = true
```

Pets are off by default; `enabled = true` starts the dashboard overlay and allows asset fetches for built-in and configured `https://` sources. The full key reference lives in [configuration.md Pets](../../reference/configuration.md#pets).
