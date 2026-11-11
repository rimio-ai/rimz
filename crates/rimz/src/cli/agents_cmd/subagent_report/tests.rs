use std::path::{Path, PathBuf};

use jiff::Timestamp;

use rimz::agents::{AgentLifecycleObservation, AgentStatus, LifecycleSignal, PermissionMode};
use rimz::disk::paths::{RuntimePaths, StatePaths};
use rimz::harness::run::response_path;
use rimz::ids::{AgentKind, WorkspaceId};
use rimz::store::message::MessageStatus;
use rimz::store::writer::AgentLifecycleIntent;
use rimz::workspace::RootClass;

use super::*;

fn child(name: &str, description: Option<&str>) -> AgentState {
    let mut child = AgentState::stub("codex", name, AgentStatus::Success);
    child.name = Some(name.to_owned());
    child.description = description.map(str::to_owned);
    child
}

fn run(status: RunStatus) -> RunRecord {
    let mut run = RunRecord::new(
        WorkspaceId::from_project_root(Path::new("/tmp/subagent-report")),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "map it".to_owned(),
        PathBuf::from("/tmp/subagent-report"),
    );
    run.status = status;
    run.subagent = true;
    run.started_at = Timestamp::from_second(1_000).unwrap();
    run.completed_at = Some(Timestamp::from_second(1_252).unwrap());
    run.updated_at = run.completed_at.unwrap();
    run
}

#[test]
fn peer_digest_uses_each_turn_task() {
    let child = child("peer", Some("stale launch description"));
    let mut first = run(RunStatus::Completed);
    first.subagent = false;
    first.prompt = "first task\nextra context".into();
    let mut value = serde_json::to_value(first).unwrap();
    value["peer"] = serde_json::json!({"launch_id": "peer"});
    let first: RunRecord = serde_json::from_value(value).unwrap();
    let mut second = first.clone();
    second.run_id = rimz::ids::RunId::new();
    second.prompt = "second task".into();
    let digest = compose_digest(
        &[
            (&child, &first, None, None, ResumeAccount::Same),
            (&child, &second, None, None, ResumeAccount::Same),
        ],
        &[],
        false,
        &Clock::local(),
    );
    assert!(digest.contains("task: \"first task\""), "{digest}");
    assert!(digest.contains("task: \"second task\""), "{digest}");
    assert!(!digest.contains("stale launch description"));
    assert!(digest.contains("2 background agents"), "{digest}");
}

#[test]
fn digest_parent_routes_to_live_successor_with_corroborated_kind() {
    let mut old = child("OLD", None);
    old.launch_id = Some(AgentSessionId::from("L"));
    old.ended_at = Some(Timestamp::from_second(1_000).unwrap());
    let mut new = child("NEW", None);
    new.launch_id = old.launch_id.clone();
    new.prior_launch_ids = vec!["previous".into()];
    let wrong = AgentState::stub("claude", "L", AgentStatus::Idle);
    let agents = [wrong, old, new.clone()];

    for parent_id in ["L", "OLD", "previous"] {
        let parent = report_parent(&agents, &AgentSessionId::from(parent_id), Some(&new.kind));
        assert_eq!(parent.map(|parent| &parent.agent_id), Some(&new.agent_id));
        assert!(parent.is_some_and(|parent| parent.ended_at.is_none()));
    }
    for id in ["OLD", "previous"] {
        assert_eq!(
            report_parent(&agents[1..], &AgentSessionId::from(id), None)
                .map(|parent| &parent.agent_id),
            Some(&new.agent_id)
        );
    }
}

#[test]
fn digest_lists_a_single_result_without_a_trailing_command() {
    let mut result = run(RunStatus::Completed);
    result.last_message = Some("Done.\n\nTwo paragraphs.\n".to_owned());
    let child = child("naming", Some("map spec/profile surfaces"));
    let response = ResponseFile {
        path: PathBuf::from("/state/out/planner/naming.output"),
        summary: FileSummary {
            bytes: 23,
            lines: 3,
            tokens: 6,
        },
    };

    assert_eq!(
        compose_digest(
            &[(&child, &result, None, Some(&response), ResumeAccount::Same)],
            &[],
            true,
            &Clock::local()
        ),
        "Your subagent settled:\n\
         - @naming: completed in 4m12s, task: \"map spec/profile surfaces\", response: /state/out/planner/naming.output (<1k tokens, 3 lines)"
    );
}

#[test]
fn digest_sizes_non_completed_results_and_appends_reason() {
    let mut completed = run(RunStatus::Completed);
    completed.last_message = Some("Done.\nSecond line.\n".to_owned());
    let blank = run(RunStatus::Completed);
    let mut timed_out = run(RunStatus::TimedOut);
    timed_out.last_message = Some("partial answer\n".to_owned());
    timed_out.failure_tail = Some("first detail\n\nprovider did not stop\n".to_owned());
    let naming = child("naming", Some("map spec/profile surfaces"));
    let runtime = child("runtime", None);
    let reviewer = child("slow-reviewer", Some("review correctness"));
    let response = ResponseFile {
        path: PathBuf::from("/state/out/planner/naming.output"),
        summary: FileSummary {
            bytes: 19,
            lines: 2,
            tokens: 1_200,
        },
    };
    let partial = ResponseFile {
        path: PathBuf::from("/state/out/planner/slow-reviewer.output"),
        summary: FileSummary {
            bytes: 15,
            lines: 1,
            tokens: 21_000,
        },
    };

    assert_eq!(
        compose_digest(
            &[
                (
                    &naming,
                    &completed,
                    None,
                    Some(&response),
                    ResumeAccount::Same
                ),
                (&runtime, &blank, None, None, ResumeAccount::Same),
                (
                    &reviewer,
                    &timed_out,
                    None,
                    Some(&partial),
                    ResumeAccount::Same
                ),
            ],
            &[],
            true,
            &Clock::local()
        ),
        "All 3 subagents settled, responses total ~22k tokens, 3 lines:\n\
         - @naming: completed in 4m12s, task: \"map spec/profile surfaces\", response: /state/out/planner/naming.output (~1.2k tokens, 2 lines)\n\
         - @runtime: completed in 4m12s, task: \"map it\", no response\n\
         - @slow-reviewer: timed out after 4m12s; provider did not stop, task: \"review correctness\", partial response: /state/out/planner/slow-reviewer.output (~21k tokens, 1 line)"
    );
}

