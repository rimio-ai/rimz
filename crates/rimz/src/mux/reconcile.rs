//! The cross-backend sidebar repair planner and transaction executor: one
//! healthy sidebar per working view. Each backend collects [`ViewSidebars`]
//! and provides native add and close effects; policy and accounting stay pure
//! and backend-neutral.

use crate::ids::PaneId;
use std::collections::{HashMap, HashSet};

/// Tally of one in-place sidebar repair pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidebarRecovery {
    /// Views that gained a verified sidebar, including add-before-close replacements.
    pub recovered: usize,
    /// Duplicate, orphaned, or replaced sidebar panes closed.
    pub closed: usize,
    /// Views whose transaction could not complete; the executor stops at the first.
    pub failed: usize,
    /// Views deferred because their backend cannot mount into a detached session.
    pub deferred: usize,
    /// Kept sidebar panes whose geometry was repaired in place.
    pub redocked: usize,
    /// Working sidebar panes that remain outside the verified dock.
    pub misdocked: usize,
}

/// Fresh renderer claims plus panes inside the first-heartbeat grace window.
#[derive(Clone, Debug, Default)]
pub struct SidebarLiveness {
    pub claimed_panes: HashSet<PaneId>,
    pub has_unlocated: bool,
    pub young_panes: HashSet<PaneId>,
    /// Oldest topology observation this repair pass may treat as current.
    pub topology_floor_ms: Option<u64>,
}

/// One view's sidebar panes in mux order and whether the view contains work or
/// managed daemon hosts. A view with neither is an orphan sidebar-only view.
pub(crate) struct ViewSidebars {
    pub view: String,
    pub sidebar_panes: Vec<PaneId>,
    pub has_working: bool,
    pub has_daemon_host: bool,
}

