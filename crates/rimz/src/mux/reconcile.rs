//! The cross-backend sidebar repair planner and transaction executor: one
//! healthy sidebar per working view. Each backend collects [`ViewSidebars`]
//! and provides native add and close effects; policy and accounting stay pure
//! and backend-neutral.

use crate::diag::record::SidebarRepairStep;
use crate::ids::PaneId;
use std::collections::{HashMap, HashSet};

/// Tally and landed structural effects of one in-place sidebar repair pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarRecovery {
    /// Views that gained a verified sidebar, including add-before-close replacements.
    pub recovered: usize,
    /// Duplicate, orphaned, or replaced sidebar panes closed.
    pub closed: usize,
    /// Views whose transaction could not complete; the executor stops at the first.
    pub failed: usize,
    /// Repairs deferred because their backend cannot act on a detached session:
    /// sidebar adds and Zellij geometry repairs alike.
    pub deferred: usize,
    /// The adds among `deferred`, so a launch reopens only for a missing sidebar.
    pub deferred_adds: usize,
    /// Surviving sidebar panes whose geometry was repaired in place.
    pub redocked: usize,
    /// Working sidebar panes that remain outside the verified dock.
    pub misdocked: usize,
    /// Per-view effects that landed, including before a transaction failed.
    pub repairs: Vec<SidebarViewRepair>,
    /// One entry per add, mount-proof cleanup kill, or close that failed.
    pub failures: Vec<SidebarRepairFailure>,
}

/// Which verdicts a pass may plan. Launch into a live session only adds where
/// a view has none; bare reload adds the same and moves no existing pane; the
/// host's heal only closes duplicates beside a claimed one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileScope {
    /// Every verdict: add, add-before-close replace, and duplicate or orphan closes.
    Full,
    /// One add per occupied view without a sidebar pane, and nothing else.
    AddMissing,
    /// The adds [`Self::AddMissing`] plans, with no geometry effect on any
    /// existing sidebar or on a pane of another view: bare reload's pass.
    /// Docking an added sidebar may still narrow, swap or stack the work panes
    /// of its own view.
    MountMissing,
    /// In each view with two or more sidebars and one claimed, close all but
    /// the first claimed one; never a view's last sidebar.
    CloseDuplicates,
}

/// One native operation a pass attempted and the mux refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarRepairFailure {
    /// Native view id: the tmux window id or the Zellij stable tab id.
    pub view: String,
    pub step: SidebarRepairStep,
    /// The pane the operation targeted, when the backend learned its id.
    pub pane: Option<PaneId>,
    pub error: String,
}

/// Structural effects that landed in one native mux view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarViewRepair {
    /// Native view id: the tmux window id or the Zellij stable tab id.
    pub view: String,
    /// Successfully closed panes, in close order.
    pub closed: Vec<PaneId>,
    /// New sidebar after its mount and current-build heartbeat were verified.
    pub added: Option<PaneId>,
}

/// Fresh renderer claims plus panes inside the first-heartbeat grace window.
#[derive(Clone, Debug, Default)]
pub struct SidebarLiveness {
    pub claimed_panes: HashSet<PaneId>,
    pub has_unlocated: bool,
    pub young_panes: HashSet<PaneId>,
}

/// Liveness evidence sampled by a backend for its native pane listing.
pub trait SidebarLivenessSource {
    fn sample(&self) -> SidebarLiveness;
}

impl SidebarLivenessSource for SidebarLiveness {
    fn sample(&self) -> SidebarLiveness {
        self.clone()
    }
}

/// One view's sidebar panes in mux order and whether the view contains work or
/// managed daemon hosts. A view with neither is an orphan sidebar-only view.
pub(super) struct ViewSidebars {
    pub(super) view: String,
    pub(super) sidebar_panes: Vec<PaneId>,
    has_working: bool,
    has_daemon_host: bool,
}

impl ViewSidebars {
    fn occupied(&self) -> bool {
        self.has_working || self.has_daemon_host
    }
}

