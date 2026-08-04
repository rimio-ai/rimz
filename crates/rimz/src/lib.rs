//! RimZ core library — domain model, store, multiplexer trait, agent hooks.
//!
//! Read [`crate::agents`] for normalized agent state and [`crate::store`] for
//! durability rules. The product contract lives in the repo's `DESIGN.md`;
//! this crate is its implementation.
//!
//! # Internal API
//!
//! RimZ ships as a binary, and this library target exists so that binary, the
//! test suite, and the benches can link the domain modules. Its items are
//! public to each other, not to dependents: names, signatures, and module
//! layout move with the implementation and every release, without a
//! deprecation cycle or a major version bump.
//!
//! What RimZ supports is the binary's surface — commands and flags, `--json`
//! output, exit codes, config keys, and persisted formats — on the terms
//! `CHANGELOG.md` states. Build against `rimz` the command; treat `cargo add
//! rimz` as unsupported.

#![deny(clippy::print_stdout)]
#![deny(clippy::print_stderr)]

pub mod agent_activity;
pub mod agents;
pub mod build_id;
pub mod channel;
pub mod child_process;
pub mod config;
pub mod daemon_content;
pub mod daemon_view;
pub mod diag;
pub mod disk_usage;
pub mod forge;
pub mod harness;
pub mod ids;
pub mod lane;
pub mod message;
pub mod mux;
pub mod observability;
pub mod osc;
pub mod pane;
pub mod proc;
pub mod reload;
pub mod remote;
pub mod remote_control;
pub mod room;
pub mod sidebar;
pub mod sidebar_pane;
pub mod sock;
pub mod store;
#[cfg(feature = "testkit")]
#[doc(hidden)]
pub mod testkit;
pub mod theme;
pub mod transcript;
pub mod trust;
pub mod tui;
pub mod uninstall;
pub mod update;
pub mod utils;
pub mod web;
pub mod workspace;
pub mod worktree;

pub use crate::agents::{
    AccountUsageSnapshot, ExtraCredits, HeadlineSpec, ProviderAccountScope, ResetCredits,
    SpendTally, SpendWindow, SpendWindowMode,
};
pub use crate::harness::run_wake::RunWakeErr;
pub use crate::harness::target::TargetErr;
pub use crate::ids::{
    AskId, EventId, MessageId, MuxName, PaneId, RunId, SidebarInstanceId, ViewKind, WorkspaceId,
};
pub use crate::pane::{ElevatedAgent, RuntimeOwner, RuntimeOwnerKind};
pub use crate::store::event::EventEnvelope;
pub use crate::store::{RuntimePaths, RuntimeProjection, RuntimeScope, StatePaths, Store};
pub use crate::workspace::{ResolvedWorkspace, WorkspaceResolver};