#[test]
fn digest_task_falls_back_to_prompt_preview_or_is_omitted() {
    let child = child("naming", None);
    let mut result = run(RunStatus::Completed);
    result.prompt = format!("{}\nnot part of the task", "x".repeat(150));
    let row = compose_digest_row(&child, &result, None);
    assert!(row.contains(&format!(
        ", task: \"{}\", no response",
        rimz::theme::fmt::command_preview(&"x".repeat(150))
    )));
    assert!(!row.contains("not part of the task"));
    result.prompt.clear();
    assert_eq!(
        compose_digest_row(&child, &result, None),
        "- @naming: completed in 4m12s, no response"
    );
}

#[test]
fn follow_up_row_uses_its_own_clock_and_never_the_launch_task() {
    let child = child("naming", Some("launch description"));
    for prompt in [None, Some("follow-up task\nnot the preview")] {
        let mut value = serde_json::to_value(run(RunStatus::Completed)).unwrap();
        value["follow_ups"] = serde_json::json!(1);
        value["follow_up"] = serde_json::json!({
            "started_at": Timestamp::from_second(1_250).unwrap(),
            "prompt": prompt,
        });
        let result: RunRecord = serde_json::from_value(value).unwrap();
        let row = compose_digest_row(&child, &result, None);
        assert!(row.contains("completed in 2s"), "{row}");
        assert!(!row.contains("launch description"), "{row}");
        match prompt {
            Some(_) => assert!(row.contains("task: \"follow-up task\""), "{row}"),
            None => assert!(!row.contains("task:"), "{row}"),
        }
    }
}

fn fixture() -> (tempfile::TempDir, ResolvedWorkspace, Store) {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace_id = WorkspaceId::from_project_root(dir.path());
    let state = StatePaths::under(workspace_id.clone(), &dir.path().join("state")).unwrap();
    let runtime = RuntimePaths::under(workspace_id.clone(), &dir.path().join("runtime")).unwrap();
    let store = Store::open(state, runtime).unwrap();
    let workspace = ResolvedWorkspace {
        workspace_id,
        project_root: dir.path().to_path_buf(),
        cwd_project_root: None,
        root_class: RootClass::Directory,
        worktree_root: dir.path().to_path_buf(),
        worktree_branch: None,
        session_name: "report-test".to_owned(),
        mux_hint: None,
    };
    (dir, workspace, store)
}

fn append_agent(store: &Store, name: &str, parent: Option<&str>) {
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(name)),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some(name.to_owned());
    if let Some(parent) = parent {
        observation.launch.parent_agent_id = Some(AgentSessionId::from(parent));
        observation.launch.parent_agent_kind = Some(AgentKind::new_unchecked("codex"));
        observation.launch.launch_depth = Some(1);
    }
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "report-test",
            agent_kind: AgentKind::new_unchecked("codex"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

fn child_run(workspace_id: &WorkspaceId, name: &str, status: RunStatus) -> RunRecord {
    let mut record = run(status);
    record.workspace_id = workspace_id.clone();
    record.agent_id = Some(AgentSessionId::from(name));
    record.agent_name = Some(name.to_owned());
    record.subagent = true;
    record
}

#[test]
fn fleet_reports_each_answer_after_a_sibling_settles() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    for name in ["a", "b"] {
        append_agent(&store, name, Some("parent"));
    }
    let mut a = child_run(&workspace.workspace_id, "a", RunStatus::Completed);
    a.last_message = Some("first answer".into());
    let b = child_run(&workspace.workspace_id, "b", RunStatus::Running);
    for record in [&a, &b] {
        run::create(store.paths(), record).unwrap();
    }
    assert_eq!(
        report_settled_child(&workspace, &store, &a).unwrap(),
        ReportOutcome::SiblingsRunning
    );
    let mut observation = AgentLifecycleObservation::new(
        Some("a".into()),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    run::record_lifecycle(
        store.paths(),
        &a.run_id,
        "codex",
        &observation,
        None,
        || None,
    )
    .unwrap();
    observation.signal = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    run::record_lifecycle(
        store.paths(),
        &a.run_id,
        "codex",
        &observation,
        Some("second answer".into()),
        || None,
    )
    .unwrap();
    observation.agent_id = Some("b".into());
    let b = run::record_lifecycle(
        store.paths(),
        &b.run_id,
        "codex",
        &observation,
        Some("b answer".into()),
        || None,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(
        report_settled_child(&workspace, &store, &b).unwrap(),
        ReportOutcome::Queued { .. }
    ));
    let messages = store.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    let digest = &messages[0].text;
    assert_eq!(
        digest
            .lines()
            .filter(|line| line.starts_with("- @"))
            .count(),
        3,
        "{digest}"
    );
    for (name, answer) in [
        ("a.output", "first answer\n"),
        ("a.2.output", "second answer\n"),
        ("b.output", "b answer\n"),
    ] {
        let path = store.paths().out_reader_dir(Some(&name[..1])).join(name);
        assert!(digest.contains(path.to_str().unwrap()), "{digest}");
        assert_eq!(std::fs::read_to_string(path).unwrap(), answer);
    }
}

#[test]
fn follow_up_digest_preserves_both_response_paths() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    append_agent(&store, "child", Some("parent"));
    let record = child_run(&workspace.workspace_id, "child", RunStatus::Running);
    run::create(store.paths(), &record).unwrap();
    let mut replies = Vec::new();
    for (index, answer) in ["first answer", "second answer"].into_iter().enumerate() {
        let mut observation = AgentLifecycleObservation::new(
            Some("child".into()),
            LifecycleSignal::TurnStarted { turn_id: None },
        );
        run::record_lifecycle(
            store.paths(),
            &record.run_id,
            "codex",
            &observation,
            None,
            || None,
        )
        .unwrap();
        observation.signal = LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        };
        let settled = run::record_lifecycle(
            store.paths(),
            &record.run_id,
            "codex",
            &observation,
            Some(answer.into()),
            || None,
        )
        .unwrap()
        .unwrap();
        let ReportOutcome::Queued { message_id, .. } =
            report_settled_child(&workspace, &store, &settled).unwrap()
        else {
            panic!("each settled turn must queue a report");
        };
        let messages = store.list_messages().unwrap();
        let digest = &messages
            .iter()
            .find(|message| message.message_id == message_id)
            .unwrap()
            .text;
        let path = digest
            .split("response: ")
            .nth(1)
            .unwrap()
            .split(" (")
            .next()
            .unwrap();
        assert!(
            path.ends_with(if index == 0 {
                "/child.output"
            } else {
                "/child.2.output"
            }),
            "{digest}"
        );
        replies.push((PathBuf::from(path), format!("{answer}\n")));
    }
    for (path, answer) in replies {
        assert_eq!(std::fs::read_to_string(path).unwrap(), answer);
    }
    run::report::join_and_settle_digest(
        &store,
        &workspace.session_name,
        &record.run_id,
        None,
        "stopped by parent",
    )
    .unwrap();
    assert!(
        store.list_messages().unwrap().is_empty(),
        "dismissal must cancel both answers' digests"
    );
    assert_eq!(
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
        ReportOutcome::NothingToReport
    );
}

