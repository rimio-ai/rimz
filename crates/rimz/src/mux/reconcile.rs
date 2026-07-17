//! The cross-backend sidebar repair planner and transaction executor: one
//! healthy sidebar per working view. Each backend collects [`ViewSidebars`]
//! and provides native add and close effects; policy and accounting stay pure
//! and backend-neutral.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use crate::ids::{MuxName, PaneId};
use crate::mux::SidebarPaneOptions;

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
}

/// One view's sidebar panes in mux order and whether the view contains work or
/// managed daemon hosts. A view with neither is an orphan sidebar-only view.
pub(crate) struct ViewSidebars {
    pub view: String,
    pub sidebar_panes: Vec<PaneId>,
    pub has_working: bool,
    pub has_daemon_host: bool,
}

/// One serialized repair transaction. Replacement keeps `old` alive until a
/// new pane mounts in the intended view and publishes a heartbeat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ViewVerdict {
    CloseDuplicates {
        view: String,
        close: Vec<PaneId>,
    },
    Add {
        view: String,
    },
    Replace {
        view: String,
        old: PaneId,
        close: Vec<PaneId>,
    },
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

    pub(crate) fn remaining_from(&self, index: usize) -> usize {
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
        let view = verdict.view();
        let result = match verdict {
            ViewVerdict::CloseDuplicates { close: panes, .. } => {
                for pane in panes {
                    if let Err(error) = close(pane) {
                        return reconcile_failure(&plan, report, index, view, error);
                    }
                    report.closed += 1;
                }
                Ok(())
            }
            ViewVerdict::Add { .. } => match if defer_adds {
                Ok(ReconcileAddOutcome::Deferred)
            } else {
                add(view)
            } {
                Ok(ReconcileAddOutcome::Verified) => {
                    report.recovered += 1;
                    Ok(())
                }
                Ok(ReconcileAddOutcome::VerifiedMisdocked) => {
                    report.recovered += 1;
                    report.misdocked += 1;
                    Ok(())
                }
                Ok(ReconcileAddOutcome::Deferred) => {
                    if !defer_adds {
                        report.deferred += 1;
                    }
                    Ok(())
                }
                Err(error) => Err(error),
            },
            ViewVerdict::Replace { close: panes, .. } => match if defer_adds {
                Ok(ReconcileAddOutcome::Deferred)
            } else {
                add(view)
            } {
                Ok(ReconcileAddOutcome::Deferred) => {
                    if !defer_adds {
                        report.deferred += 1;
                    }
                    Ok(())
                }
                Ok(outcome) => {
                    for pane in panes {
                        if let Err(error) = close(pane) {
                            return reconcile_failure(&plan, report, index, view, error);
                        }
                        report.closed += 1;
                    }
                    report.recovered += 1;
                    report.misdocked +=
                        usize::from(outcome == ReconcileAddOutcome::VerifiedMisdocked);
                    Ok(())
                }
                Err(error) => Err(error),
            },
        };
        if let Err(error) = result {
            return reconcile_failure(&plan, report, index, view, error);
        }
    }
    None
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

/// Wait until the newly-mounted pane publishes a fresh heartbeat for the
/// expected executable generation. Executors call this before committing a
/// replacement by closing its old pane.
pub(crate) fn wait_for_sidebar_heartbeat(
    opts: &SidebarPaneOptions,
    mux: MuxName,
    pane: &PaneId,
    build: &str,
) -> bool {
    #[cfg(feature = "testkit")]
    if opts
        .extra_env
        .get("RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT")
        .is_some_and(|value| value == "1")
    {
        return true;
    }
    let Ok(runtime) = crate::store::RuntimePaths::for_workspace(opts.workspace_id.clone()) else {
        return false;
    };
    let deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if crate::sidebar::fresh_sidebar_heartbeats(&runtime)
            .into_iter()
            .any(|heartbeat| {
                heartbeat.mux == mux
                    && heartbeat.session_name == opts.session_name
                    && heartbeat.pane_id.as_ref() == Some(pane)
                    && heartbeat.build.as_deref() == Some(build)
            })
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
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
                    let old = view.sidebar_panes[0].clone();
                    plan.verdicts.push(ViewVerdict::Replace {
                        view: view.view.clone(),
                        old,
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
                    old: pane("terminal_1"),
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
    fn orphan_views_close_without_adding() {
        let plan = plan_reconcile(
            &[view("orphan", &["terminal_1", "terminal_2"], false)],
            &SidebarLiveness::default(),
        );
        assert_eq!(
            plan.verdicts,
            vec![ViewVerdict::CloseDuplicates {
                view: "orphan".to_owned(),
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
                old: pane("terminal_1"),
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
                old: pane("terminal_1"),
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
                old: pane("terminal_1"),
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
                old: pane("terminal_2"),
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
                    old: pane("terminal_2"),
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
