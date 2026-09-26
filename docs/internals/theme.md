# The theme core

The theme core turns the machine's `[theme]` config into the design vocabulary every human surface paints with: color tones, glyphs, provider identity, and value formats. RimZ has two such surfaces, the CLI's `anstyle` output and the sidebar's ratatui frames, and both read the same resolved vocabulary, so a scheme or override change reaches them together.

The core lives in [`crates/rimz/src/theme/`](../../crates/rimz/src/theme/) and is renderer-neutral. It names every color as a `Tone`, and each renderer converts that `Tone` to its own terminal color type at the last moment, so the core depends on neither ratatui nor `anstyle`. Its output is display preference only: a theme changes what a frame looks like, never what an agent can do.

For what each knob does on screen, read the [theming guide](../guide/theme.md); for what each tone and glyph means, read [interface/sidebar.md](../interface/sidebar.md). This page covers how resolution works and where to change it.

## The four layers

Color flows one way through four layers, and each layer holds one decision.

```text
Layer 1  Raw          theme/raw.rs              the scheme's terminal colors, verbatim
   │                                            background, foreground, six ANSI hues,
   │                                            bright blue, selection background
   ▼
Layer 2  Semantic     theme/palette.rs          thirteen named slots, two ramps, the expense tone
   │                                            good warn caution alarm accent cool meta
   │                                            body muted faint rule selection selection_bg
   ▼
Layer 3  Component    sidebar_pane/render/      one variant per fixed sidebar UI role
   │                  theme/component.rs        Sessions, TokenTotal, WorktreePrOpen, ...
   ▼
Layer 4  Carrier      cli/render/palette.rs     Tone → anstyle::Color
                      sidebar_pane/render/      Tone → ratatui::Color
                      theme.rs
```

