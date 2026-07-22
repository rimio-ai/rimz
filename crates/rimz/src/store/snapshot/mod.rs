//! Reduced workspace snapshot. The sidebar consumes this via
//! `rimz sidebar snapshot --json`; correctness lives in the event log this is
//! derived from.
//!
//! The pipeline reads bottom-up: [`fold`] resumes the event-log rollup,
//! [`project`] reduces lifecycle events into agent state, [`panes`] binds
//! agents to live panes, [`process`] classifies non-agent commands, [`view`]
//! assembles the renderer contract through its live/group/provider submodules,
//! and [`assemble`] owns the read entry points and the persisted-snapshot fast
//! path.

mod assemble;
mod fold;
mod panes;
mod process;
mod project;
mod row;
#[cfg(test)]
mod testkit;
mod view;

use std::io;
use std::path::PathBuf;

use crate::store::atomic;
use crate::store::event_log::EventLogErr;

pub(crate) use assemble::rebuild;
pub use assemble::{build_from, build_with_cursor, read_fresh_latest};
pub(crate) use fold::{
    EventCarryover, catch_up_rollup, read_carryover, reseed_rollup_cache_for_rotation,
    write_carryover,
};
pub use fold::{ResumeOutcome, RollupCursor};
pub(crate) use fold::{lifecycle_follow_seed, lifecycle_log_generation};
pub use panes::{
    HookPaneRecoveryCandidate, HookPaneRecoveryContext, HookPaneRecoveryMethod,
    HookPaneRecoveryPhase, HookPaneRecoverySelection,
};
pub(crate) use panes::{
    LazyAgentPairingDiagnostic, LazyAgentPairingResult, compute_lazy_agent_pairings,
};
pub use panes::{SidebarOwnView, pane_start_allows_bind, stamped_agent_for_pane};
pub use process::pane_agent_kind;
pub(crate) use process::pane_worktree_path;
pub(crate) use process::{command_is_sidebar_chrome, process_is_active};
pub use row::{
    AgentCard, PaneAgent, ProcessCard, ProcessState, RowCallSplit, RowCard, SidebarRow,
    SidebarSubAgent, compose_channel,
};
pub use view::{AgentWorktreeGroup, group_live_agents_by_worktree};
pub use view::{
    DailyBudgetView, PresenceSample, RemoteControlBadge, RuntimeReapInputs, SNAPSHOT_VERSION,
    SidebarLinkFreshness, SidebarLinkHealth, SidebarPresence, SidebarProviderPanel,
    SidebarSnapshot, SidebarStatusCount, SidebarWorktreeGroup, SidebarWorktreeKind, TruthNotice,
    WorktreePrCi, WorktreePrState, WorktreeTrunkSync, actionable_unread_count, lead_unread_row,
    triage_key,
};
pub(crate) use view::{format_plan_label, sort_windows};

#[derive(Debug, thiserror::Error)]
pub enum SnapshotErr {
    #[error(transparent)]
    EventLog(#[from] EventLogErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("cannot access {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("json parse error on {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

pub type Result<T> = std::result::Result<T, SnapshotErr>;
