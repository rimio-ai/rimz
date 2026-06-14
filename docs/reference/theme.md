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

Thirteen semantic slots cover everything the sidebar paints. Each accepts `#rrggbb` hex or a raw 0–255 xterm index under `[sidebar.theme]`; an omitted slot keeps the selected scheme's tone (the bundled `TokyoNight Night` palette by default). Slot names follow the semantics rather than the shipped hues, so a light-terminal re-theme still reads `good`/`warn`/`alarm`.

The palette runs a loudness hierarchy — attention loudest, selection next, structure quiet, data coded, chrome quietest. Two rules keep it honest: `alarm` red is reserved for danger (failures, removals, the full-context crest), and one warm `caution` amber means "hot/costly/parked" everywhere.

| slot | colors |
| --- | --- |
| `good` | calm/positive: running tallies, low gauges, `+` additions, the `↗` output marker |
| `warn` | the caution floor: waiting `?` glyphs at rest, the low-mid gauge rung |
| `caution` | the warm amber "hot/costly/parked" tier: the gauge mid-band, the age-heat midpoint, and the paused `⏸` glyph (the fresh-input `↘` marker warms one step further toward `alarm`) |
| `alarm` | danger only: failed glyphs, the 100% gauge crest, `-` removals |
| `accent` | data: the `◎` sessions glyph and the `◌` cache-read marker |
| `cool` | cool informational: the `plan` posture pill, the larger window tags, the `◇` token total |
| `meta` | delegation and compaction accents: the `⇅ rc` flag, the subagent `⧉` marker, the live compacting head, and the cache-write `◍` marker |
| `body` | body content text: stat figures, capability tokens, subagent lines, and the worktree header |
| `muted` | muted chrome: labels, ages, subordinate values |
| `faint` | faintest chrome: bar tracks, `·` separators, dotted dividers |
| `rule` | the darkest chrome (the scrollbar track), a step below `faint` |
| `selection` | the selection tone: the selected card's bright `▌` spine and the dim `▎` lane bracket around its worktree |
| `selection_bg` | the selected card's background band, behind every line of the card; a subdued blend of the scheme's `colors.selection.background` toward the background, else a faint background tint. At truecolor depth the band recesses below this tone and eases darker from the `▌` spine to the rail, so the selected card reads as one recessed lit panel |

```toml
[sidebar.theme]
good = "#a0d0a0"   # hex retunes a slot
warn = 173         # a raw xterm index stays exact at every depth
```

RGB values render as RGB under truecolor depth and quantize to the nearest xterm index under `mode = "256"` or a non-truecolor `auto`; raw indexes stay exact.

The context meter resolves a health tone from the `good` → `warn` → `caution` → `alarm` ramp — healthy green through gold and orange to alarm red — in OKLab, then emits the result at the active depth. The filled bar uses that single current tone. The provider budget ("mana") bar rides the same full ramp: it anchors green at a brimming window and warms continuously toward red as it drains, with the `[sidebar.budget]` zones as its warm stops. The recede-when-healthy readers — age and attention heat, the reset-countdown burn pace, and the remote link badge — keep a quiet resting tone and ride only the warm tail of the ramp (`warn` → `caution` → `alarm`) once they leave their calm zone, since a fresh agent, a sustainable pace, or a healthy link reads quiet rather than healthy-green. RGB overrides and raw xterm indexes 16–255 participate in the ramp; ANSI indexes 0–15 are terminal-defined, so flat slot uses wear the override while the ramp keeps the scheme RGB for that slot. The attention floor before the ramp wears the flat `warn` slot, so `warn = 0..15` steps from the terminal ANSI color to the scheme warn RGB when age heat begins.

The context composition accents reuse stable sidebar tones: fresh input `↘` wears a vermilion warmed one step past the `caution` amber toward `alarm` — the costliest read in the breakdown, yet kept under the danger red so `alarm` stays exclusive to danger (retune it through the `caution` and `alarm` slots it derives from); cache-write `◍` wears the `meta` compaction/delegation violet; and the completed-compaction `↻` marker wears yellow. The `◇` token total uses `cool` blue so it stays distinct from those cost markers.

Money amounts use a fixed dollar green (`#85bb65` at truecolor depth, nearest xterm bucket at indexed depth), separate from the `good` success slot.

### How a tone resolves

