# Theming

Your terminal already runs a color scheme you picked. RimZ reads the same Alacritty scheme files, so the sidebar and every `rimz` command can wear the palette you already stare at all day:

```sh
rimz config set theme "Catppuccin Mocha"   # the palette; rimz list-themes prints every bundled name
rimz config set theme.style modern         # truecolor plus Nerd Font icons
rimz config set theme.pets.enabled true    # an animated companion on the provider dashboard
```

Those three cover what most people change. The rest of this page goes a level down: individual color roles, glyph shapes, status-head animations, the sidebar's sizing, and each provider's branding.

All of it lives in one per-machine file, `~/.rimz/theme.toml`. `rimz config set` writes that file in place and keeps your comments; `rimz config init --print` prints the commented template, which carries every key on this page with its shipped default ([configuration](./configuration.md#the-files-in-your-home-directory)). A running sidebar picks up an edit on its next refresh, a second or two later, with no restart and no reload; the sidebar's own width is the one exception, under [Display](#display). A file that will not parse falls back to built-in defaults: `rimz start` prints one line naming the file and the line that broke it, but a sidebar that is already running just snaps to the defaults without a word. When the column changes in a way you did not ask for, `rimz doctor` names the file and the line.

A theme changes what RimZ paints, never what an agent can do. Every element carries its state by shape first, so color reinforces the meaning rather than carrying it, and any palette stays readable, including no color at all. This page is the settings; [interface/sidebar.md](../interface/sidebar.md) is what each tone and glyph means on screen, and [the glyph legend](../interface/sidebar.md#reading-the-glyphs) is the meaning table.

## Color scheme

`theme.scheme` picks the palette. Unset, RimZ uses the bundled `TokyoNight Night`. The value is a bundled theme name or a path to an Alacritty TOML file, where `~` expands.

```toml
[theme]
scheme = "Catppuccin Mocha"
# scheme = "~/themes/rimz.toml"
```

The bundled catalog is the Alacritty export of [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes/tree/master/alacritty), so a name resolves to the same palette in every terminal and every multiplexer. `rimz list-themes` prints all of them with color chips ([reference](../reference/cli/config.md#list-themes)); a name with spaces needs TOML quotes.

Set it through the command rather than by hand when you can. `rimz config set theme "Catppuccin Mocha"` is shorthand for `theme.scheme`, and it refuses a name or path it cannot resolve. A hand edit naming a scheme that does not exist is ignored without a message, and the sidebar keeps the default.

Every human `rimz` command resolves the same scheme, slot overrides, provider overrides, and color depth as the sidebar. Machine-readable output stays plain: JSON, hook decisions, pane captures, scripting values, and streams carry no theme styling. Browser rooms use the active scheme while `[web] style_client` is true, which is the default ([configuration](./configuration.md#web-access)).

### Paste a palette inline

To use a palette RimZ does not bundle without saving it as a file, drop an Alacritty `[colors.*]` block at the root of `theme.toml`. An inline palette wins over `scheme`.

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

Those are the required keys: `colors.primary.background` and `foreground`, plus the six `colors.normal` hues. Two more are optional. `colors.bright.blue` is the selection accent, falling back to `normal.blue`; `colors.selection.background` is the selected-card band. A block with a key missing or a color RimZ cannot parse is ignored without a message and the scheme applies instead, so look at the sidebar after pasting one.

## Style preset

`theme.style` sets a color depth and a glyph set with one key. `modern` means truecolor plus the [Nerd Font glyphs](#glyphs); `default` means [automatic color depth](#color-depth) plus the shipped Unicode glyphs.

```toml
[theme]
style = "modern"   # or "default"
```

Either half can be set on its own instead. `theme.mode = "truecolor"` or `"256"` decides the depth whatever the preset says, and `theme.glyphs.set` decides the glyph set the same way, so you can take the Nerd Font icons at 256-color depth or pin truecolor and keep the Unicode glyphs. The one value that does not override is `mode = "auto"`, which means "let something else decide" and so leaves `modern` on truecolor.

`modern` expects a terminal that renders both halves, 24-bit color and a Nerd Font face. First-run setup probes the two separately and writes an explicit `theme.mode` or `theme.glyphs.set` only where the answer changes what you would otherwise get, so either half can degrade on its own. [Installation](./installation.md#truecolor-terminal-and-a-nerd-font-optional) lists the terminals and fonts that qualify.

## Color depth

`theme.mode` sets the palette depth.

| value | depth |
| --- | --- |
| `auto` (default) | truecolor when the terminal advertises direct color, otherwise the same tones quantized to xterm 256 indexes |
| `truecolor` | always RGB |
| `256` | always indexed |

RimZ reads the advertisement from `COLORTERM` (`truecolor` or `24bit`) or from the `$TERM` terminfo entry; Ghostty, kitty, WezTerm, iTerm2, and Alacritty all set one. Inside a RimZ tmux room, RimZ stamps `COLORTERM=truecolor` when the room starts if the launching terminal advertised it, so `auto` resolves to truecolor despite tmux's `tmux-256color` default.

`NO_COLOR`, set to any non-empty value, strips color while keeping glyph shapes and the bold and dim weights, so every gauge, status, and marker still reads.

### What a 256-color terminal loses

Four sidebar cues are lightness shifts finer than one step of the 256-color cube. At indexed depth each falls back to something the cube carries cleanly; every other cue keeps its color at any depth.

| cue | at 256 colors | under `NO_COLOR` |
| --- | --- | --- |
| a breathing pulse | the base tone with a `DIM` or `BOLD` weight | the weight alone |
| the unread shimmer or blink | the base tone, bold toggled | the bold toggle alone |
| the selected band and the unread wash | one cube cell either side of `selection_bg` | no fill |
| a calm card's dimmed name | the full brand color | the `body` tone |

The step sizes are [`[theme.display.highlight_steps]`](#highlight-steps).

## Color slots

A scheme gives RimZ a raw terminal palette: background, foreground, six hues, and a selection accent. From it RimZ derives thirteen slots, the roles everything on screen paints with. The slots are the layer to tune, because an override reaches every element that uses the slot and survives a scheme switch.

Each slot sits under `[theme]` and takes a palette role name (`background`, `foreground`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `bright_blue`), a `#rrggbb` hex, or a raw 0-255 xterm index. An omitted slot keeps the scheme's tone. A role name resolves through the active palette, so `good = "green"` tracks a pasted `[colors.normal] green` the same way it tracks a bundled scheme.

```toml
[theme]
good = "#a0d0a0"   # a hex retunes the slot
warn = 173         # a raw xterm index stays exact at every depth
caution = "yellow" # a palette role tracks the active [colors] table
```

| slot | what it paints |
| --- | --- |
| `good` | calm and positive: running tallies, low gauges, `+` additions, the `◌` cache-read marker |
| `warn` | the caution floor: resting `?` waits, the low-mid gauge rung |
| `caution` | the warm "hot or costly" amber: the gauge mid-band, the age-heat midpoint |
| `alarm` | danger: failed `!`, the full-gauge crest, `-` removals (the fresh-input `↘` marker is a deeper red one step past it) |
| `accent` | data: the `◎` sessions glyph, the `↗` output marker |
| `cool` | cool informational: large window tags, the `◇` token total, the paused `⏸` glyph |
| `meta` | delegation and compaction: the `⧉` subagent marker, the `◍` cache-write marker |
| `body` | body text: stat figures, subagent lines, worktree headers |
| `muted` | chrome: labels, ages, subordinate values |
| `faint` | faintest chrome: bar tracks, `·` separators, dotted dividers |
| `rule` | the darkest chrome, one step below `faint`: the scrollbar track |
| `selection` | the selected card's bright `▌` spine and the dim `▎` lane bracket |
| `selection_bg` | the selected card's recessed background band |

Two rules keep the palette honest: `alarm` red marks danger, and one warm `caution` amber means "hot or costly" everywhere.

The four health slots also form a ramp, `good → warn → caution → alarm`, green through gold and orange to red, and the live meters slide along it. The context meter, the remote link badge, and the draining provider budget bar ride the whole ramp. Gauges that should stay quiet while healthy ride only the warm tail: the card age clock rests neutral and warms once it leaves its calm zone, and the reset-countdown pace does the same, with a cool green tail of its own once spending runs well under pace.

An override joins the ramp when it is a hex color or an xterm index of 16 or above. A flat ANSI index of 0 to 15 is terminal-defined, so RimZ cannot know its RGB value: that slot wears your override while the ramp keeps the scheme's derived tone. Money figures sit outside the slots altogether, on a fixed dollar green (`#85bb65`), as does each provider's [brand color](#provider-styling).

## Glyphs

`[theme.glyphs]` shapes the sidebar's glyph vocabulary. `set` chooses the preset, `unicode` (the default) or `nerd_font`, and a `[theme.glyphs.<set>.<group>]` table overrides single glyphs on top of it. `rimz config set theme.glyphs nerd_font` is shorthand for `theme.glyphs.set`.

```toml
[theme.glyphs]
set = "nerd_font"

[theme.glyphs.unicode.tokens]
total = "◇"
```

Glyphs are grouped by where they appear as you read the sidebar down the column. The commented template shows both shipped sets group by group, so overriding one is uncomment-and-edit.

| group | what it covers |
| --- | --- |
| `status` | the leading status heads, one per state an agent can be in, plus the `⟲` repeated-tool marker |
| `cockpit` | the workspace, sessions, agents, and open-PR counters at the top |
| `tokens` | the token-accounting markers |
| `meter` | the drawn gauges, bars, and scrollbar |
| `clock` | the last-activity age faces |
| `value` | compact value qualifiers, such as the active-time `approx` marker |
| `worktree` | the group header's git story: branch and merge, ahead and behind, trunk state, PR and CI state |
| `pipeline` | a team's stage dots |
| `card` | the agent card body: subagents, waits, parked background |
| `process` | the CPU, memory, and IO row |
| `keys` | the help overlay's action leads |
| `chrome` | framing, spines, tabs, badges, and the help-box frame |

Each glyph must occupy one or two terminal cells; RimZ refuses anything empty or wider. Use the second cell for a trailing space when a Nerd Font icon draws double-width.

Some shapes are identical in both presets, because the terminal grid draws them more precisely than any icon: the drawn gauges, the box-drawing chrome, the `worktree.dotted` seal, the spinner frames, and the `compacting` wave. Zellij and tmux tab names sit outside the vocabulary entirely. The status suffix RimZ appends to a tab name is always the built-in Unicode `?`, `!`, `⏸`, `⢿`, or `✓`, whatever the glyph set and your overrides say.

Nerd Font mode assumes a Nerd Font v3 or later face is active. Install one from [Nerd Fonts](https://www.nerdfonts.com/font-downloads) or a Homebrew cask ([installation](./installation.md#truecolor-terminal-and-a-nerd-font-optional)) and select it as your terminal font. A non-`Mono` build draws its icons double-width; that is when a glyph needs the trailing space.

The `status` group sets head shapes only. Their color, effect, and speed live in [`[theme.animations]`](#animations), where two of the names differ: the glyph `status.attention` is the animation role `failed`, and `status.done` is `success`.

## Animations

`[theme.animations]` themes the status heads the sidebar paints; [the glyph legend](../interface/sidebar.md#reading-the-glyphs) says what each head means. There are eleven roles: `thinking`, `working`, `compacting`, `delegating`, `resolving`, `idle`, `success`, `paused`, `sleeping`, `waiting`, and `failed`.

Each role takes four optional fields, and an omitted field keeps the built-in, so a one-line override leaves the rest alone.

```toml
[theme.animations.thinking]
color = "clay"
speed = "fast"

[theme.animations.idle]
effect = "breathe"
```

- `frames` is a string, split into one frame per Unicode codepoint (`"⠁⠂⠄⡀"`), or an array, which keeps a multi-codepoint single-cell glyph intact (`["⏸︎"]`). Every frame must occupy exactly one cell.
- `color` takes a semantic slot (`good`, `warn`, `caution`, `alarm`, `accent`, `cool`, `meta`, `body`, `muted`, `faint`), a palette role name, the fixed brand tone `clay`, a `#rrggbb` hex, or a raw index.
- `effect` is `static` or `breathe`.
- `speed` is `slow`, `normal`, or `fast`, and paces both the frame advance and the effect.

The six resting heads (`idle`, `success`, `paused`, `sleeping`, `waiting`, `failed`) take their shape from [`[theme.glyphs] set`](#glyphs); the five spinners keep their Unicode frames in every preset. The cockpit's `? ! ⏸ ✓` counters show each head's still glyph. A literal blink is just a frame sequence, `frames = [" ", "!"]`.

### Unread attention

`theme.animations.unread` picks the signal an unread attention row carries. The lead glyph, the card name, the description, and the cockpit `?`, `!`, and `✓` buckets all take the same choice, so a row that needs you reads with one voice.

```toml
[theme.animations]
unread = "shimmer"   # or "bright", "blink"
```

| value | the lead row reads as |
| --- | --- |
| `shimmer` (default) | a light beam flowing across the glyph, name, and description, quickening with age |
| `bright` | a constant bright, bold crest, no motion |
| `blink` | a hard two-pole brightness toggle, quickening with age |

The moving signal is reserved for the one row that most needs you, the oldest unanswered `waiting` or `failed`. Every other unread row, an unread `✓` included, settles to the steady `bright` crest, so a single pane is the only thing moving.

An unread card also rests on a soft wash, the selection blue lifted a step, the way a mail inbox shades an unread line. The wash never moves and it survives every color depth, dropping only under `NO_COLOR`. Setting `effect = "static"` on `waiting`, `failed`, or `success` overrides the `unread` choice for that role and holds the row at a constant bold tone.

## Display

`[theme.display]` tunes the sidebar's sizing, render cadence, dashboard layout, and card detail.

| key | default | what it does |
| --- | --- | --- |
| `refresh_ms` | `100` | the animation and paint grid in milliseconds, clamped to 16 through 1000. Data polling keeps its own cadence |
| `pixel` | `auto` | `auto` lets kitty-graphics pets and context meters paint where the terminal supports them; `off` keeps both drawn out of text cells |
| `width_percent` | unset | a fixed sidebar share of each view, clamped to 10 through 90. Unset means 30% on views wider than 240 columns or whenever pets are on, and 25% otherwise |
| `max_cols` | `72` | column cap on the sidebar's share |
| `scrollbar` | `auto` | `auto` shows the overflow indicator only while the view moves; `always` and `never` pin it |
| `card_density` | `auto` | `auto` keeps the standard card; `expanded` opens every card's subagents and waits; `compact` trims resting cards |
| `recent_subagent_secs` | `900` | how long, in seconds, a finished subagent stays in a card's recent list before it moves behind `+K older`. `0` moves every finished child there at once, though a card with nothing newer to show lists them directly |
| `max_recent_subagents` | `5` | how many finished subagents the recent list holds, newest first. Running children are never capped |
| `provider_tabs` | `auto` | how the dashboard picks between stacked provider blocks and a tab rail: `auto` tabs at three providers or more, `always` at more than one, `never` not at all |
| `provider_list` | unset | which providers appear and in what order. Unset ranks them by usage: running first, then recently used, then recently logged in. A list is a strict allowlist unless it contains `"all"`, which expands the rest in usage order at that position |
| `max_provider_blocks` | `3` | how many blocks a stacked dashboard shows before eliding the rest. A tab rail and an explicit `provider_list` both ignore it |

```toml
[theme.display]
width_percent = 30        # a fixed share; omit for the width-keyed default
card_density = "compact"
provider_list = ["claude", "codex", "all"]
```

Width is the one setting here a running sidebar does not pick up. `width_percent` and `max_cols` are read when a sidebar starts, so a live room keeps its current width until the sidebar restarts. Inside the room, `a` and `d` resize it immediately and outrank both keys for the life of the session ([sidebar](./sidebar.md#setting-the-width)).

The context meter paints a pixel-precise stripe when `pixel = "auto"`, truecolor is active, and kitty graphics reaches a kitty or Ghostty client, either directly or through tmux 3.6 or later with `allow-passthrough` on. Zellij and every other path draw the half-cell bar instead, as does `NO_COLOR`. Setting `pixel = "off"` opts out of both the pixel meter and pixel pets.

### Meter color stops

Three nested tables move the stops the sidebar's meters slide through. Every key is optional; the values below are the shipped ones.

`[theme.display.context_meter]` warms a card's context read out of green. Each of `green`, `yellow`, `amber`, and `red` names a fill percentage *and* an absolute token count, and the meter takes the worse of the two, so a large-window model that is calm by percentage still warms on sheer volume. Separately, the drawn fill is log-warped so the working range keeps its visual space instead of bunching at the left edge. How hard depends on the model's window: up to 256k the fill stays linear, and the curve reaches full strength at 1M. `log_scale = false` turns the warp off. Either way the percentage on screen and the color stops read raw usage.

```toml
[theme.display.context_meter]
log_scale = true
green = { percent = 50, tokens = 128000 }
yellow = { percent = 70, tokens = 192000 }
amber = { percent = 80, tokens = 256000 }
red = { percent = 90, tokens = 384000 }
```

`[theme.display.budget_bar]` names the *remaining* budget percent at which the draining provider bar reaches each warm stop. Its nested `[theme.display.budget_bar.burn_rate]` colors the reset marker instead, by how fast you are spending against how much of the window has passed. The values are percentages of even pace: `100` is on-pace, `200` is twice as fast as the window can sustain. Below `green` the marker starts cooling, and it saturates at `deep_green` once enough of the window has elapsed.

```toml
[theme.display.budget_bar]
yellow = 50
amber = 25
red = 10

[theme.display.budget_bar.burn_rate]
green = 67
deep_green = 33
yellow = 100
amber = 150
red = 200
```

### Highlight steps

`[theme.display.highlight_steps]` sets how far the selected band and the unread wash sit from `selection_bg`, in units of 0.01 perceptual lightness. `band` recesses the selected card and `wash` lifts the unread row, both at truecolor depth; `indexed` is the single 256-color cube step taken darker for the band and lighter for the wash.

```toml
[theme.display.highlight_steps]
band = 5
wash = 1
indexed = 4
```

## Provider styling

`[theme.providers.<kind>]` restyles one provider's dashboard block over the built-in defaults: its display name, ASCII emblem, and brand color (Claude clay `#d97757`, Codex blue `#2fb1d1`, Pi green `#27a077`, Open Code orange `#ff8700`).

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

`color` takes a palette role, a `#rrggbb` hex, or a raw index. Every field is optional, so a color override leaves the shipped art and its built-in tints intact; an `ascii_art` override paints your replacement in the single brand color. A provider RimZ ships art for uses its own emblem, and every other kind gets the shared fallback.

Which blocks appear and in what order is a [Display](#display) setting; how RimZ finds the providers to show is in [configuration → Provider dashboard](./configuration.md#provider-dashboard).

## Pets

Pets put a small animated companion on the provider dashboard.

```toml
[theme.pets]
enabled = true
pet = "rocky"
```

Turning them on also widens the automatic sidebar share to 30% at any view width. The [pets guide](./pets.md) covers the rest: what the pet acts out, the built-in and [petdex.dev](https://petdex.dev/) catalogs, bringing your own sprite sheet, the pixel and cell-art render tiers, and the offline and privacy story.

## See also

- [The sidebar on screen](../interface/sidebar.md): what every tone, glyph, bar, and animation on this page means when you see it.
- [Pets](./pets.md): the dashboard companion, its catalogs, and its render tiers.
- [Configuration](./configuration.md): the rest of `~/.rimz/`, and the sidebar settings that change what it does rather than how it looks.
- [Installation](./installation.md#truecolor-terminal-and-a-nerd-font-optional): terminals that carry truecolor and fonts that carry the Nerd Font icons.
