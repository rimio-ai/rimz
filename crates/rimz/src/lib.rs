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
pub mod binding_log;
pub mod bridge;
pub mod child_process;
pub mod config;
pub mod feed;
pub mod ids;
pub mod ledger;
pub mod mux;
pub mod proc;
pub mod reload;
pub mod remote;
pub mod remote_control;
pub mod resolver;
pub mod resume;
pub mod schema;
pub mod sidebar;
pub mod sidebar_renderer;
pub mod tab_layout;
pub mod trust;
pub mod workspace;
pub mod worktree;

pub use crate::agents::{SpendTally, SpendWindow};
pub use crate::bridge::{BridgeErr, BridgeOutcome, ExpectedFrame};
pub use crate::feed::{
    AbandonReason, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod, ResolverStep,
    ResolverStepState, RuntimeOwner, RuntimeOwnerKind, Surface,
};
pub use crate::ids::{
    EventId, MuxName, PaneId, RequestId, ResolverId, SidebarInstanceId, ViewKind, WorkspaceId,
};
pub use crate::ledger::{
    AgentCard, Ledger, ProcessCard, ProcessState, RowCallSplit, RowCard, RuntimePaths,
    RuntimeProjection, RuntimeScope, SidebarOwnView, SidebarProviderPanel, SidebarResolverState,
    SidebarRow, SidebarSnapshot, SidebarStatusCount, SidebarSubAgent, SidebarWorktreeGroup,
    SidebarWorktreeKind, StatePaths, WorkspaceRecord,
};
pub use crate::schema::event::EventEnvelope;
pub use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};