/// Backend-neutral structural pane used to group native listings for repair.
pub(super) struct ReconcilePane {
    pub(super) view: String,
    pub(super) pane_id: PaneId,
    pub(super) role: ReconcilePaneRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReconcilePaneRole {
    Sidebar,
    Working,
    DaemonHost,
}

impl ReconcilePaneRole {
    pub(super) fn from_evidence(is_sidebar: bool, is_daemon_host: bool) -> Self {
        if is_sidebar {
            Self::Sidebar
        } else if is_daemon_host {
            Self::DaemonHost
        } else {
            Self::Working
        }
    }
}

/// Group participating panes by stable first-seen view order while preserving
/// native sidebar order within each view.
pub(super) fn group_reconcile_panes(
    panes: impl IntoIterator<Item = ReconcilePane>,
) -> Vec<ViewSidebars> {
    let mut views = Vec::new();
    let mut index = HashMap::new();
    for pane in panes {
        let slot = *index.entry(pane.view.clone()).or_insert_with(|| {
            views.push(ViewSidebars {
                view: pane.view,
                sidebar_panes: Vec::new(),
                has_working: false,
                has_daemon_host: false,
            });
            views.len() - 1
        });
        match pane.role {
            ReconcilePaneRole::Sidebar => views[slot].sidebar_panes.push(pane.pane_id),
            ReconcilePaneRole::Working => views[slot].has_working = true,
            ReconcilePaneRole::DaemonHost => views[slot].has_daemon_host = true,
        }
    }
    views
}

/// One serialized repair transaction. Replacement keeps existing panes alive
/// until a new pane mounts in the intended view and publishes a heartbeat.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ViewVerdict {
    CloseDuplicates { view: String, close: Vec<PaneId> },
    Add { view: String },
    Replace { view: String, close: Vec<PaneId> },
}

/// Result of a backend's native add and verification effect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum ReconcileAddOutcome {
    Verified(PaneId),
    VerifiedMisdocked(PaneId),
    Deferred,
}

/// A failed add: the error, the added pane when the backend learned it, and a
/// failed mount-proof cleanup of that pane.
#[derive(Debug)]
pub(super) struct ReconcileAddErr {
    error: Box<crate::mux::MuxErr>,
    pane: Option<PaneId>,
    cleanup: Option<Box<crate::mux::MuxErr>>,
}

impl From<crate::mux::MuxErr> for ReconcileAddErr {
    fn from(error: crate::mux::MuxErr) -> Self {
        Self {
            error: Box::new(error),
            pane: None,
            cleanup: None,
        }
    }
}

impl ReconcileAddErr {
    /// An added pane that failed its mount proof, with its cleanup's result.
    pub(super) fn unproven(
        error: crate::mux::MuxErr,
        pane: PaneId,
        cleanup: crate::mux::Result<()>,
    ) -> Self {
        Self {
            error: Box::new(error),
            pane: Some(pane),
            cleanup: cleanup.err().map(Box::new),
        }
    }
}

/// First failed transaction, retained for backend-specific warning policy.
#[derive(Debug)]
pub(super) struct ReconcileFailure {
    pub(super) view: String,
    pub(super) error: crate::mux::MuxErr,
}

impl ViewVerdict {
    fn view(&self) -> &str {
        match self {
            Self::CloseDuplicates { view, .. }
            | Self::Add { view }
            | Self::Replace { view, .. } => view,
        }
    }

