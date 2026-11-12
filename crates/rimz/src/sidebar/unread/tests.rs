use super::*;

use crate::MuxName;
use crate::pane::PaneRef;
use crate::store::snapshot::{AgentCard, RowCard, SidebarStatusCount, SidebarWorktreeGroup};
use std::path::Path;
use tempfile::TempDir;

fn runtime() -> (TempDir, RuntimePaths) {
    let dir = TempDir::new().expect("tempdir");
    let workspace = crate::ids::WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    (dir, runtime)
}

fn row(id: &str, status: AgentStatus, last_activity: i64) -> SidebarRow {
    SidebarRow {
        checkout: None,
        id: id.to_owned(),
        name: "claude".to_owned(),
        pane: Some(PaneRef::from_id(PaneId::from_parts(MuxName::Tmux, "%1"))),
        worktree_path: Some("/repo/main".to_owned()),
        worktree_branch: Some("main".to_owned()),
        channel: None,
        unread: false,
        inactive: false,
        archived: false,
        attention_score: 0,
        attention_at_ms: None,
        last_activity: jiff::Timestamp::from_millisecond(last_activity).expect("timestamp"),
        card: RowCard::Agent(std::sync::Arc::new(AgentCard {
            status,
            ..AgentCard::default()
        })),
    }
}

fn process_row(id: &str, last_activity: i64) -> SidebarRow {
    SidebarRow {
        card: RowCard::Process(crate::store::snapshot::ProcessCard::default()),
        name: "zsh".to_owned(),
        ..row(id, AgentStatus::Idle, last_activity)
    }
}

fn snapshot(rows: Vec<SidebarRow>) -> SidebarSnapshot {
    let status_counts = rows
        .iter()
        .filter_map(|row| row.status())
        .map(|status| SidebarStatusCount { status, count: 1 })
        .collect();
    let workspace = crate::ids::WorkspaceId::from_project_root(Path::new("/repo/main"));
    let mut snapshot =
        SidebarSnapshot::build_with_agents(workspace, Vec::new(), jiff::Timestamp::now());
    snapshot.panes_produced_at_ms = Some(1);
    snapshot.worktree_groups = vec![SidebarWorktreeGroup {
        stalled_since: None,
        pr_stack: Default::default(),
        key: "/repo/main".to_owned(),
        label: "main".to_owned(),
        label_qualifier: None,
        kind: crate::store::snapshot::SidebarWorktreeKind::Worktree,
        team: None,
        cohort_effort: None,
        pipeline: None,
        workflow: None,
        status_counts,
        rows: rows.into(),
        diff_added: None,
        diff_removed: None,
        commits_ahead: None,
        commits_behind: None,
        trunk: None,
        place: None,
        root_lane: false,
        inactive: None,
        clean: None,
        landed: None,
        trunk_sync: None,
        pr_state: None,
        pr_queue: None,
        pr_conflicting: false,
        ci: None,
        pr_number: None,
        pr_url: None,
    }]
    .into();
    snapshot
}

#[test]
fn overlays_reopen_once_at_their_own_stamp_despite_read_activity_or_a_live_episode() {
    for already_unread in [false, true] {
        let mut episodes = UnreadEpisodes::default();
        let marks = ReadMarks::from_entries([("raised".into(), 100)]);
        let mut initial = snapshot(vec![row("raised", AgentStatus::Success, 50)]);
        if already_unread {
            episodes.open_for_row(initial.rows().next().unwrap(), 150);
        }
        let row = &mut std::sync::Arc::make_mut(&mut initial.worktree_groups)[0];
        std::sync::Arc::make_mut(&mut row.rows)[0]
            .as_agent_mut()
            .unwrap()
            .overlay_raised_at = Some(jiff::Timestamp::from_millisecond(200).unwrap());
        let opened = episodes.reconcile(&mut initial, &marks, false).opened;
        assert_eq!(opened.len(), 1, "already_unread={already_unread}");
        assert_eq!(opened[0].episode_ms, 200);
        assert!(
            episodes
                .reconcile(&mut initial, &marks, false)
                .opened
                .is_empty()
        );
    }
}

