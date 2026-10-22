use super::*;

#[test]
fn auto_gc_assist_bounds_paths_in_sweep_order_without_truncating_count() {
    let mut outcome = full_outcome(false);
    let WorktreeSweepStatus::Swept(sweep) = &mut outcome.worktrees else {
        panic!("fixture has a worktree sweep");
    };
    let template = sweep.removed[0].clone();
    let paths = (0..60)
        .map(|index| PathBuf::from(format!("/repo-worktrees/tree-{}", 60 - index)))
        .collect::<Vec<_>>();
    sweep.removed = paths
        .iter()
        .map(|path| SweptWorktree {
            path: path.clone(),
            ..template.clone()
        })
        .collect();
    let id = rimz::WorkspaceId::from_project_root(std::path::Path::new("/test"));
    let assist = auto_gc_assist(
        &id,
        "test-abcd",
        GcScope::Room,
        Duration::ZERO,
        &Ok(outcome),
    );
    let json = serde_json::to_value(assist).unwrap();
    assert_eq!(json["worktree_paths"], serde_json::json!(&paths[..50]));
    assert_eq!(json["worktrees_removed"], 60);
}

#[test]
fn legacy_auto_gc_assist_defaults_removed_paths() {
    let id = rimz::WorkspaceId::from_project_root(std::path::Path::new("/test"));
    let legacy = serde_json::json!({
        "assist": "auto_gc", "workspace_id": id, "older_than_secs": 0,
        "reclaimed_bytes": 0, "worktrees_removed": 3, "workspaces_pruned": 0,
        "files_removed": 0, "messages_archived": 0, "problems": 0
    });
    let assist: Assist = serde_json::from_value(legacy).unwrap();
    let json = serde_json::to_value(assist).unwrap();
    assert_eq!(json["worktree_paths"], serde_json::json!([]));
    assert_eq!(json["worktrees_removed"], 3);
}

#[test]
fn orphan_empty_trees_are_rendered_serialized_and_counted_in_assists() {
    let mut outcome = GcOutcome::default();
    outcome.prune.empty_dirs.push(gc::OrphanEmptyTree {
        dir_name: "gone-abcd".into(),
        files_removed: 2,
        bytes: 42,
        agent_dirs_kept: 1,
    });
    for dry_run in [false, true] {
        outcome.dry_run = dry_run;
        let out = strip_report(&outcome);
        let action = if dry_run { "would prune" } else { "pruned" };
        assert!(
            out.contains(&format!(
                "{action}: empty-dir tree gone-abcd — 2 files (42 B)"
            )),
            "{out}"
        );
        assert!(
            out.contains("kept: empty-dir tree gone-abcd — 1 agent dir within the 7-day grace")
        );
        assert!(out.contains("✦ workspaces"));
    }
    let json = serde_json::to_value(JsonReport::from(&outcome)).unwrap();
    assert_eq!(
        json["workspaces"]["empty_dirs"],
        serde_json::json!([{"dir_name":"gone-abcd","files_removed":2,"bytes":42,"agent_dirs_kept":1}])
    );
    assert_eq!(json["reclaimed_bytes"], 42);
    let id = rimz::WorkspaceId::from_project_root(std::path::Path::new("/test"));
    let assist = auto_gc_assist(
        &id,
        "test-abcd",
        GcScope::Machine,
        Duration::ZERO,
        &Ok(outcome),
    );
    let json = serde_json::to_value(assist).unwrap();
    assert_eq!(json["files_removed"], 2);
    assert_eq!(json["reclaimed_bytes"], 42);
}

