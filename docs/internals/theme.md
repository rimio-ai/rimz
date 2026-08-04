# The theme core

RimZ paints two very different human surfaces — the CLI's `anstyle` output and the sidebar's ratatui frames — and both wear one look. The theme core is what makes that true: it turns the machine's `[theme]` config into a resolved design vocabulary (tones, glyphs, provider identity, value formats) that a renderer consumes and converts to its own terminal carrier at the last moment.

The core lives in [`crates/rimz/src/theme/`](../../crates/rimz/src/theme/) and is renderer-neutral: it names every color as a `Tone` and leaves the terminal carrier to its consumers, so it couples to neither ratatui nor `anstyle`. Its output is display preference only. A theme changes what a frame looks like and leaves what an agent can do untouched, so nothing here can break a run.

Start with the [theming guide](../guide/theme.md) for what the knobs do on screen and [interface/sidebar.md](../interface/sidebar.md) for what each tone and glyph *means*. This page is how the resolution works and where to change it.

## The four layers

Color flows one direction through four layers, and each layer exists to keep one decision in one place.

```text
Layer 1  Raw          raw.rs              the scheme's terminal colors, verbatim
   │                                      background, foreground, six ANSI hues, selection accent
   ▼
Layer 2  Semantic     palette.rs          thirteen named slots + the ramps
   │                                      good warn caution alarm accent cool meta
   │                                      body muted faint rule selection selection_bg
   ▼
Layer 3  Component    render/theme/       one variant per fixed UI role
   │                  component.rs        Sessions, TokenTotal, WorktreePrOpen, …
   ▼
Layer 4  Carrier      cli/render/palette.rs   Tone → anstyle::Color
                      render/theme.rs         Tone → ratatui::Color
```

Layers 1 and 2 are renderer-neutral and shared. Layer 3 is sidebar-local, because only the sidebar has enough distinct fixed roles to need names for them; the CLI reaches Layer 2 directly through semantic accessors plus a typed state mapping. Layer 4 is the only place a terminal color type appears.

The payoff: a scheme switch retunes Layer 1, a slot override retunes Layer 2, and every call site above them follows without edits. Adding a new sidebar element means naming a component, not picking a color.

## Module map

| module | job |
| --- | --- |
| [`mod.rs`](../../crates/rimz/src/theme/mod.rs) | The public surface: what the CLI and sidebar may import. |
| [`raw.rs`](../../crates/rimz/src/theme/raw.rs) | `RawPalette` — the imported scheme colors, plus `derive_tones`, the one place raw hues become semantic meaning. |
| [`palette.rs`](../../crates/rimz/src/theme/palette.rs) | `Palette::resolve` — slot overrides, depth quantization, the heat and calm ramps, the derived expense tone, and `ramp_tone`. |
| [`tone.rs`](../../crates/rimz/src/theme/tone.rs) | `Tone` — a resolved `Rgb` or `Indexed` color awaiting a carrier. |
| [`oklab.rs`](../../crates/rimz/src/theme/oklab.rs) | Perceptual color math: `blend`, `lift_lightness`, `warm_toward`, and the gamut fit. |
| [`glyphs.rs`](../../crates/rimz/src/theme/glyphs.rs) | The glyph catalog (one row per `GlyphRole`), preset selection, configured overrides, and the first-run Nerd Font probes. |
| [`provider.rs`](../../crates/rimz/src/theme/provider.rs) | Provider display identity: name, emblem, emblem tints, brand color. |
| [`identity.rs`](../../crates/rimz/src/theme/identity.rs) | The two scheme-independent tones: Claude clay and dollar green. |
| [`fmt.rs`](../../crates/rimz/src/theme/fmt.rs) | Renderer-independent value formats: countdowns, window labels, dollars, compact counts. |

