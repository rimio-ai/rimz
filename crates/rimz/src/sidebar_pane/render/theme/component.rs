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
//! methods — context, mana, link, and age slide the green→red ramp through
//! [`heat_tone`](super::Theme::heat_tone), while pace also has a soft→green
//! under-run tail — and the flat
//! `good`/`warn`/`alarm` accessors name the fixed positive/floor/negative chrome
//! (diff churn, trunk markers, gate notices), where naming the tier is the
//! intent. Component tokens are for the fixed categorical and neutral roles,
//! where the slot alone would not say why.
//!
//! Categorical slots (`accent`/`cool`/`meta`) are never emitted bare — every
//! such use names a component here, so intent is visible where color is used.

use ratatui::style::Color;

use super::Palette;
use super::tone_color;

/// A specific UI use of color. Resolves through [`Component::resolve`] to a
/// semantic tone; never to a raw terminal color.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Component {
    /// `◎` sessions glyph — the cockpit summary and the W/M store rows.
    Sessions,
    /// The active needs-you lens's row count below the make-up buckets.
    NeedsYouFilter,
    /// The selected worktree's lane bracket spine — the dim selection tone, so
    /// the bracket, band, and bright card spine read as one selection language.
    LaneSpine,
    /// The light `▏` beside the selected card's cursor entry — the selection
    /// tone without bold, lighter than the card spine it sits inside.
    EntrySpine,
    /// A worktree group header — a neutral, authoritative heading (`body`) that
    /// anchors the group without competing with attention or selection.
    WorktreeHeader,
    /// The dim repo qualifier after a colliding worktree group label.
    WorktreeQualifier,
    /// The `· team` cohort label on a pipeline line or worktree header.
    TeamLabel,
    /// A completed pipeline stage — the passed dots and the `Done` seal alike;
    /// stage completion in its own right, not a borrowed agent status.
    PipelinePassed,
    /// A hollow future dot for a stage this run already entered: shape says not done, tone says been there.
    PipelineRevisited,
    /// The workflow name leading a workflow strip (`meta`).
    WorkflowBadge,
    /// The `╰─` connector from a detour's parent to the current detour node (`faint`).
    WorkflowDetour,
    /// A workflow node's `×N` visit count while its cap still has room (`muted`).
    WorkflowVisits,
    /// A `×N` visit count whose cap is used up (`warn`).
    WorkflowVisitsCapped,
    /// The no-card state word on an active workflow strip (`body`).
    WorkflowState,
    /// The `parked` word on a parked workflow strip, kept after notice dismissal (`warn`).
    WorkflowParked,
    /// The `#N` linked-PR badge after a worktree name — steady cool link tone;
    /// state stays on the right marker.
    WorktreePrBadge,
    /// The `⇡/⇣` commit-delta cluster on a worktree header — the branch facts
    /// rhyme with the worktree name's neutral heading tone.
    BranchDelta,
    /// A pristine worktree at the trunk tip — faint baseline chrome.
    WorktreePristine,
    /// A landed worktree ready to remove — muted resting verdict; the merge
    /// glyph carries the state and dollar green stays the right rail's green.
    WorktreeMerged,
    /// The inactive chip's quiet fill, marking the history toggle as a control.
    InactiveChip,
    /// A local rebase/merge/cherry-pick in progress — warning tone.
    WorktreeReconciling,
    /// An open pull request for the worktree branch — cool link tone.
    WorktreePrOpen,
    /// A closed, unmerged pull request for the worktree branch — alarm verdict.
    WorktreePrClosed,
    /// A current-head PR conflict on the worktree header and cockpit PR count.
    WorktreePrConflict,
    /// A passing CI verdict for the worktree — the trunk's branch CI, or the
    /// branch's open or merged pull request — success tone.
    WorktreeCiPassing,
    /// A still-running CI verdict for the worktree — warning tone.
    WorktreeCiPending,
    /// A failing CI verdict for the worktree — alarm tone.
    WorktreeCiFailing,
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
    /// The reporting-gap suffix on a not-tracked process row.
    NotTrackedSuffix,
    /// The `tip:` label before a reporting fix.
    NotTrackedTip,
    /// The action text that enables an untracked pane's reporting.
    NotTrackedFix,
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
    /// The `⧖` pending-waits header.
    WaitHeader,
    /// The `⇅ rc` remote-control flag.
    RemoteControl,
    /// The `⇅ rc` remote-control flag when its managed server is down.
    RemoteControlDown,
    /// The `⇅ rc` remote-control flag when the user started the room without it.
    RemoteControlOff,
    /// Which-key chord text inside the help overlay.
    HelpKey,
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
    /// The context meter's expired prompt-cache fill: greyed `alarm`, red
    /// enough to read as lost without looking like the full bar.
    CacheExpired,
    /// The compacted-context empty track while its size is unknown.
    ContextCompacted,
    /// The native-compaction wall and its matching identity-line point label.
    CompactionPoint,
    /// Provider fast mode: a scheme-derived amber, independent of lifecycle state.
    FastMode,
    /// Due-message glyph on an agent card.
    MessageQueued,
    /// Active per-pane typing hold on an agent card.
    Typing,
    /// Starts needing a fix, above the footer.
    ThrottleNotice,
    /// The stall glyph and word while any resource is active.
    StallLeadActive,
    /// The stall glyph and word once every resource is fading.
    StallLeadFading,
    /// An active stall's resource word, distinct from its caution figure.
    StallWord,
    /// An active resource's current whole-percent stall.
    StallFigure,
    /// A fading resource's word, figure and downward marker.
    StallFading,
    /// The seam between stall items.
    StallSeparator,
    /// The named or counted overflow of hidden stall items.
    StallOverflow,
}

