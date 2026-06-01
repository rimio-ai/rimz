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
pub mod config;
pub mod feed;
pub mod ids;
pub mod ledger;
pub mod mux;
pub mod proc;
pub mod remote_control;
pub mod resolver;
pub mod schema;
pub mod sidebar;
pub mod trust;
pub mod workspace;

pub use crate::bridge::{BridgeErr, BridgeOutcome, ExpectedFrame};
pub use crate::feed::{
    AbandonReason, FeedItem, FeedKind, FeedStatus, Resolution, ResolutionMethod, ResolverStep,
    ResolverStepState, RuntimeOwner, RuntimeOwnerKind, Surface,
};
pub use crate::ids::{
    EventId, MuxName, PaneId, RequestId, ResolverId, SidebarInstanceId, ViewKind, WorkspaceId,
};
pub use crate::ledger::{
    Ledger, RuntimePaths, RuntimeProjection, RuntimeScope, SidebarOwnView, SidebarProviderPanel,
    SidebarResolverState, SidebarRow, SidebarRowKind, SidebarSnapshot, SidebarStatusCount,
    SidebarSubAgent, SidebarWorktreeGroup, SidebarWorktreeKind, StatePaths, WorkspaceRecord,
};
pub use crate::schema::event::EventEnvelope;
pub use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};
