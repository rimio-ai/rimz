//! The cross-backend sidebar repair planner: one healthy sidebar per working
//! view. Each backend collects [`ViewSidebars`] and executes the resulting view
//! transactions serially; the policy stays pure and backend-neutral.

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
}