Two neighbors complete the picture. [`config/`](../../crates/rimz/src/config/) owns the serialized shape — `ThemeConfig`, `ColorDepth`, `GlyphRole` and its stable wire names, `Semantic`, the Alacritty parser, and ordered catalog swatches — so the theme core resolves config it does not define. `RawPalette` converts the config parser's validated value directly. The renderer edges are [`cli/render/palette.rs`](../../crates/rimz/src/cli/render/palette.rs) and [`sidebar_pane/render/theme.rs`](../../crates/rimz/src/sidebar_pane/render/theme.rs).

## Resolving a palette

`Palette::resolve(&ThemeConfig, ColorDepth)` is the single scheme-to-semantic path. Every human surface calls it and nothing else derives tones.

**Pick the raw palette.** An inline `[colors.*]` table in `theme.toml` wins, then a `[theme] scheme` name or file path, then the bundled `TokyoNight Night`. A malformed inline palette or an unresolvable scheme name falls through to the next source rather than failing, and `RawPalette::DEFAULT` bakes the default scheme's raw colors into the binary, so resolution succeeds even when the embedded catalog is unreadable.

**Derive the thirteen slots.** `RawPalette::derive_tones` assigns meaning to the imported colors:

- The chromatic hues map straight through: `good` = green, `warn` = yellow, `alarm` = red, `accent` = cyan, `cool` = blue, `meta` = magenta.
- `caution` is the one derived hue. Blending yellow into red lands a washed-out coral, so instead `warm_toward` rotates yellow a fraction of the way toward red's hue and enriches chroma, holding lightness. Every scheme gets a vivid amber-orange for its "hot/costly" tier.
- The neutral ladder steps from background toward foreground in OKLab: `body` at 0.82, `muted` at 0.6, `faint` at 0.38, `rule` at 0.28. Deriving from both ends is what makes a light scheme darken instead of brighten.
- `selection` lifts a blend of the scheme's bright blue and foreground, so the selected card never borrows a data hue. `selection_bg` pulls the scheme's text-selection background most of the way back toward the background, because a whole-card fill wants far less contrast than a few-character highlight.

**Apply slot overrides and quantize.** Each slot accepts a palette role name, a hex value, or a raw xterm index; an omitted slot keeps the derived tone. `Tone::from_rgb` then emits `Rgb` at truecolor depth and the nearest xterm index otherwise.

**Build the ramps.** Two ramps and one derived tone sit beside the flat slots:

| derived | from | drives |
| --- | --- | --- |
| the heat ramp | `good → warn → caution → alarm` | the context meter, the remote link badge, the draining budget bar |
| the calm ramp | `body → good` | the under-pace budget burn-rate tail |
| the `expense` tone | `alarm`, chroma-enriched and deepened one step | the `↘` fresh-input marker, the reddest thing on screen |

`ramp_tone` interpolates piecewise across an N-stop ramp in OKLab, so a ramp can gain or lose stops without touching the math. Readers that should rest warm rather than healthy-green — an idle agent is stale, not optimal — map into `[HEAT_RAMP_WARM_START, 1.0]`, the warm tail starting at the second of four stops.

Ramp stops resolve through `derived_rgb_slot`, which differs from a flat slot in one case: an override naming a raw ANSI index 0–15 is terminal-defined RGB, so the ramp keeps the scheme's tone while the flat slot wears the override. An index of 16 or above joins the ramp normally.

## Color depth and graceful degradation

`ThemeMode` folds the `[theme] style` preset (`modern` implies truecolor) and then answers `depth(truecolor_advertised)`. Capability detection is renderer-local: the sidebar reads `tui::truecolor()` in `Theme::for_sidebar`, the CLI reads it once behind a `LazyLock`. The theme core takes the answer as a parameter.

Depth is more than a quantizer choice. Several cues are sub-cell lightness shifts — the recessed selection band, the unread wash, the breathing pulse, the shimmer beam, a calm card's dimmed brand name — and the 256-color cube cannot carry them; its nearest step is a jump that reads as a different color rather than the same color a touch brighter. So `Theme::lifted` in the sidebar edge branches: truecolor renders the shift as color, indexed depth renders the same *ordering* as a `DIM`/`BOLD` weight over the base tone, which is the shape `NO_COLOR` already uses. Cues that span a full color step keep their color at every depth.