/// Backend-neutral structural pane used to group native listings for repair.
pub(crate) struct ReconcilePane {
    pub view: String,
    pub pane_id: PaneId,
    pub role: ReconcilePaneRole,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReconcilePaneRole {
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
pub(crate) fn group_reconcile_panes(
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
pub(crate) enum ViewVerdict {
    CloseDuplicates { view: String, close: Vec<PaneId> },
    Add { view: String },
    Replace { view: String, close: Vec<PaneId> },
}

/// Result of a backend's native add and verification effect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReconcileAddOutcome {
    Verified,
    VerifiedMisdocked,
    Deferred,
}

/// First failed transaction, retained for backend-specific warning policy.
#[derive(Debug)]
pub(crate) struct ReconcileFailure {
    pub(crate) view: String,
    pub(crate) error: crate::mux::MuxErr,
}

impl ViewVerdict {
    pub(crate) fn view(&self) -> &str {
        match self {
            Self::CloseDuplicates { view, .. }
            | Self::Add { view }
            | Self::Replace { view, .. } => view,
        }
    }

    pub(crate) fn closes(&self) -> &[PaneId] {
        match self {
            Self::CloseDuplicates { close, .. } | Self::Replace { close, .. } => close,
            Self::Add { .. } => &[],
        }
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcilePlan {
    pub verdicts: Vec<ViewVerdict>,
}

impl ReconcilePlan {
    pub(crate) fn close_panes(&self) -> Vec<PaneId> {
        self.verdicts
            .iter()
            .flat_map(ViewVerdict::closes)
            .cloned()
            .collect()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.verdicts.is_empty()
    }

    fn remaining_from(&self, index: usize) -> usize {
        self.verdicts.len().saturating_sub(index)
    }

    pub(crate) fn has_adds(&self) -> bool {
        self.verdicts
            .iter()
            .any(|verdict| !matches!(verdict, ViewVerdict::CloseDuplicates { .. }))
    }

    pub(crate) fn add_views(&self) -> HashSet<String> {
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
pub(crate) fn execute_reconcile_plan<Add, Close>(
    plan: ReconcilePlan,
    report: &mut SidebarRecovery,
    defer_adds: bool,
    mut add: Add,
    mut close: Close,
) -> Option<ReconcileFailure>
where
    Add: FnMut(&str) -> crate::mux::Result<ReconcileAddOutcome>,
    Close: FnMut(&PaneId) -> crate::mux::Result<()>,
{
    if defer_adds {
        report.deferred += plan
            .verdicts
            .iter()
            .filter(|verdict| !matches!(verdict, ViewVerdict::CloseDuplicates { .. }))
            .count();
    }

    for (index, verdict) in plan.verdicts.iter().enumerate() {
        if let Err(error) =
            execute_reconcile_verdict(verdict, report, defer_adds, &mut add, &mut close)
        {
            return reconcile_failure(&plan, report, index, verdict.view(), error);
        }
    }
    None
}

fn execute_reconcile_verdict<Add, Close>(
    verdict: &ViewVerdict,
    report: &mut SidebarRecovery,
    defer_adds: bool,
    add: &mut Add,
    close: &mut Close,
) -> crate::mux::Result<()>
where
    Add: FnMut(&str) -> crate::mux::Result<ReconcileAddOutcome>,
    Close: FnMut(&PaneId) -> crate::mux::Result<()>,
{
    match verdict {
        ViewVerdict::CloseDuplicates { close: panes, .. } => close_panes(panes, report, close),
        ViewVerdict::Add { view } => {
            let outcome = run_add(view, defer_adds, add)?;
            record_add(outcome, defer_adds, report);
            Ok(())
        }
        ViewVerdict::Replace {
            view, close: panes, ..
        } => {
            let outcome = run_add(view, defer_adds, add)?;
            if outcome != ReconcileAddOutcome::Deferred {
                close_panes(panes, report, close)?;
            }
            record_add(outcome, defer_adds, report);
            Ok(())
        }
    }
}

fn run_add<Add>(
    view: &str,
    defer_adds: bool,
    add: &mut Add,
) -> crate::mux::Result<ReconcileAddOutcome>
where
    Add: FnMut(&str) -> crate::mux::Result<ReconcileAddOutcome>,
{
    if defer_adds {
        Ok(ReconcileAddOutcome::Deferred)
    } else {
        add(view)
    }
}

fn close_panes<Close>(
    panes: &[PaneId],
    report: &mut SidebarRecovery,
    close: &mut Close,
) -> crate::mux::Result<()>
where
    Close: FnMut(&PaneId) -> crate::mux::Result<()>,
{
    for pane in panes {
        close(pane)?;
        report.closed += 1;
    }
    Ok(())
}

fn record_add(
    outcome: ReconcileAddOutcome,
    deferred_precounted: bool,
    report: &mut SidebarRecovery,
) {
    match outcome {
        ReconcileAddOutcome::Verified => report.recovered += 1,
        ReconcileAddOutcome::VerifiedMisdocked => {
            report.recovered += 1;
            report.misdocked += 1;
        }
        ReconcileAddOutcome::Deferred if !deferred_precounted => report.deferred += 1,
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

/// Plan repair one view at a time. Claimed renderers win, then young panes.
/// An unlocated fresh heartbeat conservatively protects one physical pane per
/// occupied view. A wholly unclaimed occupied view uses add-before-close;
/// orphan sidebar-only views are close-only.
pub(crate) fn plan_reconcile(views: &[ViewSidebars], live: &SidebarLiveness) -> ReconcilePlan {
    let mut plan = ReconcilePlan::default();
    for view in views {
        let occupied = view.has_working || view.has_daemon_host;
        if occupied {
            let keep = sidebar_to_keep(view, live, live.has_unlocated);
            match keep {
                Some(keep) => {
                    let close = unkept_sidebars(view, Some(keep));
                    if !close.is_empty() {
                        plan.verdicts.push(ViewVerdict::CloseDuplicates {
                            view: view.view.clone(),
                            close,
                        });
                    }
                }
                None if view.sidebar_panes.is_empty() => {
                    plan.verdicts.push(ViewVerdict::Add {
                        view: view.view.clone(),
                    });
                }
                None => {
                    plan.verdicts.push(ViewVerdict::Replace {
                        view: view.view.clone(),
                        close: view.sidebar_panes.clone(),
                    });
                }
            }
        } else if live.has_unlocated {
            let keep = sidebar_to_keep(view, live, !view.sidebar_panes.is_empty());
            let close = unkept_sidebars(view, keep);
            if !close.is_empty() {
                plan.verdicts.push(ViewVerdict::CloseDuplicates {
                    view: view.view.clone(),
                    close,
                });
            }
        } else if !view.sidebar_panes.is_empty() {
            plan.verdicts.push(ViewVerdict::CloseDuplicates {
                view: view.view.clone(),
                close: view.sidebar_panes.clone(),
            });
        }
    }
    plan
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
mod tests {
    use super::*;
    use crate::ids::MuxName;
    use std::cell::RefCell;

    fn pane(raw: &str) -> PaneId {
        PaneId::from_parts(MuxName::Zellij, raw)
    }

    fn view(id: &str, sidebars: &[&str], has_working: bool) -> ViewSidebars {
        ViewSidebars {
            view: id.to_owned(),
            sidebar_panes: sidebars.iter().map(|raw| pane(raw)).collect(),
            has_working,
            has_daemon_host: false,
        }
    }

    fn live(claimed: &[&str]) -> SidebarLiveness {
        SidebarLiveness {
            claimed_panes: claimed.iter().map(|raw| pane(raw)).collect(),
            ..SidebarLiveness::default()
        }
    }

    fn execute(
        verdicts: Vec<ViewVerdict>,
        defer_adds: bool,
        add: impl FnMut(&str) -> crate::mux::Result<ReconcileAddOutcome>,
        close: impl FnMut(&PaneId) -> crate::mux::Result<()>,
    ) -> (SidebarRecovery, Option<ReconcileFailure>) {
        let mut report = SidebarRecovery::default();
        let failure = execute_reconcile_plan(
            ReconcilePlan { verdicts },
            &mut report,
            defer_adds,
            add,
            close,
        );
        (report, failure)
    }

    fn failure(reason: &str) -> crate::mux::MuxErr {
        crate::mux::MuxErr::Output {
            program: "test".to_owned(),
            reason: reason.to_owned(),
        }
    }

    #[test]
    fn grouping_preserves_first_seen_views_and_classifies_all_roles() {
        let panes = [
            ("second", "terminal_1", ReconcilePaneRole::Working),
            ("first", "terminal_2", ReconcilePaneRole::Sidebar),
            ("second", "terminal_3", ReconcilePaneRole::Sidebar),
            ("first", "terminal_4", ReconcilePaneRole::DaemonHost),
            ("first", "terminal_5", ReconcilePaneRole::Sidebar),
        ]
        .into_iter()
        .map(|(view, raw, role)| ReconcilePane {
            view: view.to_owned(),
            pane_id: pane(raw),
            role,
        });
        let views = group_reconcile_panes(panes);

        assert_eq!(
            views
                .iter()
                .map(|view| view.view.as_str())
                .collect::<Vec<_>>(),
            ["second", "first"]
        );
        assert!(views[0].has_working);
        assert_eq!(views[0].sidebar_panes, [pane("terminal_3")]);
        assert!(views[1].has_daemon_host);
        assert_eq!(
            views[1].sidebar_panes,
            [pane("terminal_2"), pane("terminal_5")]
        );
    }

    #[test]
    fn pane_role_precedence_is_sidebar_then_daemon_host_then_working() {
        assert_eq!(
            ReconcilePaneRole::from_evidence(true, true),
            ReconcilePaneRole::Sidebar
        );
        assert_eq!(
            ReconcilePaneRole::from_evidence(true, false),
            ReconcilePaneRole::Sidebar
        );
        assert_eq!(
            ReconcilePaneRole::from_evidence(false, true),
            ReconcilePaneRole::DaemonHost
        );
        assert_eq!(
            ReconcilePaneRole::from_evidence(false, false),
            ReconcilePaneRole::Working
        );
    }

    #[test]
    fn occupied_views_add_replace_or_close_duplicates() {
        let plan = plan_reconcile(
            &[
                view("missing", &[], true),
                view("wedged", &["terminal_1"], true),
                view("duplicate", &["terminal_2", "terminal_3"], true),
            ],
            &live(&["terminal_2"]),
        );
        assert_eq!(
            plan.verdicts,
            vec![
                ViewVerdict::Add {
                    view: "missing".to_owned(),
                },
                ViewVerdict::Replace {
                    view: "wedged".to_owned(),
                    close: vec![pane("terminal_1")],
                },
                ViewVerdict::CloseDuplicates {
                    view: "duplicate".to_owned(),
                    close: vec![pane("terminal_3")],
                },
            ]
        );
    }

    #[test]
    fn young_and_unlocated_panes_are_kept_conservatively() {
        let live = SidebarLiveness {
            has_unlocated: true,
            young_panes: [pane("terminal_2")].into(),
            ..SidebarLiveness::default()
        };
        let plan = plan_reconcile(
            &[
                view("young", &["terminal_1", "terminal_2"], true),
                view("wildcard", &["terminal_3", "terminal_4"], true),
            ],
            &live,
        );
        assert_eq!(
            plan.close_panes(),
            vec![pane("terminal_1"), pane("terminal_4")]
        );
    }

    #[test]
    fn sidebar_only_subagents_view_closes_without_adding() {
        let plan = plan_reconcile(
            &[view(
                "review subagents",
                &["terminal_1", "terminal_2"],
                false,
            )],
            &SidebarLiveness::default(),
        );
        assert_eq!(
            plan.verdicts,
            vec![ViewVerdict::CloseDuplicates {
                view: "review subagents".to_owned(),
                close: vec![pane("terminal_1"), pane("terminal_2")],
            }]
        );
    }

    #[test]
    fn replacement_closes_every_old_pane_only_after_add_verification() {
        let plan = plan_reconcile(
            &[view("view", &["terminal_1", "terminal_2"], true)],
            &SidebarLiveness::default(),
        );
        assert_eq!(
            plan.verdicts,
            vec![ViewVerdict::Replace {
                view: "view".to_owned(),
                close: vec![pane("terminal_1"), pane("terminal_2")],
            }]
        );
    }

    #[test]
    fn executor_replaces_add_before_ordered_closes() {
        let operations = RefCell::new(Vec::new());
        let (report, failed) = execute(
            vec![ViewVerdict::Replace {
                view: "view".to_owned(),
                close: vec![pane("terminal_1"), pane("terminal_2")],
            }],
            false,
            |view| {
                operations.borrow_mut().push(format!("add:{view}"));
                Ok(ReconcileAddOutcome::Verified)
            },
            |pane| {
                operations.borrow_mut().push(format!("close:{pane}"));
                Ok(())
            },
        );

        assert!(failed.is_none());
        assert_eq!(report.recovered, 1);
        assert_eq!(report.closed, 2);
        assert_eq!(
            operations.into_inner(),
            [
                "add:view",
                "close:zellij:terminal_1",
                "close:zellij:terminal_2"
            ]
        );
    }

    #[test]
    fn executor_failed_or_deferred_replacement_closes_nothing() {
        let closes = RefCell::new(Vec::new());
        let (failed_report, failed) = execute(
            vec![ViewVerdict::Replace {
                view: "failed".to_owned(),
                close: vec![pane("terminal_1")],
            }],
            false,
            |_| Err(failure("add failed")),
            |pane| {
                closes.borrow_mut().push(pane.clone());
                Ok(())
            },
        );
        assert_eq!(failed_report.failed, 1);
        assert!(failed.is_some());
        assert!(closes.borrow().is_empty());

        let (deferred_report, failed) = execute(
            vec![ViewVerdict::Replace {
                view: "deferred".to_owned(),
                close: vec![pane("terminal_2")],
            }],
            true,
            |_| panic!("deferred add must not execute"),
            |_| panic!("deferred replacement must not close"),
        );
        assert!(failed.is_none());
        assert_eq!(deferred_report.deferred, 1);
        assert_eq!(deferred_report.recovered, 0);
        assert_eq!(deferred_report.closed, 0);
    }

    #[test]
    fn executor_counts_only_successful_duplicate_closes() {
        let operations = RefCell::new(Vec::new());
        let (report, failed) = execute(
            vec![ViewVerdict::CloseDuplicates {
                view: "view".to_owned(),
                close: vec![pane("terminal_1"), pane("terminal_2"), pane("terminal_3")],
            }],
            false,
            |_| panic!("close-only plan must not add"),
            |pane| {
                operations.borrow_mut().push(pane.clone());
                if pane.raw() == "terminal_2" {
                    Err(failure("close failed"))
                } else {
                    Ok(())
                }
            },
        );

        assert_eq!(
            operations.into_inner(),
            [pane("terminal_1"), pane("terminal_2")]
        );
        assert_eq!(report.closed, 1);
        assert_eq!(report.failed, 1);
        assert_eq!(failed.expect("failure context").view, "view");
    }

    #[test]
    fn executor_failure_stops_later_transactions_and_counts_each_once() {
        let add_operations = RefCell::new(Vec::new());
        let (report, failed) = execute(
            vec![
                ViewVerdict::Add {
                    view: "first".to_owned(),
                },
                ViewVerdict::Add {
                    view: "failed".to_owned(),
                },
                ViewVerdict::Add {
                    view: "later".to_owned(),
                },
            ],
            false,
            |view| {
                add_operations.borrow_mut().push(view.to_owned());
                if view == "failed" {
                    Err(failure("add failed"))
                } else {
                    Ok(ReconcileAddOutcome::Verified)
                }
            },
            |_| Ok(()),
        );

        assert_eq!(add_operations.into_inner(), ["first", "failed"]);
        assert_eq!(report.recovered, 1);
        assert_eq!(report.failed, 2);
        assert_eq!(failed.expect("failure context").view, "failed");
    }

    #[test]
    fn executor_precounts_deferred_adds_before_close_failure() {
        let (report, failed) = execute(
            vec![
                ViewVerdict::Add {
                    view: "deferred-before".to_owned(),
                },
                ViewVerdict::CloseDuplicates {
                    view: "failed".to_owned(),
                    close: vec![pane("terminal_1")],
                },
                ViewVerdict::Replace {
                    view: "deferred-after".to_owned(),
                    close: vec![pane("terminal_2")],
                },
            ],
            true,
            |_| panic!("deferred add must not execute"),
            |_| Err(failure("close failed")),
        );

        assert!(failed.is_some());
        assert_eq!(report.deferred, 2);
        assert_eq!(report.failed, 2);
    }

    #[test]
    fn executor_counts_successful_misdocked_add() {
        let (report, failed) = execute(
            vec![ViewVerdict::Add {
                view: "view".to_owned(),
            }],
            false,
            |_| Ok(ReconcileAddOutcome::VerifiedMisdocked),
            |_| Ok(()),
        );

        assert!(failed.is_none());
        assert_eq!(report.recovered, 1);
        assert_eq!(report.misdocked, 1);
    }
}
