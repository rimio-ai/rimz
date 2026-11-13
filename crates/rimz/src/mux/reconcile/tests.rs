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
    add: impl FnMut(&str, &[PaneId]) -> Result<ReconcileAddOutcome, ReconcileAddErr>,
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
        ReconcileScope::Full,
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
        ReconcileScope::Full,
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
        ReconcileScope::Full,
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
        ReconcileScope::Full,
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
        |view, replacing| {
            assert_eq!(replacing, [pane("terminal_1"), pane("terminal_2")]);
            operations.borrow_mut().push(format!("add:{view}"));
            Ok(ReconcileAddOutcome::Verified(pane("terminal_9")))
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
        report.repairs,
        [SidebarViewRepair {
            view: "view".to_owned(),
            closed: vec![pane("terminal_1"), pane("terminal_2")],
            added: Some(pane("terminal_9")),
        }]
    );
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
        |_, replacing| {
            assert_eq!(replacing, [pane("terminal_1")]);
            Err(failure("add failed").into())
        },
        |pane| {
            closes.borrow_mut().push(pane.clone());
            Ok(())
        },
    );
    assert_eq!(failed_report.failed, 1);
    assert!(failed.is_some());
    assert!(closes.borrow().is_empty());
    assert!(failed_report.repairs.is_empty());

    let (deferred_report, failed) = execute(
        vec![ViewVerdict::Replace {
            view: "deferred".to_owned(),
            close: vec![pane("terminal_2")],
        }],
        true,
        |_, _| panic!("deferred add must not execute"),
        |_| panic!("deferred replacement must not close"),
    );
    assert!(failed.is_none());
    assert_eq!(deferred_report.deferred, 1);
    assert_eq!(deferred_report.deferred_adds, 1);
    assert_eq!(deferred_report.recovered, 0);
    assert_eq!(deferred_report.closed, 0);
    assert!(deferred_report.repairs.is_empty());
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
        |_, _| panic!("close-only plan must not add"),
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
    assert_eq!(
        report.repairs,
        [SidebarViewRepair {
            view: "view".to_owned(),
            closed: vec![pane("terminal_1")],
            added: None,
        }]
    );
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
        |view, replacing| {
            assert!(replacing.is_empty(), "Add has no panes to replace");
            add_operations.borrow_mut().push(view.to_owned());
            if view == "failed" {
                Err(failure("add failed").into())
            } else {
                Ok(ReconcileAddOutcome::Verified(pane("terminal_9")))
            }
        },
        |_| Ok(()),
    );

    assert_eq!(add_operations.into_inner(), ["first", "failed"]);
    assert_eq!(report.recovered, 1);
    assert_eq!(report.failed, 2);
    assert_eq!(
        report.repairs,
        [SidebarViewRepair {
            view: "first".to_owned(),
            closed: vec![],
            added: Some(pane("terminal_9")),
        }]
    );
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
        |_, _| panic!("deferred add must not execute"),
        |_| Err(failure("close failed")),
    );

    assert!(failed.is_some());
    assert_eq!(report.deferred, 2);
    assert_eq!(report.deferred_adds, 2);
    assert_eq!(report.failed, 2);
}

#[test]
fn executor_counts_successful_misdocked_add() {
    let (report, failed) = execute(
        vec![ViewVerdict::Add {
            view: "view".to_owned(),
        }],
        false,
        |_, _| Ok(ReconcileAddOutcome::VerifiedMisdocked(pane("terminal_9"))),
        |_| Ok(()),
    );

    assert!(failed.is_none());
    assert_eq!(report.recovered, 1);
    assert_eq!(report.misdocked, 1);
    assert_eq!(
        report.repairs,
        [SidebarViewRepair {
            view: "view".to_owned(),
            closed: vec![],
            added: Some(pane("terminal_9")),
        }]
    );
}

#[test]
fn executor_retains_verified_add_and_partial_closes_on_failure() {
    let (report, failed) = execute(
        vec![ViewVerdict::Replace {
            view: "view".to_owned(),
            close: vec![pane("terminal_1"), pane("terminal_2")],
        }],
        false,
        |_, _| Ok(ReconcileAddOutcome::Verified(pane("terminal_9"))),
        |pane| {
            if pane.raw() == "terminal_2" {
                Err(failure("close failed"))
            } else {
                Ok(())
            }
        },
    );
    assert!(failed.is_some());
    assert_eq!(report.recovered, 0);
    assert_eq!(report.closed, 1);
    assert_eq!(report.failed, 1);
    assert_eq!(
        report.repairs,
        [SidebarViewRepair {
            view: "view".to_owned(),
            closed: vec![pane("terminal_1")],
            added: Some(pane("terminal_9")),
        }]
    );
}

