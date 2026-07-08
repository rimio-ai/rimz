# Theming

One command restyles the sidebar:

```sh
rimz config set theme "Catppuccin Mocha"   # any bundled scheme; rimz list-themes shows them all
```

Theming covers the color scheme and depth, the glyph vocabulary, the status-head animations, provider branding, and an optional animated pet — everything in one per-machine file, `~/.config/rimz/theme.toml`. Every setting is a display preference: a theme changes what the sidebar paints, never what agents can do. [interface/sidebar.md](../interface/sidebar.md) says what every tone and glyph *means* on screen; this page is the knobs that restyle them.

Every element carries its state by shape first ([reading the glyphs](../interface/sidebar.md#reading-the-glyphs)), so color reinforces meaning rather than carrying it: any palette, including no color at all, stays readable.

## Changing a setting

Two ways in: `rimz config set` edits one dotted key, or open `~/.config/rimz/theme.toml` and edit it directly. This page explains the model and the choices that matter; the exhaustive key list, with every default and accepted value, is one command away:

```sh
rimz config set theme "Catppuccin Mocha"   # set one key
rimz config set theme.pets.enabled true    # any dotted key works, however deep
rimz config init --print                   # every key, its default, and accepted values
```

`rimz config set` validates before it writes, so a bad value never reaches the file: an unknown scheme is rejected with the catalog count and the custom-file hint, and a malformed color, palette, glyph, or frame is rejected with the reason. Loading stays lenient, so a stale value from an older version falls back to its default at render time rather than taking down the sidebar. The full `rimz config` surface is in [cli/maintenance.md](../reference/cli/maintenance.md).

Edits to `theme.toml` apply on the next refresh. The one exception is a custom scheme *file* edited in place: its palette is cached while the config that points at it is unchanged, so re-pick the scheme or restart the sidebar to see the edit.

## Style preset

`[theme] style` is the one-line headline that pairs a color depth with a glyph set. `modern` is truecolor plus the [Nerd Font glyphs](#glyphs); `default` is [auto color depth](#color-depth) plus the shipped Unicode glyphs. An explicit `[theme] mode` or `[theme.glyphs] set` overrides the matching half, so you can take the Nerd Font icons at `256` color or pin truecolor with Unicode glyphs.

```toml
[theme]
style = "modern"   # truecolor + Nerd Font; or "default" for auto color + Unicode
```

## Color scheme

`[theme] scheme` picks the palette; unset uses the bundled `TokyoNight Night`. Set a bundled theme name (`rimz list-themes` prints all of them; names with spaces need TOML quotes) or a path to an Alacritty TOML file (`~` expands).

```toml
[theme]
scheme = "Catppuccin Mocha"
# scheme = "~/themes/rimz.toml"
```

The bundled catalog is the Alacritty export of [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes), so a scheme name resolves to the same palette in every terminal and mux.

Zellij web rooms use the active scheme for the browser terminal when `[web.zellij] style_client` is true; see [Web CLI](../reference/cli/web.md).

To paste a palette inline instead, drop an Alacritty `[colors.*]` block at the root of `theme.toml`; an inline palette wins over `scheme`. The required keys are `colors.primary.background` / `foreground` and the six `colors.normal` hues; `colors.bright.blue` (the selection accent, falling back to `normal.blue`) and `colors.selection.background` (the selected-card band) are optional. A missing or malformed entry is named at load.

```toml
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

## Color depth

`[theme] mode` sets the palette depth: `auto` (default) emits truecolor when `COLORTERM` or the `$TERM` terminfo advertises direct color, otherwise quantizes the RGB tones to xterm 256 indexes; `truecolor` forces RGB; `256` pins indexed output. Inside a Rimz tmux room, Rimz stamps `COLORTERM=truecolor` at birth when the launcher advertises it, so `auto` resolves to truecolor despite tmux's `tmux-256color` default.

```toml
[theme]
mode = "auto"        # or "truecolor", "256"
```

`NO_COLOR` strips color entirely while keeping glyph shapes and weight modifiers, so every gauge, status, and marker still reads.

### Subtle steps and color depth

Some cues are subtle lightness shifts — a calm card name dimmed a touch, the recessed selection band, the unread *wash* (the soft background tint an unread row wears), a breathing pulse — and they render as color only at truecolor depth. At 256-color depth each falls back to a signal indexed color carries cleanly: a `DIM`/`BOLD` weight for motion, or the plain base tone for a static shift (the same shape `NO_COLOR` uses). Cues that already span a full color step — the neutral ladder, the health ramp, the one-cell selection and unread steps — stay color at every depth. The step magnitudes are tunable in [`[theme.display.highlight_steps]`](#display).

## Color slots

The scheme supplies the raw terminal palette: background, foreground, the six hues, and a selection accent. From it Rimz derives thirteen slots, the roles everything on screen wears, with the derived steps kept perceptually even so any scheme stays readable. The slots are the layer to tune: override one and every element that wears it follows, and the override survives a scheme switch.

Each slot under `[theme]` accepts a palette role name (`background`, `foreground`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `bright_blue`), a `#rrggbb` hex, or a raw 0–255 xterm index; an omitted slot keeps the scheme's tone. Role names resolve through the active palette, so `good = "green"` tracks a pasted `[colors.normal] green` just like a bundled scheme.

```toml
[theme]
good = "#a0d0a0"   # hex retunes a slot
warn = 173         # a raw xterm index stays exact at every depth
caution = "yellow" # a palette role tracks the active [colors] table
```

| slot | wears |
| --- | --- |
| `good` | calm/positive: running tallies, low gauges, `+` additions, the `◌` cache-read marker |
| `warn` | the caution floor: resting `?` waits, the low-mid gauge rung |
| `caution` | the warm "hot/costly" amber: the gauge mid-band and the age-heat midpoint |
| `alarm` | danger: failed `!`, the full-gauge crest, `-` removals (the fresh-input `↘` marker derives a deeper red one step past it) |
| `accent` | data: the `◎` sessions glyph and the `↗` output marker |
| `cool` | cool informational: the `plan` pill, large window tags, the `◇` token total, the paused `⏸` glyph |
| `meta` | delegation/compaction: the `⇅ rc` flag, the `⧉` subagent marker, the cache-write `◍` marker |
| `body` | body text: stat figures, capability tokens, subagent lines, worktree headers |
| `muted` | chrome: labels, ages, subordinate values |
| `faint` | faintest chrome: bar tracks, `·` separators, dotted dividers |
| `rule` | the darkest chrome (the scrollbar track), a step below `faint` |
| `selection` | the selected card's bright `▌` spine and the dim `▎` lane bracket |
| `selection_bg` | the selected card's recessed background band |

Two rules keep the palette honest: `alarm` red marks danger, and one warm `caution` amber means "hot/costly" everywhere. The four health slots form a ramp — `good → warn → caution → alarm`, green through gold and orange to red — that the live meters slide. The context meter, the remote link badge, and the draining provider budget bar ride the full ramp; readers that recede when healthy, such as the card age clock and the reset-countdown pace, rest quiet and ride only the warm tail once they leave their calm zone. An RGB override or an xterm index 16–255 joins the ramp; a flat ANSI index 0–15 is terminal-defined, so that slot wears your override while the ramp keeps the scheme's RGB. Money figures use a fixed dollar green outside the slots, as does each provider's [brand color](#provider-styling).

## Display

`[theme.display]` tunes the sidebar's render cadence, sizing, dashboard layout, and meter color stops.

| key | does |
| --- | --- |
| `refresh_ms` | the animation/paint grid in milliseconds (clamped internally); data polling keeps its own cadence |
| `max_cols` | creation-time cap on the sidebar pane width, so an ultra-wide terminal doesn't get an absurd split |
| `scrollbar` | `auto` shows the overflow indicator only while the view moves; `always` / `never` pin it |
| `card_density` | `auto` keeps the standard card; `expanded` shows every card's subagents; `compact` trims resting cards |
| `provider_tabs` | how the dashboard stacks vs. tabs provider blocks (`auto` / `always` / `never`) |
| `provider_list` | which providers appear and in what order; `"all"` expands the rest at that position |
| `max_provider_blocks` | cap on *stacked* blocks (a tabbed dashboard shows all) |

```toml
[theme.display]
refresh_ms = 100
max_cols = 72
scrollbar = "auto"
card_density = "auto"
provider_tabs = "auto"
```

Two nested tables set the meter color stops. The **context meter** (`[theme.display.context_meter]`) warms a card's context read from green: each stop names a fill percentage *and* an absolute token count, and severity is the worse of the two, so a large-window model calm by percentage still warms by sheer volume. The **budget bar** (`[theme.display.budget_bar]`) names the *remaining* budget percent at which the draining bar reaches each warm stop, and its nested `[theme.display.budget_bar.burn_rate]` colors the reset marker by pace (`100` = on-pace, `200` = twice as fast as the window can sustain). The shipped numbers are in the template.

```toml
[theme.display.context_meter]
amber = { percent = 75, tokens = 258000 }

[theme.display.budget_bar]
yellow = 50
amber = 25
red = 10
```

`[theme.display.highlight_steps]` sets the selected-band and unread-wash offsets from `selection_bg`, in units of 0.01 perceptual lightness. `band` recesses the selected card at truecolor depth, `wash` lifts the unread row at truecolor depth, and `indexed` is the 256-color one-cell step used darker for the band and lighter for the wash.

```toml
[theme.display.highlight_steps]
band = 5
wash = 1
indexed = 4
```

## Animations

`[theme.animations]` themes the status heads the sidebar paints (what each head means is in [the glyph legend](../interface/sidebar.md#reading-the-glyphs)). The roles are `thinking`, `working`, `compacting`, `delegating`, `resolving`, `idle`, `success`, `paused`, `waiting`, and `failed`. Each role takes four optional fields (`frames`, `color`, `effect`, `speed`), and an omitted field keeps the built-in, so a one-line override leaves the rest alone. The template lists every built-in head.

```toml
[theme.animations.thinking]
color = "clay"
speed = "fast"

[theme.animations.idle]
effect = "breathe"
```

- **`frames`** is a string (split into one frame per Unicode codepoint, e.g. `"⠁⠂⠄⡀"`) or an array (which keeps multi-codepoint single-cell glyphs intact, e.g. `["⏸︎"]`). Every frame must occupy exactly one cell.
- **`color`** accepts a semantic slot (`good`, `warn`, `caution`, `alarm`, `accent`, `cool`, `meta`, `body`, `muted`, `faint`), the brand tone `clay`, a `#rrggbb` hex, or a raw index.
- **`effect`** is `static` or `breathe`; **`speed`** is `slow`, `normal`, or `fast`, pacing both frame advance and effect.

The static heads (`idle` / `success` / `paused` / `waiting`) take their shape from [`[theme.glyphs] set`](#glyphs); the animated spinners keep their Unicode frames in every preset, and the cockpit buckets — the `? ! ⏸ ✓` counters at the top of the sidebar — show each head's still `status` glyph. A literal blink is just a frame sequence such as `frames = [" ", "!"]`.

### Unread attention

`[theme.animations] unread` picks how an unread attention row reads. The lead glyph, the card name, the description, and the cockpit `?`/`!`/`✓` buckets all carry the choice as one group, so a row that needs you reads with one voice.

```toml
[theme.animations]
unread = "shimmer"   # or "bright", "blink"
```

| `unread` | the lead row reads as |
| --- | --- |
| `shimmer` (default) | a light beam flows across the glyph, name, and description, quickening with age |
| `bright` | a constant bright, bold crest, no motion |
| `blink` | a hard 2-pole brightness toggle, quickening with age |

The continuous signal is reserved for the **one row that most needs you**, the oldest unanswered `waiting`/`failed`. Every other unread row, an unread `✓` included, settles to the steady `bright` crest, so a single pane is the only thing in motion. An unread card also grounds on a soft **wash** (the selection blue lifted a step, marking the row unseen the way a mail inbox shades an unread line), which holds still and survives across depths, dropping only under `NO_COLOR`. A per-role `effect = "static"` on `waiting`/`failed`/`success` overrides the `unread` choice and holds that row at a constant bold tone. How these cues degrade by depth is in [Subtle steps and color depth](#subtle-steps-and-color-depth).

## Glyphs

`[theme.glyphs]` shapes the sidebar's glyph vocabulary; the [glyph legend](../interface/sidebar.md#reading-the-glyphs) stays the canonical meaning table. `set` chooses the active preset, `unicode` (default) or `nerd_font`, and the matching inline tables overlay it. Glyphs are grouped by the sidebar's on-screen reading order, and the template lists every role in both shipped sets, so customizing is uncomment-and-edit. Each glyph must occupy exactly one cell, or two when a trailing space pads a double-width icon.

```toml
[theme.glyphs]
set = "nerd_font"

[theme.glyphs.unicode.tokens]
total = "◇"
```

| group | controls |
| --- | --- |
| `status` | the leading status heads |
| `cockpit` | `workspace`, `sessions`, `agents` |
| `tokens` | the token-accounting markers |
| `meter` | the drawn gauges and bars |
| `clock` | the last-activity age faces |
| `worktree` | the group header's git story: `branch`, `merge`, `ahead`, `behind`, `trunk_equal`, `trunk_branch`, `trunk_merge`, `pr_open`, `pr_closed`, `reconciling`, `dotted` |
| `card` | the agent card body |
| `process` | the CPU / mem / IO row |
| `keys` | help-overlay action leads |
| `chrome` | framing, spines, tabs, badges, and the help-box frame |

The `status` group sets head *shapes*; their color, effect, and speed stay in [`[theme.animations]`](#animations). Two names read across to animation roles: `status.attention` is the role `failed`, and `status.done` is `success`. The drawn gauges, the box-drawing chrome, the `worktree.dotted` seal, and the `compacting` wave keep their box-drawing glyphs in every preset, because the terminal grid draws them more precisely than any icon. Nerd Font mode assumes a Nerd Font v3+ face is active; on a non-`Mono` build that draws icons double-width, pad the alignment-sensitive glyphs with a trailing space.

## Provider styling

`[theme.providers.<kind>]` restyles a provider's dashboard block over the built-in defaults: display name, ASCII emblem, and brand color (Claude clay `#d97757`, Codex blue `#2fb1d1`, Pi green `#27a077`, Open Code orange `#ff8700`). Each field is optional, so a color override leaves the shipped art intact.

```toml
[theme.providers.claude]
product_name = "Claude"
color = "#d97757"
ascii_art = """
 ▐▛███▜▌
▝▜█████▛▘
  ▘▘ ▝▝
"""
```

`color` accepts a palette role, `#rrggbb`, or a raw index. Which blocks appear and in what order is a [Display](#display) and discovery setting (see [configuration.md → Provider dashboard](../reference/configuration.md#provider-dashboard)).

## Pets

Pets add a small animated companion to the provider dashboard. The pet follows the selected row's state, giving the bottom panel a little motion while the cards stay steady; `voice = false` keeps the animation and hides the captions.

```toml
[theme.pets]
enabled = true
pet = "rocky"
glyphs = "auto"
voice = true
```

| key | does |
| --- | --- |
| `enabled` | turns the dashboard pet on (off by default) |
| `pet` | which pet: a built-in id, an HTTPS URL, a local sheet path, or a petdex pet |
| `glyphs` | render tier: `auto`, `pixel`, or `sextant` |
| `voice` | canned captions on pet-action changes |

### Choosing a pet

`rimz list-pets` previews the built-ins and any petdex pets installed locally. The built-in ids are `codex`, `dewey`, `fireball`, `rocky`, `seedy`, `stacky`, `bsod`, and `null-signal`.

Bring-your-own sources use the same key:

- `pet = "https://example.com/my-pet.webp"` fetches an HTTPS WebP sheet and caches it.
- `pet = "~/art/my-pet.png"` reads a local WebP or PNG sheet.
- `pet = "wall-e"` selects a petdex pet installed under `~/.codex/pets/wall-e/`; `pet = "~/.codex/pets/wall-e/"` reads the same directory by path. Petdex manifests may point at WebP or PNG sheets.

### Crisp pixels vs cell art

`glyphs` controls the render tier. Crisp pixels need a Ghostty or kitty terminal; inside tmux they also need tmux 3.6 or newer with `allow-passthrough on` or `allow-passthrough all`. Zellij renders cell art.

| `glyphs` | renders |
| --- | --- |
| `auto` | pixels when the runtime is ready, then sextant cell art |
| `pixel` | opts past the terminal-name allowlist for newer kitty-compatible terminals, while hard gates such as tmux passthrough still apply |
| `sextant` | the most portable cell art |

On macOS, terminal graphics updates can make AppKit re-evaluate the pointer shape while kitty pixel pets animate; Rimz emits each sprite image once to minimize that traffic, and `glyphs = "sextant"` uses the same flicker-free cell-art path as Zellij when you want it fully gone.

### Offline and privacy

Built-in and URL sheets fetch once into the per-machine cache; `RIMZ_PETS_OFFLINE=1` serves the cache only. Petdex and local sheets read from disk and make no network request.

Pets run no commands. Asset loading sends only the configured asset request; prompts, transcripts, pane text, workspace paths, and provider credentials stay local.

Sheet geometry, the cache layout, and the pixel gates live in [pets.md](../internals/sidebar/pets.md).