`NO_COLOR` drops color entirely and keeps glyph shape and weight modifiers, so every gauge, status, and marker still reads. The sidebar honors it in `Theme::style`; the CLI honors it in the output stream alongside `CLICOLOR` and pipe detection.

## Why OKLab

Every derived tone — ladder steps, ramp blends, the amber warm, the highlight lifts — goes through [`oklab.rs`](../../crates/rimz/src/theme/oklab.rs) rather than raw sRGB arithmetic, because sRGB midpoints do not look like midpoints. Three operations carry the work:

- `blend` interpolates so the visual midpoint lands where the number says.
- `lift_lightness` shifts lightness holding hue. A brightening lift can overshoot the sRGB ceiling, where a per-channel clamp would skew hue (red toward pink, blue toward cyan), so chroma eases toward neutral just enough to fit.
- `warm_toward` rotates hue a fraction toward a target while enriching chroma and holding lightness, which is how `caution` and `expense` stay vivid instead of muddying.

## Glyphs

One catalog row per `GlyphRole` carries a Unicode glyph and an optional Nerd Font icon. `None` means the role keeps its Unicode shape in both presets — the drawn gauges, box-drawing chrome, spines, spinner and clock heads, and the compacting wave all take this path, because the terminal grid draws them more precisely than any icon can. `ToolRepeat` resolves the card's `⟲` identical-call marker through the same catalog and can be overridden as `theme.glyphs.<set>.status.tool_repeat`.

`GlyphSet::resolve` runs one pass: `[theme.glyphs] set` wins, else `style = "modern"` selects `nerd_font`, else Unicode; the preset fills every role; matching inline overrides land last. Renderers read glyphs through `theme.glyph(GlyphRole::…)` in the sidebar or `theme_glyphs(&ThemeConfig)` elsewhere, and the catalog is the only place a shipped glyph literal exists.

`nerd_font_probe_glyphs` and `nerd_font_probe_gradient` back the first-run setup probe, which asks the terminal whether it renders the icons and the color sweep and writes explicit `theme.mode` or `theme.glyphs.set` only when the answer changes the effective default. That is why the two halves of `modern` degrade independently.

## Provider identity

`resolve_provider_identity(kind, styles)` returns the display name, ASCII emblem, emblem tints, and brand color for a provider kind. The registered agent definition supplies the defaults; `[theme.providers.<kind>]` fields win field by field, so overriding the color leaves the shipped art and its tints intact, while overriding the art clears the tints (a replacement emblem paints in the single brand color). An unregistered kind falls back to a title-cased name and neutral xterm 244.

`resolve_provider_brand` answers the color alone, without allocating a name or cloning emblem lines. Names, tabs, and other color-only paths use it; full panels use the identity.

Two properties are worth knowing before you touch this:

- A registered definition carries both a truecolor RGB and a hand-tuned xterm index. `BrandColor::Brand` emits the RGB at truecolor depth and the *authored* index at indexed depth rather than re-quantizing, because the nearest cube cell to a brand color is often not the one a human would pick.
- Identity depends on the provider kind and the theme config alone, never on whether the dashboard currently shows that provider's panel. Human provider names, agent handles, tabs, and emblems all resolve through it; stable JSON fields keep their descriptor values.

## Interface language

Four meanings cover every human-facing use of color. Keeping to them is what makes an unfamiliar RimZ screen readable.

| meaning | rule | lives in |
| --- | --- | --- |
| Identity | the resolved provider color, for provider names, agent handles, tabs, and emblems. Models, statuses, plans, and headings use their own roles. | `theme/provider.rs` |
| State | typed lifecycle values map to success, working, waiting, paused, failed, unavailable, or neutral, and a glyph or status word carries the same meaning without color. | `cli/render/status.rs` |
| Hierarchy | `body` for primary content, `muted` for labels and metadata, `faint` for separators and placeholders, bold for emphasis; headings take the shared muted-bold treatment and `accent` marks categories. | both edges |
| Quantity | the money tone for currency, the established token-category tones, and the health ramp plus shape-readable bars for percentages and budgets. | `theme/identity.rs`, `palette.rs` |

