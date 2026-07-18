# Theme core

The shared theme core turns one machine theme into the design vocabulary used by every human renderer. It lives in `crates/rimz/src/theme/`; the CLI and native sidebar consume its resolved values without owning parallel palette or provider-brand rules.

## Resolution stack

Resolution follows one direction:

1. A bundled scheme, an explicit Alacritty palette, or the default scheme supplies the raw terminal colors.
2. Theme slot overrides derive the semantic palette and its health, calm, and expense ramps.
3. `ColorDepth` resolves every color to `Tone::Rgb` or `Tone::Indexed`; indexed resolution quantizes RGB through the shared xterm table.
4. Each renderer converts `Tone` at its boundary: `cli/render/palette.rs` produces `anstyle` styles, while `sidebar_pane/render/theme.rs` produces ratatui colors and adds sidebar-only components and animation.

`Palette::resolve` is the single scheme-to-semantic path. Renderer code consumes semantic accessors and keeps raw color construction at its edge.

## Provider identity

`resolve_provider_identity` supplies the name, emblem, emblem tints, and brand color for a provider. `[theme.providers.<kind>]` fields win over the registered agent descriptor; an unregistered kind falls back to a title-cased name and neutral xterm color 244.

Identity resolution depends on the provider kind and theme configuration, not on whether the provider dashboard currently displays a panel. Human provider names, agent handles, tabs, and emblems use this resolved identity; stable JSON fields retain their descriptor values.

## Interface language

Four meanings keep color use predictable:

- **Identity** uses the resolved provider color for provider names, agent handles, tabs, and emblems. Models, statuses, plans, and headings use their own roles.
- **State** maps typed lifecycle values to success, working, waiting, paused, failed, unavailable, or neutral. A glyph or status text carries the same meaning without color.
- **Hierarchy** uses body for primary content, muted for labels and metadata, faint for separators and placeholders, and bold for emphasis. Section headings use the shared muted-bold treatment; accent marks categories.
- **Quantities** use the money tone for currency, established token-category tones, and the health ramp plus shape-readable bars for percentages and budgets.

The CLI owns typed state mapping in `cli/render/status.rs`; sidebar component and animation roles remain renderer-local where their geometry or motion carries meaning.

## Shared values

`theme/fmt.rs` owns renderer-independent human formats: the two-unit reset countdown, CLI-width rate-limit windows, two-decimal and cap-aware dollar values, and compact whole-unit counts. `PlanLabel::format` owns descriptor plan labels. Fixed-cell sidebar labels keep their clipping and rounding rules beside the sidebar renderer.

## Raw surfaces

The theme applies to human presentation. JSON, hook stdout, pane capture, scripting values, and streaming protocols remain canonical raw data so configuration and terminal capabilities cannot change a machine-readable contract. ANSI emission stays behind the CLI renderer, where the output stream also applies `NO_COLOR`, `CLICOLOR`, and pipe detection.
