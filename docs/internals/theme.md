# The theme core

The theme core turns the machine's `[theme]` config into the vocabulary every human surface paints with: color tones, glyphs, provider identity, and value formats. Three renderers paint with it: the CLI's `anstyle` output, the sidebar's ratatui frames, and the ratatui picker of `rimz sessions`. All three read the same resolved vocabulary, so a scheme or override change reaches them together.

The core lives in [`crates/rimz/src/theme/`](../../crates/rimz/src/theme/) and is renderer-neutral. It names every color as a `Tone`, and each renderer converts that `Tone` to its own terminal color type at the last moment, so the core depends on neither ratatui nor `anstyle`. Its output is display preference only: a theme changes what a frame looks like, never what an agent can do.

The [theming guide](../guide/theme.md) says what each knob does on screen, and [interface/sidebar.md](../interface/sidebar.md) says what each tone and glyph means. This page covers how resolution works and where to change it.

## The four layers

Color flows one way through four layers, and each layer holds one decision.

```text
Layer 1  Raw          theme/raw.rs              the scheme's terminal colors, verbatim
   │                                            background, foreground, six ANSI hues,
   │                                            bright blue, selection background
   ▼
Layer 2  Semantic     theme/palette.rs          thirteen named slots, two ramps, the derived tones
   │                                            good warn caution alarm accent cool meta
   │                                            body muted faint rule selection selection_bg
   ▼
Layer 3  Component    sidebar_pane/render/      one variant per fixed sidebar UI role
   │                  theme/component.rs        Sessions, TokenTotal, WorktreePrOpen, ...
   ▼
Layer 4  Carrier      cli/render/palette.rs     Tone → anstyle::Color
                      sidebar_pane/render/      Tone → ratatui::Color
                      theme.rs
                      cli/sessions/picker.rs    Tone → ratatui::Color
```

Layers 1 and 2 are the shared core and never name a terminal color type. Layer 3 exists only in the sidebar, which has enough distinct fixed roles to need names for them, and `Component::resolve` returns a ratatui `Color` through the Layer 4 conversion. The CLI's `anstyle` output has no Layer 3: it reads Layer 2 through the accessors in `cli/render/palette.rs` and a typed state mapping. The `rimz sessions` picker resolves its own `Palette` in `crates/rimz/src/cli/sessions/picker.rs::PickerTheme` and converts with a private `tone_color`. Outside the Layer 3 and Layer 4 modules, sidebar render code passes resolved colors around and writes no color literal except `Color::Reset` (see [Boundaries](#boundaries)).

Each kind of change lands in one layer. A scheme switch replaces Layer 1, and a slot override replaces one Layer 2 slot, so every call site follows without edits. A new sidebar element names a Layer 3 component instead of picking a color. Two fixed sidebar tones sit outside the layers and ignore the scheme: `CHIP_INK`, the near-black ink on colored chips, and `VALUE_FLASH_INK`, the money settle flash, both constants in `sidebar_pane/render/theme.rs`.

## Module map