#[test]
fn reopening_keeps_a_queued_digest_until_its_answer_is_joined() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    for name in ["a", "b"] {
        append_agent(&store, name, Some("parent"));
    }
    let a = child_run(&workspace.workspace_id, "a", RunStatus::Completed);
    let b = child_run(&workspace.workspace_id, "b", RunStatus::Completed);
    for record in [&a, &b] {
        run::create(store.paths(), record).unwrap();
    }
    let ReportOutcome::Queued { message_id, .. } =
        report_settled_child(&workspace, &store, &a).unwrap()
    else {
        panic!("digest should queue");
    };
    let mut observation = AgentLifecycleObservation::new(
        Some("a".into()),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    run::record_lifecycle(
        store.paths(),
        &a.run_id,
        "codex",
        &observation,
        None,
        || None,
    )
    .unwrap();
    run::report::join_and_settle_digest(
        &store,
        &workspace.session_name,
        &b.run_id,
        Some(1),
        "joined inline",
    )
    .unwrap();
    assert!(!run::report::digest_fully_joined(store.paths(), &message_id).unwrap());
    assert!(
        store
            .list_messages()
            .unwrap()
            .iter()
            .any(|message| message.message_id == message_id)
    );
    observation.signal = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    let settled = run::record_lifecycle(
        store.paths(),
        &a.run_id,
        "codex",
        &observation,
        Some("second answer".into()),
        || None,
    )
    .unwrap()
    .unwrap();
    let ReportOutcome::Queued {
        message_id: next_message_id,
        ..
    } = report_settled_child(&workspace, &store, &settled).unwrap()
    else {
        panic!("follow-up digest should queue");
    };
    assert_ne!(next_message_id, message_id);
    let messages = store.list_messages().unwrap();
    let first = messages
        .iter()
        .find(|message| message.message_id == message_id)
        .unwrap();
    assert_eq!(first.status, MessageStatus::Queued);
    assert!(!run::report::digest_fully_joined(store.paths(), &message_id).unwrap());
    let digest = &messages
        .iter()
        .find(|message| message.message_id == next_message_id)
        .unwrap()
        .text;
    let rows = digest
        .lines()
        .filter(|line| line.starts_with("- @"))
        .collect::<Vec<_>>();
    assert_eq!(rows.len(), 1, "{digest}");
    assert!(rows[0].starts_with("- @a:"), "{digest}");
    let path = store.paths().out_reader_dir(Some("a")).join("a.2.output");
    assert!(rows[0].contains(path.to_str().unwrap()), "{digest}");
    assert!(!digest.contains("/a.output"), "{digest}");
    assert!(!digest.contains("/b.output"), "{digest}");
}

