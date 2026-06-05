//! The cross-backend sidebar reconcile planner: one healthy sidebar per
//! working view. Each backend collects its views into [`ViewSidebars`] and
//! executes the [`ReconcilePlan`]; the rule itself lives here, in one place,
//! unit-tested without a mux.

use std::collections::HashSet;

use crate::ids::PaneId;

/// Tally of one in-place sidebar reconcile pass
/// ([`MuxBackend::reconcile_sidebars`](super::MuxBackend::reconcile_sidebars)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidebarRecovery {
    /// Views (Zellij tabs / tmux windows) that gained a sidebar this pass —
    /// because they had none, or their only sidebar was unresponsive and was
    /// closed first.
    pub recovered: usize,
    /// Duplicate or unresponsive sidebar panes closed so each view keeps exactly
    /// one live sidebar.
    pub closed: usize,
    /// Views that needed a sidebar but whose in-place add failed — logged and
    /// skipped, never retried.
    pub failed: usize,
}

/// The live sidebars the runtime knows about when a reconcile runs: the panes a
/// fresh, current-protocol heartbeat claims, and whether any fresh heartbeat is
/// *unlocated* (carries no pane id — an old/edge renderer with no per-pane env).
/// An unlocated live sidebar is a wildcard for the last physical sidebar in a
/// view: reconcile keeps one possible owner, while duplicate panes still close
/// so one view never carries multiple sidebars.
#[derive(Clone, Debug, Default)]
pub struct SidebarLiveness {
    pub claimed_panes: HashSet<PaneId>,
    pub has_unlocated: bool,
}

/// One view's sidebar panes (in mux order) and how it is otherwise occupied: a
/// user-working pane (neither a sidebar nor a managed daemon host), and/or a
/// managed daemon host. A view with neither is sidebar-only — an orphan to
/// collapse; one with a daemon host is the intentional `rimzd` view.
pub(crate) struct ViewSidebars {
    pub view: String,
    pub sidebar_panes: Vec<PaneId>,
    pub has_working: bool,
    pub has_daemon_host: bool,
}

/// What a reconcile must do to converge one session to a single live sidebar per
/// working view: close these sidebar panes (duplicates + unclaimed/unresponsive),
/// then add a sidebar to these views (none survived, or none existed).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ReconcilePlan {
    pub close: Vec<PaneId>,
    pub add: Vec<String>,
}

