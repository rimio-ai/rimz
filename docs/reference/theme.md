# Theming

> [interface/sidebar.md](../interface/sidebar.md) says what every tone and glyph *means on screen*; this page is the knobs that restyle them, and `rimz config init --print` is the full annotated key list.

Theming restyles the sidebar — the color scheme, color depth, glyph vocabulary, status-head animations, and provider branding — from one per-machine file, `~/.config/rimz/theme.toml`. Every element carries its state by *shape* first ([reading the glyphs](../interface/sidebar.md#reading-the-glyphs)), so color reinforces meaning rather than carrying it, and any palette — including no color at all — stays readable. Theme settings are personal display preferences: they tune what the renderer paints, never ledger correctness, and stay outside the project trust hash.

```sh
rimz config set theme "Catppuccin Mocha"     # pick a scheme
rimz config init --print                      # every key, its default, and accepted values
```

`rimz config init --print` emits the fully-commented template — the authoritative list of every knob and default. This page explains the *model* and the choices that matter; reach for the template when you want the exhaustive key reference. `rimz config set` validates a value before it touches the file (see [Setting values](#setting-values)).

## How a tone resolves

A painted tone passes through three layers, and tuning happens in the middle one.

1. **The scheme** supplies the raw terminal palette — background, foreground, the six ANSI hues, and a selection accent — in [Alacritty's TOML shape](#schemes). This is the only place scheme color enters.
2. **Thirteen semantic slots** derive from that palette in OKLab, so the steps stay perceptually even across schemes: the ANSI hues map to chromatic slots, the neutral chrome blends background toward foreground, and selection lifts into its own cool tone. **This is the layer you tune** — override a slot and every element that uses it follows.
3. **Component tokens** are the specific UI roles — the sessions glyph, a token marker, a transition flash. Each names its role and resolves to a semantic slot (or a slot-derived runtime ramp), so the call site states intent while hue stays one central decision.

Render code always resolves a tone through a slot or a slot-derived value — never a raw terminal color — and a CI gate enforces it ([UI-color provenance](../contributing/rust-conventions.md#architectural-invariants)). The slot table is the one place hue is decided, so retuning the room means retuning slots. The derivation math lives in the renderer (`crates/rimz/src/sidebar_pane/render/theme/`); the rest of this page is the knobs.

## Style preset

`[theme] style` is the one-line headline that pairs a color depth with a glyph set. `modern` is truecolor plus the [Nerd Font glyphs](#glyphs); `default` is [auto color depth](#color-depth) plus the shipped Unicode glyphs. An explicit `[theme] mode` or `[theme.glyphs] set` overrides the matching half, so you can take the Nerd Font icons at `256` color or pin truecolor with Unicode glyphs.

```toml
[theme]
style = "modern"   # truecolor + Nerd Font; or "default" for auto color + Unicode
```

## Schemes

`[theme] scheme` picks the palette source; unset uses the bundled `TokyoNight Night`. Set a bundled Alacritty theme name (`rimz list-themes` prints all of them) or a path to an Alacritty TOML file (`~` expands).

```toml
[theme]
scheme = "Catppuccin Mocha"
# scheme = "~/themes/rimz.toml"
```

Rimz embeds the Alacritty export from [iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes) — the [bundled catalog](../../crates/rimz/themes/alacritty) is refreshed with `cargo xtask theme-refresh`, with provenance and license in [themes/README.md](../../crates/rimz/themes/README.md) and [themes/LICENSE](../../crates/rimz/themes/LICENSE) — so a name resolves the same across terminals and muxes. Names with spaces need TOML quotes.

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

A changed `scheme` applies on the next snapshot; an edited theme *file* applies after a sidebar restart, since loaded files are cached for the process lifetime.

## Color depth

`[theme] mode` sets the palette depth: `auto` (default) emits truecolor when `COLORTERM` or the `$TERM` terminfo advertises direct color, otherwise quantizes the RGB tones to xterm 256 indexes; `truecolor` forces RGB; `256` pins indexed output. Inside a Rimz tmux room, Rimz stamps `COLORTERM=truecolor` at birth when the launcher advertises it, so `auto` resolves to truecolor despite tmux's `tmux-256color` default.

```toml
[theme]
mode = "auto"        # or "truecolor", "256"
```

`NO_COLOR` strips color entirely while keeping glyph shapes and weight modifiers, so every gauge, status, and marker still reads.

### Subtle steps and color depth

Some cues are a sub-cell lightness step — a calm card name dimmed a touch, the selected band recessed below its panel, the unread wash lifted above it, a breathing pulse. These render as color only at truecolor depth. The 256-color cube spaces its levels too coarsely to carry a sub-cell shift, so at indexed depth a subtle cue falls back to the discrete signal the cube carries honestly: a weight modifier (`DIM`/`BOLD`) for motion, or the plain base tone for a static recession — the same shape `NO_COLOR` uses. Cues that already span a full cube cell — the neutral ladder, the health ramp, the one-cell selection and unread steps — stay color at every depth. The precise steps are tuned constants in the renderer's `theme.rs`.

## Palette slots

The thirteen slots under `[theme]` cover everything the sidebar paints. Each accepts a palette role name (`background`, `foreground`, `red`, `green`, `yellow`, `blue`, `magenta`, `cyan`, `bright_blue`), a `#rrggbb` hex, or a raw 0–255 xterm index; an omitted slot keeps the scheme's tone. Role names resolve through the active palette, so `good = "green"` tracks a pasted `[colors.normal] green` just like a bundled scheme.

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

Two rules keep the palette honest: `alarm` red marks danger, and one warm `caution` amber means "hot/costly" everywhere. The four health slots form a ramp — `good → warn → caution → alarm`, green through gold and orange to red — that the live meters slide in OKLab: the context meter and the draining provider budget ("mana") bar ride the full ramp, while the recede-when-healthy readers (the card age clock, the reset-countdown pace, the remote link badge) rest quiet and ride only the warm tail once they leave their calm zone. RGB overrides and xterm indexes 16–255 join the ramp; flat ANSI indexes 0–15 are terminal-defined, so a flat slot wears the override while the ramp keeps the scheme's RGB. Money figures use a fixed dollar green outside the slots, as does each provider's [brand color](#provider-styling).

## Display

`[theme.display]` tunes the sidebar's render cadence, sizing, dashboard layout, and meter color stops.

| key | does |
| --- | --- |
| `refresh_ms` | the animation/paint grid in milliseconds (clamped internally); data polling stays on `--tick-seconds` |
| `max_cols` | creation-time cap on the sidebar pane width, so an ultra-wide terminal doesn't get an absurd split |
| `scrollbar` | `auto` shows the overflow indicator only while the view moves; `always` / `never` pin it |
| `glow` | the transition-flash tier — see [Glow](#glow) |
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

Two nested tables set the meter color stops. The **context meter** (`[theme.display.context_meter]`) warms a card's context read from green; each stop names a fill percentage *and* an absolute token count, and severity is the worse of the two, so a large-window model calm by percentage still warms by sheer volume. The **budget bar** (`[theme.display.budget_bar]`) names the *remaining* budget percent at which the draining bar reaches each warm stop, and its nested `[theme.display.budget_bar.burn_rate]` colors the reset marker by pace (`100` = on-pace, `200` = twice as fast as the window can sustain). The shipped numbers are in the template.

```toml
[theme.display.context_meter]
amber = { percent = 75, tokens = 258000 }

[theme.display.budget_bar]
yellow = 50
amber = 25
red = 10
```

## Animations

`[theme.animations]` themes the status heads the sidebar paints (what each head means is in [the glyph legend](../interface/sidebar.md#reading-the-glyphs)). The roles are `thinking`, `working`, `compacting`, `delegating`, `resolving`, `idle`, `success`, `paused`, `waiting`, and `failed`. Each role takes four optional fields — `frames`, `color`, `effect`, `speed` — and an omitted field keeps the built-in, so a one-line override leaves the rest alone. The template lists every built-in head.

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

The static heads (`idle` / `success` / `paused` / `waiting`) take their shape from [`[theme.glyphs] set`](#glyphs); the animated spinners keep their Unicode frames in every preset, with the `status` glyph as the still representative the cockpit buckets show. A literal blink is just a frame sequence such as `frames = [" ", "!"]`.

### Unread attention

`[theme.animations] unread` picks how an unread attention row reads. The lead glyph, the card name, the description, and the cockpit `?`/`!`/`✓` buckets all carry the choice as one group, so a row that needs you reads with one voice.

```toml
[theme.animations]
unread = "shimmer"   # or "bright", "blink"
```

| `unread` | the lead row reads as |
| --- | --- |
| `shimmer` (default) | a light beam flows across the glyph, name, and description, quickening with age |
| `bright` | a constant bright, bold crest — no motion |
| `blink` | a hard 2-pole brightness toggle, quickening with age |

The continuous signal is reserved for the **one row that most needs you** — the oldest unanswered `waiting`/`failed`. Every other unread row, an unread `✓` included, settles to the steady `bright` crest, so a single pane is the only thing in motion. An unread card also grounds on a soft **wash** — the selection blue lifted a step, marking the row unseen the way a mail inbox shades an unread line — which holds still and survives across depths, dropping only under `NO_COLOR`. A per-role `effect = "static"` on `waiting`/`failed`/`success` overrides the `unread` choice and holds that row at a constant bold tone. How these cues degrade by depth is in [Subtle steps and color depth](#subtle-steps-and-color-depth).

## Glyphs

`[theme.glyphs]` shapes the sidebar's glyph vocabulary; the [glyph legend](../interface/sidebar.md#reading-the-glyphs) stays the canonical meaning table. `set` chooses the active preset — `unicode` (default) or `nerd_font` — and the matching inline tables overlay it. Glyphs are grouped by the sidebar's on-screen reading order, and `rimz config init --print` lists every role in both shipped sets, so customizing is uncomment-and-edit. Each glyph must occupy exactly one cell, or two when a trailing space pads a double-width icon.

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

`[theme.providers.<kind>]` restyles a provider's dashboard block — display name, ASCII emblem, and brand color — over the built-in defaults (Claude clay `#d97757`, Codex blue `#2fb1d1`, Pi green `#27a077`, Open Code orange `#ff8700`). Each field is optional, so a color override leaves the shipped art intact.

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

`color` accepts a palette role, `#rrggbb`, or a raw index. Which blocks appear and in what order is a [Display](#display) and discovery setting (see [configuration.md → Provider dashboard](./configuration.md#provider-dashboard)); account and emblem resolution is in [provider.md](../internals/agents/provider.md).

## Glow

`[theme.display] glow` gates the post-render transition-flash tier layered over the base palette. `auto` (default) follows the same truecolor signal as palette depth; `always` forces the flashes when a real truecolor terminal under-advertises (pair it with `mode = "truecolor"` for RGB base tones); `never` keeps the plain render. The base attention effect — the unread blink or shimmer — is part of status-head rendering and follows `mode` plus `NO_COLOR`, independent of `glow`.

```toml
[theme.display]
glow = "auto"        # or "always", "never"
```

## Setting values

`rimz config set` validates before it writes, so a bad value never reaches the file:

```sh
rimz config set theme "TokyoNight Night"
rimz config set theme.good '#a0d0a0'
rimz config set theme.display.glow always
rimz config set theme.glyphs.set nerd_font
rimz config set theme.glyphs.unicode.tokens.total '◇'
rimz config set theme.animations.unread shimmer
rimz config set theme.providers.codex.color 33
```

An unknown scheme is rejected with the catalog count and the custom-file hint; a malformed color, palette, glyph, or frame is rejected with the reason. Loading stays lenient, so a stale or older value falls back to the default `TokyoNight Night` scheme at render time rather than taking down the sidebar. The full `rimz config` surface is in [cli/maintenance.md](./cli/maintenance.md).