#[test]
fn gc_json_and_assist_report_classes() {
    let mut outcome = GcOutcome::default();
    outcome.prune.failed.push(gc::FailedWorkspace {
        dir_name: "bad-abcd".into(),
        error: "cannot access /x: boom".into(),
    });
    outcome.budget_ledgers.ledgers = 2;
    outcome.runtime.rooms.push(gc::RoomReport {
        name: "test-abcd".to_owned(),
        classes: vec![
            gc::ClassReport {
                class: "audit".to_owned(),
                files_removed: 2,
                bytes_removed: 12,
                locks_would_check: 0,
                bytes_retained: Some(84),
            },
            gc::ClassReport {
                class: "transcript".to_owned(),
                files_removed: 1,
                bytes_removed: 34,
                bytes_retained: Some(56),
                ..gc::ClassReport::default()
            },
        ],
    });
    let json = serde_json::to_value(JsonReport::from(&outcome)).unwrap();
    assert_eq!(json["rooms"][0]["classes"][0]["bytes_removed"], 12);
    assert_eq!(json["rooms"][0]["classes"][0]["bytes_retained"], 84);
    assert_eq!(json["rooms"][0]["classes"][1]["bytes_retained"], 56);
    let text = strip_report(&outcome);
    assert!(
        text.contains("audit · 2 files · 12 B · 84 B kept"),
        "{text}"
    );
    assert!(
        text.contains("transcript · 1 file · 34 B · 56 B kept"),
        "{text}"
    );
    let id = rimz::WorkspaceId::from_project_root(std::path::Path::new("/test"));
    outcome.runtime.rooms.push(gc::RoomReport {
        name: "other-abcd".to_owned(),
        classes: vec![gc::ClassReport {
            class: "audit".to_owned(),
            bytes_removed: 99,
            ..gc::ClassReport::default()
        }],
    });
    let assist = auto_gc_assist(
        &id,
        "test-abcd",
        GcScope::Room,
        Duration::ZERO,
        &Ok(outcome),
    );
    let json = serde_json::to_value(assist).unwrap();
    assert_eq!(json["class_bytes"]["audit"], 12);
    assert_eq!(json["worktree_paths"], serde_json::json!([]));
    assert_eq!(json["class_bytes"]["transcript"], 34);
    assert_eq!(json["files_removed"], 2);
    assert_eq!(json["scope"], "room");
    assert_eq!(json["problems"], 1);
    assert!(json["error"].is_null());
}

#[test]
fn room_report_names_scope_and_skips_machine_area() {
    let outcome = GcOutcome {
        scope: GcScope::Room,
        room_name: "test-abcd".to_owned(),
        ..clean_outcome()
    };
    let out = strip_report(&outcome);
    assert!(out.contains("checked 7 of 8 areas · cutoff 1h · room test-abcd"));
    assert!(out.contains("– workspaces      skipped — machine area, run with --all"));
    let json = serde_json::to_value(JsonReport::from(&outcome)).unwrap();
    assert_eq!(json["scope"], "room");
    assert_eq!(json["workspaces"]["skipped"], "room_scope");
    assert_eq!(json["workspaces"]["failed"], serde_json::json!([]));
    assert_eq!(json["workspaces"]["empty_dirs"], serde_json::json!([]));
}

#[test]
fn render_report_names_all_clean_checks() {
    let out = strip_report(&clean_outcome());

    assert!(out.contains("gc — all clean, nothing to reclaim"));
    assert!(out.contains("checked 8 areas · cutoff 1h · machine"));
    assert!(out.contains("✓ worktrees"));
    assert!(out.contains("3 kept — 2 in use, 1 with unlanded work"));
    assert!(out.contains("✓ workspaces"));
    assert!(out.contains("4 healthy"));
    assert!(out.contains("✓ runtime"));
    assert!(out.contains("6 roots scanned, all fresh"));
    assert!(out.contains("✓ temp files"));
    assert!(out.contains("✓ messages"));
    assert!(out.contains("✓ event log"));
    assert!(out.contains("✓ agent cache"));
    assert!(out.contains("✓ loop schedules"));
}

#[test]
fn render_report_names_every_kept_worktree_and_reason() {
    let out = strip_report(&GcOutcome {
        older_than: Duration::from_secs(3600),
        worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
            kept: vec![
                KeptWorktree {
                    name: "active".to_owned(),
                    path: PathBuf::from("/repo-worktrees/active"),
                    reasons: vec![KeepReason::InUse, KeepReason::Dirty, KeepReason::Unlanded],
                    holders: rimz::worktree::Holders {
                        teams: vec!["forge".into()],
                        ..rimz::worktree::Holders::default()
                    },
                },
                KeptWorktree {
                    name: "dirty".to_owned(),
                    path: PathBuf::from("/repo-worktrees/dirty"),
                    reasons: vec![KeepReason::Dirty, KeepReason::Unlanded],
                    holders: rimz::worktree::Holders::default(),
                },
                KeptWorktree {
                    name: "unlanded".to_owned(),
                    path: PathBuf::from("/repo-worktrees/unlanded"),
                    reasons: vec![KeepReason::Unlanded],
                    holders: rimz::worktree::Holders::default(),
                },
            ],
            ..WorktreeSweep::default()
        }),
        ..GcOutcome::default()
    });

    assert!(out.contains("3 kept — 1 in use, 1 with uncommitted changes, 1 with unlanded work"));
    assert!(out.contains("kept: active — in use, uncommitted changes, unlanded work"));
    assert!(out.contains("(team forge)"), "{out}");
    assert!(out.contains("kept: dirty — uncommitted changes, unlanded work"));
    assert!(out.contains("kept: unlanded — unlanded work"));
}