    fn closes(&self) -> &[PaneId] {
        match self {
            Self::CloseDuplicates { close, .. } | Self::Replace { close, .. } => close,
            Self::Add { .. } => &[],
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ReconcilePlan {
    verdicts: Vec<ViewVerdict>,
}

impl ReconcilePlan {
    pub(super) fn close_panes(&self) -> Vec<PaneId> {
        self.verdicts
            .iter()
            .flat_map(ViewVerdict::closes)
            .cloned()
            .collect()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.verdicts.is_empty()
    }

    fn remaining_from(&self, index: usize) -> usize {
        self.verdicts.len().saturating_sub(index)
    }

    pub(super) fn has_adds(&self) -> bool {
        self.verdicts
            .iter()
            .any(|verdict| !matches!(verdict, ViewVerdict::CloseDuplicates { .. }))
    }

    pub(super) fn add_views(&self) -> HashSet<String> {
        self.verdicts
            .iter()
            .filter_map(|verdict| match verdict {
                ViewVerdict::Add { view } | ViewVerdict::Replace { view, .. } => Some(view.clone()),
                ViewVerdict::CloseDuplicates { .. } => None,
            })
            .collect()
    }
}

/// Execute view transactions in planner order. `defer_adds` pre-counts every
/// add transaction before execution so a later close failure preserves the
/// detached-Zellij accounting contract while leaving those views untouched.
pub(super) fn execute_reconcile_plan<Add, Close>(
    plan: ReconcilePlan,
    report: &mut SidebarRecovery,
    defer_adds: bool,
    mut add: Add,
    mut close: Close,
) -> Option<ReconcileFailure>
where
    Add: FnMut(&str, &[PaneId]) -> Result<ReconcileAddOutcome, ReconcileAddErr>,
    Close: FnMut(&PaneId) -> crate::mux::Result<()>,
{
    if defer_adds {
        let adds = plan
            .verdicts
            .iter()
            .filter(|verdict| !matches!(verdict, ViewVerdict::CloseDuplicates { .. }))
            .count();
        report.deferred += adds;
        report.deferred_adds += adds;
    }

    for (index, verdict) in plan.verdicts.iter().enumerate() {
        let mut repair = SidebarViewRepair {
            view: verdict.view().to_owned(),
            closed: Vec::new(),
            added: None,
        };
        let result = execute_reconcile_verdict(
            verdict,
            report,
            &mut repair,
            defer_adds,
            &mut add,
            &mut close,
        );
        if !repair.closed.is_empty() || repair.added.is_some() {
            report.repairs.push(repair);
        }
        if let Err(error) = result {
            return reconcile_failure(&plan, report, index, verdict.view(), error);
        }
    }
    None
}

/// Execute a close-only plan, as [`ReconcileScope::CloseDuplicates`] plans:
/// such a plan holds no add, so none is ever attempted.
pub(super) fn execute_close_plan<Close>(
    plan: ReconcilePlan,
    report: &mut SidebarRecovery,
    close: Close,
) -> Option<ReconcileFailure>
where
    Close: FnMut(&PaneId) -> crate::mux::Result<()>,
{
    let refuse_add = |view: &str, _: &[PaneId]| {
        Err(ReconcileAddErr::from(crate::mux::MuxErr::Output {
            program: "rimz".to_owned(),
            reason: format!("a close-only sidebar pass planned an add in view `{view}`"),
        }))
    };
    execute_reconcile_plan(plan, report, false, refuse_add, close)
}

fn execute_reconcile_verdict<Add, Close>(
    verdict: &ViewVerdict,
    report: &mut SidebarRecovery,
    repair: &mut SidebarViewRepair,
    defer_adds: bool,
    add: &mut Add,
    close: &mut Close,
) -> crate::mux::Result<()>
where
    Add: FnMut(&str, &[PaneId]) -> Result<ReconcileAddOutcome, ReconcileAddErr>,
    Close: FnMut(&PaneId) -> crate::mux::Result<()>,
{
    match verdict {
        ViewVerdict::CloseDuplicates { close: panes, .. } => {
            close_panes(panes, report, repair, close)
        }
        ViewVerdict::Add { view } | ViewVerdict::Replace { view, .. } => {
            let outcome = run_add(view, verdict.closes(), defer_adds, add)
                .map_err(|err| record_add_failure(view, err, report))?;
            if let ReconcileAddOutcome::Verified(pane)
            | ReconcileAddOutcome::VerifiedMisdocked(pane) = &outcome
            {
                repair.added = Some(pane.clone());
                close_panes(verdict.closes(), report, repair, close)?;
            }
            record_add(outcome, defer_adds, report);
            Ok(())
        }
    }
}

fn run_add<Add>(
    view: &str,
    replacing: &[PaneId],
    defer_adds: bool,
    add: &mut Add,
) -> Result<ReconcileAddOutcome, ReconcileAddErr>
where
    Add: FnMut(&str, &[PaneId]) -> Result<ReconcileAddOutcome, ReconcileAddErr>,
{
    if defer_adds {
        Ok(ReconcileAddOutcome::Deferred)
    } else {
        add(view, replacing)
    }
}

fn close_panes<Close>(
    panes: &[PaneId],
    report: &mut SidebarRecovery,
    repair: &mut SidebarViewRepair,
    close: &mut Close,
) -> crate::mux::Result<()>
where
    Close: FnMut(&PaneId) -> crate::mux::Result<()>,
{
    for pane in panes {
        if let Err(error) = close(pane) {
            report.failures.push(SidebarRepairFailure {
                view: repair.view.clone(),
                step: SidebarRepairStep::Close,
                pane: Some(pane.clone()),
                error: error.to_string(),
            });
            return Err(error);
        }
        report.closed += 1;
        repair.closed.push(pane.clone());
    }
    Ok(())
}

fn record_add_failure(
    view: &str,
    err: ReconcileAddErr,
    report: &mut SidebarRecovery,
) -> crate::mux::MuxErr {
    let failure = |step, error: &crate::mux::MuxErr| SidebarRepairFailure {
        view: view.to_owned(),
        step,
        pane: err.pane.clone(),
        error: error.to_string(),
    };
    report
        .failures
        .push(failure(SidebarRepairStep::Add, &err.error));
    if let Some(cleanup) = &err.cleanup {
        report
            .failures
            .push(failure(SidebarRepairStep::CleanupKill, cleanup));
    }
    *err.error
}

fn record_add(
    outcome: ReconcileAddOutcome,
    deferred_precounted: bool,
    report: &mut SidebarRecovery,
) {
    match outcome {
        ReconcileAddOutcome::Verified(_) => report.recovered += 1,
        ReconcileAddOutcome::VerifiedMisdocked(_) => {
            report.recovered += 1;
            report.misdocked += 1;
        }
        ReconcileAddOutcome::Deferred if !deferred_precounted => {
            report.deferred += 1;
            report.deferred_adds += 1;
        }
        ReconcileAddOutcome::Deferred => {}
    }
}

fn reconcile_failure(
    plan: &ReconcilePlan,
    report: &mut SidebarRecovery,
    index: usize,
    view: &str,
    error: crate::mux::MuxErr,
) -> Option<ReconcileFailure> {
    report.failed += plan.remaining_from(index);
    Some(ReconcileFailure {
        view: view.to_owned(),
        error,
    })
}

/// Plan repair one view at a time within `scope`. In the full scope claimed
/// renderers win, then young panes. An unlocated fresh heartbeat
/// conservatively protects one physical pane per occupied view. A wholly
/// unclaimed occupied view uses add-before-close; orphan sidebar-only views
/// are close-only.
pub(super) fn plan_reconcile(
    views: &[ViewSidebars],
    live: &SidebarLiveness,
    scope: ReconcileScope,
) -> ReconcilePlan {
    let verdicts = views.iter().filter_map(|view| match scope {
        ReconcileScope::Full => full_verdict(view, live),
        ReconcileScope::AddMissing | ReconcileScope::MountMissing => {
            (view.occupied() && view.sidebar_panes.is_empty()).then(|| ViewVerdict::Add {
                view: view.view.clone(),
            })
        }
        ReconcileScope::CloseDuplicates => duplicate_verdict(view, live),
    });
    ReconcilePlan {
        verdicts: verdicts.collect(),
    }
}

fn full_verdict(view: &ViewSidebars, live: &SidebarLiveness) -> Option<ViewVerdict> {
    if view.occupied() {
        let keep = sidebar_to_keep(view, live, live.has_unlocated);
        return match keep {
            Some(keep) => close_verdict(view, unkept_sidebars(view, Some(keep))),
            None if view.sidebar_panes.is_empty() => Some(ViewVerdict::Add {
                view: view.view.clone(),
            }),
            None => Some(ViewVerdict::Replace {
                view: view.view.clone(),
                close: view.sidebar_panes.clone(),
            }),
        };
    }
    if live.has_unlocated {
        let keep = sidebar_to_keep(view, live, !view.sidebar_panes.is_empty());
        return close_verdict(view, unkept_sidebars(view, keep));
    }
    close_verdict(view, view.sidebar_panes.clone())
}

fn duplicate_verdict(view: &ViewSidebars, live: &SidebarLiveness) -> Option<ViewVerdict> {
    if view.sidebar_panes.len() < 2 {
        return None;
    }
    let keep = view
        .sidebar_panes
        .iter()
        .position(|pane| live.claimed_panes.contains(pane))?;
    close_verdict(view, unkept_sidebars(view, Some(keep)))
}

fn close_verdict(view: &ViewSidebars, close: Vec<PaneId>) -> Option<ViewVerdict> {
    (!close.is_empty()).then(|| ViewVerdict::CloseDuplicates {
        view: view.view.clone(),
        close,
    })
}

fn sidebar_to_keep(
    view: &ViewSidebars,
    live: &SidebarLiveness,
    keep_unclaimed: bool,
) -> Option<usize> {
    view.sidebar_panes
        .iter()
        .position(|pane| live.claimed_panes.contains(pane))
        .or_else(|| {
            view.sidebar_panes
                .iter()
                .position(|pane| live.young_panes.contains(pane))
        })
        .or_else(|| {
            keep_unclaimed
                .then_some(0)
                .filter(|_| !view.sidebar_panes.is_empty())
        })
}

fn unkept_sidebars(view: &ViewSidebars, keep: Option<usize>) -> Vec<PaneId> {
    view.sidebar_panes
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != keep)
        .map(|(_, pane)| pane.clone())
        .collect()
}

#[cfg(test)]
mod tests;