#[test]
fn opens_for_every_needs_a_look_status_including_paused() {
    for status in [
        AgentStatus::Waiting,
        AgentStatus::Failed,
        AgentStatus::Paused,
        AgentStatus::Success,
    ] {
        let mut episodes = UnreadEpisodes::default();
        let mut snapshot = snapshot(vec![row("a", status, 1_000)]);
        let out = episodes.reconcile(&mut snapshot, &ReadMarks::default(), false);

        assert_eq!(out.opened.len(), 1, "{status:?} opens");
        assert!(snapshot.worktree_groups[0].rows[0].unread);
    }
}

#[test]
fn sleeping_cycle_notifies_only_when_the_result_finishes() {
    let mut episodes = UnreadEpisodes::default();
    let mut notifications = crate::sidebar::notify::NotificationState::default();
    let prefs = crate::config::NotificationsPrefs {
        triggers: vec![crate::config::NotificationTrigger::Success],
        coalesce_ms: 0,
        ..Default::default()
    };
    for (status, at) in [
        (AgentStatus::Sleeping, 1_000),
        (AgentStatus::Running, 2_000),
        (AgentStatus::Success, 3_000),
    ] {
        let mut snapshot = snapshot(vec![row("a", status, at)]);
        let out = episodes.reconcile(&mut snapshot, &ReadMarks::default(), false);
        let sent = notifications.evaluate(&snapshot, &out.opened, &prefs, at as u64);
        let finished = status == AgentStatus::Success;
        assert_eq!(snapshot.worktree_groups[0].rows[0].unread, finished);
        assert_eq!(out.opened.len(), usize::from(finished));
        assert_eq!(sent.len(), usize::from(finished));
        if finished {
            assert_eq!(
                sent[0].notification_kind,
                crate::config::NotificationKind::Success
            );
        }
    }
}

#[test]
fn stays_unread_across_return_to_running() {
    let mut episodes = UnreadEpisodes::default();
    let mut waiting = snapshot(vec![row("a", AgentStatus::Waiting, 1_000)]);
    episodes.reconcile(&mut waiting, &ReadMarks::default(), false);

    for status in [AgentStatus::Running, AgentStatus::Sleeping] {
        let mut resumed = snapshot(vec![row("a", status, 2_000)]);
        let out = episodes.reconcile(&mut resumed, &ReadMarks::default(), false);

        assert!(out.opened.is_empty());
        assert!(resumed.worktree_groups[0].rows[0].unread);
    }
}

#[test]
fn read_mark_clears_only_when_it_reaches_episode() {
    let mut episodes = UnreadEpisodes::default();
    let mut first = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
    episodes.reconcile(&mut first, &ReadMarks::default(), false);

    let mut old_mark = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
    episodes.reconcile(
        &mut old_mark,
        &ReadMarks::from_entries([("a".to_owned(), 999)]),
        false,
    );
    assert!(old_mark.worktree_groups[0].rows[0].unread);

    let mut reached = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
    let out = episodes.reconcile(
        &mut reached,
        &ReadMarks::from_entries([("a".to_owned(), 1_000)]),
        false,
    );
    assert!(!reached.worktree_groups[0].rows[0].unread);
    assert!(out.changed);
    assert!(
        out.cleared.is_empty(),
        "the read-mark writer owns focus/mark_read clear tracing"
    );
}

#[test]
fn promoted_permission_opens_after_a_receipt_during_review() {
    let at = jiff::Timestamp::from_second(1_000).unwrap();
    let mut source = crate::testkit::agent_state("codex", "review", at);
    source.status = AgentStatus::Waiting;
    source.mode = Some(crate::agents::PermissionMode::Auto);
    source.waiting_since = Some(at);
    source.open_ask = Some(crate::agents::OpenAsk {
        extra_questions: 0,
        id: crate::ids::AskId::new(),
        kind: crate::agents::AskKind::Permission,
        detail: None,
        native_key: None,
        since: at,
    });
    let mut episodes = UnreadEpisodes::default();
    let marks = ReadMarks::from_entries([("review".to_owned(), 1_010_000)]);
    for second in [1_059, 1_060] {
        let now = jiff::Timestamp::from_second(second).unwrap();
        let mut row = row("review", AgentStatus::Waiting, at.as_millisecond());
        row.name = "codex".to_owned();
        row.card = RowCard::Agent(std::sync::Arc::new(AgentCard::from_agent(&source, now)));
        let mut snapshot = snapshot(vec![row]);
        snapshot.agents = vec![source.clone()].into();
        snapshot.now = now;
        let out = episodes.reconcile(&mut snapshot, &marks, false);
        assert_eq!(out.opened.len(), usize::from(second == 1_060));
        if second == 1_060 {
            assert_eq!(out.opened[0].episode_ms, 1_060_000);
            assert!(snapshot.worktree_groups[0].rows[0].unread);
        }
    }
}