Layers 1 and 2 are the shared core and never name a terminal color type. Layer 3 exists only in the sidebar, which has enough distinct fixed roles to need names for them; `Component::resolve` already returns a ratatui `Color` through the Layer 4 conversion. The CLI has no Layer 3 and reads Layer 2 directly through semantic accessors and a typed state mapping. Outside the Layer 3 and Layer 4 modules, sidebar render code passes resolved colors around and writes no color literal except `Color::Reset` (see [Boundaries](#boundaries)).

Each kind of change lands in one layer. A scheme switch replaces Layer 1 and a slot override replaces one Layer 2 slot; every call site above follows without edits. A new sidebar element names a Layer 3 component instead of picking a color.

## Module map

| module | job |
| --- | --- |
| [`mod.rs`](../../crates/rimz/src/theme/mod.rs) | The public surface the CLI and sidebar import. |
| [`raw.rs`](../../crates/rimz/src/theme/raw.rs) | `RawPalette`, the imported scheme colors, and `derive_tones`, the one place raw hues gain semantic meaning. `RawPalette::DEFAULT` holds the default scheme's colors. |
| [`palette.rs`](../../crates/rimz/src/theme/palette.rs) | `Palette::resolve`: raw palette selection, slot overrides, depth quantization, the heat and calm ramps, the expense tone, and `ramp_tone`. |
| [`tone.rs`](../../crates/rimz/src/theme/tone.rs) | `Tone`, a resolved `Rgb` or `Indexed` color awaiting a renderer. |
| [`oklab.rs`](../../crates/rimz/src/theme/oklab.rs) | Perceptual color math: `blend`, `lift_lightness`, `warm_toward`, and the gamut fit. |
| [`glyphs.rs`](../../crates/rimz/src/theme/glyphs.rs) | The glyph catalog (one row per `GlyphRole`), `GlyphSet::resolve`, and the first-run Nerd Font probes. |
| [`provider.rs`](../../crates/rimz/src/theme/provider.rs) | Provider display identity: name, emblem, emblem tints, brand color. |
| [`identity.rs`](../../crates/rimz/src/theme/identity.rs) | `Identity`, the two scheme-independent tones: Claude clay (`#d97757`) and dollar green (`#85bb65`). |
| [`fmt.rs`](../../crates/rimz/src/theme/fmt.rs) | Renderer-independent value formats (see [Shared value formats](#shared-value-formats)). |

The serialized config shape lives in [`config/`](../../crates/rimz/src/config/), so the theme core resolves types it does not define. `config/theme.rs` holds `ThemeConfig` and the style folds (`effective_theme_mode`, `glyph_set_source`); `config/color.rs` holds `ColorDepth`, `ThemeMode`, `ThemeColor`, `PaletteRole`, `Semantic`, and the xterm quantizer; `config/scheme.rs` holds the bundled catalog, the Alacritty parser that `RawPalette` converts from, and `scheme_swatches`; `config/glyphs.rs` holds `GlyphRole` and its config names.

## Perceptual color math

Every derived tone goes through [`oklab.rs`](../../crates/rimz/src/theme/oklab.rs) instead of sRGB arithmetic, because an sRGB midpoint does not look like a midpoint. Inputs and outputs are 8-bit sRGB tuples; the OKLab forms stay internal. Three operations do the work:

- `blend(left, right, amount)` interpolates in OKLab, so the visual midpoint lands where `amount` says.
- `lift_lightness(rgb, delta)` shifts lightness and holds hue. A brightening lift can overshoot the sRGB ceiling, where a per-channel clamp would skew hue (red toward pink, blue toward cyan), so chroma eases toward neutral just enough to fit. A dimming lift is a plain lightness drop.
- `warm_toward(base, target, rotate, chroma_scale)` rotates `base` a fraction of the way toward `target`'s hue, scales chroma, and holds `base`'s lightness. It keeps a derived hue vivid where `blend` would desaturate through the midpoint.

## Resolving a palette

`Palette::resolve(&ThemeConfig, ColorDepth)` is the single path from scheme to semantic slots. Both renderers call it, and no other code derives the slots; renderer effects such as the lifts in [Color depth](#color-depth-and-graceful-degradation) start from its output. It runs four steps.

### 1. Pick the raw palette

`raw_palette_for_theme` takes the first source that parses:

1. an inline Alacritty `[colors.*]` table at the root of `theme.toml` (`config::parse_colors`);
2. `[theme] scheme`, a bundled scheme name or a file path (`config::explicit_scheme`);
3. the bundled default, `TokyoNight Night` (`DEFAULT_SCHEME`);
4. `RawPalette::DEFAULT`, the same colors compiled into the binary, used when the embedded catalog cannot be read.

A malformed inline table or an unresolvable scheme name falls through to the next source instead of failing, so resolution always returns a palette. Load-time validation reports those errors to the user separately.

### 2. Derive the thirteen slots

`RawPalette::derive_tones` assigns meaning to the raw colors and returns a `config::Semantic`:

| slot | derivation |
| --- | --- |
| `good`, `warn`, `alarm` | green, yellow, red, unchanged |
| `accent`, `cool`, `meta` | cyan, blue, magenta, unchanged |
| `caution` | `warm_toward(yellow, red, 0.22, 1.35)`: yellow rotated a fifth of the way to red's hue with richer chroma, an amber-orange on every scheme where a blend would give a washed-out coral |
| `body`, `muted`, `faint`, `rule` | `blend(background, foreground, t)` with `t` = 0.82, 0.6, 0.38, 0.28 |
| `selection` | `blend(bright_blue, foreground, 0.42)` lifted by 0.05, so the selected card never borrows a data hue |
| `selection_bg` | `blend(background, selection_background, 0.22)` when the scheme ships a selection background, else `blend(background, blue, 0.12)` |

The neutral ladder steps from background toward foreground, which is why it darkens on a light scheme and brightens on a dark one. `selection_bg` stays close to the background because it fills a whole card, which wants far less contrast than a text-selection highlight.

### 3. Apply slot overrides and quantize

Each slot in `ThemeConfig` is an optional `ThemeColor`: a `PaletteRole` name, an RGB hex, or a raw xterm index. An omitted slot keeps the derived tone. A role or RGB value then passes through `Tone::from_rgb`, which emits `Tone::Rgb` at truecolor depth and the nearest xterm index at indexed depth. A raw index becomes `Tone::Indexed` unchanged at both depths.

### 4. Build the ramps and the expense tone

Beside the flat slots, `Palette` carries two ramps and one derived tone:

| derived | stops | read by |
| --- | --- | --- |
| heat ramp | `good → warn → caution → alarm` | `Theme::heat_tone`: the context meter, the remote link badge, the provider budget bar and its window label, the Codex reset-credit expiry while auto-redeem is off; `Theme::warm_heat_tone`: the card age clock and the over-pace budget tail |
| calm ramp | `body → good` | `Theme::calm_tone`: the under-pace budget tail, the Codex reset-credit expiry while auto-redeem is armed or holding |
| `expense` | `alarm` with chroma scaled by `INPUT_EXPENSE_CHROMA` (1.30), then lightness lowered by `INPUT_EXPENSE_DEEPEN` (0.09) | `Component::Input`, the `↘` fresh-input marker and the reddest tone on screen |

`ramp_tone(ramp, amount)` interpolates piecewise across any number of stops in OKLab, so a ramp can gain or lose stops without touching the math. `warm_heat_tone` maps its amount into `[HEAT_RAMP_WARM_START, 1.0]`, the tail from `warn` onward, for readers whose low end should rest warm instead of healthy green: an idle agent is stale, not optimal.

Ramp stops resolve through `derived_rgb_slot`, which differs from a flat slot in one case. An override naming a raw xterm index 0 to 15 has a terminal-defined RGB value the core cannot know, so the ramp keeps the derived tone while the flat slot wears the override. An index of 16 or above converts through `xterm_rgb` and joins the ramp. The expense tone derives from the ramp's `alarm` stop and follows the same rule.

## Color depth and graceful degradation

Depth is decided at the renderer and passed into the core. `ThemeConfig::effective_theme_mode` folds the style preset into `[theme] mode`: an explicit mode wins, otherwise `style = "modern"` means truecolor and anything else means auto. `ThemeMode::depth(truecolor_advertised)` then returns `ColorDepth::Truecolor` or `ColorDepth::Indexed`. The advertisement comes from `tui::truecolor()` (`COLORTERM` or terminfo, cached once per process), which the sidebar reads in `Theme::for_sidebar` and the CLI reads when its `LazyLock` theme first loads.

Several sidebar cues are lightness shifts smaller than one step of the 256-color cube. At indexed depth the nearest cube cell is a jump that reads as a different color, so `sidebar_pane/render/theme.rs` renders each cue differently by depth:

| cue | truecolor | indexed | `NO_COLOR` |
| --- | --- | --- | --- |
| breathing pulse (`Theme::breathe`) | `lift_lightness` of the tone | base tone plus the sample's `DIM`/`BOLD` modifier | the modifier alone |
| unread blink and shimmer beam (`Theme::pulse`, `Theme::shimmer_cell`, through `Theme::lifted`) | lifted tone, held bold | base tone, bold toggled by pole or under the beam | the bold toggle alone |
| selected band and unread wash (`Theme::selection_band`, `Theme::unread_wash`) | `selection_bg` stepped by `highlight_steps.band` down or `.wash` up | `selection_bg` stepped one cube cell by `highlight_steps.indexed` | no fill |
| calm card brand name (`Theme::body_brand`) | brand dimmed by `SOFT_BRAND_DIM` (0.05) | full brand | `body` tone style |

Cues that already span a full color step, such as the neutral ladder and the heat ramp, keep their color at every depth. The `[theme.display.highlight_steps]` units are 0.01 OKLab lightness; the defaults are `band = 5`, `wash = 1`, `indexed = 4`.

`NO_COLOR` (set and non-empty, read by `tui::no_color()`) drops color and keeps glyph shapes and weight modifiers, so every gauge, status, and marker still reads. The sidebar applies it in `Theme::style` and the helpers above. The CLI applies it at the output stream: `render::out()` is an `anstream::AutoStream` that strips ANSI for `NO_COLOR`, `CLICOLOR`, `--color never`, or a pipe ([rust-conventions.md](../contributing/rust-conventions.md)).

## Glyphs

`GLYPH_CATALOG` in `theme/glyphs.rs` holds one row per `GlyphRole`, in the enum's discriminant order: a Unicode glyph and an optional Nerd Font icon. A `None` icon keeps the Unicode shape in both presets. The drawn gauges, box-drawing chrome, spines, spinner and clock heads, and the compacting wave all take that path, because the terminal grid draws them more precisely than an icon. The catalog is the home of every shipped glyph; sidebar render code writes none of them by hand, apart from the spinner frame sequences in `render/animation.rs`.

`GlyphSet::resolve(&ThemeConfig)` runs one pass:

1. `ThemeConfig::glyph_set_source` picks the preset: `[theme.glyphs] set` if present, else `nerd_font` under `style = "modern"`, else `unicode`.
2. Every role takes the preset's glyph, falling back to Unicode where the Nerd Font column is `None`.
3. Each inline override under `[theme.glyphs.<set>]` for the chosen set replaces its role.

The sidebar reads glyphs through `Theme::glyph(GlyphRole)`; CLI renderers call `theme_glyphs(&ThemeConfig)`, which resolves the set once and returns a lookup closure. `agent_status_glyph_role` maps each `AgentStatus` to its status role for every surface, and `strip_status_glyph_suffix` removes a status glyph from any built-in or configured set off the end of a Zellij tab or tmux window name, so the mux backends can match a room tab by its base name.

`nerd_font_probe_glyphs` (eight catalog icons) and `nerd_font_probe_gradient` (a color sweep) back the first-run setup probe in `cli/first_run.rs`. The probe asks whether the terminal renders each, and writes `theme.mode` or `theme.glyphs.set` only when the answer changes the effective default, so the color and glyph halves of `modern` degrade independently.

## Provider identity

`resolve_provider_identity(kind, styles)` returns a `ResolvedProviderIdentity`: display name, ASCII emblem lines, emblem tints, and `BrandColor`. The registered agent definition (`agents::spec_by_kind`) supplies the name and brand, and the emblem catalog (`agents::emblem_for`) supplies the art and tints. `[theme.providers.<kind>]` fields then win field by field:

- `product_name` replaces the name.
- `color` replaces the brand and leaves the art and its tints intact.
- `ascii_art` replaces the art and clears the tints, so the replacement paints in the single brand color.

An unregistered kind gets a title-cased name (`provider_title_case`) and `BrandColor::Indexed(244)`, a neutral grey. `resolve_provider_brand` returns the brand alone without allocating the name or cloning emblem lines; name, tab, and other color-only paths use it, and full dashboard panels use the whole identity.

`BrandColor::tone(&Palette)` converts a brand to a `Tone`. A registered definition's `BrandColor::Brand` carries both a truecolor RGB and a hand-picked xterm index, and at indexed depth it emits the authored index instead of quantizing the RGB, because the nearest cube cell to a brand color is often not the one a person would pick. A configured `color` resolves like a slot override: a role through the palette, RGB quantized by depth, a raw index unchanged.

Identity depends only on the provider kind and the theme config, never on whether the dashboard shows that provider's panel. Human-facing provider names, agent handles, tabs, and emblems all resolve through it; JSON fields keep the definition's own values.

## Interface language

Four meanings cover every human-facing use of color, and a new surface picks from them instead of choosing hues.

| meaning | rule | home |
| --- | --- | --- |
| Identity | the resolved provider brand, for provider names, agent handles, tabs, and emblems; models, statuses, plans, and headings use their own roles | `theme/provider.rs` |
| State | typed lifecycle values map to success, working, waiting, paused, failed, unavailable, or neutral, and a glyph or status word carries the same meaning without color | `cli/render/status.rs` |
| Hierarchy | `body` for primary content, `muted` for labels and metadata, `faint` for separators and placeholders, bold for emphasis; headings are muted bold and `accent` marks categories | both renderers |
| Quantity | dollar green (`Identity::Money`) for currency, the token-category tones, and the heat ramp plus shape-readable bars for percentages and budgets | `theme/identity.rs`, `theme/palette.rs` |

In the CLI, `cli/render/status.rs` holds the whole state mapping. `StateRole` names the seven roles and `role` maps each to a palette accessor (`Success` good, `Working` cool, `Waiting` and `Paused` warn, `Failed` and `Unavailable` alarm, `Neutral` muted). One function per typed enum (`agent`, `run`, `message`, `trust`, `provider`) matches `AgentStatus`, `RunStatus`, `MessageStatus`, `TrustState`, and `ProviderStatus` with no wildcard, so a new variant is a compile error in this file instead of a silent default that colors one state two ways in two commands. `agent` also takes the `TurnPhase`: a running agent that is reasoning reads cool, one that is acting reads good.

In the sidebar, `Component` in `render/theme/component.rs` is the equivalent for fixed roles, and `Component::resolve` is its one mapping to a palette slot. `Theme` has no `accent`, `cool`, or `meta` accessor, so sidebar render code reaches a categorical slot only by naming a component: the intent shows at the call site while the hue stays one central decision. The one other route is the animation defaults in `render/animation.rs`, which pick status-head colors through `Palette::animation_color` (paused and sleeping heads default to `cool`, the compacting and resolving spinners to `meta`) because those colors are user-configurable under `[theme.animations]`. Two kinds of color stay off the component layer. Amount-driven tones come from `Theme::heat_tone`, `warm_heat_tone`, and `calm_tone`, and fixed positive, floor, and negative chrome (diff churn, trunk markers, gate notices) uses the flat `Theme::good`, `warn`, and `alarm`, where the tier name is the intent.

## Shared value formats

[`fmt.rs`](../../crates/rimz/src/theme/fmt.rs) holds the value formats both renderers share, so one number prints the same way in `rimz stats` and on a sidebar card:

| function | output |
| --- | --- |
| `reset_countdown` | two-unit reset countdown: `5h00m`, `1d01h` |
| `window_label`, `duration_label` | rate-limit window labels: the scope label if any, else `5h`, `7d`, `90m` |
| `dollars2`, `dollars_cap` | thousands-grouped dollars; `dollars_cap`, sidebar-only, drops `.00` on whole amounts |
| `group_thousands` | `12,345` |
| `compact_count` | whole-unit token counts: `999`, `12k`, `3m` |
| `fmt_bytes` | 1024-based sizes: `512 B`, `1.5 MB` |
| `command_preview` | a command over 120 characters cut to its first 60 and last 59 around `…` |

Fixed-width sidebar labels keep their own clipping and rounding beside the sidebar renderer, where the column budget decides, and plan labels stay with the agent definition that owns the plan vocabulary.

## Boundaries

The theme applies to human presentation only. JSON, hook stdout, pane capture, scripting values, and streaming protocols stay canonical raw data, so neither configuration nor terminal capability can change a machine-readable contract, and ANSI emission stays inside the two renderers.

Four checks in [`xtask/src/invariants.rs`](../../xtask/src/invariants.rs) guard the boundaries; `cargo xtask invariants` runs them. Test modules are exempt from all four.

| invariant | enforces |
| --- | --- |
| `ensure_no_hardcoded_ui_colors` | Sidebar render code names a `Component` or a semantic accessor, never a ratatui `Color` variant. Only `Color::Reset` may be written by hand; `Indexed` and `Rgb` values come from the theme pipeline. The render-side `theme.rs`, its `theme/` directory, and the ANSI quantizer `ansi.rs` are exempt. |
| `ensure_cli_color_provenance` | CLI colors resolve through `cli/render/palette.rs` accessors; no other CLI file constructs an `anstyle` color. |
| `ensure_no_hardcoded_glyphs` | Sidebar render code outside `render/animation.rs` writes none of the catalog's shipped glyph literals (a fixed list in the check) outside comments; it reads them through `Theme::glyph`. |
| `ensure_brand_resolution_single_home` | Only the agent definition modules, `theme/provider.rs`, and the sidebar fixture read a definition's brand color field directly; everything else resolves brand through the theme core. |

## Where to make a change

| to | change |
| --- | --- |
| retune a hue for every element that wears it | the slot's derivation in `RawPalette::derive_tones`, or, for one machine, the slot override in `theme.toml` |
| color a new sidebar element | add a `Component` variant, map it in `Component::resolve`, and add it to `Component::ALL` for the golden test |
| color a new CLI state | the typed mapping in `cli/render/status.rs` |
| add a configurable glyph | add the role in the `glyph_roles!` table in `config/glyphs.rs`, then its row at the same position in `GLYPH_CATALOG`, then a commented row per set in `theme.template.toml` (a template test walks `GlyphRole::ALL` and `AnimationRole::ALL`, so the drift fails the build) |
| change how a ramp sweeps | the stop arrays in `Palette::resolve_with_raw`; `ramp_tone` needs no edit |
| brand a new provider | the agent definition's brand fields; `[theme.providers.<kind>]` stays the user's override |
| add a shared human format | `theme/fmt.rs` |

Sidebar-only presentation lives with the sidebar renderer in [`sidebar_pane/render/`](../../crates/rimz/src/sidebar_pane/render/): animation roles, breath and shimmer sampling, and fixed-cell label geometry are in the [sidebar internals](./sidebar/sidebar.md), and the dashboard pet is in [pets.md](./sidebar/pets.md).
