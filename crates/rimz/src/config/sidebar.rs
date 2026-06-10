use std::collections::BTreeMap;
use std::num::{NonZeroU16, NonZeroU32};

use serde::{Deserialize, Serialize};

use crate::sidebar::timing::{DEFAULT_REFRESH_MS, MAX_REFRESH_MS, MIN_REFRESH_MS};

/// `[sidebar] scrollbar`: when the agent cards overflow their viewport, how
/// the right-margin scrollbar shows. Display-only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScrollbarMode {
    /// Show the bar only while the viewport is moving — a wheel scroll or the
    /// selection-driven auto-follow — then hide it about a second after the
    /// view settles.
    #[default]
    Auto,
    /// Keep the bar up whenever the cards overflow.
    Always,
    /// Never paint the bar.
    Never,
}

/// `[sidebar] glow`: whether the truecolor effects tier — the attention glow
/// and the brief transition flashes — runs over the composed frame.
/// Display-only; with the tier off the modifier-based attention breath alone
/// carries the cue.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GlowMode {
    /// Follow the terminal: run when `COLORTERM` advertises 24-bit color.
    #[default]
    Auto,
    /// Force the pass on. The lever for a truecolor terminal the environment
    /// under-advertises — an SSH hop forwards `TERM` but drops `COLORTERM`.
    /// `NO_COLOR` still wins.
    Always,
    /// Pin the plain 256-color render on any terminal.
    Never,
}

/// `[sidebar] provider_tabs`: how the bottom provider dashboard switches
/// between stacked account blocks and a tab rail. Display-only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderTabsMode {
    /// Stack one or two providers so both accounts stay visible; tab three or
    /// more providers to keep the dashboard bounded.
    #[default]
    Auto,
    /// Tab whenever more than one provider is present.
    Always,
    /// Always stack provider blocks; no tab rail is painted.
    Never,
}

/// `[sidebar] card_density`: how much detail resting agent cards show.
/// Display-only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CardDensityMode {
    /// Keep the default card shape: resting cards show their normal detail, and
    /// the selected card appends its subagents.
    #[default]
    Auto,
    /// Show each card's subagent section while keeping the normal card lines.
    Expanded,
    /// Trim resting cards by status; the selected agent still opens to the
    /// full default card.
    Compact,
}