#[test]
fn render_report_lists_active_checklist_details() {
    let out = strip_report(&full_outcome(false));

    assert!(out.contains("gc — reclaimed"));
    assert!(out.contains("· 3 problems"));
    assert!(out.contains("✦ worktrees"));
    assert!(out.contains("2 removed · 1.4 GB · 3 kept"));
    assert!(out.contains("kept: active — in use"));
    assert!(out.contains("kept: shell — in use"));
    assert!(out.contains("kept: unlanded — unlanded work"));
    assert!(out.contains("removed: demo"));
    assert!(out.contains("landed, branch deleted"));
    assert!(out.contains("removed: gc-info"));
    assert!(out.contains("landed, branch kept (not proven landed)"));
    assert!(
        out.contains(
            "removed: 1 heartbeat · 2 sidecars · 1 probe · 1 dir · 1 account budget ledger"
        )
    );
    assert!(out.contains("message archive failed: archive boom"));
    assert!(out.contains("✗ failed: /repo-worktrees/wip — remove boom"));
    assert!(
        out.contains("pruned: ws_0123456789abcdef01234567 — project folder gone: /gone (2 KB)")
    );
    assert!(out.contains("⚠ event log"));
    assert!(out.contains("corruption repaired — 9 B cut, 3 frames kept"));
}

#[test]
fn render_report_uses_dry_run_framing() {
    let out = strip_report(&full_outcome(true));

    assert!(out.contains("gc — would reclaim"));
    assert!(out.contains("(dry run)"));
    assert!(out.contains("checked 4 of 8 areas · cutoff 1h"));
    assert!(out.contains("would remove 2 · 1.4 GB · 3 kept"));
    assert!(out.contains("kept: active — in use"));
    assert!(out.contains("kept: shell — in use"));
    assert!(out.contains("kept: unlanded — unlanded work"));
    assert!(out.contains("would prune 1 · 2 KB"));
    assert!(out.contains("would remove 6 stale files · 13 KB"));
    assert!(out.contains("probe · 1 dir · 1 account budget ledger"));
    assert!(out.contains("would remove 2 orphaned"));
    assert!(out.contains("would remove: demo"));
    assert!(out.contains("would prune: ws_0123456789abcdef01234567"));
    assert!(out.contains("– messages        skipped (dry run)"));
    assert!(out.contains("– event log       skipped (dry run)"));
    assert!(out.contains("– agent cache     skipped (dry run)"));
    assert!(out.contains("– loop schedules  skipped (dry run)"));
    assert!(!out.contains("branch kept (not proven landed)"));
    assert!(!out.contains("message archive failed"));
}

#[test]
fn render_report_names_skipped_worktree_area() {
    let out = strip_report(&GcOutcome {
        older_than: Duration::from_secs(3600),
        worktrees: WorktreeSweepStatus::Skipped(WorktreeSkip::NotARepo),
        ..GcOutcome::default()
    });

    assert!(out.contains("checked 7 of 8 areas · cutoff 1h"));
    assert!(out.contains("– worktrees       skipped — not inside a git repo"));
}

#[test]
fn render_report_counts_failures_as_problems() {
    let out = strip_report(&GcOutcome {
        older_than: Duration::from_secs(3600),
        worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
            failed: vec![FailedWorktree {
                path: PathBuf::from("/bad"),
                error: "boom".to_owned(),
            }],
            ..WorktreeSweep::default()
        }),
        ..GcOutcome::default()
    });

    assert!(out.contains("· 1 problem"));
    assert!(out.contains("✗ worktrees       1 removal failed"));
    assert!(out.contains("✗ failed: /bad — boom"));

    let mut outcome = GcOutcome::default();
    outcome.prune.failed.push(gc::FailedWorkspace {
        dir_name: "bad-abcd".into(),
        error: "cannot access /x: boom".into(),
    });
    let out = strip_report(&outcome);
    assert!(out.contains("· 1 problem"), "{out}");
    assert!(out.contains("✗ workspaces      1 removal failed"));
    assert!(out.contains("✗ failed: bad-abcd — cannot access /x: boom"));
    outcome.prune.removed = full_outcome(false).prune.removed;
    let out = strip_report(&outcome);
    assert!(out.contains("✦ workspaces"));
    assert!(out.contains("✗ failed: bad-abcd — cannot access /x: boom"));
    outcome.prune.removed.clear();
    outcome.prune.failed.push(outcome.prune.failed[0].clone());
    assert!(strip_report(&outcome).contains("✗ workspaces      2 removals failed"));
}

