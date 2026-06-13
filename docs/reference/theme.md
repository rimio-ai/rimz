# Theming

> See [interface/sidebar.md](../interface/sidebar.md) for what every tone and glyph means on screen; this doc is the knobs that restyle them.

The sidebar's palette, status-head animations, and provider brand styling are per-machine display settings under `[sidebar]` in `~/.config/rimz/config.toml`. Glyph shapes carry every state and color reinforces them ([reading the glyphs](../interface/sidebar.md#reading-the-glyphs)), so any theme — including no color at all — keeps the room readable. Theme settings are personal display preferences and stay outside the project trust hash.

```sh
rimz config set sidebar.theme "TokyoNight Night"
```

Every key below also lives as commented TOML in the generated template: `rimz config init --print`.

## Schemes

`[sidebar.theme] scheme` picks the palette source. `rimz list-themes` prints every bundled name.

| `scheme` | character | source |
| --- | --- | --- |
| unset | the shipped default | bundled `TokyoNight Night` |
| `Afterglow`, `Catppuccin Mocha`, ... | theme-defined | bundled Alacritty catalog |
| `/path/to/theme.toml` | theme-defined | custom Alacritty TOML |

```toml
[sidebar.theme]
scheme = "TokyoNight Night"
```

### Bundled Themes

Rimz embeds the Alacritty export from [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes), so theme names work the same across terminals and muxes. Pick a name exactly as it appears under [crates/rimz/themes/alacritty](../../crates/rimz/themes/alacritty); names with spaces need TOML quotes.

The checked-in catalog is refreshed with `cargo xtask theme-refresh`; its provenance and license live in [crates/rimz/themes/README.md](../../crates/rimz/themes/README.md) and [crates/rimz/themes/LICENSE](../../crates/rimz/themes/LICENSE).

```toml
[sidebar.theme]
scheme = "Catppuccin Mocha"
# scheme = "Solarized Light"
```

The bundled Alacritty TOML supplies `background`, `foreground`, the six normal ANSI hues, and `bright.blue` for the selection accent. Themes without `bright.blue` use `normal.blue` for that slot.

### Custom Theme Paths

`scheme` also accepts a path to an Alacritty TOML file, with `~` expanded. A changed `scheme` value applies on the next snapshot; loaded theme files are cached for the sidebar process lifetime, so edits inside one apply after a sidebar restart.

```toml
[sidebar.theme]
scheme = "~/themes/rimz.toml"
```

## Color Depth