#[test]
fn later_activity_after_read_opens_a_new_episode() {
    let mut episodes = UnreadEpisodes::default();
    let mut first = snapshot(vec![row("a", AgentStatus::Success, 1_000)]);
    episodes.reconcile(&mut first, &ReadMarks::default(), false);

    let marks = ReadMarks::from_entries([("a".to_owned(), 1_500)]);
    let mut later = snapshot(vec![row("a", AgentStatus::Success, 2_000)]);
    let out = episodes.reconcile(&mut later, &marks, false);

    assert!(out.changed);
    assert!(
        out.cleared.is_empty(),
        "read-reached pruning must not emit a second clear trace"
    );
    assert_eq!(out.opened.len(), 1);
    assert_eq!(out.opened[0].episode_ms, 2_000);
    assert!(later.worktree_groups[0].rows[0].unread);
}

#[test]
fn read_reached_prune_wins_over_row_gone_trace() {
    let mut episodes = UnreadEpisodes::default();
    let row = row("a", AgentStatus::Success, 1_000);
    episodes.open_for_row(&row, 1_000);
    let mut empty = snapshot(Vec::new());

    let out = episodes.reconcile(
        &mut empty,
        &ReadMarks::from_entries([("a".to_owned(), 1_000)]),
        false,
    );

    assert!(out.changed);
    assert!(
        out.cleared.is_empty(),
        "a human read receipt owns the clear trace even if the row vanished before pruning"
    );
}

#[test]
fn row_gone_clear_does_not_guess_an_agent_id_from_row_id() {
    let mut episodes = UnreadEpisodes::default();
    let row = row("a", AgentStatus::Success, 1_000);
    episodes.open_for_row(&row, 1_000);
    let mut empty = snapshot(Vec::new());

    let out = episodes.reconcile(&mut empty, &ReadMarks::default(), false);

    assert_eq!(out.cleared.len(), 1);
    assert_eq!(out.cleared[0].cause, UnreadClearCause::RowGone);
    assert!(out.cleared[0].agent_id.is_none());
}

#[test]
fn row_label_fallback_uses_agent_handle() {
    let mut row = row("abcdefghi", AgentStatus::Waiting, 1_000);
    row.as_agent_mut().unwrap().handle = Some("planner".to_owned());

    assert_eq!(row_label(&row), "planner abcdefgh");
}

#[test]
fn frameless_reconcile_neither_opens_nor_prunes() {
    let mut episodes = UnreadEpisodes::default();
    let open_row = row("a", AgentStatus::Success, 1_000);
    episodes.open_for_row(&open_row, 1_000);
    let mut frameless = snapshot(vec![row("b", AgentStatus::Waiting, 2_000)]);
    frameless.panes_produced_at_ms = None;

    let out = episodes.reconcile(&mut frameless, &ReadMarks::default(), false);

    assert!(!out.changed);
    assert!(out.opened.is_empty());
    assert!(out.cleared.is_empty());
    assert!(episodes.episodes.contains_key("a"));
    assert!(!episodes.episodes.contains_key("b"));
}

#[test]
fn cold_start_opens_silently() {
    let mut episodes = UnreadEpisodes::default();
    let mut snapshot = snapshot(vec![row("a", AgentStatus::Waiting, 1_000)]);
    let out = episodes.reconcile(&mut snapshot, &ReadMarks::default(), true);

    assert!(out.opened[0].silent);
    assert!(snapshot.worktree_groups[0].rows[0].unread);
}

