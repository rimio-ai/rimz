//! Reduced workspace snapshot. The sidebar consumes this via
//! `rimz sidebar snapshot --json`; correctness lives in the feed files and
//! event log this is derived from.
//!
//! The pipeline reads bottom-up: [`fold`] resumes the event-log rollup,
//! [`project`] reduces lifecycle events into agent state, [`panes`] binds
//! agents to live panes, [`process`] classifies non-agent commands,
//! [`view`] assembles the renderer contract, and [`assemble`] owns the
//! read entry points and the persisted-snapshot fast path.

mod assemble;
mod fold;
mod panes;
mod process;
mod project;
#[cfg(test)]
mod testkit;
mod view;

use std::io;
use std::path::PathBuf;

use crate::ledger::atomic;
use crate::ledger::event_log::EventLogErr;
use crate::ledger::feed_store::FeedStoreErr;

pub(crate) use assemble::rebuild;
pub use assemble::{build_from, build_with_cursor, read_fresh_latest};
pub use fold::RollupCursor;
pub use fold::agent_tombstones_for_events;
pub(crate) use fold::{
    EventCarryover, agent_rollup_with_carryover, catch_up_rollup, read_carryover,
    reseed_rollup_cache_for_rotation, write_carryover,
};
pub use panes::SidebarOwnView;
pub use process::command_agent_kind;
pub use view::{
    SidebarProviderPanel, SidebarResolverState, SidebarRow, SidebarRowKind, SidebarSnapshot,
    SidebarStatusCount, SidebarSubAgent, SidebarWorktreeGroup, SidebarWorktreeKind,
};

#[derive(Debug, thiserror::Error)]
pub enum SnapshotErr {
    #[error(transparent)]
    FeedStore(#[from] FeedStoreErr),
    #[error(transparent)]
    EventLog(#[from] EventLogErr),
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error("io error on {path}: {source}")]
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
