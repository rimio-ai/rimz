//! The cross-backend sidebar reconcile planner: one healthy sidebar per
//! working view. Each backend collects its views into [`ViewSidebars`] and
//! executes the [`ReconcilePlan`]; the rule itself lives here, in one place,
//! unit-tested without a mux.

use std::collections::{HashMap, HashSet};

use crate::ids::PaneId;

/// Tally of one in-place sidebar reconcile pass
/// ([`MuxBackend::reconcile_sidebars`](super::MuxBackend::reconcile_sidebars)).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SidebarRecovery {
    /// Views (Zellij tabs / tmux windows) that gained a sidebar this pass —
    /// because they had none, or their only sidebar was unresponsive and was
    /// closed first.
    pub recovered: usize,
    /// Views whose stale-build sidebar was closed and successfully re-added.
    pub restarted: usize,
    /// Stale-build sidebar panes closed as part of reload fallback. Kept out of
    /// `closed` so the user-facing duplicate/unresponsive bucket stays honest.
    pub stale_closed: usize,
    /// Duplicate or unresponsive sidebar panes closed so each view keeps exactly
    /// one live sidebar.
    pub closed: usize,
    /// Views that needed a sidebar but whose in-place add failed — logged and
    /// skipped, never retried.
    pub failed: usize,
    /// Views whose in-place add was deferred for want of an attached client —
    /// Zellij drops the mount on a detached session while the spawned renderer
    /// keeps running, so adding there would only leak. The next reconcile on an
    /// attached session adds them.
    pub deferred: usize,
    /// Kept sidebar panes whose geometry was repaired in place — moved to the
    /// left column and/or resized toward the layout width — renderer untouched.
    pub redocked: usize,
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
    /// Panes known to be live sidebars on the wrong build after the reload
    /// convergence wait. Even an unlocated current-build heartbeat must not
    /// protect these panes from replacement.
    pub stale_panes: HashSet<PaneId>,
    /// Panes whose sidebar serve process was born within
    /// [`crate::sidebar::FRESH_PANE_GRACE`] — too young for a first heartbeat,
    /// so the planner reads "unclaimed" as "still starting", never "wedged".
    /// Keeps back-to-back reloads from closing the sidebar the previous run
    /// just added.
    pub young_panes: HashSet<PaneId>,
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
    pub restart_add: HashSet<String>,
    pub stale_close_views: HashMap<PaneId, String>,
}

/// Plan the reconcile for one session, view by view:
/// - **Working or daemon view** — keep exactly one sidebar pane, close the
///   rest, and add one if none survived, so duplicates collapse to one and a
///   wedged sidebar is replaced. The keeper is the first *claimed* (live) pane;
///   with none claimed, the first *young* pane (serve process inside the
///   fresh-pane grace) is kept tentatively and nothing is added — its first
///   heartbeat simply hasn't landed, and the next pass settles it either way.
///   A young extra beside a claimed keeper still closes: that is a botched-add
///   duplicate, not a starting renderer. The daemon view (`rimzd`) is born with
///   a sidebar beside its managed hosts and earns the same convergence — but
///   never the collapse below, since its hosts are managed, not work.
/// - **Orphan sidebar-only view** — no working pane and no daemon host, so its
///   working siblings all closed but the sidebar never self-closed (a wedged
///   renderer that stopped ticking). Close every sidebar pane and let the view
///   collapse; reload cannot rely on self-close for a renderer that is no longer
///   ticking.
///
/// When a live sidebar is unlocated (a fresh heartbeat carrying no pane id), each
/// view is handled conservatively: keep one physical sidebar as the possible
/// owner, close duplicate panes, add only when an occupied view has none, and
/// leave a single orphan for self-close.
/// First-seen order; shared by both backends so the rule lives in one place and
/// is unit-tested without a mux.
pub(crate) fn plan_reconcile(views: &[ViewSidebars], live: &SidebarLiveness) -> ReconcilePlan {
    let mut plan = ReconcilePlan::default();
    for view in views {
        if view.has_working || view.has_daemon_host {
            let keep = sidebar_to_keep(view, live, live.has_unlocated);
            let close_from = plan.close.len();
            close_unkept_sidebars(view, keep, &mut plan.close);
            record_stale_close_views(view, live, close_from, &mut plan);
            if keep.is_none() {
                plan.add.push(view.view.clone());
                if view
                    .sidebar_panes
                    .iter()
                    .any(|pane| live.stale_panes.contains(pane))
                {
                    plan.restart_add.insert(view.view.clone());
                }
            }
        } else if live.has_unlocated {
            // Orphan sidebar-only view: keep one possible owner for self-close,
            // but still collapse duplicates so a tab never accumulates chrome.
            let keep = sidebar_to_keep(view, live, !view.sidebar_panes.is_empty());
            let close_from = plan.close.len();
            close_unkept_sidebars(view, keep, &mut plan.close);
            record_stale_close_views(view, live, close_from, &mut plan);
        } else {
            // Orphan sidebar-only view: close every sidebar pane so the view
            // collapses. Without a wildcard there is no live owner to preserve.
            let close_from = plan.close.len();
            plan.close.extend(view.sidebar_panes.iter().cloned());
            record_stale_close_views(view, live, close_from, &mut plan);
        }
    }
    plan
}