A painted tone passes through three layers. The **raw palette** is the imported terminal color — `background`, `foreground`, the ANSI hues, and `colors.selection.background` from the selected scheme. The **thirteen semantic slots** above derive from it — the neutral chrome blended from background toward foreground in OKLab, the chromatic slots mapped from the ANSI hues, with `caution` warmed from yellow toward red into a vivid amber and `selection` lifted into its own bright cool tone over the dark `selection_bg` band — and are the layer you tune. **Component tokens** are the specific UI roles — the sessions glyph, a token marker, a worktree header, a transition flash — and each names its role and resolves to a semantic slot, or to a tone derived from the slots (the fresh-input `↘` marker warms `caution` a step toward `alarm` into the expense vermilion). They are internal: restyle the room through the thirteen slots and every component that aliases or derives from them follows. The one place hue is decided stays the slot table, and a CI gate keeps render code from naming a raw color directly (see [rust-conventions.md](../contributing/rust-conventions.md#architectural-invariants)).

The capability line's window token shows this layering: it reads its size through a neutral→cool→accent salience ramp — `faint` below 128k, `muted` at 128k, `cool` at 258k, `accent` at 1M+ — so a bigger window reads louder while borrowing no provider's brand color. Only true external identity (the provider brand emblems and the dollar green) holds a fixed hue outside the slots.

### Subtle steps and color depth

A subtle tone step — a small lightness dim or a breathing lift — renders as color only at truecolor depth. The 256-color cube spaces its channel levels about forty apart, coarser than such a step, so quantizing a sub-cell shift either snaps to a distant cell (a hard jump in the wrong direction) or collides with the base tone (no change at all). At indexed depth a subtle adjustment falls back to the discrete signal the cube carries honestly — a weight modifier (`DIM` or `BOLD`) for an animated pulse, or the unmodified base tone for a static recession — the same shape `NO_COLOR` already uses.

The calm card name shows the rule: at truecolor it dims its brand lightness a step, and at 256-color it keeps the full brand while the selection bar and the description carry the calm cue. Tones that already differ by a full cube cell — the neutral ladder and the health ramp — stay color at every depth, since each step lands on its own index.

At 256-color the selected card's `selection_bg` band is a full dark fill, not a subtle step, so it lands on its own cube cell and renders flat; only `NO_COLOR` drops it, leaving the bright `▌` spine and bold weight to carry the selection. At truecolor the band recesses below `selection_bg` into a lit panel — the spine column starts at the midpoint of its former gradient and eases a small per-column lightness falloff darker to the rail — so the selected card sinks into a well, set off from the lighter unread wash that rises above the card surface. That recessed ramp is a sub-cell step, so the 256-color cube keeps the flat `selection_bg` fill, the same fallback the breathing lift takes.

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

`colors.primary.background`, `colors.primary.foreground`, and `colors.normal.red` / `green` / `yellow` / `blue` / `magenta` / `cyan` are required; `colors.bright.blue` (the selection source) and `colors.selection.background` (the band) are optional. The load error names a missing palette entry or malformed color. The thirteen slots derive from those keys:

| slot | derived from |
| --- | --- |
| `good` | normal green |
| `warn` | normal yellow |
| `caution` | yellow warmed toward red and enriched — an OKLCH hue rotation plus chroma boost — into a vivid amber |
| `alarm` | normal red |
| `accent` | normal cyan |
| `cool` | normal blue |
| `meta` | normal magenta |
| `body` / `muted` / `faint` / `rule` | background toward foreground at 82% / 60% / 38% / 28% |
| `selection` | bright blue (or normal blue when absent), lifted brighter and off the data-cool slot |
| `selection_bg` | `colors.selection.background` subdued toward background for a faint full-card band, or a faint background-toward-blue tint when absent |

Blends run in OKLab, so the derived tones stay perceptually even across themes. `[sidebar.theme]` slot overrides win over any derived tone, so a near-miss theme needs only the one slot pinned.

Brightening runs in the same space and holds hue: a lift raises OKLab lightness and eases chroma toward the gamut boundary rather than letting the RGB channels hard-clamp, so an unread row brightens toward its crest — whether the blink toggles to it, `bright` holds it, or the shimmer beam sweeps across it — without drifting toward white or a neighboring hue.

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
| `paused` | `⏸︎` | `caution` | — |
| `waiting` | `?` | `warn` | — |
| `failed` | `!` | `alarm` | — |

`frames` accepts either a string or an array. A string splits into one frame per Unicode codepoint, which fits single-codepoint runs such as `"⠁⠂⠄⡀"`. An array keeps multi-codepoint single-cell glyphs intact, such as `["⏸︎"]`. Every frame must occupy exactly one terminal cell; empty frame lists, empty glyphs, zero-width glyphs, and multi-cell glyphs are rejected.

`color` accepts the semantic palette slots `good`, `warn`, `caution`, `alarm`, `accent`, `cool`, `meta`, `body`, `muted`, and `faint`, the brand tone `clay`, a `#rrggbb` hex color, or a raw 256-color index. Semantic slots retune through `[sidebar.theme]`; hex values follow the active depth; raw indexes and `clay` pass through as explicit tones. `effect` is `static` or `breathe`; `speed` is `slow`, `normal`, or `fast` for both frame advance and effect cadence. For `waiting`, `failed`, and unread `success` row heads, an omitted `effect` keeps the [unread attention effect](#unread-attention) below, `effect = "static"` quiets it to a constant bold tone, and `speed` paces it. A literal frame blink is a frame sequence such as `frames = [" ", "!"]`.

Every role uses the same model: frames, color, effect, and speed. The unread attention signal carries across the lead glyph, the card name, the description, and the make-up `?`/`!`/`✓` buckets as one group — age heat sliding their base tone toward alarm — until the pane is focused; the cockpit count and per-bucket counts use still representative frames.

### Unread attention

`[sidebar.animations] unread` picks how an unread attention row reads. The lead glyph, the card name, the description, and the cockpit `?`/`!`/`✓` make-up buckets all carry the choice as one group, so a row that needs you reads with one voice.

The continuous signal is reserved for the **one row that most needs you** — the oldest unread row that needs an answer (`waiting` or `failed`, the `␣` triage head). Only that lead row carries the chosen `shimmer` or `blink`, so a single pane is the only thing in motion; every other unread row, an unread `✓` result included, settles to the steady `bright` crest — unmistakable by contrast against the calm rows, but still. A transition flash already announced each row — an ask as it enters, a result as its turn lands — so the steady crest is the rest after the announcement, not a missed cue.

An unread card also grounds on a soft, uniform **wash** — one panel marking the row as unseen the way a mail inbox shades an unread line, with the row's status carried by its `?`/`!`/`✓` glyph. The wash is a lighter tint of the selection blue: the `selection_bg` panel lifted in lightness with its cool hue held, so it lands on the same cool-blue family the scheme derives for the selection band, one clear step brighter. A whole-card surface reads at a scanning glance where the one-cell glyph cannot, and it holds still, so continuous motion stays reserved to the lead row. The selected card keeps the attention through its bright `▌` spine and the per-column lit-panel falloff, so the brighter unread fill is the "needs you" surface without ever reading as selection; the selection band stays the selected card's signature and wins when a card is both selected and unread, and off truecolor the wash drops to the weight-carried unread look, the surface being a truecolor refinement.

| `unread` | the lead row reads as |
| --- | --- |
| `shimmer` (default) | a light beam flows left-to-right across the glyph, the name, and the description, each on its own sweep, the flow quickening with age |
| `bright` | the bright crest held constant — brighter and bold, no motion (every unread row reads this way) |
| `blink` | a hard 2-pole brightness toggle between the resting tone and the crest, the rate quickening with age |

`blink` and `bright` rise to the same gentle crest; the shimmer beam rides a brighter one, because it lights only the few cells under its moving center rather than the whole row at once, so a matching crest would read far fainter. All ride the same gamut-safe lift, holding hue as they brighten. A per-role `effect = "static"` on `waiting`, `failed`, or `success` wins over the `unread` choice, holding that role's row at a constant bold tone. At truecolor the effect rides an OKLab lightness lift; the 256-color cube and `NO_COLOR` carry it on weight — a held bold for `bright` and `blink`, a moving bold cell for `shimmer` — so the signal reads at every depth ([subtle steps and color depth](#subtle-steps-and-color-depth)).

```toml
[sidebar.animations]
unread = "shimmer"   # or "bright", "blink"
```

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

`[sidebar] glow` gates the post-render transition-flash tier layered over the base palette. The unread attention effect is part of base status-head rendering and follows `[sidebar.theme].mode` plus the `NO_COLOR` fallback. `auto` (default) follows `COLORTERM`; `always` forces transition flashes when a real truecolor terminal under-advertises, such as an SSH hop that forwards `TERM` but drops `COLORTERM` — pair it with `mode = "truecolor"` for RGB base tones; `never` keeps the plain render plus the base attention effect.

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
rimz config set sidebar.animations.unread shimmer
rimz config set sidebar.animations.idle.effect breathe
rimz config set sidebar.providers.codex.color 33
```

`rimz config set` validates before it writes: an unknown scheme is rejected with the bundled-catalog count and custom-file path hint, and a malformed color, Alacritty file, or frame is rejected before the file changes. Loading stays lenient, so an older or stale scheme value falls back to the default `TokyoNight Night` scheme at render time instead of taking down the sidebar. The full `rimz config` surface is in [cli/maintenance.md](./cli/maintenance.md).