#[test]
fn json_report_serializes_gc_schema() {
    let mut outcome = full_outcome(false);
    outcome.prune.failed.push(gc::FailedWorkspace {
        dir_name: "bad-abcd".into(),
        error: "cannot access /x: boom".into(),
    });
    let value = serde_json::to_value(JsonReport::from(&outcome)).unwrap();
    assert_eq!(
        value["workspaces"]["failed"],
        serde_json::json!([
            {"dir_name": "bad-abcd", "error": "cannot access /x: boom"}
        ])
    );

    assert_eq!(value["dry_run"], false);
    assert_eq!(value["scope"], "machine");
    assert!(value["workspaces"]["skipped"].is_null());
    assert_eq!(value["older_than_secs"], 3600);
    assert_eq!(value["worktrees"]["removed"][0]["name"], "demo");
    assert_eq!(value["worktrees"]["removed"][0]["branch_deleted"], true);
    assert_eq!(value["worktrees"]["removed"][1]["branch_deleted"], false);
    assert_eq!(
        value["worktrees"]["removed"][1]["archive_error"],
        "archive boom"
    );
    assert_eq!(value["worktrees"]["failed"][0]["error"], "remove boom");
    assert_eq!(
        value["worktrees"]["kept"][0]["holders"]["loop_tasks"],
        serde_json::json!([])
    );
    assert_eq!(
        value["worktrees"]["kept"][0]["holders"]["teams"],
        serde_json::json!([])
    );
    assert_eq!(
        value["worktrees"]["kept"][0]["reasons"],
        serde_json::json!(["in_use"])
    );
    assert_eq!(
        value["worktrees"]["kept"][2]["reasons"],
        serde_json::json!(["unlanded"])
    );
    assert!(value["worktrees"]["skipped"].is_null());
    assert_eq!(
        value["workspaces"]["removed"][0]["reason"],
        "project_root_gone"
    );
    assert_eq!(value["runtime"]["roots_scanned"], 2);
    assert_eq!(value["budget_ledgers_removed"], 1);
    assert_eq!(value["messages"]["archived"], 1);
    assert_eq!(value["messages"]["reconciled"], 1);
    assert_eq!(value["repair"]["bytes_truncated"], 9);
    assert_eq!(value["store_maintenance"], "done");

    let dry_run = serde_json::to_value(JsonReport::from(&full_outcome(true))).unwrap();
    assert_eq!(dry_run["workspaces"]["failed"], serde_json::json!([]));
    assert!(dry_run["repair"].is_null());
    assert!(dry_run["worktrees"]["removed"][0]["branch_deleted"].is_null());
    assert_eq!(dry_run["store_maintenance"], "skipped_dry_run");

    let no_store = serde_json::to_value(JsonReport::from(&GcOutcome {
        store_maintenance: StoreMaintenance::SkippedNoStore,
        worktrees: WorktreeSweepStatus::Skipped(WorktreeSkip::NoStore),
        ..GcOutcome::default()
    }))
    .unwrap();
    assert_eq!(no_store["store_maintenance"], "skipped_no_store");
    assert_eq!(no_store["worktrees"]["skipped"], "no_store");
}

#[test]
fn workspace_reports_use_directory_name_without_a_record() {
    let removed = gc::RemovedWorkspace {
        workspace_id: None,
        dir_name: "unfinished".to_owned(),
        reason: gc::PruneReason::AbandonedScaffold,
        bytes: 0,
        project_root: None,
    };
    assert!(removed_workspace_detail(&removed).starts_with("unfinished — abandoned setup"));
    let json = serde_json::to_value(JsonRemovedWorkspace::from(&removed)).unwrap();
    assert!(json["workspace_id"].is_null());
    assert_eq!(json["dir_name"], "unfinished");
    let removed = gc::RemovedWorkspace {
        reason: gc::PruneReason::IncompatibleLayout,
        ..removed
    };
    assert!(
        removed_workspace_detail(&removed)
            .starts_with("unfinished — written by an older RimZ (layout 1), unusable (")
    );
    assert_eq!(prune_reason_json(removed.reason), "incompatible_layout");
}