#[cfg(test)]
impl Component {
    /// Every variant, for the exhaustive golden/coverage tests. Keep in sync
    /// with the enum; the no-wildcard match in [`Component::resolve`] (and the
    /// golden table's mirror) makes a new variant a compile error until it is
    /// mapped.
    pub(crate) const ALL: &'static [Component] = &[
        Component::Sessions,
        Component::NeedsYouFilter,
        Component::LaneSpine,
        Component::EntrySpine,
        Component::WorktreeHeader,
        Component::WorktreeQualifier,
        Component::TeamLabel,
        Component::PipelinePassed,
        Component::PipelineRevisited,
        Component::WorkflowBadge,
        Component::WorkflowDetour,
        Component::WorkflowVisits,
        Component::WorkflowVisitsCapped,
        Component::WorkflowState,
        Component::WorkflowParked,
        Component::WorktreePrBadge,
        Component::BranchDelta,
        Component::WorktreePristine,
        Component::WorktreeMerged,
        Component::InactiveChip,
        Component::WorktreeReconciling,
        Component::WorktreePrOpen,
        Component::WorktreePrClosed,
        Component::WorktreePrConflict,
        Component::WorktreeCiPassing,
        Component::WorktreeCiPending,
        Component::WorktreeCiFailing,
        Component::CacheRead,
        Component::StoreLabel,
        Component::TokenTotal,
        Component::ProcCpu,
        Component::ProcMem,
        Component::ProcIo,
        Component::NotTrackedSuffix,
        Component::NotTrackedTip,
        Component::NotTrackedFix,
        Component::Output,
        Component::Input,
        Component::CacheWrite,
        Component::Compaction,
        Component::SubagentHeader,
        Component::WaitHeader,
        Component::RemoteControl,
        Component::RemoteControlDown,
        Component::RemoteControlOff,
        Component::HelpKey,
        Component::WindowSmall,
        Component::WindowMedium,
        Component::WindowLarge,
        Component::WindowHuge,
        Component::CacheExpired,
        Component::ContextCompacted,
        Component::CompactionPoint,
        Component::FastMode,
        Component::MessageQueued,
        Component::Typing,
        Component::ThrottleNotice,
        Component::StallLeadActive,
        Component::StallLeadFading,
        Component::StallWord,
        Component::StallFigure,
        Component::StallFading,
        Component::StallSeparator,
        Component::StallOverflow,
    ];
}

impl Component {
    /// The one mapping from UI role to a resolved palette tone.
    pub(crate) fn resolve(self, palette: &Palette) -> Color {
        use Component::*;
        tone_color(match self {
            Sessions | Output | HelpKey | WindowHuge | Typing => palette.accent,
            LaneSpine | EntrySpine => palette.selection,
            WorktreeHeader | BranchDelta | WorkflowState | StallWord | NotTrackedFix => {
                palette.body
            }
            WorktreePristine | WindowSmall | WorkflowDetour | StallSeparator => palette.faint,
            WorktreePrConflict | StallLeadActive | StallFigure => palette.caution,
            ProcMem | CacheRead | RemoteControl | WorktreeCiPassing | PipelinePassed => {
                palette.good
            }
            WorktreeReconciling | Compaction | WorktreeCiPending | PipelineRevisited
            | ThrottleNotice | WorkflowVisitsCapped | WorkflowParked => palette.warn,
            WorktreePrBadge | WorktreePrOpen | StoreLabel | TokenTotal | ProcCpu | WindowLarge => {
                palette.cool
            }
            TeamLabel | SubagentHeader | WaitHeader | ProcIo | CacheWrite | CompactionPoint
            | WorkflowBadge => palette.meta,
            RemoteControlDown | WorktreePrClosed | WorktreeCiFailing => palette.alarm,
            Input => palette.expense,
            CacheExpired => palette.cache_expired,
            FastMode => palette.fast_mode(),
            WorktreeQualifier | WorktreeMerged | InactiveChip | WindowMedium | RemoteControlOff
            | ContextCompacted | WorkflowVisits | StallLeadFading | StallFading | StallOverflow
            | NotTrackedSuffix | NotTrackedTip | MessageQueued | NeedsYouFilter => palette.muted,
        })
    }
}