/// The index of the pane a view keeps, by signal strength: a claimed (live)
/// pane wins, then a young one (heartbeat pending), then — only under
/// `keep_unclaimed` (the unlocated wildcard, or an orphan's possible owner) —
/// the first pane at all.
fn sidebar_to_keep(
    view: &ViewSidebars,
    live: &SidebarLiveness,
    keep_unclaimed: bool,
) -> Option<usize> {
    view.sidebar_panes
        .iter()
        .position(|pane| live.claimed_panes.contains(pane) && !live.stale_panes.contains(pane))
        .or_else(|| {
            view.sidebar_panes.iter().position(|pane| {
                live.young_panes.contains(pane) && !live.stale_panes.contains(pane)
            })
        })
        .or_else(|| {
            keep_unclaimed.then(|| {
                view.sidebar_panes
                    .iter()
                    .position(|pane| !live.stale_panes.contains(pane))
            })?
        })
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

fn record_stale_close_views(
    view: &ViewSidebars,
    live: &SidebarLiveness,
    close_from: usize,
    plan: &mut ReconcilePlan,
) {
    for pane in &plan.close[close_from..] {
        if live.stale_panes.contains(pane) {
            plan.stale_close_views
                .insert(pane.clone(), view.view.clone());
        }
    }
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

    fn live_with_young(claimed: &[&str], young: &[&str]) -> SidebarLiveness {
        SidebarLiveness {
            claimed_panes: claimed.iter().map(|raw| pane(raw)).collect(),
            young_panes: young.iter().map(|raw| pane(raw)).collect(),
            ..SidebarLiveness::default()
        }
    }

    fn unlocated() -> SidebarLiveness {
        SidebarLiveness {
            has_unlocated: true,
            ..SidebarLiveness::default()
        }
    }

    fn unlocated_with_stale(stale: &[&str]) -> SidebarLiveness {
        SidebarLiveness {
            has_unlocated: true,
            stale_panes: stale.iter().map(|raw| pane(raw)).collect(),
            ..SidebarLiveness::default()
        }
    }

    fn daemon_view(sidebars: &[&str]) -> ViewSidebars {
        ViewSidebars {
            view: "0".to_owned(),
            sidebar_panes: sidebars.iter().map(|raw| pane(raw)).collect(),
            has_working: false,
            has_daemon_host: true,
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
    fn reconcile_leaves_a_healthy_daemon_view_untouched() {
        // The daemon view (`rimzd`) has a live sidebar beside managed hosts —
        // intentional, nothing to do.
        assert_eq!(
            plan_reconcile(&[daemon_view(&["terminal_2"])], &live(&["terminal_2"])),
            ReconcilePlan::default(),
        );
    }

    #[test]
    fn reconcile_adds_to_a_daemon_view_missing_its_sidebar() {
        // The daemon view is born with a sidebar; one that lost it gains one
        // back, same as a working view.
        let plan = plan_reconcile(&[daemon_view(&[])], &live(&[]));
        assert_eq!(plan.close, Vec::<PaneId>::new());
        assert_eq!(plan.add, vec!["0".to_owned()]);
    }

    #[test]
    fn reconcile_never_collapses_the_daemon_view() {
        // A wedged (unclaimed) sidebar in the daemon view is replaced in place —
        // the view holds managed hosts, never the sidebar-only orphan collapse.
        let plan = plan_reconcile(&[daemon_view(&["terminal_2"])], &live(&[]));
        assert_eq!(plan.close, vec![pane("terminal_2")]);
        assert_eq!(plan.add, vec!["0".to_owned()]);
    }

    #[test]
    fn reconcile_graces_a_young_unclaimed_lone_sidebar() {
        // A sidebar added seconds ago has no heartbeat yet; reconcile keeps it
        // tentatively and adds nothing — the next pass settles it either way.
        let views = vec![view("15", &["terminal_15"], true)];
        let plan = plan_reconcile(&views, &live_with_young(&[], &["terminal_15"]));
        assert_eq!(plan, ReconcilePlan::default());
    }

    #[test]
    fn reconcile_keeps_one_young_pane_and_closes_the_rest() {
        // Two young unclaimed panes are a cross-talk double-add: keep the
        // first, close the other, and never stack an add on top.
        let views = vec![view("15", &["terminal_15", "terminal_16"], true)];
        let plan = plan_reconcile(
            &views,
            &live_with_young(&[], &["terminal_15", "terminal_16"]),
        );
        assert_eq!(plan.close, vec![pane("terminal_16")]);
        assert!(plan.add.is_empty(), "a young keeper means no add");
    }

    #[test]
    fn reconcile_closes_a_young_extra_beside_a_claimed_keeper() {
        // Young or not, an extra sidebar pane beside a claimed live one is a
        // botched-add duplicate, never a starting renderer worth waiting on.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let plan = plan_reconcile(&views, &live_with_young(&["terminal_15"], &["terminal_99"]));
        assert_eq!(plan.close, vec![pane("terminal_99")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_prefers_the_claimed_keeper_over_an_earlier_young_pane() {
        // The claimed pane wins the keep even when a young pane precedes it in
        // mux order — a heartbeat is the stronger signal.
        let views = vec![view("15", &["terminal_14", "terminal_15"], true)];
        let plan = plan_reconcile(&views, &live_with_young(&["terminal_15"], &["terminal_14"]));
        assert_eq!(plan.close, vec![pane("terminal_14")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_closes_duplicate_sidebars_under_an_unlocated_wildcard() {
        // An unlocated heartbeat might own one of the panes, but the view still
        // keeps only one physical sidebar.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let plan = plan_reconcile(&views, &unlocated());
        assert_eq!(plan.close, vec![pane("terminal_99")]);
        assert!(plan.add.is_empty(), "one possible owner remains in the tab");
    }

    #[test]
    fn reconcile_prefers_a_claimed_sidebar_when_collapsing_unlocated_duplicates() {
        // A claimed pane is the best owner signal; the unlocated wildcard only
        // protects an unclaimed pane when no claimed one exists in the view.
        let views = vec![view("15", &["terminal_15", "terminal_99"], true)];
        let wildcard_with_claim = SidebarLiveness {
            claimed_panes: [pane("terminal_99")].into(),
            has_unlocated: true,
            ..SidebarLiveness::default()
        };
        let plan = plan_reconcile(&views, &wildcard_with_claim);
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
        let plan = plan_reconcile(
            &[daemon_view(&["terminal_2", "terminal_3"])],
            &live(&["terminal_2"]),
        );
        assert_eq!(plan.close, vec![pane("terminal_3")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_leaves_an_orphan_view_alone_under_an_unlocated_wildcard() {
        // An unlocated live sidebar might own the orphan's pane, so don't close
        // blind — leave it for self-close.
        let views = vec![view("16", &["terminal_16"], false)];
        assert_eq!(
            plan_reconcile(&views, &unlocated()),
            ReconcilePlan::default()
        );
    }

    #[test]
    fn reconcile_collapses_duplicate_orphan_sidebars_under_an_unlocated_wildcard() {
        // Keep one possible owner for self-close, but close duplicate chrome.
        let views = vec![view("16", &["terminal_16", "terminal_17"], false)];
        let plan = plan_reconcile(&views, &unlocated());
        assert_eq!(plan.close, vec![pane("terminal_17")]);
        assert!(plan.add.is_empty());
    }

    #[test]
    fn reconcile_with_an_unlocated_live_sidebar_never_closes_blind() {
        // A fresh heartbeat with no pane id is a wildcard for the last physical
        // sidebar in a view; only add to a working view that has none at all.
        let views = vec![view("15", &["terminal_15"], true), view("12", &[], true)];
        let plan = plan_reconcile(&views, &unlocated());
        assert!(plan.close.is_empty(), "never close blind under a wildcard");
        assert_eq!(plan.add, vec!["12".to_owned()]);
    }

    #[test]
    fn unlocated_wildcard_does_not_protect_a_known_stale_pane() {
        // Reload can know a located pane is still on the old build while another
        // current-build heartbeat has no pane id. The wildcard preserves only an
        // unknown owner; it must not shield the known stale pane.
        let views = vec![view("15", &["terminal_15"], true)];
        let plan = plan_reconcile(&views, &unlocated_with_stale(&["terminal_15"]));
        assert_eq!(plan.close, vec![pane("terminal_15")]);
        assert_eq!(plan.add, vec!["15".to_owned()]);
        assert!(plan.restart_add.contains("15"));
        assert_eq!(
            plan.stale_close_views.get(&pane("terminal_15")),
            Some(&"15".to_owned()),
        );
    }

    #[test]
    fn unlocated_wildcard_keeps_a_non_stale_candidate_beside_a_stale_one() {
        let views = vec![view("15", &["terminal_15", "terminal_16"], true)];
        let plan = plan_reconcile(&views, &unlocated_with_stale(&["terminal_15"]));
        assert_eq!(plan.close, vec![pane("terminal_15")]);
        assert!(plan.add.is_empty(), "the non-stale possible owner remains");
        assert!(plan.restart_add.is_empty());
        assert_eq!(
            plan.stale_close_views.get(&pane("terminal_15")),
            Some(&"15".to_owned()),
        );
    }
}
