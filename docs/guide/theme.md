# Theming

The sidebar wears a theme: the color scheme and depth, the glyph vocabulary, the status-head animations, provider branding, and an optional animated pet. All of it is display preference in one per-machine file, `~/.config/rimz/theme.toml`. A theme changes what the sidebar paints, never what an agent can do, so nothing on this page can break a run.

RimZ reads color schemes from [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes/tree/master/alacritty), so a bundled name or the palette you already run in your terminal is one line away:

```sh
rimz config set theme "Catppuccin Mocha"   # any bundled scheme; rimz list-themes shows them all
```

Every element carries its state by shape first (the [glyph legend](../interface/sidebar.md#reading-the-glyphs) is the meaning table), so color reinforces meaning rather than carrying it, and any palette stays readable, including no color at all. This page is the knobs; [interface/sidebar.md](../interface/sidebar.md) is what every tone and glyph *means* on screen.

## Common changes

Three settings cover what most people touch. Each is a dotted key that `rimz config set` writes straight into `theme.toml`:

```sh
rimz config set theme.style modern         # truecolor + Nerd Font glyphs; "default" = auto color + Unicode
rimz config set theme.pets.enabled true    # an animated companion on the provider dashboard
```

A [scheme](#color-scheme) restyles the whole column, the [style preset](#style-preset) is the one-line headline that pairs a color depth with a glyph set, and a [pet](#pets) adds motion to the provider dashboard. Everything else on this page tunes a detail beneath these three.

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

`[theme] mode` sets the palette depth: `auto` (default) emits truecolor when `COLORTERM` or the `$TERM` terminfo advertises direct color, otherwise quantizes the RGB tones to xterm 256 indexes; `truecolor` forces RGB; `256` pins indexed output. Inside a RimZ tmux room, RimZ stamps `COLORTERM=truecolor` at birth when the launcher advertises it, so `auto` resolves to truecolor despite tmux's `tmux-256color` default.

```toml
[theme]
mode = "auto"        # or "truecolor", "256"
```

`NO_COLOR` strips color entirely while keeping glyph shapes and weight modifiers, so every gauge, status, and marker still reads.

### Subtle steps and color depth

Some cues are subtle lightness shifts — a calm card name dimmed a touch, the recessed selection band, the unread *wash* (the soft background tint an unread row wears), a breathing pulse — and they render as color only at truecolor depth. At 256-color depth each falls back to a signal indexed color carries cleanly: a `DIM`/`BOLD` weight for motion, or the plain base tone for a static shift (the same shape `NO_COLOR` uses). Cues that already span a full color step — the neutral ladder, the health ramp, the one-cell selection and unread steps — stay color at every depth. The step magnitudes are tunable in [`[theme.display.highlight_steps]`](#display).

## Color slots

The scheme supplies the raw terminal palette: background, foreground, the six hues, and a selection accent. From it RimZ derives thirteen slots, the roles everything on screen wears, with the derived steps kept perceptually even so any scheme stays readable. The slots are the layer to tune: override one and every element that wears it follows, and the override survives a scheme switch.

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
| `max_cols` | cap on sidebar birth width when no room-runtime width override is selected; `a`/`d` selections may exceed it |
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

Two nested tables set the meter color stops. The **context meter** (`[theme.display.context_meter]`) warms a card's context read from green: each stop names a fill percentage *and* an absolute token count, and severity is the worse of the two, so a large-window model calm by percentage still warms by sheer volume. The **budget bar** (`[theme.display.budget_bar]`) names the *remaining* budget percent at which the draining bar reaches each warm stop, and its nested `[theme.display.budget_bar.burn_rate]` colors the reset marker by pace (`100` = on-pace, `200` = twice as fast as the window can sustain); `green = 67` starts the cool under-pace tail and `deep_green = 33` saturates it once enough of the window has elapsed. The shipped numbers are in the template.

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

`color` accepts a palette role, `#rrggbb`, or a raw index. Which blocks appear and in what order is a [Display](#display) and discovery setting (see [configuration.md → Provider dashboard](./configuration.md#provider-dashboard)).

## Pets

Pets add a small animated companion to the provider dashboard, configured through `[theme.pets]`:

```toml
[theme.pets]
enabled = true
pet = "rocky"
```

The [pets guide](./pets.md) covers the rest: what the pet acts out, the built-in and [petdex.dev](https://petdex.dev/) catalogs, bring-your-own sprite sheets, the pixel and cell-art render tiers, and the offline and privacy story.