#[test]
fn add_scopes_add_only_to_sidebar_less_occupied_views() {
    for scope in [ReconcileScope::AddMissing, ReconcileScope::MountMissing] {
        let plan = plan_reconcile(
            &[
                view("missing", &[], true),
                view("wedged", &["terminal_1"], true),
                view("duplicate", &["terminal_2", "terminal_3"], true),
                view("orphan", &["terminal_4", "terminal_5"], false),
            ],
            &SidebarLiveness::default(),
            scope,
        );
        assert_eq!(
            plan.verdicts,
            [ViewVerdict::Add {
                view: "missing".to_owned(),
            }],
            "{scope:?}"
        );
    }
}

#[test]
fn close_duplicates_scope_keeps_the_first_claimed_sidebar_only() {
    let live = SidebarLiveness {
        claimed_panes: ["terminal_2", "terminal_3", "terminal_6", "terminal_9"]
            .map(pane)
            .into(),
        has_unlocated: true,
        young_panes: [pane("terminal_4")].into(),
    };
    let plan = plan_reconcile(
        &[
            view("claimed", &["terminal_1", "terminal_2", "terminal_3"], true),
            view("unclaimed", &["terminal_4", "terminal_5"], true),
            view("single", &["terminal_6"], true),
            view("missing", &[], true),
            view("orphan", &["terminal_8", "terminal_9"], false),
        ],
        &live,
        ReconcileScope::CloseDuplicates,
    );
    assert_eq!(
        plan.verdicts,
        [
            ViewVerdict::CloseDuplicates {
                view: "claimed".to_owned(),
                close: vec![pane("terminal_1"), pane("terminal_3")],
            },
            ViewVerdict::CloseDuplicates {
                view: "orphan".to_owned(),
                close: vec![pane("terminal_8")],
            },
        ]
    );
}

#[test]
fn executor_records_one_failure_per_failed_operation() {
    let failures = |report: SidebarRecovery| {
        report
            .failures
            .into_iter()
            .map(|failure| {
                assert!(!failure.error.is_empty());
                (failure.view, failure.step, failure.pane)
            })
            .collect::<Vec<_>>()
    };
    let add = |view: &str| ViewVerdict::Add {
        view: view.to_owned(),
    };

    let (report, _) = execute(
        vec![add("unproven"), add("never")],
        false,
        |view, _| {
            assert_eq!(view, "unproven", "a stopped pass attempts nothing more");
            Err(ReconcileAddErr::unproven(
                failure("mount unproven"),
                pane("terminal_9"),
                Err(failure("kill failed")),
            ))
        },
        |_| Ok(()),
    );
    assert_eq!(
        failures(report),
        [
            (
                "unproven".to_owned(),
                SidebarRepairStep::Add,
                Some(pane("terminal_9"))
            ),
            (
                "unproven".to_owned(),
                SidebarRepairStep::CleanupKill,
                Some(pane("terminal_9"))
            ),
        ]
    );

    let (report, _) = execute(
        vec![add("split")],
        false,
        |_, _| Err(failure("split failed").into()),
        |_| Ok(()),
    );
    assert_eq!(
        failures(report),
        [("split".to_owned(), SidebarRepairStep::Add, None)]
    );

    let (report, _) = execute(
        vec![
            ViewVerdict::CloseDuplicates {
                view: "close".to_owned(),
                close: vec![pane("terminal_1"), pane("terminal_2")],
            },
            add("never"),
        ],
        false,
        |_, _| panic!("a stopped pass attempts nothing more"),
        |pane| {
            if pane.raw() == "terminal_2" {
                Err(failure("close failed"))
            } else {
                Ok(())
            }
        },
    );
    assert_eq!(report.failed, 2);
    assert_eq!(
        failures(report),
        [(
            "close".to_owned(),
            SidebarRepairStep::Close,
            Some(pane("terminal_2"))
        )]
    );

    let (report, _) = execute(
        vec![add("proven")],
        false,
        |_, _| {
            Err(ReconcileAddErr::unproven(
                failure("mount unproven"),
                pane("terminal_7"),
                Ok(()),
            ))
        },
        |_| Ok(()),
    );
    assert_eq!(
        failures(report),
        [(
            "proven".to_owned(),
            SidebarRepairStep::Add,
            Some(pane("terminal_7"))
        )],
        "a cleanup that closed its pane records no kill failure"
    );
}
