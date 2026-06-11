# Theming

> See [interface/sidebar.md](../interface/sidebar.md) for what every tone and glyph means on screen; this doc is the knobs that restyle them.

The sidebar's palette, status-head animations, and provider brand styling are per-machine display settings under `[sidebar]` in `~/.config/rimz/config.toml`. Glyph shapes carry every state and color reinforces them ([reading the glyphs](../interface/sidebar.md#reading-the-glyphs)), so any theme — including no color at all — keeps the room readable. Theme settings are personal display preferences and stay outside the project trust hash.

```sh
rimz config set sidebar.theme.scheme slate
```

Every key below also lives as commented TOML in the generated template: `rimz config init --print`.

## Schemes

`[sidebar.theme] scheme` picks the palette source.

| `scheme` | character | source |
| --- | --- | --- |
| `auto` (default) | matches your terminal | Ghostty's active theme, falling back to `clay` |
| `clay` | warm, earthen | built-in |
| `slate` | cool, blue-leaning | built-in |
| `classic` | neutral | built-in |

```toml
[sidebar.theme]
scheme = "slate"
```

### Following Ghostty

`scheme = "auto"` follows Ghostty's active theme when the sidebar process can read it and falls back to the built-in `clay` palette. The sidebar reads `~/.config/ghostty/config`, takes the last `theme =` line, and loads the named theme through the lookup ladder below; a `theme = dark:X,light:Y` pair picks the dark side because muxes do not expose a reliable light/dark signal, and a plain comma list picks the first entry.

Auto-detection is cached for the sidebar process lifetime, so changing the terminal theme needs a sidebar restart; explicit `scheme = "slate"` or `scheme = "/path/to/theme"` is the pin lever.

### Any Ghostty Theme

`scheme` accepts any Ghostty theme name or theme-file path, resolved in order:

1. `~/.config/ghostty/themes/<name>`
2. `$GHOSTTY_RESOURCES_DIR/themes/<name>`
3. the value as a path, with `~` expanded

```toml
[sidebar.theme]
scheme = "Catppuccin Mocha"   # an installed Ghostty theme
# scheme = "~/themes/mine"    # or any Ghostty-format file
```

A changed `scheme` value applies on the next snapshot; loaded theme files are cached like auto-detection, so edits inside one apply after a sidebar restart.

## Color Depth