`[sidebar.theme] mode` sets the palette depth. `auto` (default) uses truecolor when `COLORTERM` advertises it and otherwise quantizes the selected RGB tones to xterm 256 indexes; `truecolor` forces RGB, which is useful across SSH or mux hops that forward `TERM` but drop `COLORTERM`; `256` pins indexed output. Use [`glow = "always"`](#glow) separately when the same hop also under-advertises transition-flash support.

```toml
[sidebar.theme]
mode = "auto"        # or "truecolor", "256"
```

## Palette Slots

Twelve semantic slots cover everything the sidebar paints. Each accepts `#rrggbb` hex or a raw 0–255 xterm index under `[sidebar.theme]`; an omitted slot keeps the selected scheme's tone (the bundled `TokyoNight Night` palette by default). Slot names follow the semantics rather than the shipped hues, so a light-terminal re-theme still reads `good`/`warn`/`alarm`.

| slot | colors |
| --- | --- |
| `good` | calm/positive: running tallies, low gauges, `+` additions |
| `warn` | caution: waiting glyphs at rest, mid gauges |
| `caution` | the amber badge/gauge rung between warning and alarm, and the age-heat midpoint |
| `alarm` | failed glyphs, high gauges, `-` removals |
| `accent` | structure: worktree headers, the selected lane spine |
| `cool` | cool informational: the `plan` posture pill, the larger window tags, the `◇` token total |
| `meta` | delegation and compaction accents: the `⇅ rc` flag, the subagent `⧉` marker, the live compacting head, and the cache-write `◍` marker |
| `body` | body content text: stat figures, capability tokens, subagent lines |
| `muted` | muted chrome: labels, ages, subordinate values |
| `faint` | faintest chrome: bar tracks, `·` separators, dotted dividers |
| `rule` | the darkest chrome (the scrollbar track), a step below `faint` |
| `selection` | the selected-row `▌` accent bar |

```toml
[sidebar.theme]
good = "#a0d0a0"   # hex retunes a slot
warn = 173         # a raw xterm index stays exact at every depth
```

RGB values render as RGB under truecolor depth and quantize to the nearest xterm index under `mode = "256"` or a non-truecolor `auto`; raw indexes stay exact.

The context meter resolves a health tone from the `good` → `warn` → `caution` → `alarm` ramp — healthy green through gold and orange to alarm red — in OKLab, then emits the result at the active depth. The filled bar uses that single current tone. The provider budget ("mana") bar rides the same full ramp: it anchors green at a brimming window and warms continuously toward red as it drains, with the `[sidebar.budget]` zones as its warm stops. The recede-when-healthy readers — age and attention heat, the reset-countdown burn pace, and the remote link badge — keep a quiet resting tone and ride only the warm tail of the ramp (`warn` → `caution` → `alarm`) once they leave their calm zone, since a fresh agent, a sustainable pace, or a healthy link reads quiet rather than healthy-green. RGB overrides and raw xterm indexes 16–255 participate in the ramp; ANSI indexes 0–15 are terminal-defined, so flat slot uses wear the override while the ramp keeps the scheme RGB for that slot. The attention floor before the ramp wears the flat `warn` slot, so `warn = 0..15` steps from the terminal ANSI color to the scheme warn RGB when age heat begins.

The context composition accents reuse stable sidebar tones: fresh input `↘` wears `heat_tone(1.0)`, the 100% context-fill red; cache-write `◍` wears the `meta` compaction/delegation violet; and the completed-compaction `↻` marker wears yellow. The `◇` token total uses `cool` blue so it stays distinct from those cost markers.

Money amounts use a fixed dollar green (`#85bb65` at truecolor depth, nearest xterm bucket at indexed depth), separate from the `good` success slot.

### How a tone resolves

A painted tone passes through three layers. The **raw palette** is the imported terminal color — `background`, `foreground`, and the ANSI hues from the selected scheme. The **twelve semantic slots** above derive from it (the neutral chrome blended from background toward foreground in OKLab, the chromatic slots mapped from the ANSI hues) and are the layer you tune. **Component tokens** are the specific UI roles — the sessions glyph, a token marker, a worktree header, a transition flash — and each names its role and resolves to one semantic slot. They are internal: restyle the room through the twelve slots and every component that aliases a slot follows. The one place hue is decided stays the slot table, and a CI gate keeps render code from naming a raw color directly (see [rust-conventions.md](../contributing/rust-conventions.md#architectural-invariants)).

The capability line's window token shows this layering: it reads its size through a neutral→cool→accent salience ramp — `faint` below 128k, `muted` at 128k, `cool` at 258k, `accent` at 1M+ — so a bigger window reads louder while borrowing no provider's brand color. Only true external identity (the provider brand emblems and the dollar green) holds a fixed hue outside the slots.

### Subtle steps and color depth

A subtle tone step — a small lightness dim or a breathing lift — renders as color only at truecolor depth. The 256-color cube spaces its channel levels about forty apart, coarser than such a step, so quantizing a sub-cell shift either snaps to a distant cell (a hard jump in the wrong direction) or collides with the base tone (no change at all). At indexed depth a subtle adjustment falls back to the discrete signal the cube carries honestly — a weight modifier (`DIM` or `BOLD`) for an animated pulse, or the unmodified base tone for a static recession — the same shape `NO_COLOR` already uses.

The calm card name shows the rule: at truecolor it dims its brand lightness a step, and at 256-color it keeps the full brand while the selection bar and the description carry the calm cue. Tones that already differ by a full cube cell — the neutral ladder and the health ramp — stay color at every depth, since each step lands on its own index.

## Custom Theme Files

Custom themes are Alacritty TOML files. A minimal valid theme:

```toml
[colors.primary]
background = '#1a1b26'
foreground = '#c0caf5'

[colors.normal]
red = '#f7768e'
green = '#9ece6a'
yellow = '#e0af68'
blue = '#7aa2f7'
magenta = '#bb9af7'
cyan = '#7dcfff'

[colors.bright]
blue = '#7aa2f7'
```

`colors.primary.background`, `colors.primary.foreground`, and `colors.normal.red` / `green` / `yellow` / `blue` / `magenta` / `cyan` are required; `colors.bright.blue` is optional and falls back to `colors.normal.blue`. The load error names a missing palette entry or malformed color. The twelve slots derive from those keys:

| slot | derived from |
| --- | --- |
| `good` | normal green |
| `warn` | normal yellow |
| `caution` | yellow blended halfway toward red |
| `alarm` | normal red |
| `accent` | normal cyan |
| `cool` | normal blue |
| `meta` | normal magenta |
| `body` / `muted` / `faint` / `rule` | background toward foreground at 82% / 60% / 38% / 28% |
| `selection` | bright blue, or normal blue when bright blue is absent |

Blends run in OKLab, so the derived tones stay perceptually even across themes. `[sidebar.theme]` slot overrides win over any derived tone, so a near-miss theme needs only the one slot pinned.

Brightening runs in the same space and holds hue: a lift raises OKLab lightness and eases chroma toward the gamut boundary rather than letting the RGB channels hard-clamp, so the unread blink pulses brighter without drifting toward white or a neighboring hue.

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

`color` accepts the semantic palette slots `good`, `warn`, `alarm`, `accent`, `cool`, `meta`, `body`, `muted`, and `faint`, the brand tone `clay`, a `#rrggbb` hex color, or a raw 256-color index. Semantic slots retune through `[sidebar.theme]`; hex values follow the active depth; raw indexes and `clay` pass through as explicit tones. `effect` is `static` or `breathe`; `speed` is `slow`, `normal`, or `fast` for both frame advance and effect cadence. For `waiting`, `failed`, and unread `success` row heads, an omitted `effect` keeps the shipped blink, `effect = "static"` quiets it, and `speed` tunes it when the blink is active. A literal blink is a frame sequence such as `frames = [" ", "!"]`.

Every role uses the same model: frames, color, effect, and speed. The built-in attention blink — a 2-pole square wave whose rate quickens with age — still applies age heat across the lead glyph, the card name, the description, and the make-up `?`/`!` buckets unless an explicit static effect quiets it; the cockpit count and per-bucket counts use still representative frames.

## Provider Styling

`[sidebar.providers.<kind>]` restyles a provider's dashboard block — display name, ASCII emblem, and brand color — over the built-in defaults (claude clay, codex blue, pi forest green).

```toml
[sidebar.providers.claude]
product_name = "Claude Code"
color = "#d97757"
ascii_art = "CLAUDE"
```

`color` accepts either `#rrggbb` or a raw 0-255 index; RGB values carry a quantized fallback for indexed renderers. Each field is optional, so a color override can leave the shipped art intact. Block selection and ordering stay in [configuration.md](./configuration.md#provider-dashboard); account and emblem resolution is in [internals/agents/provider.md](../internals/agents/provider.md).

## Glow

`[sidebar] glow` gates the post-render transition-flash tier layered over the base palette. The attention/result blink is part of base status-head rendering and follows `[sidebar.theme].mode` plus the `NO_COLOR` fallback. `auto` (default) follows `COLORTERM`; `always` forces transition flashes when a real truecolor terminal under-advertises, such as an SSH hop that forwards `TERM` but drops `COLORTERM` — pair it with `mode = "truecolor"` for RGB base tones; `never` keeps the plain render plus the base blink.

```toml
[sidebar]
glow = "auto"        # or "always", "never"
```

Glyph shapes carry every state, so `NO_COLOR` suppresses color while keeping shape and depth modifiers where they add signal; each status, meter, and marker still reads by shape.

## Changing Values

```sh
rimz config set sidebar.theme "TokyoNight Night"
rimz config set sidebar.theme.good '#a0d0a0'
rimz config set sidebar.glow always
rimz config set sidebar.animations.idle.effect breathe
rimz config set sidebar.providers.codex.color 33
```

`rimz config set` validates before it writes: an unknown scheme is rejected with the bundled-catalog count and custom-file path hint, and a malformed color, Alacritty file, or frame is rejected before the file changes. Loading stays lenient, so an older or stale scheme value falls back to the default `TokyoNight Night` scheme at render time instead of taking down the sidebar. The full `rimz config` surface is in [cli/maintenance.md](./cli/maintenance.md).