#[test]
fn joining_current_answer_leaves_the_first_answer_owed() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    append_agent(&store, "a", Some("parent"));
    let mut a = child_run(&workspace.workspace_id, "a", RunStatus::Completed);
    a.last_message = Some("first answer".into());
    run::create(store.paths(), &a).unwrap();
    run::publish_response(store.paths(), &a).unwrap();
    let mut observation = AgentLifecycleObservation::new(
        Some("a".into()),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    run::record_lifecycle(
        store.paths(),
        &a.run_id,
        "codex",
        &observation,
        None,
        || None,
    )
    .unwrap();
    observation.signal = LifecycleSignal::TurnEnded {
        errored: false,
        parked_on_background: false,
        turn_id: None,
    };
    let settled = run::record_lifecycle(
        store.paths(),
        &a.run_id,
        "codex",
        &observation,
        Some("second answer".into()),
        || None,
    )
    .unwrap()
    .unwrap();
    run::report::join_and_settle_digest(
        &store,
        &workspace.session_name,
        &a.run_id,
        Some(2),
        "joined inline",
    )
    .unwrap();
    assert_eq!(
        rimz::harness::owed::owed_wake(
            &store,
            &AgentKind::new_unchecked("codex"),
            &AgentSessionId::from("parent")
        )
        .unwrap(),
        Some(rimz::harness::owed::OwedWake::Subagents)
    );
    let ReportOutcome::Queued { .. } = report_settled_child(&workspace, &store, &settled).unwrap()
    else {
        panic!("first answer is still owed");
    };
    let messages = store.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].text.contains("/a.output"));
    assert!(!messages[0].text.contains("/a.2.output"));
}

#[test]
fn fleet_header_and_heading_follow_launch_kind() {
    for flags in [[true, true], [false, false], [true, false]] {
        let (_dir, workspace, store) = fixture();
        append_agent(&store, "parent", None);
        for (name, subagent) in ["first", "second"].into_iter().zip(flags) {
            append_agent(&store, name, Some("parent"));
            let mut record = child_run(&workspace.workspace_id, name, RunStatus::Completed);
            record.subagent = subagent;
            run::create(store.paths(), &record).unwrap();
        }
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap();
        let messages = store.list_messages().unwrap();
        assert_eq!(messages.len(), 1);
        let (notice, noun) = if flags == [true, true] {
            (HarnessNotice::SubagentReport, "subagents")
        } else {
            (HarnessNotice::AgentReport, "background agents")
        };
        assert_eq!(messages[0].sender, MessageSender::Harness { notice });
        assert!(
            messages[0]
                .text
                .starts_with(&format!("All 2 {noun} settled:"))
        );
    }
}

#[test]
fn report_fleet_stamps_all_rows_before_queueing_once() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    for name in ["first", "second"] {
        append_agent(&store, name, Some("parent"));
    }
    let first = child_run(&workspace.workspace_id, "first", RunStatus::Completed);
    let second = child_run(&workspace.workspace_id, "second", RunStatus::Canceled);
    for record in [&first, &second] {
        run::create(store.paths(), record).unwrap();
    }

    let ReportOutcome::Queued { message_id, .. } =
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap()
    else {
        panic!("digest should queue");
    };
    for record in [&first, &second] {
        assert_eq!(
            run::load(store.paths(), &record.run_id)
                .unwrap()
                .report_message_id,
            Some(message_id.clone())
        );
    }
    assert_eq!(
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
        ReportOutcome::NothingToReport
    );
    assert_eq!(store.list_messages().unwrap().len(), 1);

    append_agent(&store, "loner", None);
    assert_eq!(
        report_fleet(&workspace, &store, &AgentSessionId::from("loner")).unwrap(),
        ReportOutcome::NothingToReport,
        "a launcher with no members reports without reading the runs"
    );
}