State mapping is centralized deliberately: `cli/render/status.rs` matches on the typed enum (`AgentStatus`, `RunStatus`, `MessageStatus`, `TrustState`, `ProviderStatus`), so a new variant is a compile error in one file rather than a silent fall-through that colors the same state differently in two commands.

The sidebar's equivalent for fixed roles is the `Component` token. Categorical slots (`accent`, `cool`, `meta`) are never emitted bare there — every such use names a component, so the intent is visible at the call site while the hue stays one central decision. Amount-driven tones stay off the component layer and live on `Theme` methods (`heat_tone`, `warm_heat_tone`, `calm_tone`), and the flat `good`/`warn`/`alarm` accessors cover fixed positive/floor/negative chrome where naming the tier *is* the intent.

## Shared value formats

[`fmt.rs`](../../crates/rimz/src/theme/fmt.rs) holds the human value vocabulary that both renderers share: the two-unit reset countdown (`5h00m`, `1d01h`), CLI-width rate-limit window labels, thousands-grouped dollar values with and without cap rounding, and compact whole-unit token counts. It lives here for the same reason the palette does — one number should print the same way in `rimz stats` and on a card.

Fixed-cell sidebar labels keep their clipping and rounding beside the sidebar renderer, where the column budget is the deciding fact, and plan labels stay with the agent definition that owns the plan vocabulary.

## Boundaries

**The theme applies to human presentation only.** JSON, hook stdout, pane capture, scripting values, and streaming protocols stay canonical raw data, so neither configuration nor terminal capability can change a machine-readable contract. ANSI emission stays behind the renderer edges.

Four grep invariants in [`xtask/src/invariants.rs`](../../xtask/src/invariants.rs) hold the boundaries, and `cargo xtask invariants` runs them:

| invariant | enforces |
| --- | --- |
| `ensure_no_hardcoded_ui_colors` | sidebar render code names a `Component` or a semantic accessor, never a `Color` variant. Only `Color::Reset` may be written by hand; `Color::Indexed`/`Rgb` are values the theme pipeline mints. |
| `ensure_cli_color_provenance` | CLI colors resolve through `cli::render::palette` accessors; no `anstyle` color is constructed elsewhere. |
| `ensure_no_hardcoded_glyphs` | sidebar render glyphs route through `theme.glyph(GlyphRole::…)`; literals live in the catalog and in `render/animation.rs`. |
| `ensure_brand_resolution_single_home` | provider brand identity resolves through `theme::resolve_provider_identity`, not by reaching into a definition's brand field. |

## Where to make a change

| you want to | change |
| --- | --- |
| retune a hue for every element that wears it | the slot's derivation in `raw.rs`, or the user-facing override in `theme.toml` |
| color a new sidebar element | add a `Component` variant and map it in `Component::resolve` |
| color a new CLI state | add the typed mapping in `cli/render/status.rs` |
| add a configurable glyph | add a `GlyphRole` in `config/glyphs.rs`, then its catalog row in `theme/glyphs.rs` |
| change how a ramp sweeps | the stop list in `Palette::resolve_with_raw`; `ramp_tone` needs no edit |
| brand a new provider | the agent definition's brand fields; `[theme.providers.<kind>]` stays the user's override |
| add a shared human format | `theme/fmt.rs`, so both renderers print it identically |

Sidebar-only concerns stay one level out: animation roles, breath and shimmer sampling, and the fixed-cell label geometry live in [`sidebar_pane/render/`](../../crates/rimz/src/sidebar_pane/render/) with the [sidebar internals](./sidebar/sidebar.md), and the dashboard pet has its own page in [pets.md](./sidebar/pets.md).