#[test]
fn mark_rows_unread_skips_process_rows_without_panicking() {
    // `opened_unread` asserts an agent status; a process row carries none.
    // The durable path must skip it rather than unwind, whether the target
    // came from the renderer's `m` key.
    // (Regression: R1-01.)
    let (_dir, runtime) = runtime();
    let rows = vec![
        process_row("term", 1_000),
        row("a", AgentStatus::Waiting, 2_000),
    ];

    let opened = mark_rows_unread(&runtime, &rows, 5_000).expect("mark unread");

    assert_eq!(opened.len(), 1, "only the agent row opens an episode");
    assert_eq!(opened[0].row_id, "a");
    let loaded = UnreadEpisodes::load(&runtime);
    assert!(loaded.episodes.contains_key("a"));
    assert!(!loaded.episodes.contains_key("term"));
}

#[test]
fn mark_rows_unread_writes_nothing_for_only_process_rows() {
    // No agent row means no episode and no file write — the unread file stays
    // absent rather than being rewritten with an empty map.
    let (_dir, runtime) = runtime();

    let opened =
        mark_rows_unread(&runtime, &[process_row("term", 1_000)], 5_000).expect("mark unread");

    assert!(opened.is_empty());
    assert!(
        !runtime.unread_path().exists(),
        "no agent rows means no durable write",
    );
}

#[test]
fn load_persist_round_trips_and_garbage_reads_empty() {
    let (_dir, runtime) = runtime();
    let mut episodes = UnreadEpisodes::default();
    let row = row("a", AgentStatus::Waiting, 1_000);
    episodes.open_for_row(&row, 1_000);
    episodes.persist(&runtime).expect("persist");

    let loaded = UnreadEpisodes::load(&runtime);
    assert_eq!(loaded.episodes.get("a"), Some(&1_000));

    fs::write(runtime.unread_path(), b"{ not json").expect("garbage");
    assert_eq!(UnreadEpisodes::load(&runtime), UnreadEpisodes::default());
}

#[test]
fn manual_mark_lands_after_an_existing_receipt_and_survives_reconcile() {
    // (receipt, last activity, now): a receipt in the mark's own ms, and one
    // ahead of a clock that stepped back.
    for (receipt, last_activity, now) in [(5_000, 1_000, 5_000), (9_000, 1_000, 5_000)] {
        let (_dir, runtime) = runtime();
        crate::sidebar::read_marks::write_read_marks(&runtime, ["a".to_owned()], receipt)
            .expect("receipt");

        let opened = mark_rows_unread(
            &runtime,
            &[row("a", AgentStatus::Running, last_activity)],
            now,
        )
        .expect("mark unread");
        assert_eq!(opened[0].episode_ms, receipt + 1, "receipt={receipt}");

        let mut held = snapshot(vec![row("a", AgentStatus::Running, last_activity)]);
        reconcile_snapshot(&runtime, &mut held);
        assert!(held.worktree_groups[0].rows[0].unread, "receipt={receipt}");
        assert_eq!(
            UnreadEpisodes::load(&runtime).episodes.get("a"),
            Some(&(receipt + 1)),
            "receipt={receipt}"
        );

        crate::sidebar::read_marks::write_read_marks(&runtime, ["a".to_owned()], receipt + 1)
            .expect("read");
        let mut read = snapshot(vec![row("a", AgentStatus::Running, last_activity)]);
        reconcile_snapshot(&runtime, &mut read);
        assert!(!read.worktree_groups[0].rows[0].unread, "receipt={receipt}");
    }
}

fn hold_unread_lock(runtime: &RuntimePaths) -> crate::disk::lock::WorkspaceLock {
    crate::disk::lock::WorkspaceLock::acquire_with_timeout(
        &runtime.unread_lock_path(),
        std::time::Duration::from_millis(250),
    )
    .expect("unread lock")
}