fn append_peer(store: &Store, name: &str, launcher: Option<&str>) {
    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(name)),
        LifecycleSignal::Registered,
    );
    observation.agent_name = Some(name.to_owned());
    observation.launch.launch_depth = Some(1);
    observation.launch.launched_by = launcher.map(|launcher| {
        Box::new(rimz::agents::LaunchedBy {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: AgentSessionId::from(launcher),
        })
    });
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "report-test",
            agent_kind: AgentKind::new_unchecked("codex"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

#[test]
fn backstop_settles_dead_and_stranded_peer_turns_before_reporting() {
    for ended in [false, true] {
        let (_dir, workspace, store) = fixture();
        append_agent(&store, "launcher", None);
        append_peer(&store, "peer", Some("launcher"));
        store
            .begin_agent_launch_batch(
                &[rimz::store::writer::AgentLaunchRequest {
                    login: rimz::store::writer::LaunchLogin::RoomDefault,
                    kind: AgentKind::new_unchecked("codex"),
                    agent_id: "peer".into(),
                    name: rimz::store::writer::AgentLaunchName::Mint,
                    launch: rimz::agents::LaunchParams {
                        launched_by: Some(Box::new(rimz::agents::LaunchedBy {
                            kind: AgentKind::new_unchecked("codex"),
                            agent_id: "launcher".into(),
                        })),
                        ..Default::default()
                    },
                    run_id: None,
                    prompt: None,
                }],
                rimz::store::writer::AgentLaunchScope {
                    session_name: "report-test".into(),
                    cwd: workspace.worktree_root.clone(),
                    branch: None,
                    description: None,
                },
            )
            .unwrap();
        if ended {
            let observation =
                AgentLifecycleObservation::new(Some("peer".into()), LifecycleSignal::Ended);
            store
                .append_agent_lifecycle(AgentLifecycleIntent {
                    session_name: "report-test",
                    agent_kind: AgentKind::new_unchecked("codex"),
                    event_name: "test",
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        }
        let mut record = child_run(&workspace.workspace_id, "peer", RunStatus::Running);
        record.subagent = false;
        record.peer = Some(rimz::store::run::PeerRun {
            launch_id: "peer".into(),
            opened_by: Vec::new(),
        });
        record.parked_at = (!ended).then_some(Timestamp::now());
        record.last_message = Some("waiting".into());
        run::create(store.paths(), &record).unwrap();
        settle_peer_turns(&store, &"launcher".into()).unwrap();
        assert_eq!(
            run::load(store.paths(), &record.run_id).unwrap().status,
            RunStatus::Failed
        );
        assert!(matches!(
            report_fleet(&workspace, &store, &"launcher".into()).unwrap(),
            ReportOutcome::Queued { .. }
        ));
        assert_eq!(store.list_messages().unwrap().len(), 1);
    }
}

#[test]
fn idle_ended_peer_reports_nothing() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "launcher", None);
    append_peer(&store, "peer", Some("launcher"));
    let observation = AgentLifecycleObservation::new(Some("peer".into()), LifecycleSignal::Ended);
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "report-test",
            agent_kind: AgentKind::new_unchecked("codex"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
    settle_peer_turns(&store, &"launcher".into()).unwrap();
    assert!(run::list(store.paths()).unwrap().is_empty());
    assert_eq!(
        report_fleet(&workspace, &store, &"launcher".into()).unwrap(),
        ReportOutcome::NothingToReport
    );
    assert!(store.list_messages().unwrap().is_empty());
}

#[test]
fn two_peer_turns_survive_a_running_sibling_and_share_one_digest() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "launcher", None);
    append_peer(&store, "peer", Some("launcher"));
    append_agent(&store, "child", Some("launcher"));
    let mut first = child_run(&workspace.workspace_id, "peer", RunStatus::Completed);
    first.subagent = false;
    first.peer = Some(rimz::store::run::PeerRun {
        launch_id: "peer".into(),
        opened_by: Vec::new(),
    });
    first.prompt = "first task".into();
    first.last_message = Some("first answer".into());
    let mut second = first.clone();
    second.run_id = rimz::RunId::new();
    second.prompt = "second task".into();
    second.last_message = Some("second answer".into());
    let child = child_run(&workspace.workspace_id, "child", RunStatus::Running);
    for record in [&first, &second, &child] {
        run::create(store.paths(), record).unwrap();
    }
    assert_eq!(
        report_settled_child(&workspace, &store, &second).unwrap(),
        ReportOutcome::SiblingsRunning
    );
    let first_path = response_path(store.paths(), &first).unwrap();
    let second_path = response_path(store.paths(), &second).unwrap();
    assert_eq!(
        std::fs::read_to_string(&first_path).unwrap(),
        "first answer\n"
    );
    assert_eq!(
        std::fs::read_to_string(&second_path).unwrap(),
        "second answer\n"
    );
    let child = run::fail_if_nonterminal(store.paths(), &child.run_id, "")
        .unwrap()
        .unwrap();
    assert!(matches!(
        report_settled_child(&workspace, &store, &child).unwrap(),
        ReportOutcome::Queued { .. }
    ));
    let messages = store.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]
            .text
            .starts_with("All 3 background agents settled")
    );
    assert!(messages[0].text.contains("first task"));
    assert!(messages[0].text.contains("second task"));
    assert_eq!(messages[0].text.matches("- @peer:").count(), 2);
    assert_eq!(
        std::fs::read_to_string(first_path).unwrap(),
        "first answer\n"
    );
    assert_eq!(
        std::fs::read_to_string(second_path).unwrap(),
        "second answer\n"
    );
}

#[test]
fn settled_background_peer_reports_to_its_launcher_only() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "launcher", None);
    append_peer(&store, "peer", Some("launcher"));
    append_peer(&store, "shell-peer", None);
    append_agent(&store, "child", Some("launcher"));
    let mut peer = child_run(&workspace.workspace_id, "peer", RunStatus::Completed);
    peer.subagent = false;
    peer.last_message = Some("peer answer".to_owned());
    let mut shell_peer = child_run(&workspace.workspace_id, "shell-peer", RunStatus::Completed);
    shell_peer.subagent = false;
    let child = child_run(&workspace.workspace_id, "child", RunStatus::Running);
    for record in [&peer, &shell_peer, &child] {
        run::create(store.paths(), record).unwrap();
    }

    assert_eq!(
        report_settled_child(&workspace, &store, &shell_peer).unwrap(),
        ReportOutcome::NoParent
    );
    assert_eq!(
        report_settled_child(&workspace, &store, &peer).unwrap(),
        ReportOutcome::SiblingsRunning
    );
    let path = response_path(store.paths(), &peer).unwrap();
    assert!(path.exists(), "running siblings must not delay publication");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "peer answer\n");
    std::fs::File::open(&path)
        .unwrap()
        .set_modified(std::time::SystemTime::UNIX_EPOCH)
        .unwrap();
    let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
    let child = run::fail_if_nonterminal(store.paths(), &child.run_id, "")
        .unwrap()
        .unwrap();
    assert!(matches!(
        report_settled_child(&workspace, &store, &child).unwrap(),
        ReportOutcome::Queued { .. }
    ));
    let messages = store.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(
        messages[0]
            .text
            .starts_with("All 2 background agents settled:")
    );
    assert!(messages[0].text.contains("@peer"));
    assert!(messages[0].text.contains("@child"));
    assert!(!messages[0].text.contains("@shell-peer"));
    assert_eq!(
        std::fs::read_to_string(response_path(store.paths(), &peer).unwrap()).unwrap(),
        "peer answer\n"
    );
    assert_eq!(
        std::fs::metadata(path).unwrap().modified().unwrap(),
        modified
    );
}

