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
pub mod agents_spec;
pub mod autoping;
pub mod binding_log;
pub mod bridge;
pub mod build_id;
pub mod child_process;
pub mod config;
pub mod diag;
pub mod feed;
pub mod ids;
pub mod launch;
pub mod ledger;
pub mod message;
pub mod mux;
pub mod notify_log;
pub mod observability;
pub mod osc;
pub mod petname;
pub mod proc;
pub mod reload;
pub mod remote;
pub mod remote_control;
pub mod resolver;
pub mod resume;
pub mod rotating_log;
pub mod run;
pub mod schema;
pub mod sidebar;
pub mod sidebar_pane;
pub mod sock;
pub mod target;
pub mod trust;
pub mod tui;
pub mod workspace;
pub mod worktree;
pub(crate) mod worktree_include;

pub use crate::agents::{AccountUsageSnapshot, ExtraCredits, SpendTally, SpendWindow};
pub use crate::bridge::{BridgeErr, BridgeOutcome, ExpectedFrame};
pub use crate::feed::{
    AbandonReason, ElevatedAgent, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod,
    ResolverStep, ResolverStepState, RuntimeOwner, RuntimeOwnerKind, Surface,
};
pub use crate::ids::{
    EventId, MessageId, MuxName, PaneId, RequestId, ResolverId, RunId, SidebarInstanceId, ViewKind,
    WorkspaceId,
};
pub use crate::ledger::{
    AgentCard, Ledger, PaneAgent, ProcessCard, ProcessState, RowCallSplit, RowCard, RuntimePaths,
    RuntimeProjection, RuntimeScope, SidebarLinkFreshness, SidebarLinkHealth, SidebarOwnView,
    SidebarProviderPanel, SidebarResolverState, SidebarRow, SidebarSnapshot, SidebarStatusCount,
    SidebarSubAgent, SidebarWorktreeGroup, SidebarWorktreeKind, StatePaths, TruthNotice,
    WorkspaceRecord, lead_unread_row,
};
pub use crate::schema::event::EventEnvelope;
pub use crate::target::TargetErr;
pub use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};
