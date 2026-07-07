//! Layer 3 — component tokens: the specific UI uses of color. Each names a
//! role (the sessions glyph, a token-total marker) and resolves to a
//! semantic token, so the call site states intent while the hue stays one
//! central decision. Resolution reads the already-resolved [`Palette`] slots,
//! so a scheme or per-slot override flows through every component that aliases
//! it.
//!
//! The `resolve` match below is the single mapping from UI role to meaning.
//! Two kinds of color stay off this layer: runtime-severity tones (the health
//! ramp, breathing pulses) are amount-driven and live on [`Theme`](super::Theme)
//! methods — every scale (context, mana, pace, link, age) slides the green→red
//! ramp through [`heat_tone`](super::Theme::heat_tone) — while the flat
//! `good`/`warn`/`alarm` accessors name the fixed positive/floor/negative chrome
//! (diff churn, trunk markers, gate notices), where naming the tier is the
//! intent. Component tokens are for the fixed categorical and neutral roles,
//! where the slot alone would not say why.
//!
//! Categorical slots (`accent`/`cool`/`meta`) are never emitted bare — every
//! such use names a component here, so intent is visible where color is used.

use ratatui::style::Color;

use super::Palette;

/// A specific UI use of color. Resolves through [`Component::resolve`] to a
/// semantic tone; never to a raw terminal color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Component {
    /// `◎` sessions glyph — the cockpit summary and the W/M store rows.
    Sessions,
    /// The selected worktree's lane bracket spine — the dim selection tone, so
    /// the bracket, band, and bright card spine read as one selection language.
    LaneSpine,
    /// A worktree group header — a neutral, authoritative heading (`body`) that
    /// anchors the group without competing with attention or selection.
    WorktreeHeader,
    /// The `⇡/⇣` commit-delta cluster on a worktree header — the branch facts
    /// rhyme with the worktree name's neutral heading tone.
    BranchDelta,
    /// A pristine worktree at the trunk tip — faint baseline chrome.
    WorktreePristine,
    /// A landed worktree ready to remove — bright positive action tone.
    WorktreeMerged,
    /// A local rebase/merge/cherry-pick in progress — warning tone.
    WorktreeReconciling,
    /// An open pull request for the worktree branch — cool link tone.
    WorktreePrOpen,
    /// A closed pull request for the worktree branch — muted historical tone.
    WorktreePrClosed,
    /// The `◌` cache-read token marker.
    CacheRead,
    /// The `W:`/`M:` timeframe label on a store row.
    StoreLabel,
    /// The `◇` token-total marker.
    TokenTotal,
    /// The process `C` (CPU) marker.
    ProcCpu,
    /// The process `M` (memory) marker.
    ProcMem,
    /// The process `⇅` (I/O) marker.
    ProcIo,
    /// The `↗` output token marker.
    Output,
    /// The `↘` fresh-input token marker — the costliest read wears the `expense`
    /// red, the reddest marker in the sidebar: `alarm` saturated and deepened a
    /// step past the ramp's red stop, so the input read always reads redder than
    /// the context bar's scaled-to-red cache-read run.
    Input,
    /// The `◍` cache-write token marker — the compaction/delegation violet.
    CacheWrite,
    /// The `↻` completed-compaction count marker.
    Compaction,
    /// The `⧉` subagents header.
    SubagentHeader,
    /// The `⇅ rc` remote-control flag.
    RemoteControl,
    /// Which-key chord text inside the help overlay.
    HelpKey,
    /// The unknown-provider fallback for an agent card's name.
    UnknownBrand,
    /// The capability-line window token, by size class: a neutral→cool→accent
    /// salience ramp so a bigger window reads louder without borrowing any
    /// provider identity. Small (`<128k`).
    WindowSmall,
    /// Window token, `128k`+ tier.
    WindowMedium,
    /// Window token, `258k`+ tier.
    WindowLarge,
    /// Window token, `1M`+ tier — the loudest, an accent (never a brand clay).
    WindowHuge,
}

#[cfg(test)]
impl Component {
    /// Every variant, for the exhaustive golden/coverage tests. Keep in sync
    /// with the enum; the no-wildcard match in [`Component::resolve`] (and the
    /// golden table's mirror) makes a new variant a compile error until it is
    /// mapped.
    pub(crate) const ALL: &'static [Component] = &[
        Component::Sessions,
        Component::LaneSpine,
        Component::WorktreeHeader,
        Component::BranchDelta,
        Component::WorktreePristine,
        Component::WorktreeMerged,
        Component::WorktreeReconciling,
        Component::WorktreePrOpen,
        Component::WorktreePrClosed,
        Component::CacheRead,
        Component::StoreLabel,
        Component::TokenTotal,
        Component::ProcCpu,
        Component::ProcMem,
        Component::ProcIo,
        Component::Output,
        Component::Input,
        Component::CacheWrite,
        Component::Compaction,
        Component::SubagentHeader,
        Component::RemoteControl,
        Component::HelpKey,
        Component::UnknownBrand,
        Component::WindowSmall,
        Component::WindowMedium,
        Component::WindowLarge,
        Component::WindowHuge,
    ];
}

impl Component {
    /// The one mapping from UI role to a resolved palette tone.
    pub(crate) fn resolve(self, palette: &Palette) -> Color {
        use Component::*;
        match self {
            Sessions | Output | HelpKey | WindowHuge => palette.accent,
            LaneSpine => palette.selection,
            WorktreeHeader | BranchDelta => palette.body,
            WorktreePristine | WindowSmall => palette.faint,
            WorktreeMerged | ProcMem | CacheRead => palette.good,
            WorktreeReconciling | Compaction => palette.warn,
            WorktreePrOpen | StoreLabel | TokenTotal | ProcCpu | WindowLarge => palette.cool,
            SubagentHeader | RemoteControl | ProcIo | CacheWrite => palette.meta,
            Input => palette.expense,
            WorktreePrClosed | WindowMedium | UnknownBrand => palette.muted,
        }
    }
}