/// Sidebar display preferences. A personal, machine-wide tuning of how the
/// renderer paints; it never affects ledger correctness.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarConfig {
    /// Base render cadence in milliseconds. This controls animation and
    /// event-coalesced paint timing; data polling stays on `--tick-seconds`.
    pub refresh_ms: u16,
    /// Per-provider styling for the bottom dashboard panel, keyed by agent kind
    /// (`claude`/`codex`/`pi`/…). Any field a user omits falls back to the
    /// built-in default for that kind, so overriding just the color leaves the
    /// shipped emblem intact.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub providers: BTreeMap<String, SidebarProviderStyle>,
    /// Most provider blocks the pinned dashboard shows before the rest are
    /// elided. Providers are few, so the cap rarely bites; it bounds the panel
    /// height on a box that links many accounts.
    pub max_provider_blocks: usize,
    /// How provider blocks lay out: `auto` stacks one or two providers and tabs
    /// three or more; `always` tabs whenever more than one provider is present;
    /// `never` always stacks. Resolved producer-side onto the snapshot like
    /// the rest of `[sidebar]`.
    pub provider_tabs: ProviderTabsMode,
    /// Provider kinds to show in the dashboard and their order. Empty means all
    /// discovered providers, still governed by `max_provider_blocks`; `"all"`
    /// expands to every remaining discovered provider at that position; without
    /// `"all"` this is a strict allowlist. An explicit list bypasses
    /// `max_provider_blocks`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_list: Vec<String>,
    /// Cap on the sidebar pane width in columns. Every sidebar pane targets the
    /// standard percentage of the view at this cap; on an ultra-wide terminal
    /// the percentage alone grows absurd, so a pane born above the cap is
    /// shrunk to it once, when it is created. Creation-time only: a manual
    /// resize afterwards sticks.
    pub max_cols: NonZeroU16,
    /// The context meter's severity bands — where the card's context read
    /// leaves calm blue for yellow, amber, and red. Display-only; it tunes the
    /// colour ramp, never the ledger.
    pub context: ContextSeverityConfig,
    /// The provider dashboard's budget-bar color zones — where the draining
    /// mana bar leaves green for yellow, amber, and red as the remaining
    /// budget shrinks. Display-only; it tunes the colour ramp, never the
    /// ledger.
    pub budget: BudgetZonesConfig,
    /// Attention timing knobs. These decide when a silent running row becomes
    /// an actionable `!`; the renderer's heat colours remain separate display
    /// grammar.
    pub attention: AttentionConfig,
    /// Preferred comparison target for the worktree header's git stats (the
    /// `+/-` diff, the `⇡`/`⇣` commit delta, and the `≡`/`✓` landed markers).
    /// Tried
    /// first in the trunk ladder, per repo: a repo where the branch doesn't
    /// resolve falls back to the `main` → `master` → remote-default detection,
    /// so one machine-wide value never breaks other projects. Unset means
    /// detection alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
    /// Palette overrides for the renderer's semantic color slots. Each slot is
    /// a 256-color index; an omitted slot keeps the built-in tone, so an
    /// absent section paints exactly the shipped palette. Resolved
    /// producer-side onto the snapshot like `providers`, so every renderer of
    /// the workspace paints the same tones.
    #[serde(skip_serializing_if = "SidebarThemeConfig::is_unset")]
    pub theme: SidebarThemeConfig,
    /// How the agent-cards scrollbar shows when the cards overflow. `auto`
    /// (default) paints it only while the viewport moves and hides it once the
    /// view settles; `always` keeps it up; `never` removes it. Resolved
    /// producer-side onto the snapshot like the rest of `[sidebar]`.
    pub scrollbar: ScrollbarMode,
    /// Whether the truecolor glow tier runs — the attention glow and the
    /// brief transition flashes the renderer layers over the composed frame.
    /// `auto` (default) follows the terminal's 24-bit advertisement
    /// (`COLORTERM`); `always` forces the pass where the advertisement is
    /// missing — an SSH hop forwards `TERM` but drops `COLORTERM`; `never`
    /// keeps the plain 256-color render with the modifier-based attention
    /// breath. `NO_COLOR` beats every mode. Resolved producer-side onto the
    /// snapshot like the rest of `[sidebar]`.
    pub glow: GlowMode,
    /// How much detail resting agent cards show. `auto` keeps the standard
    /// card shape; `expanded` shows every card's subagent section; `compact`
    /// trims resting cards by status while the selected card opens to the full
    /// form. Resolved producer-side onto the snapshot like the rest of
    /// `[sidebar]`.
    pub card_density: CardDensityMode,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            providers: BTreeMap::new(),
            refresh_ms: DEFAULT_REFRESH_MS,
            max_provider_blocks: default_max_provider_blocks(),
            provider_tabs: ProviderTabsMode::default(),
            provider_list: Vec::new(),
            max_cols: default_sidebar_max_cols(),
            context: ContextSeverityConfig::default(),
            budget: BudgetZonesConfig::default(),
            attention: AttentionConfig::default(),
            trunk: None,
            theme: SidebarThemeConfig::default(),
            scrollbar: ScrollbarMode::default(),
            glow: GlowMode::default(),
            card_density: CardDensityMode::default(),
        }
    }
}

impl SidebarConfig {
    pub fn resolved_refresh_ms(&self) -> u16 {
        self.refresh_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS)
    }
}

/// `[sidebar.theme]`: per-machine overrides for the renderer's semantic palette
/// slots, each a 256-color index. Display-only — it tunes tones, never the
/// glyph grammar (shape still carries every state under `NO_COLOR`), and never
/// the ledger. Slot names follow the semantics, not the shipped hues, so a
/// user re-theming to light terminals reads `good`/`warn`/`alarm` rather than
/// `green`/`yellow`/`red`.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarThemeConfig {
    /// Calm/positive: running tallies, low gauges, `+` additions, cache reads.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub good: Option<u8>,
    /// Caution: waiting glyphs at rest, mid gauges, cache writes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warn: Option<u8>,
    /// Alarm: failed glyphs, high gauges, `-` removals, fresh input.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alarm: Option<u8>,
    /// Structure accent: worktree headers and the selected lane spine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub accent: Option<u8>,
    /// Cool informational: the `plan` posture pill, window tags.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cool: Option<u8>,
    /// Delegation/meta: the `⇅ rc` flag, the subagent `⧉` marker.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub meta: Option<u8>,
    /// Soft content text: stat figures, capability tokens, subagent lines —
    /// a step above `dim`, just below full-strength text.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft: Option<u8>,
    /// Dim chrome: labels, ages, subordinate values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dim: Option<u8>,
    /// Faintest chrome: bar tracks, `·` separators, dotted dividers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub faint: Option<u8>,
    /// The darkest chrome (the scrollbar track) — a step below `faint`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<u8>,
    /// The selected-row `▌` accent bar.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<u8>,
}