#[test]
fn a_manual_mark_is_never_lost_to_a_writer_mid_transaction() {
    let (_dir, runtime) = runtime();
    let manual = row("manual", AgentStatus::Running, 1_000);

    let lock = hold_unread_lock(&runtime);
    let mut held = UnreadEpisodes::load(&runtime);
    held.open_for_row(&row("host", AgentStatus::Waiting, 1_000), 1_000);
    let during = mark_rows_unread(&runtime, std::slice::from_ref(&manual), 5_000);
    held.persist(&runtime).expect("replace");
    drop(lock);
    if during.is_ok() {
        assert!(
            UnreadEpisodes::load(&runtime)
                .episodes
                .contains_key("manual"),
            "a mark that returned Ok was dropped by the other writer's replace"
        );
    }

    mark_rows_unread(&runtime, std::slice::from_ref(&manual), 5_000).expect("after release");
    let loaded = UnreadEpisodes::load(&runtime);
    assert!(loaded.episodes.contains_key("host"));
    assert!(loaded.episodes.contains_key("manual"));
}

#[test]
fn host_reconcile_under_a_held_lock_only_derives_and_opens_next_pass() {
    let (_dir, runtime) = runtime();
    mark_rows_unread(&runtime, &[row("seen", AgentStatus::Running, 1_000)], 5_000).expect("seed");
    let before = fs::read_to_string(runtime.unread_path()).expect("episodes");
    let rows = || {
        snapshot(vec![
            row("seen", AgentStatus::Running, 1_000),
            row("ask", AgentStatus::Waiting, 2_000),
        ])
    };

    let lock = hold_unread_lock(&runtime);
    let mut held = rows();
    let out = reconcile_snapshot(&runtime, &mut held);
    assert_eq!(
        fs::read_to_string(runtime.unread_path()).expect("episodes"),
        before
    );
    assert_eq!(out, ReconcileOutcome::default());
    let unread = held.rows().map(|row| row.unread).collect::<Vec<_>>();
    assert_eq!(unread, [true, false]);
    drop(lock);

    let mut next = rows();
    let out = reconcile_snapshot(&runtime, &mut next);
    let opened = out
        .opened
        .iter()
        .map(|open| open.row_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(opened, ["ask"]);
}

/// An unlanded group quiet past the archive window whose only needs-a-look row
/// has no episode yet: the final-fold order decides its `inactive`.
fn quiet_unlanded_success() -> SidebarSnapshot {
    let quiet_ms = jiff::Timestamp::now().as_millisecond() - 2 * 86_400_000;
    snapshot(vec![row("done", AgentStatus::Success, quiet_ms)])
}

#[test]
fn attention_sorts_on_enrichment_bits_before_unread_writes_its_own() {
    let (_dir, ordered_runtime) = runtime();
    let mut snapshot = quiet_unlanded_success();
    crate::sidebar::engine::reconcile_final_fold(&ordered_runtime, &mut snapshot);
    assert!(snapshot.worktree_groups[0].inactive.is_some());
    assert!(snapshot.worktree_groups[0].rows[0].unread);

    let (_swapped_dir, swapped_runtime) = runtime();
    let mut swapped = quiet_unlanded_success();
    reconcile_snapshot(&swapped_runtime, &mut swapped);
    crate::sidebar::attention::reconcile_snapshot(&swapped_runtime, &mut swapped);
    assert!(swapped.worktree_groups[0].inactive.is_none());
}

#[test]
fn the_room_signature_reads_nothing_unread_reconcile_writes() {
    let (_dir, runtime) = runtime();
    let mut snapshot = snapshot(vec![
        row("done", AgentStatus::Success, 1_000),
        row("ask", AgentStatus::Waiting, 2_000),
    ]);
    crate::sidebar::attention::reconcile_snapshot(&runtime, &mut snapshot);
    let events = crate::sidebar::event_store::EventStore::default();
    let sig = |snapshot: &SidebarSnapshot| {
        let pulled = crate::sidebar::observe::PulledFrameSig::from_snapshot(snapshot);
        crate::sidebar::observe::extract_sig(snapshot, &pulled, &events, 0, 0, 7_000)
    };
    let before = sig(&snapshot);
    let unread_before = snapshot.rows().map(|row| row.unread).collect::<Vec<_>>();
    reconcile_snapshot(&runtime, &mut snapshot);
    let unread_after = snapshot.rows().map(|row| row.unread).collect::<Vec<_>>();
    assert_ne!(unread_before, unread_after);
    assert_eq!(sig(&snapshot), before);
}
