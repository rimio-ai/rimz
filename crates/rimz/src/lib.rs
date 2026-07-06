//! Rimz core library — domain model, ledger, multiplexer trait, agent hooks.
//!
//! Read [`crate::feed`] for the surface/status/kind vocabulary that names every
//! decision Rimz routes. Read [`crate::ledger`] for durability rules. The
//! product contract lives in the repo's `DESIGN.md`; this crate is its
//! implementation.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

pub mod agent_activity;
pub mod agents;
pub mod bridge;
pub mod build_id;
pub mod channel;
pub mod chat;
pub mod child_process;
pub mod config;
pub mod daemon_content;
pub mod diag;
pub mod feed;
pub mod forge;
pub mod harness;
pub mod ids;
pub mod lane;
pub mod ledger;
pub mod message;
pub mod mux;
pub mod observability;
pub mod osc;
pub mod pane;
pub mod proc;
pub mod reload;
pub mod remote;
pub mod remote_control;
pub mod sidebar;
pub mod sidebar_pane;
pub mod sock;
pub mod storage;
#[cfg(feature = "testkit")]
#[doc(hidden)]
pub mod testkit;
pub mod trust;
pub mod tui;
pub mod uninstall;
pub mod web;
pub mod workspace;
pub mod worktree;

pub use crate::agents::{
    AccountUsageSnapshot, ExtraCredits, HeadlineSpec, ResetCredits, SpendTally, SpendWindow,
    SpendWindowMode,
};
pub use crate::bridge::{BridgeErr, BridgeOutcome, ExpectedFrame};
pub use crate::feed::{
    AbandonReason, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod, Surface,
};
pub use crate::harness::target::TargetErr;
pub use crate::ids::{
    EventId, MessageId, MuxName, PaneId, RequestId, RunId, SidebarInstanceId, ViewKind, WorkspaceId,
};
pub use crate::ledger::event::EventEnvelope;
pub use crate::ledger::{
    AgentCard, Ledger, PaneAgent, PresenceSample, ProcessCard, ProcessState, RowCallSplit, RowCard,
    RuntimePaths, RuntimeProjection, RuntimeScope, SidebarLinkFreshness, SidebarLinkHealth,
    SidebarOwnView, SidebarPresence, SidebarProviderPanel, SidebarRow, SidebarSnapshot,
    SidebarStatusCount, SidebarSubAgent, SidebarWorktreeGroup, SidebarWorktreeKind, StatePaths,
    TruthNotice, WorkspaceRecord, WorktreePrState, WorktreeTrunkSync, actionable_unread_count,
    lead_unread_row, triage_key,
};
pub use crate::pane::{ElevatedAgent, RuntimeOwner, RuntimeOwnerKind};
pub use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};
