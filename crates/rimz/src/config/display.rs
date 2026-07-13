use std::num::NonZeroU16;

use serde::{Deserialize, Serialize};

use crate::sidebar::timing::{DEFAULT_REFRESH_MS, MAX_REFRESH_MS, MIN_REFRESH_MS};

/// `[theme.display] scrollbar`: when the agent cards overflow their viewport,
/// how the right-margin scrollbar shows. Display-only.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ScrollbarMode {
    /// Show the bar only while the viewport is moving - a wheel scroll or the
    /// selection-driven auto-follow - then hide it about a second after the view
    /// settles.
    #[default]
    Auto,
    /// Keep the bar up whenever the cards overflow.
    Always,
    /// Never paint the bar.
    Never,
}

/// `[theme.display] pixel`: whether kitty-graphics pixel surfaces may paint.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PixelMode {
    /// Use pixel pets and context meters when the terminal path supports them.
    #[default]
    Auto,
    /// Keep every surface on its cell-rendered tier.
    Off,
}

/// `[theme.display] provider_tabs`: how the bottom provider dashboard switches
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

impl ProviderTabsMode {
    /// Whether a dashboard of `count` providers lays out as a tab rail rather
    /// than stacked blocks: `auto` tabs at three or more, `always` at more than
    /// one, `never` not at all. A tabbed dashboard is bounded by its single
    /// active block, so it shows every provider - `max_provider_blocks` only
    /// trims the stacked layout.
    pub fn tabs(self, count: usize) -> bool {
        match self {
            ProviderTabsMode::Auto => count >= 3,
            ProviderTabsMode::Always => count > 1,
            ProviderTabsMode::Never => false,
        }
    }
}

/// `[theme.display] card_density`: how much detail resting agent cards show.
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
    /// Trim resting cards by status; the selected agent still opens to the full
    /// default card.
    Compact,
}

/// Sidebar render preferences. A personal, machine-wide tuning of how the
/// renderer paints; it never affects store correctness.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct DisplayConfig {
    /// Base render cadence in milliseconds. This controls animation and
    /// event-coalesced paint timing; data polling stays on `--tick-seconds`.
    pub refresh_ms: u16,
    /// Master switch for kitty-graphics rendering. Display-only.
    pub pixel: PixelMode,
    /// Most provider blocks the *stacked* dashboard shows before the rest are
    /// elided; a tabbed dashboard is height-bounded by its active block, so it
    /// shows every provider regardless of this cap. Providers are few, so the
    /// cap rarely bites; it bounds the panel height on a box that links many
    /// accounts and explicitly stacks them.
    pub max_provider_blocks: usize,
    /// How provider blocks lay out: `auto` stacks one or two providers and tabs
    /// three or more; `always` tabs whenever more than one provider is present;
    /// `never` always stacks. Resolved producer-side onto the snapshot like the
    /// rest of `[theme.display]`.
    pub provider_tabs: ProviderTabsMode,
    /// Provider kinds to show in the dashboard and their order. Empty means all
    /// discovered providers in usage-rank order, still governed by
    /// `max_provider_blocks`; an explicit list overrides both the set and the
    /// order. `"all"` expands to every remaining discovered provider at that
    /// position in usage-rank order;
    /// without `"all"` this is a strict allowlist. An explicit list bypasses
    /// `max_provider_blocks`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_list: Vec<String>,
    /// Sidebar pane width as a percentage of each view, capped by `max_cols`.
    /// Unset uses 30% above 240 view columns and 25% at or below; an explicit
    /// value stays fixed and is clamped to 10-90 when used. A room-wide `a`/`d`
    /// width selection outranks this percentage and the cap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width_percent: Option<u16>,
    /// Cap on the sidebar pane width in columns. Reconcile converges live panes
    /// toward the configured percentage up to this cap unless a room-wide
    /// `a`/`d` width selection is present.
    pub max_cols: NonZeroU16,
    /// How the agent-cards scrollbar shows when the cards overflow. `auto`
    /// (default) paints it only while the viewport moves and hides it once the
    /// view settles; `always` keeps it up; `never` removes it. Resolved
    /// producer-side onto the snapshot like the rest of `[theme.display]`.
    pub scrollbar: ScrollbarMode,
    /// How much detail resting agent cards show. `auto` keeps the standard card
    /// shape; `expanded` shows every card's subagent section; `compact` trims
    /// resting cards by status while the selected card opens to the full form.
    /// Resolved producer-side onto the snapshot like the rest of
    /// `[theme.display]`.
    pub card_density: CardDensityMode,
    /// The context meter's color stops - where the card's context read leaves
    /// calm green and reaches yellow, amber, and red. Display-only; it tunes the
    /// color ramp, never the store.
    pub context_meter: ContextMeterConfig,
    /// The provider dashboard's budget-bar color zones - where the draining
    /// mana bar leaves green for yellow, amber, and red as the remaining budget
    /// shrinks. Display-only; it tunes the color ramp, never the store.
    pub budget_bar: BudgetBarConfig,
    /// How far the selected-card band and unread-row wash step off the
    /// `selection_bg` panel, in units of 0.01 OKLab lightness. Display-only.
    pub highlight_steps: HighlightStepsConfig,
}

impl Default for DisplayConfig {
    fn default() -> Self {
        Self {
            refresh_ms: DEFAULT_REFRESH_MS,
            pixel: PixelMode::default(),
            max_provider_blocks: default_max_provider_blocks(),
            provider_tabs: ProviderTabsMode::default(),
            provider_list: Vec::new(),
            width_percent: None,
            max_cols: default_sidebar_max_cols(),
            scrollbar: ScrollbarMode::default(),
            card_density: CardDensityMode::default(),
            context_meter: ContextMeterConfig::default(),
            budget_bar: BudgetBarConfig::default(),
            highlight_steps: HighlightStepsConfig::default(),
        }
    }
}