fn clean_outcome() -> GcOutcome {
    GcOutcome {
        older_than: Duration::from_secs(3600),
        runtime: gc::GcReport {
            runtime_roots_scanned: 6,
            ..gc::GcReport::default()
        },
        prune: gc::WorkspacePruneReport {
            kept: 4,
            ..gc::WorkspacePruneReport::default()
        },
        worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
            kept: kept_worktrees(),
            ..WorktreeSweep::default()
        }),
        ..GcOutcome::default()
    }
}

fn full_outcome(dry_run: bool) -> GcOutcome {
    GcOutcome {
        scope: GcScope::Machine,
        room_name: String::new(),
        project_root: PathBuf::new(),
        dry_run,
        older_than: Duration::from_secs(3600),
        runtime: gc::GcReport {
            runtime_roots_scanned: 2,
            heartbeat_files_removed: 1,
            sidecar_files_removed: 2,
            sidebar_sockets_removed: 0,
            probe_markers_removed: 1,
            dirs_removed: 1,
            bytes_removed: 13_018,
            ..gc::GcReport::default()
        },
        temps: gc::TempSweepReport {
            files_removed: 2,
            bytes_removed: 68,
        },
        store_maintenance: if dry_run {
            StoreMaintenance::SkippedDryRun
        } else {
            StoreMaintenance::Done {
                archived: 1,
                reconciled: 1,
                repaired: RepairOutcome {
                    bytes_truncated: 9,
                    frames_kept: 3,
                },
                carryover_pruned: 1,
            }
        },
        schedules_reaped: usize::from(!dry_run),
        wait_logs_pruned: usize::from(!dry_run),
        budget_ledgers: rimz::harness::budget::AccountLedgerSweep {
            ledgers: 1,
            bytes: 40,
        },
        prune: gc::WorkspacePruneReport {
            removed: vec![gc::RemovedWorkspace {
                workspace_id: Some(
                    rimz::WorkspaceId::parse("ws_0123456789abcdef01234567").unwrap(),
                ),
                dir_name: "gone-0123".to_owned(),
                reason: gc::PruneReason::ProjectRootGone,
                bytes: 2048,
                project_root: Some(PathBuf::from("/gone")),
            }],
            kept: 0,
            failed: Vec::new(),
            retained_unreadable: Vec::new(),
            empty_dirs: Vec::new(),
        },
        worktrees: WorktreeSweepStatus::Swept(WorktreeSweep {
            removed: vec![
                SweptWorktree {
                    name: "demo".to_owned(),
                    branch: "demo".to_owned(),
                    path: PathBuf::from("/repo-worktrees/demo"),
                    bytes: 1_503_238_553,
                    branch_deletion: (!dry_run).then_some(rimz::worktree::BranchDeletion::Deleted),
                    archive_error: None,
                },
                SweptWorktree {
                    name: "gc-info".to_owned(),
                    branch: "gc-info".to_owned(),
                    path: PathBuf::from("/repo-worktrees/gc-info"),
                    bytes: 12,
                    branch_deletion: (!dry_run)
                        .then_some(rimz::worktree::BranchDeletion::KeptUnmerged),
                    archive_error: (!dry_run).then_some("archive boom".to_owned()),
                },
            ],
            kept: kept_worktrees(),
            failed: if dry_run {
                Vec::new()
            } else {
                vec![FailedWorktree {
                    path: PathBuf::from("/repo-worktrees/wip"),
                    error: "remove boom".to_owned(),
                }]
            },
        }),
    }
}

fn kept_worktrees() -> Vec<KeptWorktree> {
    vec![
        KeptWorktree {
            name: "active".to_owned(),
            path: PathBuf::from("/repo-worktrees/active"),
            reasons: vec![KeepReason::InUse],
            holders: rimz::worktree::Holders::default(),
        },
        KeptWorktree {
            name: "shell".to_owned(),
            path: PathBuf::from("/repo-worktrees/shell"),
            reasons: vec![KeepReason::InUse],
            holders: rimz::worktree::Holders::default(),
        },
        KeptWorktree {
            name: "unlanded".to_owned(),
            path: PathBuf::from("/repo-worktrees/unlanded"),
            reasons: vec![KeepReason::Unlanded],
            holders: rimz::worktree::Holders::default(),
        },
    ]
}

fn strip_report(outcome: &GcOutcome) -> String {
    let mut stream = anstream::StripStream::new(Vec::new());
    render_report(outcome, &mut stream).expect("render report");
    String::from_utf8(stream.into_inner()).expect("utf-8")
}
