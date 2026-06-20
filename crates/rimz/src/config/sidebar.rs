use std::num::NonZeroU16;

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

/// `[sidebar] glow`: whether the post-render transition flashes run over the
/// composed frame. Display-only; with the tier off the base status-head
/// rendering still carries the attention blink.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GlowMode {
    /// Follow the terminal: run when `COLORTERM` or terminfo advertises 24-bit color.
    #[default]
    Auto,
    /// Force the pass on. The lever for a truecolor terminal whose capability
    /// signal is missing from both the environment and terminfo.
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

impl ProviderTabsMode {
    /// Whether a dashboard of `count` providers lays out as a tab rail rather
    /// than stacked blocks: `auto` tabs at three or more, `always` at more than
    /// one, `never` not at all. A tabbed dashboard is bounded by its single
    /// active block, so it shows every provider — `max_provider_blocks` only
    /// trims the stacked layout.
    pub fn tabs(self, count: usize) -> bool {
        match self {
            ProviderTabsMode::Auto => count >= 3,
            ProviderTabsMode::Always => count > 1,
            ProviderTabsMode::Never => false,
        }
    }
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
    /// Most provider blocks the *stacked* dashboard shows before the rest are
    /// elided; a tabbed dashboard is height-bounded by its active block, so it
    /// shows every provider regardless of this cap. Providers are few, so the
    /// cap rarely bites; it bounds the panel height on a box that links many
    /// accounts and explicitly stacks them.
    pub max_provider_blocks: usize,
    /// How provider blocks lay out: `auto` stacks one or two providers and tabs
    /// three or more; `always` tabs whenever more than one provider is present;
    /// `never` always stacks. Resolved producer-side onto the snapshot like
    /// the rest of `[sidebar]`.
    pub provider_tabs: ProviderTabsMode,
    /// Provider kinds to show in the dashboard and their order. Empty means all
    /// discovered providers in the registry's display order (`claude, codex, pi,
    /// opencode`), still governed by `max_provider_blocks`; an explicit list
    /// overrides both the set and the order. `"all"` expands to every remaining
    /// discovered provider at that position (in that same display order);
    /// without `"all"` this is a strict allowlist. An explicit list bypasses
    /// `max_provider_blocks`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_list: Vec<String>,
    /// Cap on the sidebar pane width in columns. Every sidebar pane targets the
    /// standard percentage of the view at this cap; on an ultra-wide terminal
    /// the percentage alone grows absurd, so a pane born above the cap is
    /// shrunk to it once, when it is created. Creation-time only: a manual
    /// resize afterwards sticks.
    pub max_cols: NonZeroU16,
    /// The context meter's color stops — where the card's context read leaves
    /// calm green and reaches yellow, amber, and red. Display-only; it tunes the
    /// colour ramp, never the ledger.
    pub context: ContextSeverityConfig,
    /// The provider dashboard's budget-bar color zones — where the draining
    /// mana bar leaves green for yellow, amber, and red as the remaining
    /// budget shrinks. Display-only; it tunes the colour ramp, never the
    /// ledger.
    pub budget: BudgetZonesConfig,
    /// Preferred comparison target for the worktree header's git stats (the
    /// `+/-` diff, the `⇡`/`⇣` commit delta, and the `≡`/`✓` landed markers).
    /// Tried
    /// first in the trunk ladder, per repo: a repo where the branch doesn't
    /// resolve falls back to the `main` → `master` → remote-default detection,
    /// so one machine-wide value never breaks other projects. Unset means
    /// detection alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trunk: Option<String>,
    /// How the agent-cards scrollbar shows when the cards overflow. `auto`
    /// (default) paints it only while the viewport moves and hides it once the
    /// view settles; `always` keeps it up; `never` removes it. Resolved
    /// producer-side onto the snapshot like the rest of `[sidebar]`.
    pub scrollbar: ScrollbarMode,
    /// Whether the transition-flash tier runs over the composed frame. `auto`
    /// (default) follows the terminal's 24-bit advertisement from `COLORTERM`
    /// or terminfo; `always` forces the pass where both are missing; `never`
    /// keeps the plain base render and pulse. `NO_COLOR` beats every mode.
    /// Resolved producer-side onto the snapshot like the rest of `[sidebar]`.
    pub glow: GlowMode,
    /// How much detail resting agent cards show. `auto` keeps the standard
    /// card shape; `expanded` shows every card's subagent section; `compact`
    /// trims resting cards by status while the selected card opens to the full
    /// form. Resolved producer-side onto the snapshot like the rest of
    /// `[sidebar]`.
    pub card_density: CardDensityMode,
    /// The global multiplexer chord that focuses the sidebar from any pane — a
    /// toggle, so pressing it again returns to your last working pane. Rimz
    /// registers it room-scoped at session birth (tmux as a `bind-key`, Zellij
    /// through the presence plugin), so it never touches your global config.
    /// Default `Alt+p`; set empty or `off` to register nothing and leave your
    /// keybinds untouched.
    pub focus_key: String,
}

impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            refresh_ms: DEFAULT_REFRESH_MS,
            max_provider_blocks: default_max_provider_blocks(),
            provider_tabs: ProviderTabsMode::default(),
            provider_list: Vec::new(),
            max_cols: default_sidebar_max_cols(),
            context: ContextSeverityConfig::default(),
            budget: BudgetZonesConfig::default(),
            trunk: None,
            scrollbar: ScrollbarMode::default(),
            glow: GlowMode::default(),
            card_density: CardDensityMode::default(),
            focus_key: default_focus_key(),
        }
    }
}

impl SidebarConfig {
    pub fn resolved_refresh_ms(&self) -> u16 {
        self.refresh_ms.clamp(MIN_REFRESH_MS, MAX_REFRESH_MS)
    }

    /// The focus-sidebar chord to register and display, or `None` when the user
    /// disabled it (empty / `off` / `none`).
    pub fn focus_key_label(&self) -> Option<&str> {
        let key = self.focus_key.trim();
        if key.is_empty() || key.eq_ignore_ascii_case("off") || key.eq_ignore_ascii_case("none") {
            None
        } else {
            Some(key)
        }
    }
}

/// The shipped default focus-sidebar chord: `Alt+p`, a toggle that reaches the
/// sidebar and returns to the last pane. `Alt` survives the terminal and
/// Zellij's locked mode; the user can rebind or disable it.
pub fn default_focus_key() -> String {
    "Alt+p".to_owned()
}

/// The context meter's severity bands: `green` is where the meter starts
/// leaving calm, then `yellow`, `amber`, and `red` name the reached color stops
/// on both axes — the fill percentage and the absolute tokens in the window.
/// Severity is the worse of the two axes, so a large-window model calm by
/// percentage still warms by sheer volume. Below `green` on both axes the meter
/// rests calm green.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct ContextSeverityConfig {
    /// Where the meter leaves calm green and starts warming toward yellow.
    pub green: ContextBand,
    /// Where the meter reaches yellow and starts warming toward amber.
    pub yellow: ContextBand,
    /// Where the meter reaches amber and starts warming toward red.
    pub amber: ContextBand,
    /// Where the meter reaches red and stays red.
    pub red: ContextBand,
}

impl Default for ContextSeverityConfig {
    fn default() -> Self {
        Self {
            green: ContextBand {
                percent: 40,
                tokens: 100_000,
            },
            yellow: ContextBand {
                percent: 60,
                tokens: 160_000,
            },
            amber: ContextBand {
                percent: 75,
                tokens: 258_000,
            },
            red: ContextBand {
                percent: 90,
                tokens: 420_000,
            },
        }
    }
}

/// One context color stop's thresholds: the stop is reached once *either* axis
/// reaches its value (`value >= threshold`, inclusive).
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct ContextBand {
    /// Fill percentage (0–100) of the context window.
    pub percent: u8,
    /// Absolute tokens occupying the window.
    pub tokens: u64,
}

/// The provider dashboard's budget ramp control points. The draining bar slides
/// the full health ramp green → gold → amber → red, anchored green at a brimming
/// window; each field names the *remaining* budget (in percent) at which the bar
/// reaches that warm stop, with the spans between them interpolated. The nested
/// pace fields color the reset marker by burn rate against elapsed window time
/// once pace leaves the sustainable floor. A fully spent window's full-width red
/// track is a shape rule independent of these stops.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BudgetZonesConfig {
    /// Remaining % at which the draining bar reaches warn (gold); above it the
    /// bar interpolates from green toward this stop.
    pub yellow: u8,
    /// Remaining % at which the bar reaches caution (amber).
    pub amber: u8,
    /// Remaining % at which the bar reaches alarm (red), staying red below it.
    pub red: u8,
    /// Pace control points for the reset marker.
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

/// `[sidebar.budget.pace]`: reset-marker warm-tail control points by burn rate.
/// Values are percentages of even pace: `100` means budget use matches elapsed
/// window time, `200` means it is burning twice as fast as the reset can sustain.
/// A sustainable pace keeps the marker at the soft tier; past `yellow` it slides
/// the warm tail gold → amber → red.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct BudgetPaceConfig {
    /// Pace % at which the marker leaves the soft tier for warn (gold).
    pub yellow: u16,
    /// Pace % at which the marker reaches caution (amber).
    pub amber: u16,
    /// Pace % at which the marker reaches alarm (red), staying red above it.
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