impl SidebarThemeConfig {
    /// Whether every slot is unset — the serialized config omits the section.
    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }
}

/// `[sidebar.attention]`: timing knobs for the attention projection. The
/// values are per-machine display/routing preferences, never ledger truth.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct AttentionConfig {
    /// Seconds a `running` agent may record no completed tool or turn activity
    /// before the sidebar projects it to the actionable `!` attention bucket.
    pub stalled_after_secs: NonZeroU32,
}

impl Default for AttentionConfig {
    fn default() -> Self {
        Self {
            stalled_after_secs: NonZeroU32::new(crate::feed::DEFAULT_STALL_AFTER_SECS)
                .expect("non-zero default stall window"),
        }
    }
}

/// The context meter's severity bands: each tier names the inclusive lower
/// bound where it begins, on both axes — the fill percentage and the absolute
/// tokens in the window. Severity is the worse of the two axes, so a
/// large-window model calm by percentage still warms by sheer volume. Below
/// `yellow` on both axes the meter rests calm blue.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextSeverityConfig {
    /// Where the meter leaves calm blue for yellow.
    pub yellow: ContextBand,
    /// Where yellow deepens to amber.
    pub amber: ContextBand,
    /// Where amber escalates to red.
    pub red: ContextBand,
}

impl Default for ContextSeverityConfig {
    fn default() -> Self {
        Self {
            yellow: ContextBand {
                percent: 60,
                tokens: 160_000,
            },
            // 258k matches Codex's effective GPT-5.5 window (272k catalog ×
            // 95%), so a Codex session deepens to amber as it crosses its own
            // ceiling.
            amber: ContextBand {
                percent: 80,
                tokens: 258_000,
            },
            red: ContextBand {
                percent: 95,
                tokens: 420_000,
            },
        }
    }
}

/// One severity tier's entry thresholds: the tier begins once *either* axis
/// reaches its value (`value >= threshold`, inclusive).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContextBand {
    /// Fill percentage (0–100) of the context window.
    pub percent: u8,
    /// Absolute tokens occupying the window.
    pub tokens: u64,
}

/// The provider dashboard's budget color zones. The bar fields name the
/// exclusive upper bound of *remaining* budget (in percent) where each tier
/// applies, so the draining bar crosses into the tier as the remaining figure
/// drops below the bound. At or above `yellow` the bar stays green. The nested
/// pace fields color the reset marker by burn rate against elapsed window time
/// once pace crosses the yellow threshold. A fully spent window's full-width red
/// track is a shape rule independent of these zones.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BudgetZonesConfig {
    /// Remaining % below which the bar leaves green for yellow.
    pub yellow: u8,
    /// Remaining % below which yellow deepens to amber.
    pub amber: u8,
    /// Remaining % below which the bar goes red.
    pub red: u8,
    /// Pace thresholds for the reset marker.
    pub pace: BudgetPaceConfig,
}

impl Default for BudgetZonesConfig {
    fn default() -> Self {
        Self {
            yellow: 50,
            amber: 25,
            red: 10,
            pace: BudgetPaceConfig::default(),
        }
    }
}

/// `[sidebar.budget.pace]`: reset-marker color bands by burn rate. Values are
/// percentages of even pace: `100` means budget use matches elapsed window time,
/// `200` means it is burning twice as fast as the reset can sustain.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BudgetPaceConfig {
    /// Pace % above which the marker leaves the default foreground for yellow.
    pub yellow: u16,
    /// Pace % above which yellow deepens to amber.
    pub amber: u16,
    /// Pace % above which the marker goes red.
    pub red: u16,
}

impl Default for BudgetPaceConfig {
    fn default() -> Self {
        Self {
            yellow: 100,
            amber: 150,
            red: 200,
        }
    }
}

/// Default column cap on the sidebar pane width: comfortably past the widest
/// card tier while keeping a 30% split from swallowing an ultra-wide terminal.
fn default_sidebar_max_cols() -> NonZeroU16 {
    // Provably non-zero literal.
    NonZeroU16::new(72).expect("non-zero literal")
}

/// Default cap on provider blocks in the bottom dashboard.
fn default_max_provider_blocks() -> usize {
    3
}

/// Per-provider styling: the ASCII emblem and brand color for the bottom
/// dashboard. Every field is optional; an omitted field uses the built-in
/// default for the provider kind, so a user overrides just the art or just the
/// color without restating both.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct SidebarProviderStyle {
    /// Display name for the panel header (`Claude`, `Codex`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product_name: Option<String>,
    /// Multi-line ASCII emblem painted at the left of the provider block.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ascii_art: Option<String>,
    /// 256-color index for the emblem (the provider's brand color).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u8>,
}