#[test]
fn dismissed_child_is_not_reported() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    for name in ["first", "second"] {
        append_agent(&store, name, Some("parent"));
    }
    let first = child_run(&workspace.workspace_id, "first", RunStatus::Completed);
    let mut second = child_run(&workspace.workspace_id, "second", RunStatus::Completed);
    second.agent_name = Some("receipt-name".into());
    second.last_message = Some("joined answer".into());
    for record in [&first, &second] {
        run::create(store.paths(), record).unwrap();
    }
    run::report::join_and_settle_digest(
        &store,
        &workspace.session_name,
        &second.run_id,
        None,
        "stopped by parent",
    )
    .unwrap();
    run::cancel(store.paths(), &second.run_id).unwrap();

    assert!(matches!(
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
        ReportOutcome::Queued { .. }
    ));
    let messages = store.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    assert!(messages[0].text.contains("Your subagent settled"));
    assert!(messages[0].text.contains("@first"));
    assert!(!messages[0].text.contains("@second"));
    assert!(
        response_path(store.paths(), &second).unwrap().exists(),
        "joined rows still publish at the run's name"
    );
    assert_eq!(
        std::fs::read_to_string(response_path(store.paths(), &second).unwrap()).unwrap(),
        "joined answer\n"
    );
}

#[test]
fn dismissing_every_child_reports_nothing() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    for name in ["first", "second"] {
        append_agent(&store, name, Some("parent"));
        let record = child_run(&workspace.workspace_id, name, RunStatus::Running);
        run::create(store.paths(), &record).unwrap();
        run::report::join_and_settle_digest(
            &store,
            &workspace.session_name,
            &record.run_id,
            None,
            "stopped by parent",
        )
        .unwrap();
        run::cancel(store.paths(), &record.run_id).unwrap();
    }

    assert_eq!(
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
        ReportOutcome::NothingToReport
    );
    assert!(store.list_messages().unwrap().is_empty());
}

#[test]
fn dismissing_every_row_of_a_queued_digest_cancels_it() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    let records = ["first", "second"].map(|name| {
        append_agent(&store, name, Some("parent"));
        let record = child_run(&workspace.workspace_id, name, RunStatus::Completed);
        run::create(store.paths(), &record).unwrap();
        record
    });
    let ReportOutcome::Queued { message_id, .. } =
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap()
    else {
        panic!("digest should queue");
    };

    for record in &records {
        run::report::join_and_settle_digest(
            &store,
            &workspace.session_name,
            &record.run_id,
            None,
            "stopped by parent",
        )
        .unwrap();
    }

    assert!(store.list_messages().unwrap().is_empty());
    let history = store.list_message_history().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].message_id, message_id);
    assert_eq!(history[0].status, MessageStatus::Canceled);
}

#[test]
fn a_running_detached_sibling_neither_holds_nor_joins_the_digest() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    for name in ["a", "b"] {
        append_agent(&store, name, Some("parent"));
    }
    let mut attached = child_run(&workspace.workspace_id, "a", RunStatus::Completed);
    attached.last_message = Some("attached answer".into());
    let mut detached = child_run(&workspace.workspace_id, "b", RunStatus::Running);
    detached.report_to = rimz::store::run::ReportTo::Nobody;
    for record in [&attached, &detached] {
        run::create(store.paths(), record).unwrap();
    }
    assert!(matches!(
        report_settled_child(&workspace, &store, &attached).unwrap(),
        ReportOutcome::Queued { .. }
    ));
    let messages = store.list_messages().unwrap();
    assert_eq!(messages.len(), 1);
    let digest = &messages[0].text;
    assert!(digest.starts_with("Your subagent settled:"), "{digest}");
    assert!(digest.contains("- @a:"), "{digest}");
    assert!(!digest.contains("@b"), "{digest}");

    let observation = AgentLifecycleObservation::new(
        Some("b".into()),
        LifecycleSignal::TurnEnded {
            errored: false,
            parked_on_background: false,
            turn_id: None,
        },
    );
    let detached = run::record_lifecycle(
        store.paths(),
        &detached.run_id,
        "codex",
        &observation,
        Some("detached answer".into()),
        || None,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        report_settled_child(&workspace, &store, &detached).unwrap(),
        ReportOutcome::NothingToReport
    );
    assert_eq!(store.list_messages().unwrap().len(), 1);
    let detached = run::load(store.paths(), &detached.run_id).unwrap();
    assert!(detached.report_message_id.is_none());
    assert_eq!(
        std::fs::read_to_string(response_path(store.paths(), &detached).unwrap()).unwrap(),
        "detached answer\n",
        "the response file is published without a report"
    );
}

fn plus_one_clock(now: &str) -> Clock {
    Clock {
        now: now.parse().unwrap(),
        tz: jiff::tz::TimeZone::fixed(jiff::tz::offset(1)),
    }
}

fn limit_failed(cause: rimz::store::run::LimitFailureCause, refill_at: Option<&str>) -> RunRecord {
    let mut record = run(RunStatus::Failed);
    record.completed_at = Some(record.started_at + std::time::Duration::from_secs(12 * 60));
    record.updated_at = record.completed_at.unwrap();
    record.failure_tail = Some("stopped on a provider limit: \"Usage limit reached\"".into());
    record.limit_failure = Some(rimz::store::run::LimitFailure {
        cause,
        refill_at: refill_at.map(|at| at.parse().unwrap()),
        label: Some("Usage limit reached".into()),
    });
    record
}