/// Plan the reconcile for one session, view by view:
/// - **Working view** — keep exactly one *claimed* (live) sidebar pane, close the
///   rest, and add one if none survived, so duplicates collapse to one and a
///   wedged sidebar is replaced.
/// - **Orphan sidebar-only view** — no working pane and no daemon host, so its
///   working siblings all closed but the sidebar never self-closed (a wedged
///   renderer that stopped ticking). Close every sidebar pane and let the view
///   collapse; reload cannot rely on self-close for a renderer that is no longer
///   ticking.
/// - **Daemon view** — a sidebar beside managed daemon hosts (`rimzd`) is
///   intentional; leave it alone.
///
/// When a live sidebar is unlocated (a fresh heartbeat carrying no pane id), each
/// view is handled conservatively: keep one physical sidebar as the possible
/// owner, close duplicate panes, add only when a working view has none, and leave
/// a single orphan for self-close.
/// First-seen order; shared by both backends so the rule lives in one place and
/// is unit-tested without a mux.
pub(crate) fn plan_reconcile(views: &[ViewSidebars], live: &SidebarLiveness) -> ReconcilePlan {
    let mut plan = ReconcilePlan::default();
    for view in views {
        if view.has_working {
            let keep = sidebar_to_keep(view, live, live.has_unlocated);
            close_unkept_sidebars(view, keep, &mut plan.close);
            if keep.is_none() {
                plan.add.push(view.view.clone());
            }
        } else if view.has_daemon_host {
            let keep = sidebar_to_keep(view, live, !view.sidebar_panes.is_empty());
            close_unkept_sidebars(view, keep, &mut plan.close);
        } else if live.has_unlocated {
            // Orphan sidebar-only view: keep one possible owner for self-close,
            // but still collapse duplicates so a tab never accumulates chrome.
            let keep = sidebar_to_keep(view, live, !view.sidebar_panes.is_empty());
            close_unkept_sidebars(view, keep, &mut plan.close);
        } else {
            // Orphan sidebar-only view: close every sidebar pane so the view
            // collapses. Without a wildcard there is no live owner to preserve.
            plan.close.extend(view.sidebar_panes.iter().cloned());
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
        .or_else(|| (keep_unclaimed && !view.sidebar_panes.is_empty()).then_some(0))
}

fn close_unkept_sidebars(view: &ViewSidebars, keep: Option<usize>, close: &mut Vec<PaneId>) {
    close.extend(
        view.sidebar_panes
            .iter()
            .enumerate()
            .filter(|(index, _pane)| Some(*index) != keep)
            .map(|(_index, pane)| pane.clone()),
    );
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
            has_unlocated: false,
        }
    }

    #[test]
    fn reconcile_adds_to_a_working_view_without_a_sidebar() {
        let views = vec![view("12", &[], true)];
        let plan = plan_reconcile(&views, &live(&[]));
        assert_eq!(plan.close, Vec::<PaneId>::new());
        assert_eq!(plan.add, vec!["12".to_owned()]);
    }

    #[test]
    fn reconcile_leaves_a_healthy_view_untouched() {
        // One sidebar pane, claimed live, plus a working pane: nothing to do.
        let views = vec![view("15", &["terminal_15"], true)];
        let plan = plan_reconcile(&views, &live(&["terminal_15"]));
        assert_eq!(plan, ReconcilePlan::default());
    }

    #[test]
    fn reconcile_closes_duplicates_keeping_one_live() {
        // Two sidebar panes in one tab; the live one is kept, the other closed.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let plan = plan_reconcile(&views, &live(&["terminal_15"]));
        assert_eq!(plan.close, vec![pane("terminal_99")]);
        assert!(plan.add.is_empty(), "a live sidebar already serves the tab");
    }

    #[test]
    fn reconcile_replaces_an_unresponsive_only_sidebar() {
        // The tab's lone sidebar is not claimed (wedged): close it and add fresh.
        let views = vec![view("15", &["terminal_15"], true)];
        let plan = plan_reconcile(&views, &live(&[]));
        assert_eq!(plan.close, vec![pane("terminal_15")]);
        assert_eq!(plan.add, vec!["15".to_owned()]);
    }

    #[test]
    fn reconcile_collapses_an_orphan_sidebar_only_view() {
        // A sidebar-only view (working siblings all closed, no daemon host) is an
        // orphan a wedged renderer never self-closed: close every sidebar pane so
        // the view collapses, and add nothing — there is no working pane to serve.
        let views = vec![view("16", &["terminal_16", "terminal_17"], false)];
        let plan = plan_reconcile(&views, &live(&["terminal_16"]));
        assert_eq!(plan.close, vec![pane("terminal_16"), pane("terminal_17")]);
        assert!(
            plan.add.is_empty(),
            "no working pane means no sidebar to add"
        );
    }

    #[test]
    fn reconcile_leaves_the_daemon_view_alone() {
        // The daemon view (`rimzd`) has a sidebar beside managed hosts but no
        // working pane — intentional, never collapsed.
        let daemon = ViewSidebars {
            view: "0".to_owned(),
            sidebar_panes: vec![pane("terminal_2")],
            has_working: false,
            has_daemon_host: true,
        };
        assert_eq!(
            plan_reconcile(&[daemon], &live(&["terminal_2"])),
            ReconcilePlan::default(),
        );
    }

    #[test]
    fn reconcile_closes_duplicate_sidebars_under_an_unlocated_wildcard() {
        // An unlocated heartbeat might own one of the panes, but the view still
        // keeps only one physical sidebar.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert_eq!(plan.close, vec![pane("terminal_99")]);
        assert!(plan.add.is_empty(), "one possible owner remains in the tab");
    }

    #[test]
    fn reconcile_prefers_a_claimed_sidebar_when_collapsing_unlocated_duplicates() {
        // A claimed pane is the best owner signal; the unlocated wildcard only
        // protects an unclaimed pane when no claimed one exists in the view.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let unlocated = SidebarLiveness {
            claimed_panes: [pane("terminal_99")].into(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert_eq!(plan.close, vec![pane("terminal_15")]);
        assert!(
            plan.add.is_empty(),
            "the claimed sidebar already serves the tab"
        );
    }

    #[test]
    fn reconcile_closes_duplicate_sidebars_in_the_daemon_view() {
        // The daemon view itself is intentional, but duplicate chrome in that
        // view is not.
        let daemon = ViewSidebars {
            view: "0".to_owned(),
            sidebar_panes: vec![pane("terminal_2"), pane("terminal_3")],
            has_working: false,
            has_daemon_host: true,
        };
        let plan = plan_reconcile(&[daemon], &live(&["terminal_2"]));
        assert_eq!(plan.close, vec![pane("terminal_3")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_leaves_an_orphan_view_alone_under_an_unlocated_wildcard() {
        // An unlocated live sidebar might own the orphan's pane, so don't close
        // blind — leave it for self-close.
        let views = vec![view("16", &["terminal_16"], false)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        assert_eq!(plan_reconcile(&views, &unlocated), ReconcilePlan::default());
    }

    #[test]
    fn reconcile_collapses_duplicate_orphan_sidebars_under_an_unlocated_wildcard() {
        // Keep one possible owner for self-close, but close duplicate chrome.
        let views = vec![view("16", &["terminal_16", "terminal_17"], false)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert_eq!(plan.close, vec![pane("terminal_17")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_with_an_unlocated_live_sidebar_never_closes_blind() {
        // A fresh heartbeat with no pane id is a wildcard for the last physical
        // sidebar in a view; only add to a working view that has none at all.
        let views = vec![view("15", &["terminal_15"], true), view("12", &[], true)];
        let unlocated = SidebarLiveness {
            claimed_panes: HashSet::new(),
            has_unlocated: true,
        };
        let plan = plan_reconcile(&views, &unlocated);
        assert!(plan.close.is_empty(), "never close blind under a wildcard");
        assert_eq!(plan.add, vec!["12".to_owned()]);
    }
}
