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
    /// Views whose sidebar add or repair could not complete this pass — logged
    /// and left for a later reconcile.
    pub failed: usize,
    /// Views whose in-place add or geometry repair was deferred for want of an
    /// attached client — Zellij drops pane mounts and relayouts without a screen
    /// thread, so the next reconcile on an attached session performs them.
    pub deferred: usize,
    /// Kept sidebar panes whose geometry was repaired in place — moved to the
    /// left column and/or resized toward the layout width — renderer untouched.
    pub redocked: usize,
    /// Working sidebar panes that remain outside the verified full-height left
    /// dock after the bounded repair path. The renderer is kept so the view
    /// still has a sidebar, and the user-facing reload report surfaces the
    /// geometry failure.
    pub misdocked: usize,
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

    fn panes(raws: &[&str]) -> Vec<PaneId> {
        raws.iter().map(|raw| pane(raw)).collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    fn assert_plan(
        label: &str,
        views: Vec<ViewSidebars>,
        live: SidebarLiveness,
        close: &[&str],
        add: &[&str],
    ) -> ReconcilePlan {
        let plan = plan_reconcile(&views, &live);
        assert_eq!(plan.close, panes(close), "{label}: close");
        assert_eq!(plan.add, strings(add), "{label}: add");
        plan
    }

    #[test]
    fn working_and_daemon_views_converge_to_one_sidebar() {
        for (label, views, live, close, add) in [
            (
                "working missing",
                vec![view("12", &[], true)],
                live(&[]),
                vec![],
                vec!["12"],
            ),
            (
                "working healthy",
                vec![view("15", &["terminal_15"], true)],
                live(&["terminal_15"]),
                vec![],
                vec![],
            ),
            (
                "working duplicate",
                vec![view("15", &["terminal_15", "terminal_99"], true)],
                live(&["terminal_15"]),
                vec!["terminal_99"],
                vec![],
            ),
            (
                "working wedged",
                vec![view("15", &["terminal_15"], true)],
                live(&[]),
                vec!["terminal_15"],
                vec!["15"],
            ),
            (
                "daemon healthy",
                vec![daemon_view(&["terminal_2"])],
                live(&["terminal_2"]),
                vec![],
                vec![],
            ),
            (
                "daemon missing",
                vec![daemon_view(&[])],
                live(&[]),
                vec![],
                vec!["0"],
            ),
            (
                "daemon wedged",
                vec![daemon_view(&["terminal_2"])],
                live(&[]),
                vec!["terminal_2"],
                vec!["0"],
            ),
            (
                "daemon duplicate",
                vec![daemon_view(&["terminal_2", "terminal_3"])],
                live(&["terminal_2"]),
                vec!["terminal_3"],
                vec![],
            ),
        ] {
            let plan = assert_plan(label, views, live, &close, &add);
            assert!(plan.restart_add.is_empty(), "{label}: restart_add");
            assert!(plan.stale_close_views.is_empty(), "{label}: stale map");
        }
    }

    #[test]
    fn orphan_views_collapse_unless_an_unlocated_heartbeat_may_own_one() {
        for (label, live, sidebars, close) in [
            (
                "located orphan",
                live(&["terminal_16"]),
                vec!["terminal_16", "terminal_17"],
                vec!["terminal_16", "terminal_17"],
            ),
            (
                "wildcard lone orphan",
                unlocated(),
                vec!["terminal_16"],
                vec![],
            ),
            (
                "wildcard duplicate orphan",
                unlocated(),
                vec!["terminal_16", "terminal_17"],
                vec!["terminal_17"],
            ),
        ] {
            let plan = assert_plan(label, vec![view("16", &sidebars, false)], live, &close, &[]);
            assert!(plan.restart_add.is_empty(), "{label}: restart_add");
        }
    }

    #[test]
    fn young_panes_are_tentative_until_a_claimed_signal_wins() {
        for (label, live, sidebars, close) in [
            (
                "young lone",
                live_with_young(&[], &["terminal_15"]),
                vec!["terminal_15"],
                vec![],
            ),
            (
                "young duplicate",
                live_with_young(&[], &["terminal_15", "terminal_16"]),
                vec!["terminal_15", "terminal_16"],
                vec!["terminal_16"],
            ),
            (
                "young extra beside claimed",
                live_with_young(&["terminal_15"], &["terminal_99"]),
                vec!["terminal_15", "terminal_99"],
                vec!["terminal_99"],
            ),
            (
                "claimed beats earlier young",
                live_with_young(&["terminal_15"], &["terminal_14"]),
                vec!["terminal_14", "terminal_15"],
                vec!["terminal_14"],
            ),
        ] {
            assert_plan(label, vec![view("15", &sidebars, true)], live, &close, &[]);
        }
    }

    #[test]
    fn unlocated_wildcard_keeps_a_possible_owner_but_not_duplicates() {
        let wildcard_with_claim = SidebarLiveness {
            claimed_panes: [pane("terminal_99")].into(),
            has_unlocated: true,
            ..SidebarLiveness::default()
        };
        for (label, views, live, close, add) in [
            (
                "working duplicate",
                vec![view("15", &["terminal_15", "terminal_99"], true)],
                unlocated(),
                vec!["terminal_99"],
                vec![],
            ),
            (
                "claimed preferred",
                vec![view("15", &["terminal_15", "terminal_99"], true)],
                wildcard_with_claim,
                vec!["terminal_15"],
                vec![],
            ),
            (
                "empty working view still recovers",
                vec![view("15", &["terminal_15"], true), view("12", &[], true)],
                unlocated(),
                vec![],
                vec!["12"],
            ),
        ] {
            assert_plan(label, views, live, &close, &add);
        }
    }

    #[test]
    fn stale_panes_are_replaced_even_under_an_unlocated_wildcard() {
        let replaced = assert_plan(
            "stale only",
            vec![view("15", &["terminal_15"], true)],
            unlocated_with_stale(&["terminal_15"]),
            &["terminal_15"],
            &["15"],
        );
        assert!(replaced.restart_add.contains("15"));
        assert_eq!(
            replaced.stale_close_views.get(&pane("terminal_15")),
            Some(&"15".to_owned()),
        );

        let kept_candidate = assert_plan(
            "stale beside possible owner",
            vec![view("15", &["terminal_15", "terminal_16"], true)],
            unlocated_with_stale(&["terminal_15"]),
            &["terminal_15"],
            &[],
        );
        assert!(kept_candidate.restart_add.is_empty());
        assert_eq!(
            kept_candidate.stale_close_views.get(&pane("terminal_15")),
            Some(&"15".to_owned()),
        );
    }
}
