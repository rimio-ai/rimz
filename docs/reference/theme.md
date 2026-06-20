# Theming

> See [interface/sidebar.md](../interface/sidebar.md) for what every tone and glyph means on screen; this doc is the knobs that restyle them.

The sidebar's palette, glyph set, status-head animations, and provider brand styling are per-machine display settings in `~/.config/rimz/theme.toml`. Glyph shapes carry every state and color reinforces them ([reading the glyphs](../interface/sidebar.md#reading-the-glyphs)), so any theme — including no color at all — keeps the room readable. Theme settings are personal display preferences and stay outside the project trust hash.

```sh
rimz config set theme "TokyoNight Night"
```

Every key below also lives as commented TOML in the generated template: `rimz config init --print`.

## Style preset

`[theme] style` is the one-line headline that pairs color depth with the glyph set. `modern` runs truecolor with the [Nerd Font glyph set](#glyphs); `default` keeps [auto color depth](#color-depth) and the shipped Unicode glyphs. An explicit `[theme] mode` or `[theme.glyphs] set` overrides the matching half of the preset, so you can take the Nerd Font icons at `256` color or pin truecolor with the Unicode glyphs.

```toml
[theme]
style = "modern"   # truecolor + Nerd Font; or "default" for auto color + Unicode
```

## Schemes

`[theme] scheme` picks the palette source. `rimz list-themes` prints every bundled name.

| `scheme` | character | source |
| --- | --- | --- |
| unset | the shipped default | bundled `TokyoNight Night` |
| `Afterglow`, `Catppuccin Mocha`, ... | theme-defined | bundled Alacritty catalog |
| `/path/to/theme.toml` | theme-defined | custom Alacritty TOML |

```toml
[theme]
scheme = "TokyoNight Night"
```

### Bundled Themes

Rimz embeds the Alacritty export from [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes), so theme names work the same across terminals and muxes. Pick a name exactly as it appears under [crates/rimz/themes/alacritty](../../crates/rimz/themes/alacritty); names with spaces need TOML quotes.

The checked-in catalog is refreshed with `cargo xtask theme-refresh`; its provenance and license live in [crates/rimz/themes/README.md](../../crates/rimz/themes/README.md) and [crates/rimz/themes/LICENSE](../../crates/rimz/themes/LICENSE).

```toml
[theme]
scheme = "Catppuccin Mocha"
# scheme = "Solarized Light"
```

The bundled Alacritty TOML supplies `background`, `foreground`, the six normal ANSI hues, and `bright.blue` for the selection accent. Themes without `bright.blue` use `normal.blue` for that slot.

### Pasteable palette and custom theme paths

`theme.toml` accepts an Alacritty palette at the file root. Paste a `[colors.*]` block directly from an Alacritty theme, and Rimz lifts it into `theme.colors` before deriving semantic tones. Inline `[colors.*]` wins over `theme.scheme`; if no inline palette is present, `scheme` selects a bundled name or a file path.

```toml
[theme]
scheme = "TokyoNight Night"

[colors.primary]
background = "#1a1b26"
foreground = "#c0caf5"

[colors.normal]
red = "#f7768e"
green = "#9ece6a"
yellow = "#e0af68"
blue = "#7aa2f7"
magenta = "#bb9af7"
cyan = "#7dcfff"
```

`scheme` also accepts a path to an Alacritty TOML file, with `~` expanded. A changed `scheme` value applies on the next snapshot; loaded theme files are cached for the sidebar process lifetime, so edits inside one apply after a sidebar restart.

```toml
[theme]
scheme = "~/themes/rimz.toml"
```

## Color Depth

`[theme] mode` sets the palette depth. `auto` (default) uses truecolor when `COLORTERM` advertises it or the terminfo entry for `$TERM` declares direct color (`RGB`, `Tc`, or the `setrgbf`/`setrgbb` pair), so Ghostty, kitty, and WezTerm over SSH get RGB without forwarding `COLORTERM`; otherwise it quantizes the selected RGB tones to xterm 256 indexes. `truecolor` forces RGB for a terminal whose terminfo lacks the capability; `256` pins indexed output. Use [`glow = "always"`](#glow) separately when the same hop also under-advertises transition-flash support.

```toml
[theme]
mode = "auto"        # or "truecolor", "256"
```

## Palette Slots

Thirteen semantic slots cover everything the sidebar paints. Each accepts a palette role name (`background`, `foreground`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `bright_blue`), `#rrggbb` hex, or a raw 0–255 xterm index under `[theme]`; an omitted slot keeps the selected scheme's tone (the bundled `TokyoNight Night` palette by default). Role names resolve through the active raw palette, so `good = "green"` follows a pasted `[colors.normal] green` just like a bundled scheme.

The palette runs a loudness hierarchy — attention loudest, selection next, structure quiet, data coded, chrome quietest. Two rules keep it honest: `alarm` red marks danger (failures, removals, the full-context crest), and one warm `caution` amber means "hot/costly" everywhere — with the fresh-input `↘` burning a step redder than `alarm` itself, so the costliest read is the hottest marker on screen.

| slot | colors |
| --- | --- |
| `good` | calm/positive: running tallies, low gauges, `+` additions, the `◌` cache-read marker |
| `warn` | the caution floor: waiting `?` glyphs at rest, the low-mid gauge rung |
| `caution` | the warm amber "hot/costly" tier: the gauge mid-band and the age-heat midpoint (the fresh-input `↘` marker burns redder still — see `alarm`) |
| `alarm` | danger: failed `!` glyphs, the 100% gauge crest, `-` removals; the fresh-input `↘` marker derives a deeper red a step past it |
| `accent` | data: the `◎` sessions glyph and the `↗` output marker |
| `cool` | cool informational: the `plan` posture pill, the larger window tags, the `◇` token total, and the paused `⏸` glyph |
| `meta` | delegation and compaction accents: the `⇅ rc` flag, the subagent `⧉` marker, the live compacting head, and the cache-write `◍` marker |
| `body` | body content text: stat figures, capability tokens, subagent lines, and the worktree header |
| `muted` | muted chrome: labels, ages, subordinate values |
| `faint` | faintest chrome: bar tracks, `·` separators, dotted dividers |
| `rule` | the darkest chrome (the scrollbar track), a step below `faint` |
| `selection` | the selection tone: the selected card's bright `▌` spine and the dim `▎` lane bracket around its worktree |
| `selection_bg` | the selected card's background band, behind every line of the card; a subdued blend of the scheme's `colors.selection.background` toward the background, else a faint background tint. The band recesses below this tone — a fine step at truecolor, one xterm cell darker at 256-color — so the selected card reads as one recessed panel |

```toml
[theme]
good = "#a0d0a0"   # hex retunes a slot
warn = 173         # a raw xterm index stays exact at every depth
caution = "yellow" # palette role name tracks the active [colors] table
```

RGB values render as RGB under truecolor depth and quantize to the nearest xterm index under `mode = "256"` or a non-truecolor `auto`; raw indexes stay exact.

The context meter resolves a health tone from the `good` → `warn` → `caution` → `alarm` ramp — healthy green through gold and orange to alarm red — in OKLab, then emits the result at the active depth. The filled bar uses that single current tone. The provider budget ("mana") bar rides the same full ramp: it anchors green at a brimming window and warms continuously toward red as it drains, with the `[sidebar.budget]` zones as its warm stops. The recede-when-healthy readers — the card age clock, the reset-countdown burn pace, and the remote link badge — keep a quiet resting tone and ride only the warm tail of the ramp (`warn` → `caution` → `alarm`) once they leave their calm zone, since a fresh agent, a sustainable pace, or a healthy link reads quiet rather than healthy-green. RGB overrides and raw xterm indexes 16–255 participate in the ramp; ANSI indexes 0–15 are terminal-defined, so flat slot uses wear the override while the ramp keeps the scheme RGB for that slot. The waiting `?`, failed `!`, and paused `⏸` glyphs wear their flat `warn`, `alarm`, and `cool` slots, held steady at any age, so a slot override restyles them directly.

The context composition accents reuse stable sidebar tones: fresh input `↘` wears a deep red a step past the `alarm` stop — the costliest read in the breakdown and the reddest marker on screen, so it always reads hotter than the bar's scaled-to-red cache-read run (retune it through the `alarm` slot it derives from); cache-read `◌` wears green; output `↗` wears blue; cache-write `◍` wears the `meta` compaction/delegation violet; and the completed-compaction `↻` marker wears yellow. The `◇` token total uses `cool` blue so it stays distinct from those cost markers.

Money amounts use a fixed dollar green (`#85bb65` at truecolor depth, nearest xterm bucket at indexed depth), separate from the `good` success slot.

### How a tone resolves

A painted tone passes through three layers. The **raw palette** is the imported terminal color — `background`, `foreground`, the ANSI hues, and `colors.selection.background` from the selected scheme. The **thirteen semantic slots** above derive from it — the neutral chrome blended from background toward foreground in OKLab, the chromatic slots mapped from the ANSI hues, with `caution` warmed from yellow toward red into a vivid amber and `selection` lifted into its own bright cool tone over the dark `selection_bg` band — and are the layer you tune. **Component tokens** are the specific UI roles — the sessions glyph, a token marker, a worktree header, a transition flash — and each names its role and resolves to a semantic slot, or to a tone derived from the slots (the fresh-input `↘` marker deepens `alarm` a step past the ramp's red stop into the expense red). They are internal: restyle the room through the thirteen slots and every component that aliases or derives from them follows. The one place hue is decided stays the slot table, and a CI gate keeps render code from naming a raw color directly (see [rust-conventions.md](../contributing/rust-conventions.md#architectural-invariants)).

The capability line's window token shows this layering: it reads its size through a neutral→cool→accent salience ramp — `faint` below 128k, `muted` at 128k, `cool` at 258k, `accent` at 1M+ — so a bigger window reads louder while borrowing no provider's brand color. Only true external identity (the provider brand emblems and the dollar green) holds a fixed hue outside the slots.

### Subtle steps and color depth

A subtle tone step — a small lightness dim or a breathing lift — renders as color only at truecolor depth. The 256-color cube spaces its channel levels about forty apart, coarser than such a step, so quantizing a sub-cell shift either snaps to a distant cell (a hard jump in the wrong direction) or collides with the base tone (no change at all). At indexed depth a subtle adjustment falls back to the discrete signal the cube carries honestly — a weight modifier (`DIM` or `BOLD`) for an animated pulse, or the unmodified base tone for a static recession — the same shape `NO_COLOR` already uses.

The calm card name shows the rule: at truecolor it dims its brand lightness a step, and at 256-color it keeps the full brand while the selection bar and the description carry the calm cue. Tones that already differ by a full cube cell — the neutral ladder and the health ramp — stay color at every depth, since each step lands on its own index.

The selected band and the unread wash step the same rule a different way. At truecolor the band recesses a fine sub-cell step below `selection_bg` and the wash lifts a fine step above it, so the selected card sinks into a well set off from the lighter unread surface that rises over the card. Those steps are finer than a cube cell, so at 256-color, rather than collapse onto the panel, each is sized to cross one whole xterm cell: the band steps one cell darker, the wash one cell lighter, and `selection_bg`'s own cell sits between them. The near-background panel lands on the cube's fine 24-step gray ramp, where a single cell is a small, even step — so the three-way ordering (band below panel below wash) survives the quantization the breathing lift cannot. Only `NO_COLOR` drops the surfaces, leaving the bright `▌` spine and bold weight to carry the selection and the unread bold weight to carry the unread cue.

## Alacritty palette shape

Inline `[colors.*]` and custom `scheme` files use the same Alacritty TOML shape. A minimal valid palette:

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

Blends run in OKLab, so the derived tones stay perceptually even across themes. `[theme]` slot overrides win over any derived tone, so a near-miss theme needs only the one slot pinned.

Brightening runs in the same space and holds hue: a lift raises OKLab lightness and eases chroma toward the gamut boundary rather than letting the RGB channels hard-clamp, so an unread row brightens toward its crest — whether the blink toggles to it, `bright` holds it, or the shimmer beam sweeps across it — without drifting toward white or a neighboring hue.

## Animations

`[theme.animations]` themes the status heads the sidebar paints; what each head means in the room is in [the glyph legend](../interface/sidebar.md#reading-the-glyphs). The roles are `thinking`, `working`, `compacting`, `delegating`, `resolving`, `idle`, `success`, `paused`, `waiting`, and `failed`. Each role is optional, and each field inside a role is optional; an omitted field keeps the built-in value for that role, so a one-line `idle.effect = "breathe"` override leaves the idle glyph and neutral default tone alone. The static status heads (`idle`/`success`/`paused`/`waiting`/`attention`) follow `[theme.glyphs] set`, so `nerd_font` swaps each for its curated icon; the animated spinners (`thinking`/`working`/`delegating`/`resolving`) and the `compacting` wave keep their Unicode frames in every preset. Any explicit `frames` override wins.

```toml
[theme.animations.thinking]
frames = "⠁⠂⠄⡀⡈⡐⡠⣀⣁⣂⣄⣌⣔⣤⣥⣦⣮⣶⣷⣿⡿⠿⢟⠟⡛⠛⠫⢋⠋⠍⡉⠉⠑⠡⢁"
color = "clay"
effect = "static"
speed = "fast"

[theme.animations.idle]
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
| `paused` | `⏸︎` | `cool` | — |
| `waiting` | `?` | `warn` | — |
| `failed` | `!` | `alarm` | — |

`frames` accepts either a string or an array. A string splits into one frame per Unicode codepoint, which fits single-codepoint runs such as `"⠁⠂⠄⡀"`. An array keeps multi-codepoint single-cell glyphs intact, such as `["⏸︎"]`. Every frame must occupy exactly one terminal cell; empty frame lists, empty glyphs, zero-width glyphs, and multi-cell glyphs are rejected.

`color` accepts the semantic palette slots `good`, `warn`, `caution`, `alarm`, `accent`, `cool`, `meta`, `body`, `muted`, and `faint`, the brand tone `clay`, a `#rrggbb` hex color, or a raw 256-color index. Semantic slots retune through `[theme]`; hex values follow the active depth; raw indexes and `clay` pass through as explicit tones. `effect` is `static` or `breathe`; `speed` is `slow`, `normal`, or `fast` for both frame advance and effect cadence. For `waiting`, `failed`, and unread `success` row heads, an omitted `effect` keeps the [unread attention effect](#unread-attention) below, `effect = "static"` quiets it to a constant bold tone, and `speed` paces it. A literal frame blink is a frame sequence such as `frames = [" ", "!"]`.

Every role uses the same model: frames, color, effect, and speed. The unread attention signal carries across the lead glyph, the card name, the description, and the make-up `?`/`!`/`✓` buckets as one group — each holding its fixed tone until the pane is focused; the cockpit count and per-bucket counts use still representative frames.

### Unread attention

`[theme.animations] unread` picks how an unread attention row reads. The lead glyph, the card name, the description, and the cockpit `?`/`!`/`✓` make-up buckets all carry the choice as one group, so a row that needs you reads with one voice.

The continuous signal is reserved for the **one row that most needs you** — the oldest unread row that needs an answer (`waiting` or `failed`, the `␣` triage head). Only that lead row carries the chosen `shimmer` or `blink`, so a single pane is the only thing in motion; every other unread row, an unread `✓` result included, settles to the steady `bright` crest — unmistakable by contrast against the calm rows, but still. A transition flash already announced each row — an ask as it enters, a result as its turn lands — so the steady crest is the rest after the announcement, not a missed cue.

An unread card also grounds on a soft, uniform **wash** — one panel marking the row as unseen the way a mail inbox shades an unread line, with the row's status carried by its `?`/`!`/`✓` glyph. The wash is a lighter tint of the selection blue: the `selection_bg` panel lifted in lightness with its cool hue held, so it lands on the same cool-blue family the scheme derives for the selection band, one clear step brighter. A whole-card surface reads at a scanning glance where the one-cell glyph cannot, and it holds still, so continuous motion stays reserved to the lead row. The selected card keeps the attention through its bright `▌` spine and its recessed band, so the brighter unread fill is the "needs you" surface without ever reading as selection; the selection band stays the selected card's signature and wins when a card is both selected and unread. The wash holds across depths — a fine sub-cell tint above the panel at truecolor, one xterm cell lighter at 256-color (see [subtle steps and color depth](#subtle-steps-and-color-depth)) — and only `NO_COLOR` drops it to the weight-carried unread look.

| `unread` | the lead row reads as |
| --- | --- |
| `shimmer` (default) | a light beam flows left-to-right across the glyph, the name, and the description, each on its own sweep, the flow quickening with age |
| `bright` | the bright crest held constant — brighter and bold, no motion (every unread row reads this way) |
| `blink` | a hard 2-pole brightness toggle between the resting tone and the crest, the rate quickening with age |

`blink` and `bright` rise to the same gentle crest; the shimmer beam rides a brighter one, because it lights only the few cells under its moving center rather than the whole row at once, so a matching crest would read far fainter. All ride the same gamut-safe lift, holding hue as they brighten. A per-role `effect = "static"` on `waiting`, `failed`, or `success` wins over the `unread` choice, holding that role's row at a constant bold tone. At truecolor the effect rides an OKLab lightness lift; the 256-color cube and `NO_COLOR` carry it on weight — a held bold for `bright` and `blink`, a moving bold cell for `shimmer` — so the signal reads at every depth ([subtle steps and color depth](#subtle-steps-and-color-depth)).

```toml
[theme.animations]
unread = "shimmer"   # or "bright", "blink"
```

## Glyphs

`[theme.glyphs]` shapes the sidebar vocabulary, grouped the way the sidebar reads on screen. The [glyph legend](../interface/sidebar.md#reading-the-glyphs) stays the canonical meaning table; this section changes the shapes that carry those meanings. `rimz config init` writes both shipped sets as **active defaults** — `[theme.glyphs.unicode.*]` and `[theme.glyphs.nerd_font.*]` — so customizing is direct: paste a different glyph over any value and it takes effect on the next render. Each glyph must occupy exactly one terminal cell, or two when a trailing space pads a double-width icon.

Set a whole zone in one block. The cockpit make-up row is the `status` group:

```toml
[theme.glyphs.unicode.status]
waiting = "?"   attention = "!"   paused = "⏸︎"
done    = "✓"   working   = "⢿"   idle   = "○"
```

### Groups

The groups follow the on-screen reading order:

| group | controls |
| --- | --- |
| `status` | the leading status heads — `waiting`, `attention`, `paused`, `done`, `idle`, `working`, `thinking`, `delegating`, `resolving`, `compacting` |
| `cockpit` | `workspace`, `sessions`, `agents` |
| `tokens` | `total`, `input`, `output`, `cache_read`, `cache_write`, `filled`, `compaction` |
| `meter` | the drawn gauges: `context_full`/`context_empty`, the `bar_*`/`mana_*` fills, `scroll_*` |
| `clock` | the quarter-age faces `q1`…`q4`, `over` |
| `worktree` | `branch`, `ahead`, `behind`, `trunk_equal`, `trunk_clear`, `dotted` |
| `card` | `subagents`, `todo_done`, `todo_pending`, `parked_bg` |
| `process` | `cpu`, `mem`, `io` |
| `chrome` | `alert`, `remote_link`, `remote_control`, `infinity`, the `tab_cap_*`/`spine_*` framing, and `hairline` |

The `status` group sets the head **shapes**; their colour, effect, and speed stay in [`[theme.animations]`](#animations), and the built-in animation frames follow `[theme.glyphs] set`. Two `status` names read across to animation roles: `status.attention` is the animation role `failed`, and `status.done` is `success`. A single-frame head (`waiting`/`attention`/`paused`/`done`/`idle`) reads its `status` shape directly; the animated heads (`working`/`thinking`/`delegating`/`resolving`) cycle a Unicode spinner sequence in every preset, with the `status` glyph as the still representative the cockpit buckets show.

### The `set` selector

`set` chooses which inline table is active:

| `set` | source |
| --- | --- |
| unset or `unicode` | `[theme.glyphs.unicode.*]` |
| `nerd_font` | `[theme.glyphs.nerd_font.*]` |

Nerd Font mode assumes a Nerd Font v3+ face is active in the terminal. The drawn gauges — the context ratio bar, the provider budget (mana) bar, and the scrollbar — along with the `chrome` spines/caps/hairline, the `worktree.dotted` seal, and the `status.compacting` wave, carry their shape from the terminal grid more precisely than any icon, so they keep their box-drawing glyphs in every preset.

The age clock fills the `clock` quarter faces in Unicode, and an eighth-filling `circle_slice` series in `nerd_font`, so the icon tracks elapsed time twice as finely. Every glyph ships as a single cell, which aligns on the `Mono` Nerd Font builds — `JetBrainsMono Nerd Font Mono`, `FiraCode Nerd Font` — where each icon advances one column. A face that draws icons double-width, such as the non-`Mono` `Cascadia Code NF`, keeps its columns aligned by padding the alignment-sensitive glyphs with a trailing space in a per-glyph override under `[theme.glyphs.nerd_font.cockpit]` and `[theme.glyphs.nerd_font.chrome]`.

`rimz config set` validates named sets, known role names, and glyph width before it writes — for example `rimz config set theme.glyphs.unicode.status.waiting "?"` or `rimz config set theme.glyphs.set nerd_font`.

## Provider Styling

`[theme.providers.<kind>]` restyles a provider's dashboard block — display name, ASCII emblem, and brand color — over the built-in defaults (claude clay, codex blue, pi forest green).

```toml
[theme.providers.claude]
product_name = "Claude Code"
color = "#d97757"
ascii_art = "CLAUDE"
```

`color` accepts a palette role name, `#rrggbb`, or a raw 0-255 index; RGB and role values carry a quantized fallback for indexed renderers. Each field is optional, so a color override can leave the shipped art intact. Block selection and ordering stay in [configuration.md](./configuration.md#provider-dashboard); account and emblem resolution is in [internals/agents/provider.md](../internals/agents/provider.md).

## Glow

`[sidebar] glow` gates the post-render transition-flash tier layered over the base palette. The unread attention effect is part of base status-head rendering and follows ``theme.mode`` plus the `NO_COLOR` fallback. `auto` (default) follows the same `COLORTERM` or terminfo truecolor signal as palette depth; `always` forces transition flashes when a real truecolor terminal under-advertises, such as an SSH hop where terminfo also lacks the capability — pair it with `mode = "truecolor"` for RGB base tones; `never` keeps the plain render plus the base attention effect.

```toml
[sidebar]
glow = "auto"        # or "always", "never"
```

Glyph shapes carry every state, so `NO_COLOR` suppresses color while keeping shape and depth modifiers where they add signal; each status, meter, and marker still reads by shape.

## Changing Values

```sh
rimz config set theme "TokyoNight Night"
rimz config set theme.good '#a0d0a0'
rimz config set sidebar.glow always
rimz config set theme.glyphs.set nerd_font
rimz config set theme.glyphs.unicode.status.working '⢿'
rimz config set theme.glyphs.unicode.tokens.total '◇'
rimz config set theme.animations.unread shimmer
rimz config set theme.animations.idle.effect breathe
rimz config set theme.providers.codex.color 33
```

`rimz config set` validates before it writes: an unknown scheme is rejected with the bundled-catalog count and custom-file path hint, and a malformed color, Alacritty palette, glyph, or frame is rejected before the file changes. Loading stays lenient, so an older or stale scheme value falls back to the default `TokyoNight Night` scheme at render time instead of taking down the sidebar. The full `rimz config` surface is in [cli/maintenance.md](./cli/maintenance.md).