#[test]
fn a_limit_failure_row_names_what_the_parent_can_do() {
    use rimz::store::run::LimitFailureCause as Cause;
    let child = child("still-silver", Some("review correctness"));
    let clock = plus_one_clock("2026-11-11T17:50:00Z");
    let refill = Some("2026-11-11T20:00:00Z");
    let passed = Some("2026-11-11T17:00:00Z");
    let changed = ResumeAccount::Changed(Some("2026-11-11T20:00:00Z".parse().unwrap()));
    let cases = [
        (Cause::RefillKnown, refill, ResumeAccount::Unreadable),
        (Cause::RefillKnown, refill, ResumeAccount::Changed(None)),
        (Cause::RefillUnknown, None, changed),
        (Cause::ContinueFailed, None, ResumeAccount::Same),
        (
            Cause::BeyondHoldLimit,
            Some("2026-11-12T03:00:00Z"),
            ResumeAccount::Same,
        ),
        (Cause::RefillKnown, refill, ResumeAccount::Same),
        (Cause::Refilled, None, ResumeAccount::Same),
        (Cause::RefillKnown, passed, ResumeAccount::Same),
        (Cause::BeyondHoldLimit, passed, ResumeAccount::Same),
        (Cause::RefillUnknown, None, ResumeAccount::Same),
    ];
    let rows = cases.map(|(cause, refill_at, account)| {
        let record = limit_failed(cause, refill_at);
        compose_answer_row(&child, &record, None, None, account, &clock)
    });
    let row = |tail: &str| {
        format!(
            "- @still-silver: failed in 12m; stopped on a provider limit: \"Usage limit reached\"{tail}, task: \"review correctness\", no response"
        )
    };
    let refilled = row("; the limit has refilled. Message it to continue it");
    let after =
        row("; it refills in 2h10m (at 21:00 +01:00). Message it after the refill to continue it");
    assert_eq!(
        rows,
        [
            row(
                ". The room's codex account has changed and cannot read its session: do the task yourself"
            ),
            row(
                ". The room's codex account has changed since it stopped: message it to retry now; if that fails too, do the task yourself"
            ),
            after.clone(),
            row(
                ", and continuing it after the refill failed. Message it to retry; if that fails too, do the task yourself"
            ),
            row(
                "; it refills in 9h10m (at 04:00 +01:00 tomorrow). Do the task yourself; a message after the refill continues it"
            ),
            after,
            refilled.clone(),
            refilled.clone(),
            refilled,
            row(". Message it to retry; if that fails too, do the task yourself"),
        ]
    );

    let mut unlabeled = limit_failed(Cause::RefillUnknown, None);
    if let Some(failure) = unlabeled.limit_failure.as_mut() {
        failure.label = None;
    }
    assert_eq!(
        compose_answer_row(&child, &unlabeled, None, None, ResumeAccount::Same, &clock),
        "- @still-silver: failed in 12m; stopped on a provider limit. Message it to retry; if that fails too, do the task yourself, task: \"review correctness\", no response"
    );
}

#[test]
fn an_earlier_answer_renders_its_own_limit_failure() {
    let child = child("still-silver", Some("review correctness"));
    let clock = plus_one_clock("2026-11-11T17:50:00Z");
    let failed = limit_failed(
        rimz::store::run::LimitFailureCause::RefillKnown,
        Some("2026-11-11T20:00:00Z"),
    );
    let mut value = serde_json::to_value(&failed).unwrap();
    value["earlier_answers"] = serde_json::json!([{
        "ordinal": 1,
        "status": "failed",
        "started_at": failed.started_at,
        "completed_at": failed.completed_at,
        "failure_tail": failed.failure_tail,
        "limit_failure": failed.limit_failure,
    }]);
    value["status"] = serde_json::json!("running");
    value["follow_ups"] = serde_json::json!(1);
    value["limit_failure"] = serde_json::Value::Null;
    value["failure_tail"] = serde_json::Value::Null;
    let reopened: RunRecord = serde_json::from_value(value).unwrap();
    let answer = &reopened.earlier_answers[0];
    assert_eq!(
        compose_answer_row(
            &child,
            &reopened,
            Some(answer),
            None,
            ResumeAccount::Same,
            &clock
        ),
        "- @still-silver: failed in 12m; stopped on a provider limit: \"Usage limit reached\"; it refills in 2h10m (at 21:00 +01:00). Message it after the refill to continue it, task: \"review correctness\", no response"
    );
}

fn held(name: &str, until: &str) -> (AgentState, RunRecord) {
    let mut child = child(name, None);
    child.status = AgentStatus::Running;
    let at = child.last_activity + std::time::Duration::from_secs(1);
    let mut context = rimz::agents::AgentContext::new(child.kind.as_str(), at);
    context.turn_error = Some(rimz::agents::AgentTurnError {
        ended_turn: false,
        class: rimz::agents::TurnErrorClass::PausedRateLimit,
        at,
        label: Some("Usage limit reached".into()),
    });
    child.context = Some(context);
    let mut record = run(RunStatus::Running);
    record.completed_at = None;
    record.limit_hold = Some(rimz::store::run::LimitHold {
        activity: child.last_activity,
        since: at,
        until: until.parse().unwrap(),
    });
    (child, record)
}

