//! Requested and pending spend-history rebuild outcomes.

use std::path::PathBuf;

use jiff::civil::Date;
use serde::{Deserialize, Serialize};

use crate::disk::atomic::AtomicErr;
use crate::disk::lock::LockErr;
use crate::disk::paths::{PathErr, SpendingHistoryPaths};
use crate::ids::LoginKey;

use super::history::{self, HistoryErr, Maintenance};

/// Saved days selected by a history rebuild.
#[derive(Clone, Copy, Debug)]
pub enum RebuildScope {
    Pending,
    All,
}

/// One appended replacement account-day.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RebuiltDay {
    pub login: LoginKey,
    pub day: Date,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_usd: Option<f64>,
    pub after_usd: f64,
}

/// An account-day retained without replacement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeptDay {
    pub login: LoginKey,
    pub day: Date,
}

/// Effects of one rebuild, across every discovered history account.
#[derive(Clone, Debug, Default)]
pub struct RebuildReport {
    pub days: Vec<Date>,
    pub rebuilt: Vec<RebuiltDay>,
    pub unchanged: usize,
    pub kept: Vec<KeptDay>,
    pub files: usize,
    pub deferred_days: Vec<KeptDay>,
}

/// Why a requested rebuild could not finish.
#[derive(Debug, thiserror::Error)]
pub enum RebuildErr {
    #[error("transcript discovery was incomplete; saved spend history was not rebuilt")]
    DiscoveryIncomplete,
    #[error("spending producer lock is busy or unavailable; retry rimz stats --rebuild")]
    ProducerBusy,
    #[error("spending history maintenance is unavailable; retry rimz stats --rebuild")]
    MaintenanceUnavailable,
    #[error("spending history format mark is unusable: {0}")]
    FormatUnusable(PathBuf),
    #[error("spending producer lock is unavailable: {0}; retry rimz stats --rebuild")]
    ProducerLock(#[source] LockErr),
    #[error("spending history maintenance is unavailable: {0}; retry rimz stats --rebuild")]
    MaintenanceLock(#[source] LockErr),
    #[error(transparent)]
    Paths(#[from] PathErr),
    #[error("{source}")]
    History {
        #[source]
        source: AtomicErr,
        report: Box<RebuildReport>,
    },
}

impl From<AtomicErr> for RebuildErr {
    fn from(source: AtomicErr) -> Self {
        Self::History {
            source,
            report: Box::default(),
        }
    }
}

pub(super) struct RebuildRun {
    pub(super) scope: RebuildScope,
    pub(super) report: Result<RebuildReport, RebuildErr>,
}

impl RebuildRun {
    pub(super) fn new(scope: RebuildScope) -> Self {
        Self {
            scope,
            report: Ok(RebuildReport::default()),
        }
    }
}

pub(super) fn maintenance(paths: &SpendingHistoryPaths) -> Result<Maintenance, RebuildErr> {
    match history::try_maintain(paths) {
        Ok(Some(hold)) => Ok(hold),
        Ok(None) if matches!(history::format_mark(paths), history::FormatMark::Unusable) => {
            Err(RebuildErr::FormatUnusable(paths.format()))
        }
        Ok(None) => Err(RebuildErr::MaintenanceUnavailable),
        Err(HistoryErr::Atomic(error)) => Err(error.into()),
        Err(HistoryErr::Lock(error)) => Err(RebuildErr::MaintenanceLock(error)),
    }
}