| module | job |
| --- | --- |
| [`mod.rs`](../../crates/rimz/src/theme/mod.rs) | The public surface the CLI and sidebar import. |
| [`raw.rs`](../../crates/rimz/src/theme/raw.rs) | `RawPalette`, the imported scheme colors, and `derive_tones`, the one place raw hues gain semantic meaning. `RawPalette::DEFAULT` holds the default scheme's colors. |
| [`palette.rs`](../../crates/rimz/src/theme/palette.rs) | `Palette::resolve`: raw palette selection, slot overrides, depth quantization, the heat and calm ramps, the expense, fast-mode, and cache-expired tones, and `ramp_tone`. |
| [`tone.rs`](../../crates/rimz/src/theme/tone.rs) | `Tone`, a resolved `Rgb` or `Indexed` color awaiting a renderer. |
| [`oklab.rs`](../../crates/rimz/src/theme/oklab.rs) | Perceptual color math: `blend`, `lift_lightness`, `warm_toward`, and the gamut fit, plus WCAG contrast from linearized sRGB. |
| [`glyphs.rs`](../../crates/rimz/src/theme/glyphs.rs) | The glyph catalog (one row per `GlyphRole`), `GlyphSet::resolve`, and the first-run Nerd Font probes. |
| [`provider.rs`](../../crates/rimz/src/theme/provider.rs) | Provider display identity: name, emblem, emblem tints, brand color, and the model shades derived from the brand. |
| [`identity.rs`](../../crates/rimz/src/theme/identity.rs) | `Identity`, the two scheme-independent tones: Claude clay (`#d97757`) and dollar green (`#85bb65`). |
| [`fmt.rs`](../../crates/rimz/src/theme/fmt.rs) | Renderer-independent value formats (see [Shared value formats](#shared-value-formats)). |

The serialized config shape lives in [`config/`](../../crates/rimz/src/config/), so the theme core resolves types it does not define:

- `config/theme.rs` holds `ThemeConfig` and the style folds `effective_theme_mode` and `glyph_set_source`.
- `config/color.rs` holds `ColorDepth`, `ThemeMode`, `ThemeColor`, `PaletteRole`, `Semantic`, and the xterm quantizer.
- `config/scheme.rs` holds the bundled scheme catalog, the Alacritty parser that `RawPalette` converts from, and `scheme_swatches`.
- `config/glyphs.rs` holds `GlyphRole` and its config names.

## Perceptual color math

Every derived tone goes through [`oklab.rs`](../../crates/rimz/src/theme/oklab.rs) instead of sRGB arithmetic, because an sRGB midpoint does not look like a midpoint. Inputs and outputs are 8-bit sRGB tuples, and the OKLab forms stay internal. Three operations do the work:

- `blend(left, right, amount)` interpolates in OKLab, so the visual midpoint lands where `amount` says.
- `lift_lightness(rgb, delta)` shifts lightness and holds hue. A brightening lift can overshoot the sRGB ceiling, where a per-channel clamp would skew hue (red toward pink, blue toward cyan), so chroma eases toward neutral just enough to fit. A dimming lift is a plain lightness drop.
- `warm_toward(base, target, rotate, chroma_scale)` rotates `base` a fraction of the way toward `target`'s hue, scales chroma, and holds `base`'s lightness. It keeps a derived hue vivid where `blend` would desaturate through the midpoint.

## Resolving a palette

`Palette::resolve(&ThemeConfig, ColorDepth)` is the single path from scheme to semantic slots. Every renderer calls it, and no other code derives the slots. Renderer effects such as the lifts in [Subtle steps and color depth](#subtle-steps-and-color-depth) start from its output. It runs four steps.

### 1. Pick the raw palette

`raw_palette_for_theme` takes the first source that parses:

1. an inline Alacritty `[colors.*]` table at the root of `theme.toml` (`config::parse_colors`)
2. `[theme] scheme`, a bundled scheme name or a file path (`config::explicit_scheme`)
3. the bundled default, `TokyoNight Night` (`DEFAULT_SCHEME`)
4. `RawPalette::DEFAULT`, the same colors compiled into the binary, used when the embedded catalog cannot be read

A malformed inline table or an unresolvable scheme name falls through to the next source, so resolution always returns a palette and never fails a render. The bundled catalog is the Alacritty files under [`crates/rimz/themes/alacritty/`](../../crates/rimz/themes/alacritty/). `crates/rimz/build.rs` compresses them into one embedded map that `config/scheme.rs` reads, and `cargo xtask theme-refresh` re-imports them from upstream.

The config health path reports what the fall-through hides. `broken_machine_files_in` in `config.rs` runs `config::scheme::validate_theme` over every configured source, independent of precedence, and returns one `ConfigErr::Theme` per invalid source. Each message names the palette actually in effect. `rimz start` prints these as notices and still opens the room, and `rimz doctor` lists them in config health. `MachineConfig::load` does not run this check.

### 2. Derive the thirteen slots

`RawPalette::derive_tones` assigns meaning to the raw colors and returns a `config::Semantic`:

| slot | derivation |
| --- | --- |
| `good`, `warn`, `alarm` | green, yellow, red, unchanged |
| `accent`, `cool`, `meta` | cyan, blue, magenta, unchanged |
| `caution` | `warm_toward(yellow, red, 0.22, 1.35)`: yellow rotated about a fifth of the way to red's hue with richer chroma, an amber-orange on every scheme where a blend would give a washed-out coral |
| `body`, `muted`, `faint`, `rule` | `blend(background, foreground, t)` with `t` = 0.82, 0.6, 0.38, 0.28 |
| `selection` | `blend(bright_blue, foreground, 0.42)` lifted by 0.05, so the selected card never borrows a data hue |
| `selection_bg` | `blend(background, selection_background, 0.22)` when the scheme ships a selection background, else `blend(background, blue, 0.12)` |

The neutral ladder steps from background toward foreground, which is why it darkens on a light scheme and brightens on a dark one. `selection_bg` stays close to the background because it fills a whole card, which wants far less contrast than a text-selection highlight.

### 3. Apply slot overrides and quantize

Each slot in `ThemeConfig` is an optional `ThemeColor`: a `PaletteRole` name, an RGB hex, or a raw xterm index. An omitted slot keeps the derived tone. A role or RGB value then passes through `Tone::from_rgb`, which emits `Tone::Rgb` at truecolor depth and the nearest xterm index at indexed depth. A raw index becomes `Tone::Indexed` unchanged at both depths.

### 4. Build the ramps and derived tones

Beside the flat slots, `Palette` carries two ramps and three derived tones. None of them is a config slot.

| derived | built from | worn by |
| --- | --- | --- |
| heat ramp | stops `good → warn → caution → alarm` | the accessors in the next table |
| calm ramp | stops `body → good` | the accessors in the next table |
| `expense` | the ramp's `alarm` stop, chroma scaled by `INPUT_EXPENSE_CHROMA` (1.30), then lightness shifted by `INPUT_EXPENSE_DEEPEN` (-0.09) | `Component::Input`, the `↘` fresh-input marker and the reddest tone on screen |
| `cache_expired` | the ramp's `alarm` stop blended halfway to `muted` (`CACHE_EXPIRED_GREY`, 0.5), then depth-quantized | `Component::CacheExpired`: the context meter's expired-cache fill and lead glyph, its age clock, the stale line's age past the lifetime, and the card's expired-cache notice |
| `fast_mode` | `warm_toward(warn, raw red, 0.10, 1.15)`, then the contrast walk below | `Component::FastMode` and the CLI's bold fast-mode badge (`crates/rimz/src/cli/render/palette.rs::fast_mode`) |

The fast-mode walk keeps the amber readable on any background. It steps `FAST_MODE_LIGHTNESS_STEP` (0.04) in OKLab lightness toward whichever of white or black contrasts more with the scheme background, with white winning ties. It then repeats `FAST_MODE_CONTRAST_STEP` (0.01) steps until the emitted color clears WCAG `FAST_MODE_MIN_CONTRAST` (3:1) against that background. At indexed depth the check reads the quantized cell's RGB, and the walk stops at lightness 0 or 1.

The ramps reach the screen through five accessors, and nothing else reads them:

| accessor | input | colors |
| --- | --- | --- |
| `Palette::budget_tone` | a remaining percent and the `[theme.display.budget_bar]` zones | the sidebar's provider budget bar and its window label, and the window percentages of `rimz accounts` |
| `Palette::pace_tone` | a `PaceReading` and the `[theme.display.budget_bar.burn_rate]` bands | the sidebar reset marker and the CLI pace reading: the heat ramp's warm tail for overburn, the calm ramp for underspend once 40% of the window has elapsed (`COOL_PACE_MIN_ELAPSED`), and no tone at rest |
| `Theme::heat_tone` | an amount from 0 to 1 | the context meter, the remote link badge, and the Codex reset-credit expiry while auto-redeem is off |
| `Theme::warm_heat_tone` | an amount from 0 to 1, mapped into the warm tail | the child quiet clock, and the card age clock's `warn → caution → alarm` cache warning band ([band width](./sidebar/sidebar.md#the-line-template)). An unknown TTL keeps the clock muted. |
| `Theme::calm_tone` | an amount from 0 to 1 | the Codex reset-credit expiry while auto-redeem is armed or holding |

The two `Palette` accessors serve the CLI and the sidebar, and the three `Theme` accessors live in `sidebar_pane/render/theme.rs`. `ramp_tone(ramp, amount)` interpolates piecewise across any number of stops in OKLab, so a ramp can gain or lose stops without touching the math. The warm tail is the span `[HEAT_RAMP_WARM_START, 1.0]`, from `warn` onward. It is for readers whose low end should rest warm instead of healthy green: an idle agent is stale, not optimal.

Ramp stops resolve through `derived_rgb_slot`, which differs from a flat slot in one case. An override naming a raw xterm index 0 to 15 has a terminal-defined RGB value the core cannot know, so the ramp keeps the derived tone while the flat slot wears the override. An index of 16 or above converts through `xterm_rgb` and joins the ramp. The derived tones read their sources through the same rule: `expense` reads `alarm`, `cache_expired` reads `alarm` and `muted`, and `fast_mode` reads `warn`. A user recolors a derived tone only through its source slot, so a new derived tone is also named in that slot's row of the [guide's slot table](../guide/theme.md#color-slots).

## Color depth and graceful degradation

Depth is decided at the renderer and passed into the core. `ThemeConfig::effective_theme_mode` folds the style preset into `[theme] mode`: an explicit `truecolor` or `256` mode wins. Under `auto`, the default, `style = "modern"` means truecolor and anything else stays auto, so writing `mode = "auto"` does not undo `modern`. `ThemeMode::depth(truecolor_advertised)` then returns `ColorDepth::Truecolor` or `ColorDepth::Indexed`. The advertisement comes from `tui::truecolor()` (`COLORTERM` or terminfo, cached once per process). The sidebar reads it in `Theme::for_sidebar`, the CLI when its `LazyLock` theme in `cli/render/palette.rs` first loads, and the picker in `PickerTheme::load`.

### Subtle steps and color depth

Several sidebar cues are lightness shifts smaller than one step of the 256-color cube. At indexed depth the nearest cube cell is a jump that reads as a different color, so `sidebar_pane/render/theme.rs` renders each cue differently by depth:

| cue | truecolor | indexed | `NO_COLOR` |
| --- | --- | --- | --- |
| breathing pulse (`Theme::breathe`) | `lift_lightness` of the tone | base tone plus the sample's `DIM` or `BOLD` modifier | the modifier alone |
| unread blink and shimmer beam (`Theme::pulse`, `Theme::shimmer_cell`, through `Theme::lifted`) | lifted tone, held bold | base tone, bold toggled by pole or under the beam | the bold toggle alone |
| selected-card band and unread wash (`Theme::selection_band`, `Theme::unread_wash`) | `selection_bg` stepped down by `highlight_steps.band` or up by `.wash` | `selection_bg` stepped one cube cell by `highlight_steps.indexed` | no fill |
| calm card brand name (`Theme::body_brand`) | brand dimmed by `SOFT_BRAND_DIM` (0.05) | full brand | `body` tone style |

Cues that already span a full color step, such as the neutral ladder and the heat ramp, keep their color at every depth.

These helpers round-trip a color through RGB (`color_to_rgb`) and quantize it again, so at indexed depth "base tone" means the nearest 256-color cell to that RGB. For a slot override naming a raw xterm index 0 to 15, the helpers assume a fixed ANSI RGB for that index. The breathing, unread, and calm-brand cues then show a cube cell instead of the terminal's own color for that index. The `[theme.display.highlight_steps]` units are 0.01 OKLab lightness, and the defaults are `band = 5`, `wash = 1`, `indexed = 4`.

`NO_COLOR` (set and non-empty, read by `tui::no_color()`) drops color and keeps glyph shapes and weight modifiers, so every gauge, status, and marker still reads. The sidebar applies it in `Theme::style` and the helpers above. The CLI applies it at the output stream: `render::out()` is an `anstream::AutoStream`, which strips ANSI when stdout is not a terminal, `NO_COLOR` is set, or `CLICOLOR=0`. The global `--color` flag overrides that choice: `never` always strips, and `always` keeps color even through a pipe or under `NO_COLOR` ([rust-conventions.md](../contributing/rust-conventions.md)).

## Glyphs

`GLYPH_CATALOG` in `theme/glyphs.rs` is the home of every shipped glyph. It holds one row per `GlyphRole`, in the enum's discriminant order: a Unicode glyph and an optional Nerd Font icon. A `None` icon keeps the Unicode shape in both presets. The drawn gauges, box-drawing chrome, spines, spinner and clock heads, and the compacting wave all take that path, because the terminal grid draws them more precisely than an icon. Sidebar render code writes no shipped glyph by hand, with two exceptions. The spinner frame sequences live in `render/animation.rs`. The Nerd Font elapsed clock draws eighths from its own `NERD_AGE_SLICES` table in `sidebar_pane/render/labels/glyphs.rs` (`elapsed_glyph`), so `[theme.glyphs.nerd_font]` clock overrides do not reach it.

`GlyphSet::resolve(&ThemeConfig)` runs one pass:

1. `ThemeConfig::glyph_set_source` picks the preset: `[theme.glyphs] set` if present, else `nerd_font` under `style = "modern"`, else `unicode`.
2. Every role takes the preset's glyph, falling back to Unicode where the Nerd Font column is `None`.
3. Each inline override under `[theme.glyphs.<set>]` for the chosen set replaces its role.

The sidebar reads glyphs through `Theme::glyph(GlyphRole)`. CLI renderers call `theme_glyphs(&ThemeConfig)`, which resolves the set once and returns a lookup closure. `agent_status_glyph_role` maps each `AgentStatus` to its status role, and the sidebar's status glyph reads it.

Tab and window names are the exception to configured glyphs. `crates/rimz/src/sidebar/produce/tab_status.rs::TabStatus::glyph` appends a fixed status suffix (`!`, `?`, `⏸`, `⢿`, `✓`) to a Zellij tab or tmux window name, whatever the glyph set. `strip_status_glyph_suffix` removes a waiting, attention, paused, working, or done glyph from any built-in or configured set off the end of a name, so the mux backends can match a room tab by its base name.

`nerd_font_probe_glyphs` (eight catalog icons) and `nerd_font_probe_gradient` (a color sweep) back the first-run setup probe in `cli/first_run.rs`. The probe asks whether the terminal renders each, and writes `theme.mode` or `theme.glyphs.set` only when the answer differs from the effective default, so the color and glyph halves of `modern` degrade independently. The probe checks only Nerd Font icons. The Unicode set relies on stock fallback fonts instead, and the `glyphs.rs` test `unicode_glyphs_stay_in_fallback_covered_blocks` confines its codepoints to the Unicode blocks those fonts carry, plus a short list of justified exceptions.

## Provider identity

`resolve_provider_identity(kind, styles)` returns a `ResolvedProviderIdentity`: display name, ASCII emblem lines, emblem tints, and `BrandColor`. The registered agent definition (`agents::spec_by_kind`) supplies the name and brand, and the emblem catalog (`agents::emblem_for`) supplies the art and tints. Non-empty `[theme.providers.<kind>]` fields then win field by field:

- `product_name` replaces the name.
- `color` replaces the brand and leaves the art and its tints intact.
- `ascii_art` replaces the art and clears the tints, so the replacement paints in the single brand color.

An unregistered kind gets a title-cased name (`provider_title_case`) and `BrandColor::Indexed(244)`, a neutral grey. `resolve_provider_brand` returns the brand alone, without allocating the name or cloning emblem lines. Name, tab, and other color-only paths use it, and full dashboard panels use the whole identity.

`BrandColor::tone(&Palette)` converts a brand to a `Tone`. A registered definition's `BrandColor::Brand` carries both a truecolor RGB and a hand-picked xterm index. At indexed depth it emits the authored index instead of quantizing the RGB, because the nearest cube cell to a brand color is often not the one a person would pick. A configured `color` resolves like a slot override: a role through the palette, RGB quantized by depth, a raw index unchanged.

`crates/rimz/src/theme/provider.rs::BrandColor::shade` derives model tones from that resolved brand, provider colour overrides included. Step zero is exactly the identity tone. Later ranks blend in OKLab toward the farther of black and white: light brands, including Grok's white, darken, and dark brands lighten. Blending keeps a saturated brand inside the gamut, where a clamped lightness drop would stall on a dark hue. At the palette's depth, each rank's emitted colour sits at least `MODEL_SHADE_STRIDE` (0.1) lightness past the previous rank's, so two models of one provider read apart in a stacked bar. A rank beyond the lightness range keeps its final tone. The stride is measured on `crates/rimz/src/theme/palette.rs::emitted_rgb`, which reads an ANSI index 0 to 15 as its xterm default RGB, so a terminal with a custom ANSI palette can show a brand authored as an ANSI index nearer its first shade than the stride. Its one caller outside tests is `crates/rimz/src/cli/render/palette.rs::model_shade`, and the held `rimz stats` dashboard picks each model's rank ([stats.md](./stats.md#held-dashboard-rows)).

Identity depends only on the provider kind and the theme config, never on whether the dashboard shows that provider's panel. Human-facing provider names, agent handles, tabs, and emblems all resolve through it, and JSON fields keep the definition's own values. One field breaks that rule: `rimz accounts --json` serializes `product_name` from the resolved identity, so it carries a `[theme.providers.<kind>] product_name` override (`crates/rimz/src/cli/accounts/report.rs::ProviderReport`).

## Interface language

Four meanings cover every human-facing use of color, and a new surface picks from them instead of choosing hues.

| meaning | rule | home |
| --- | --- | --- |
| Identity | the resolved provider brand, for provider names, agent handles, tabs, and emblems. Model spend markers derive shades from that brand; statuses, plans, and headings use their own roles. | `theme/provider.rs` |
| State | typed lifecycle values map to a state role, and a glyph or status word carries the same meaning without color | `cli/render/status.rs` |
| Hierarchy | `body` for primary content, `muted` for labels and metadata, `faint` for separators and placeholders, bold for emphasis. Headings are muted bold, and `accent` marks categories. | every renderer |
| Quantity | dollar green (`Identity::Money`) for currency, the token-category tones, and the heat ramp plus shape-readable bars for percentages and budgets | `theme/identity.rs`, `theme/palette.rs` |

In the CLI, `cli/render/status.rs` holds the whole state mapping. `StateRole` names eight roles, and `role` maps each to a palette accessor:

| role | tone |
| --- | --- |
| `Success` | `good` |
| `Working` | `cool` |
| `Waiting`, `Paused` | `warn` |
| `Failed`, `Unavailable` | `alarm` |
| `Neutral`, `Unknown` | `muted` |

One function per typed enum matches it with no wildcard: `agent` (`AgentStatus`), `run` (`RunStatus`), `workflow` (the workflow `RunState`), `message` (`MessageStatus`), `trust` (`TrustState`), `account` (`AccountStatus`), `provider` (`ProviderStatus`), `worktree` (`LandedVerdict`), and `lsp` (`lsp::registry::State`). A new variant is then a compile error in this file, never a silent default that colors one state two ways in two commands. `lsp` returns the role instead of a style, because `render::verdict` and doctor's health tally consume it. `agent` also takes the `TurnPhase`: a running agent that is reasoning reads cool, and one that is acting reads good. `account` reads `Ready` good and every account problem warn. `Unknown` maps no typed status: `rimz loop` passes it for a condition term whose value was not read, and `render::verdict` gives it the `?` glyph.

In the sidebar, `Component` in `render/theme/component.rs` is the equivalent for fixed roles, and `Component::resolve` is its one mapping to a palette slot. `Theme` has no `accent`, `cool`, or `meta` accessor, so sidebar render code reaches a categorical slot only by naming a component: the intent shows at the call site while the hue stays one central decision. The one other route is the animation defaults in `render/animation.rs`, which pick status-head colors through `Palette::animation_color` because those colors are user-configurable under `[theme.animations]`. There the paused and sleeping heads default to `cool`, and the compacting and resolving spinners to `meta`.

Two kinds of sidebar color stay off the component layer. Amount-driven tones come from `Theme::heat_tone`, `warm_heat_tone`, and `calm_tone`. Fixed positive, floor, and negative chrome (diff churn, trunk markers, gate notices) uses the flat `Theme::good`, `warn`, and `alarm`, where the tier name is the intent.

## Shared value formats

[`fmt.rs`](../../crates/rimz/src/theme/fmt.rs) holds shared value formats, including budget-window labels and the quantities used by `rimz stats` and sidebar cards:

| function | output |
| --- | --- |
| `reset_countdown` | two-unit reset countdown: `5h00m`, `1d01h` |
| `window_label`, `window_duration_label` | budget-window labels: `window_label` uses a scope label, delegates known durations, or returns `usage`; `window_duration_label` uses `mo` for 28 to 31 days and exact duration units otherwise |
| `duration_label` | exact duration units for non-window uses: `5h`, `30d`, `730h`, `90m` |
| `active_duration` | agent-time for attribution and stats: `45s`, `41m`, `6h12m`, `1d03h` |
| `run_duration` | a run or stage duration: one unit below an hour, `reset_countdown`'s two units above |
| `expiry_label` | a reset-credit expiry floored to one unit: `3d`, `5h`, `40m` |
| `dollars2` | thousands-grouped dollars with cents: `$1,250.25` |
| `dollars_cap` | `dollars2` without `.00` on whole amounts: `$50` |
| `dollars_left` | dollars left, floored to fit a budget row: cents below $10, whole dollars below $100,000, then `k` and `M` |
| `dollars_estimate` | approximate cache-rebuild dollars to two significant figures (`~$1.40`, `~$12`), omitted below $0.005 |
| `group_thousands` | `12,345` |
| `compact_count` | whole-unit counts: `999`, `12k`, `3M`, `2B` |
| `fmt_bytes` | 1024-based sizes: `512 B`, `1.5 MB` |
| `command_preview` | a command over 120 characters cut to its first 60 and last 59 around `…` |

Fixed-width sidebar labels keep their own clipping and rounding in `sidebar_pane/render/fmt.rs`, where the column budget decides, and plan labels stay with the agent definition that owns the plan vocabulary.

## Boundaries

The theme applies to human presentation only. JSON, hook stdout, pane capture, scripting values, and streaming protocols stay canonical raw data, so neither configuration nor terminal capability can change a machine-readable contract, and ANSI emission stays inside the renderers. The one known break is the `product_name` field of `rimz accounts --json` ([Provider identity](#provider-identity)).

Four checks in [`xtask/src/invariants.rs`](../../xtask/src/invariants.rs) guard the boundaries, and `cargo xtask invariants` runs them. Test modules are exempt from all four. No check scans the `rimz sessions` picker, which builds ratatui colors under `cli/`, so its colors stay on the theme pipeline only by review.

| invariant | enforces |
| --- | --- |
| `ensure_no_hardcoded_ui_colors` | Sidebar render code names a `Component` or a semantic accessor, never a ratatui `Color` variant. Only `Color::Reset` may be written by hand, and `Indexed` and `Rgb` values come from the theme pipeline. The render-side `theme.rs`, its `theme/` directory, and the ANSI quantizer `ansi.rs` are exempt. |
| `ensure_cli_color_provenance` | CLI colors resolve through `cli/render/palette.rs` accessors, and no other CLI file constructs an `anstyle` color. |
| `ensure_no_hardcoded_glyphs` | Sidebar render code outside `render/animation.rs` writes none of the catalog's shipped glyph literals (a fixed list in the check) outside comments. It reads them through `Theme::glyph`. |
| `ensure_brand_resolution_single_home` | Only the agent definition modules and `theme/provider.rs` read a definition's brand color field directly. Everything else, including scenario presets, resolves the brand through the theme core. |

## Where to make a change

| to | change |
| --- | --- |
| retune a hue for every element that wears it | the slot's derivation in `RawPalette::derive_tones`, or, for one machine, the slot override in `theme.toml` |
| color a new sidebar element | add a `Component` variant, map it in `Component::resolve`, and add it to the test-only `Component::ALL`, which the coverage test in `render/theme/tests.rs` walks |
| color a new CLI state | the typed mapping in `cli/render/status.rs` |
| add a configurable glyph | add the role in the `glyph_roles!` table in `config/glyphs.rs`, then its row at the same position in `GLYPH_CATALOG`, then a commented row per set in `config/templates/theme.template.toml` (template tests walk `GlyphRole::ALL` and `AnimationRole::ALL`, so drift fails the build) |
| change how a ramp sweeps | the stop arrays in `Palette::resolve_with_raw` (`ramp_tone` needs no edit) |
| brand a new provider | the agent definition's brand fields (`[theme.providers.<kind>]` stays the user's override) |
| refresh the bundled schemes | `cargo xtask theme-refresh`, which re-imports the upstream Alacritty catalog |
| add a shared human format | `theme/fmt.rs` |

Sidebar-only presentation lives with the sidebar renderer in [`sidebar_pane/render/`](../../crates/rimz/src/sidebar_pane/render/): animation roles, breath and shimmer sampling, and fixed-cell label geometry are in the [sidebar internals](./sidebar/sidebar.md), and the dashboard pet is in [pets.md](./sidebar/pets.md).