impl DisplayConfig {
    pub fn resolved_refresh_ms(&self) -> u16 {
        self.refresh_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS)
    }

    pub fn is_unset(&self) -> bool {
        *self == Self::default()
    }
}

/// The context meter's severity bands: `green` is where the meter starts
/// leaving calm, then `yellow`, `amber`, and `red` name the reached color stops
/// on both axes - the fill percentage and the absolute tokens in the window.
/// Severity is the worse of the two axes, so a large-window model calm by
/// percentage still warms by sheer volume. Below `green` on both axes the meter
/// rests calm green.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextMeterConfig {
    /// Where the meter leaves calm green and starts warming toward yellow.
    pub green: ContextBand,
    /// Where the meter reaches yellow and starts warming toward amber.
    pub yellow: ContextBand,
    /// Where the meter reaches amber and starts warming toward red.
    pub amber: ContextBand,
    /// Where the meter reaches red and stays red.
    pub red: ContextBand,
}

impl Default for ContextMeterConfig {
    fn default() -> Self {
        Self {
            green: ContextBand {
                percent: 50,
                tokens: 128_000,
            },
            yellow: ContextBand {
                percent: 70,
                tokens: 192_000,
            },
            amber: ContextBand {
                percent: 80,
                tokens: 256_000,
            },
            red: ContextBand {
                percent: 90,
                tokens: 384_000,
            },
        }
    }
}

/// One context color stop's thresholds: the stop is reached once *either* axis
/// reaches its value (`value >= threshold`, inclusive).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContextBand {
    /// Fill percentage (0-100) of the context window.
    pub percent: u8,
    /// Absolute tokens occupying the window.
    pub tokens: u64,
}

/// The provider dashboard's budget ramp control points. The draining bar slides
/// the full health ramp green -> gold -> amber -> red, anchored green at a
/// brimming window; each field names the *remaining* budget (in percent) at
/// which the bar reaches that warm stop, with the spans between them
/// interpolated. The nested burn-rate fields color the reset marker by burn
/// rate against elapsed window time once pace leaves the sustainable floor in
/// either direction. A fully spent window's full-width red track is a shape
/// rule independent of these stops.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BudgetBarConfig {
    /// Remaining % at which the draining bar reaches warn (gold); above it the
    /// bar interpolates from green toward this stop.
    pub yellow: u8,
    /// Remaining % at which the bar reaches caution (amber).
    pub amber: u8,
    /// Remaining % at which the bar reaches alarm (red), staying red below it.
    pub red: u8,
    /// Burn-rate control points for the reset marker.
    pub burn_rate: BudgetBurnRateConfig,
}

impl Default for BudgetBarConfig {
    fn default() -> Self {
        Self {
            yellow: 50,
            amber: 25,
            red: 10,
            burn_rate: BudgetBurnRateConfig::default(),
        }
    }
}

/// `[theme.display.budget_bar.burn_rate]`: reset-marker pace control points.
/// Values are percentages of even pace: `100` means budget use matches elapsed
/// window time, `200` means it is burning twice as fast as the reset can
/// sustain. The marker slides gold -> amber -> red past `yellow`; below `green`
/// it cools from the soft tier toward green, saturating at `deep_green` once the
/// renderer's elapsed-share gate admits the cool signal.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BudgetBurnRateConfig {
    /// Pace % below which the marker leaves the soft tier toward green.
    pub green: u16,
    /// Pace % at which the marker reaches full green, staying green below it.
    pub deep_green: u16,
    /// Pace % at which the marker leaves the soft tier for warn (gold).
    pub yellow: u16,
    /// Pace % at which the marker reaches caution (amber).
    pub amber: u16,
    /// Pace % at which the marker reaches alarm (red), staying red above it.
    pub red: u16,
}

impl Default for BudgetBurnRateConfig {
    fn default() -> Self {
        Self {
            green: 67,
            deep_green: 33,
            yellow: 100,
            amber: 150,
            red: 200,
        }
    }
}

/// `[theme.display.highlight_steps]`: how far the selected-card band and the
/// unread-row wash step off the `selection_bg` panel, counted in units of 0.01
/// OKLab lightness: `band = 5` is a 0.05 step. The band recesses below the
/// panel and the wash lifts above it; at truecolor each is its own sub-cell
/// step, while `indexed` is the single one-cell step the 256-color cube takes
/// either side of the panel (band darker, wash lighter) so the cube carries the
/// same ordering the finer truecolor steps draw. Display-only.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct HighlightStepsConfig {
    /// Truecolor: OKLab-lightness units the selected-card band recesses below
    /// `selection_bg`.
    pub band: u8,
    /// Truecolor: OKLab-lightness units the unread-row wash lifts above
    /// `selection_bg`.
    pub wash: u8,
    /// 256-color: the one-cell OKLab-lightness step taken either side of the
    /// panel: band darker, wash lighter.
    pub indexed: u8,
}

impl Default for HighlightStepsConfig {
    fn default() -> Self {
        Self {
            band: 5,
            wash: 1,
            indexed: 4,
        }
    }
}

/// Default column cap on the sidebar pane width: comfortably past the widest
/// card tier while keeping the configured split from swallowing an ultra-wide
/// terminal.
fn default_sidebar_max_cols() -> NonZeroU16 {
    // Provably non-zero literal.
    NonZeroU16::new(72).expect("non-zero literal")
}

/// Default cap on provider blocks in the bottom dashboard.
fn default_max_provider_blocks() -> usize {
    3
}