#[test]
fn a_held_child_gets_a_line_and_the_heading_counts_it() {
    let clock = plus_one_clock("2026-11-11T16:55:00Z");
    let (otter, otter_run) = held("calm-otter", "2026-11-11T20:00:00Z");
    let (fox, fox_run) = held("quick-fox", "2026-11-11T16:00:00Z");
    let naming = child("naming", None);
    let completed = run(RunStatus::Completed);
    let response = ResponseFile {
        path: PathBuf::from("/state/out/planner/naming.output"),
        summary: FileSummary {
            bytes: 19,
            lines: 2,
            tokens: 1_200,
        },
    };
    let answer = (
        &naming,
        &completed,
        None,
        Some(&response),
        ResumeAccount::Same,
    );
    let waiting = "- @calm-otter: paused on a provider limit: \"Usage limit reached\"; it refills in 3h05m (at 21:00 +01:00) and RimZ continues it then. Its report follows";
    let continuing = "- @quick-fox: paused on a provider limit: \"Usage limit reached\"; the limit has refilled and RimZ is continuing it. Its report follows";
    let row = "- @naming: completed in 4m12s, task: \"map it\", response: /state/out/planner/naming.output (~1.2k tokens, 2 lines)";
    assert_eq!(
        compose_digest(&[answer], &[(&otter, &otter_run)], true, &clock),
        format!("1 of 2 subagents settled:\n{row}\n{waiting}")
    );
    assert_eq!(
        compose_digest(
            &[answer, answer],
            &[(&otter, &otter_run), (&fox, &fox_run)],
            true,
            &clock
        ),
        format!(
            "2 of 4 subagents settled, responses total ~2.4k tokens, 4 lines:\n{row}\n{row}\n{waiting}\n{continuing}"
        )
    );
    let mut unlabeled = otter.clone();
    unlabeled.context = None;
    assert!(
        compose_digest(&[answer], &[(&unlabeled, &otter_run)], true, &clock)
            .contains("- @calm-otter: paused on a provider limit; it refills in 3h05m"),
        "a row without a displayed marker drops only the label"
    );
    assert_eq!(
        compose_digest(&[answer], &[], true, &clock),
        format!("Your subagent settled:\n{row}"),
        "no held line leaves the heading as it was"
    );
}

fn start_turn(store: &Store, name: &str) {
    let observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from(name)),
        LifecycleSignal::TurnStarted { turn_id: None },
    );
    store
        .append_agent_lifecycle(AgentLifecycleIntent {
            session_name: "report-test",
            agent_kind: AgentKind::new_unchecked("codex"),
            event_name: "test",
            observation: &observation,
            spawned_subagents: &[],
        })
        .unwrap();
}

fn seed_limit_marker(store: &Store, name: &str) {
    let at = Timestamp::now() + std::time::Duration::from_secs(5);
    let mut context = rimz::agents::AgentContext::new("codex", at);
    context.turn_error = Some(rimz::agents::AgentTurnError {
        ended_turn: false,
        class: rimz::agents::TurnErrorClass::PausedRateLimit,
        at,
        label: Some("Usage limit reached".into()),
    });
    rimz::store::agent_context::write_record(
        store.runtime_paths(),
        &rimz::agents::context::record::AgentContextRecord::new("codex", name, context),
    )
    .unwrap();
}

#[test]
fn a_held_child_neither_blocks_nor_joins_its_siblings_report() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    for name in ["held", "a", "b"] {
        append_agent(&store, name, Some("parent"));
    }
    start_turn(&store, "held");
    seed_limit_marker(&store, "held");
    let mut held = child_run(&workspace.workspace_id, "held", RunStatus::Running);
    let now = Timestamp::now();
    held.limit_hold = Some(rimz::store::run::LimitHold {
        activity: now,
        since: now,
        until: now + std::time::Duration::from_secs(3 * 3600),
    });
    let a = child_run(&workspace.workspace_id, "a", RunStatus::Completed);
    let mut b = child_run(&workspace.workspace_id, "b", RunStatus::Running);
    for record in [&held, &a, &b] {
        run::create(store.paths(), record).unwrap();
    }
    assert_eq!(
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
        ReportOutcome::SiblingsRunning,
        "a working sibling still holds the report"
    );
    b.status = RunStatus::Completed;
    run::create(store.paths(), &b).unwrap();
    let mut digests = Vec::new();
    for _ in 0..2 {
        let ReportOutcome::Queued { message_id, .. } =
            report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap()
        else {
            panic!("a held child must not hold its siblings' report");
        };
        let messages = store.list_messages().unwrap();
        let text = messages
            .iter()
            .find(|message| message.message_id == message_id)
            .unwrap()
            .text
            .clone();
        digests.push(text);
        let held = run::load(store.paths(), &held.run_id).unwrap();
        assert!(
            held.report_message_id.is_none(),
            "the held run is not stamped"
        );
        assert!(held.limit_hold.is_some());
        let answered = run::load(store.paths(), &b.run_id).unwrap();
        assert!(answered.report_message_id.is_some());
        // Reopen the sibling so the next report has an answer to carry.
        let mut reopened = answered;
        reopened.status = RunStatus::Completed;
        reopened.report_message_id = None;
        run::create(store.paths(), &reopened).unwrap();
    }
    for digest in &digests {
        assert!(
            digest.starts_with("1 of 2 subagents settled:")
                || digest.starts_with("2 of 3 subagents settled"),
            "{digest}"
        );
        assert!(
            digest.contains(
                "- @held: paused on a provider limit: \"Usage limit reached\"; it refills in "
            ),
            "the label comes from the stored context: {digest}"
        );
    }
    assert_eq!(
        digests
            .iter()
            .filter(|digest| digest.contains("@held:"))
            .count(),
        2,
        "the held line repeats in every report"
    );
}

#[test]
fn a_fleet_with_only_held_children_reports_nothing() {
    let (_dir, workspace, store) = fixture();
    append_agent(&store, "parent", None);
    append_agent(&store, "held", Some("parent"));
    let mut held = child_run(&workspace.workspace_id, "held", RunStatus::Running);
    let now = Timestamp::now();
    held.limit_hold = Some(rimz::store::run::LimitHold {
        activity: now,
        since: now,
        until: now,
    });
    run::create(store.paths(), &held).unwrap();
    assert_eq!(
        report_fleet(&workspace, &store, &AgentSessionId::from("parent")).unwrap(),
        ReportOutcome::NothingToReport
    );
    assert!(store.list_messages().unwrap().is_empty());
}