`[sidebar.theme] mode` sets the palette depth. `auto` (default) uses truecolor when `COLORTERM` advertises it and otherwise quantizes the selected RGB tones to xterm 256 indexes; `truecolor` forces RGB, which is useful across SSH or mux hops that forward `TERM` but drop `COLORTERM` (pair it with [`glow = "always"`](#glow)); `256` pins indexed output.

```toml
[sidebar.theme]
mode = "auto"        # or "truecolor", "256"
```

## Palette Slots

Twelve semantic slots cover everything the sidebar paints. Each accepts `#rrggbb` hex or a raw 0–255 xterm index under `[sidebar.theme]`; an omitted slot keeps the selected scheme's tone. Slot names follow the semantics rather than the shipped hues, so a light-terminal re-theme still reads `good`/`warn`/`alarm`.

| slot | colors | clay default |
| --- | --- | --- |
| `good` | calm/positive: running tallies, low gauges, `+` additions, cache reads | `#96c293` |
| `warn` | caution: waiting glyphs at rest, mid gauges, cache writes | `#dfb66d` |
| `caution` | the amber badge/gauge rung between warning and alarm | `#e0915c` |
| `alarm` | failed glyphs, high gauges, `-` removals, fresh input | `#de6e6e` |
| `accent` | structure: worktree headers, the selected lane spine | `#72b3aa` |
| `cool` | cool informational: the `plan` posture pill, window tags | `#7fa8de` |
| `meta` | delegation: the `⇅ rc` flag, the subagent `⧉` marker | `#b49be0` |
| `soft` | soft content text: stat figures, capability tokens, subagent lines | `#a6a19a` |
| `dim` | dim chrome: labels, ages, subordinate values | `#767168` |
| `faint` | faintest chrome: bar tracks, `·` separators, dotted dividers | `#45423d` |
| `rule` | the darkest chrome (the scrollbar track), a step below `faint` | `#343230` |
| `selection` | the selected-row `▌` accent bar | `#8ab3e0` |

```toml
[sidebar.theme]
good = "#a0d0a0"   # hex retunes a slot
warn = 173         # a raw xterm index stays exact at every depth
```

RGB values render as RGB under truecolor depth and quantize to the nearest xterm index under `mode = "256"` or a non-truecolor `auto`; raw indexes stay exact.

## Custom Theme Files

Custom themes are Ghostty-format theme files — the same files Ghostty ships and the community publishes. A minimal valid theme:

```
background = #1a1b26
foreground = #c0caf5
palette = 1=#f7768e
palette = 2=#9ece6a
palette = 3=#e0af68
palette = 4=#7aa2f7
palette = 5=#bb9af7
palette = 6=#7dcfff
palette = 12=#7aa2f7
```

`background`, `foreground`, and palette entries 1–6 and 12 are required; the load error names a missing entry. The twelve slots derive from those keys:

| slot | derived from |
| --- | --- |
| `good` | green — palette 2 |
| `warn` | yellow — palette 3 |
| `caution` | yellow blended halfway toward red |
| `alarm` | red — palette 1 |
| `accent` | cyan — palette 6 |
| `cool` | blue — palette 4 |
| `meta` | magenta — palette 5 |
| `soft` / `dim` / `faint` / `rule` | background toward foreground at 65% / 45% / 25% / 18% |
| `selection` | bright blue — palette 12 |

Blends run in OKLab, so the derived tones stay perceptually even across themes. `[sidebar.theme]` slot overrides win over any derived tone, so a near-miss theme needs only the one slot pinned.

## Animations

`[sidebar.animations]` themes the status heads the sidebar paints; what each head means in the room is in [the glyph legend](../interface/sidebar.md#reading-the-glyphs). The roles are `thinking`, `working`, `compacting`, `delegating`, `resolving`, `idle`, `success`, `paused`, `waiting`, and `failed`. Each role is optional, and each field inside a role is optional; an omitted field keeps the built-in value for that role, so a one-line `idle.effect = "breathe"` override leaves the idle glyph and neutral default tone alone.

```toml
[sidebar.animations.thinking]
frames = "⠁⠂⠄⡀⡈⡐⡠⣀⣁⣂⣄⣌⣔⣤⣥⣦⣮⣶⣷⣿⡿⠿⢟⠟⡛⠛⠫⢋⠋⠍⡉⠉⠑⠡⢁"
color = "clay"
effect = "static"
speed = "fast"

[sidebar.animations.idle]
effect = "breathe"
```

The built-in heads:

| role | frames | color | speed |
| --- | --- | --- | --- |
| `thinking` | braille fill cycle | `clay` | fast |
| `working` | `⣾⣽⣻⢿⡿⣟⣯⣷` | `clay` | fast |
| `compacting` | `▁▃▄▅▆▇▆▅▄▃` | `meta` | fast |
| `delegating` | `⢄⢂⢁⡁⡈⡐⡠` | `clay` | fast |
| `resolving` | `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` | `meta` | fast |
| `idle` | `○` | `good` | normal |
| `success` | `✓` | `good` | normal |
| `paused` | `⏸︎` | `warn` | — |
| `waiting` | `?` | `warn` | — |
| `failed` | `!` | `alarm` | — |

`frames` accepts either a string or an array. A string splits into one frame per Unicode codepoint, which fits single-codepoint runs such as `"⠁⠂⠄⡀"`. An array keeps multi-codepoint single-cell glyphs intact, such as `["⏸︎"]`. Every frame must occupy exactly one terminal cell; empty frame lists, empty glyphs, zero-width glyphs, and multi-cell glyphs are rejected.

`color` accepts the semantic palette slots `good`, `warn`, `alarm`, `accent`, `cool`, `meta`, `soft`, `dim`, and `faint`, the brand tone `clay`, a `#rrggbb` hex color, or a raw 256-color index. Semantic slots retune through `[sidebar.theme]`; hex values follow the active depth; raw indexes and `clay` pass through as explicit tones. `effect` is `static`, `breathe`, or `blink`; `speed` is `slow`, `normal`, or `fast` for both frame advance and effect cadence.

`waiting`, `failed`, and `paused` honor one `frames` value and `color`. The attention age heat, unread hard-blink, and held pause grammar are product behavior, so `effect` and `speed` on those roles are ignored, and multiple frames for those roles are rejected.

## Provider Styling

`[sidebar.providers.<kind>]` restyles a provider's dashboard block — display name, ASCII emblem, and brand color — over the built-in defaults (claude clay, codex blue, pi forest green).

```toml
[sidebar.providers.claude]
product_name = "Claude Code"
color = "#d97757"
ascii_art = "CLAUDE"
```

`color` accepts either `#rrggbb` or a raw 0-255 index; RGB values carry a quantized fallback for indexed renderers. Each field is optional, so a color override can leave the shipped art intact. Block selection and ordering stay in [configuration.md](./configuration.md#provider-dashboard); account and emblem resolution is in [internals/agents/account.md](../internals/agents/account.md).

## Glow

`[sidebar] glow` gates the truecolor effects tier — the attention glow and transition flashes layered over the base palette. `auto` (default) follows `COLORTERM`; `always` forces the tier when a real truecolor terminal under-advertises, such as an SSH hop that forwards `TERM` but drops `COLORTERM` — pair it with `mode = "truecolor"`; `never` keeps the plain render.

```toml
[sidebar]
glow = "auto"        # or "always", "never"
```

Glyph shapes carry every state, so `NO_COLOR` suppresses all color and effects while each status, meter, and marker still reads by shape.

## Changing Values

```sh
rimz config set sidebar.theme.scheme slate
rimz config set sidebar.theme.good '#a0d0a0'
rimz config set sidebar.glow always
rimz config set sidebar.animations.idle.effect breathe
rimz config set sidebar.providers.codex.color 33
```

`rimz config set` validates before it writes: an unknown scheme is rejected with the built-in names and the Ghostty theme directories searched, and a malformed color or frame is rejected before the file changes. Loading stays lenient, so an older binary tolerates a newer file. The full `rimz config` surface is in [cli/maintenance.md](./cli/maintenance.md).
