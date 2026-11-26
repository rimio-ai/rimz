use super::*;
use jiff::Timestamp;

#[test]
fn self_signal_requires_a_concrete_other_agent() {
    let target = TaskTarget {
        kind: crate::ids::AgentKind::new_unchecked("claude"),
        session: "self".into(),
        handle: "@self".to_owned(),
    };
    for key in ["handle", "session"] {
        let matches = BTreeMap::from([(key.to_owned(), "*".to_owned())]);
        assert!(matches!(
            validate_self_signal(&"agent.ended".parse().unwrap(), &matches, &target, None),
            Err(DeliveryScopeFailure::SelfSignal)
        ));
    }
}

#[test]
fn self_wait_provenance_admits_only_one_shot_triggers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let workspace = crate::workspace::WorkspaceResolver::resolve(dir.path(), None).unwrap();
    let condition = |hold: Option<Duration>, lifetime| DeliveryTrigger::Condition {
        expr: crate::harness::when::WhenExpr::parse(&["pr=merged".to_owned()]).unwrap(),
        hold,
        lifetime,
        check_in: None,
    };
    let now = Timestamp::now();
    let cases = [
        (
            "watch",
            DeliveryTrigger::Watch {
                spec: WatchSpec::Command("true".to_owned()),
                on: CheckOn::Any,
                timeout: None,
            },
            true,
        ),
        (
            "once condition",
            condition(None, SubscriptionLifetime::Once),
            true,
        ),
        (
            "standing condition",
            condition(None, SubscriptionLifetime::Standing),
            false,
        ),
        (
            "held condition",
            condition(Some(Duration::from_secs(60)), SubscriptionLifetime::Once),
            false,
        ),
        (
            "signal",
            DeliveryTrigger::Signal {
                selector: "deploy.done".parse().unwrap(),
                matches: BTreeMap::new(),
                lifetime: SubscriptionLifetime::Once,
            },
            false,
        ),
    ];
    for (case, trigger, admitted) in cases {
        let spec = DeliverySpec {
            name: DeliveryName::Mint,
            label: None,
            target: TaskTarget {
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                session: "self".into(),
                handle: "@self".to_owned(),
            },
            trigger,
            prompt: DeliveryPrompt::None,
            provenance: DeliveryProvenance::SelfWait { reader: None },
            deadline: None,
            max_strikes: None,
        };
        match build_entry(&workspace, spec, now) {
            Ok((_, task)) => {
                assert!(admitted, "{case} was admitted");
                let meta = task
                    .entry()
                    .wait_meta
                    .as_ref()
                    .expect("self waits keep wait_meta");
                assert_eq!(meta.armed_at, now, "{case}");
            }
            Err(ArmFailure::InvalidProvenance(reason)) => {
                assert!(!admitted, "{case} was refused: {reason}");
                assert_eq!(
                    reason,
                    "self waits require a once condition without a hold, or a watched command, \
                     without a prompt",
                    "{case}"
                );
            }
            Err(error) => panic!("{case}: {error}"),
        }
    }
}

fn idle_stop_fixture() -> (
    tempfile::TempDir,
    crate::disk::paths::StatePaths,
    crate::disk::paths::RuntimePaths,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let id = crate::ids::WorkspaceId::from_project_root(dir.path());
    let paths = crate::disk::paths::StatePaths::under(id.clone(), &dir.path().join("state"))
        .expect("state paths");
    let workspace = crate::workspace::WorkspaceResolver::resolve(dir.path(), None).unwrap();
    crate::workspace::record::write(
        &paths,
        &crate::workspace::record::WorkspaceRecord::from_resolved(&workspace),
    )
    .unwrap();
    let runtime = crate::disk::paths::RuntimePaths::under(id, &dir.path().join("runtime"))
        .expect("runtime paths");
    for session in ["stopping", "resting"] {
        crate::store::idle_stop::arm(
            &paths,
            crate::store::idle_stop::IdleStopRequest {
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                agent_id: session.into(),
                stop: crate::agents::state::IdleStop {
                    after_secs: 180,
                    requested_at: Timestamp::UNIX_EPOCH,
                    requested_by: None,
                },
            },
        )
        .expect("arm idle stop");
    }
    (dir, paths, runtime)
}

/// An ended agent's retirement when `retire`, else a replaced session's
/// release from a resumed restart, which keeps the conversation.
fn retire_or_release(
    retire: bool,
    paths: &crate::disk::paths::StatePaths,
    runtime: &crate::disk::paths::RuntimePaths,
    root: &Path,
    kind: &crate::ids::AgentKind,
    session: &crate::ids::AgentSessionId,
    cgroup_root: &Path,
) -> Result<usize, RetireFailure> {
    if retire {
        return retire_session_in(paths, runtime, root, kind, session, cgroup_root);
    }
    release_replaced_session_in(paths, runtime, kind, session, false, cgroup_root)
        .map(|()| 0)
        .map_err(|message| RetireFailure {
            message,
            dropped: 0,
        })
}

fn idle_stop_sessions(paths: &crate::disk::paths::StatePaths) -> Vec<String> {
    crate::store::idle_stop::read(paths)
        .into_iter()
        .map(|request| request.agent_id.to_string())
        .collect()
}

#[test]
fn raised_attention_survives_only_a_resumed_release() {
    for retire in [true, false] {
        let (dir, paths, runtime) = idle_stop_fixture();
        let record = serde_json::json!({"version": 1,
            "asks": [{"kind": "claude", "agent_id": "stopping",
                "ask_id": crate::ids::AskId::new(), "state": "open",
                "raised_at": "1970-01-01T00:00:00Z", "basis_at": "1970-01-01T00:00:00Z"}],
            "alerts": [{"kind": "claude", "agent_id": "stopping", "text": "Review",
                "raised_at": "1970-01-01T00:00:00Z"}],
            "human_turns": [{"kind": "claude", "agent_id": "stopping",
                "at": "1970-01-01T00:00:00Z"}]});
        crate::disk::atomic::write_temp_then_rename(&paths.raised, &record).unwrap();
        retire_or_release(
            retire,
            &paths,
            &runtime,
            dir.path(),
            &crate::ids::AgentKind::new_unchecked("claude"),
            &"stopping".into(),
            Path::new("/sys/fs/cgroup"),
        )
        .unwrap();
        let record = crate::store::raised::read(&paths);
        assert_eq!(
            (
                record.asks.len(),
                record.alerts.len(),
                record.human_turns.len()
            ),
            (
                usize::from(!retire),
                usize::from(!retire),
                usize::from(!retire)
            ),
            "retire={retire}"
        );
    }
}

#[test]
fn ended_session_with_only_a_raised_record_is_retired() {
    let (dir, paths, runtime) = idle_stop_fixture();
    let record = serde_json::json!({"version": 1, "asks": [], "alerts": [],
        "human_turns": [{"kind": "claude", "agent_id": "record-only",
            "at": "1970-01-01T00:00:00Z"}]});
    crate::disk::atomic::write_temp_then_rename(&paths.raised, &record).unwrap();
    let mut ended =
        crate::agents::AgentState::stub("claude", "record-only", crate::agents::AgentStatus::Idle);
    ended.ended_at = Some(jiff::Timestamp::UNIX_EPOCH);
    retire_ended_sessions_in(&paths, &runtime, dir.path(), || vec![ended].into()).unwrap();
    let bytes: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&paths.raised).unwrap()).unwrap();
    assert_eq!(bytes["human_turns"], serde_json::json!([]));
}

/// A stored pid, check, or file wait, which this RimZ no longer runs, is retired with a record
/// naming why, and the rest of the file still loads.
#[test]
fn a_legacy_wait_row_is_retired_with_a_record_and_the_rest_loads() {
    let (dir, paths, runtime) = idle_stop_fixture();
    let stamp = Timestamp::now().as_nanosecond();
    let target = serde_json::json!({"kind": "claude", "session": "resting", "handle": "@claude"});
    let meta = serde_json::json!({"armed_at": "2026-01-01T00:00:00Z"});
    let row = |watch: serde_json::Value| serde_json::json!({"root": dir.path(), "wait": target, "wait-meta": meta, "watch": watch});
    let legacy = [
        (
            format!("wait-pid-{stamp}"),
            serde_json::json!({"pid": 4242}),
        ),
        (
            format!("wait-check-{stamp}"),
            serde_json::json!({"check": "true", "every": "1s", "on": "success"}),
        ),
        (
            format!("wait-file-{stamp}"),
            serde_json::json!({"file": "/repo/app.log", "grep": "ready"}),
        ),
    ];
    let mut rows = serde_json::Map::new();
    for (name, watch) in &legacy {
        rows.insert(name.clone(), row(watch.clone()));
    }
    rows.insert("shell-kept".into(), row(serde_json::json!("sleep 600")));
    // A newer RimZ's watch shape this one cannot read is kept for it, not retired.
    rows.insert("shell-future".into(), row(serde_json::json!({"future": 1})));
    crate::disk::atomic::write_temp_then_rename(
        &super::super::instances::path(&paths.root),
        &serde_json::Value::Object(rows),
    )
    .unwrap();

    retire_ended_sessions_in(&paths, &runtime, dir.path(), || Vec::new().into()).unwrap();

    let left = super::super::instances::load_strict_from(&paths.root).unwrap();
    assert_eq!(
        left.0.keys().collect::<Vec<_>>(),
        ["shell-future", "shell-kept"]
    );
    assert!(left.0["shell-kept"].refused.is_none());
    let log = std::fs::read_to_string(crate::disk::paths::logs_dir().join("loop-runs.log.jsonl"))
        .unwrap_or_default();
    for (name, _) in &legacy {
        let record = log
            .lines()
            .map(|line| serde_json::from_str::<super::super::run_log::LoopRunRecord>(line).unwrap())
            .find(|record| &record.task == name)
            .unwrap_or_else(|| panic!("no record retires {name}: {log}"));
        assert_eq!(
            record.result,
            super::super::run_log::LoopRunResult::Canceled
        );
        assert_eq!(
            record.error.as_deref(),
            Some(LEGACY_WAIT_RETIRED),
            "{record:?}"
        );
    }
}

#[test]
#[cfg(target_os = "linux")]
fn retiring_hibernating_session_thaws_and_withdraws() {
    for retire in [true, false] {
        let (dir, paths, runtime) = idle_stop_fixture();
        let uid = nix::unistd::Uid::current().as_raw();
        let scope = dir.path().join(format!(
            "user.slice/user-{uid}.slice/user@{uid}.service/app.slice/rimz-agent-test.scope"
        ));
        std::fs::create_dir_all(&scope).unwrap();
        std::fs::write(scope.join("cgroup.freeze"), "1").unwrap();
        let entry = crate::store::hibernate::Entry {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            agent_id: "stopping".into(),
            pane_id: "tmux:%3".parse().unwrap(),
            scope: scope.clone(),
            provider_pid: 10,
            frozen_at: Timestamp::UNIX_EPOCH,
            anon_at_freeze: 0,
            step: 0,
            wake_error: None,
            reclaimed_bytes: 100,
        };
        crate::store::hibernate::record(&paths, entry.clone()).unwrap();
        retire_or_release(
            retire,
            &paths,
            &runtime,
            dir.path(),
            &entry.kind,
            &entry.agent_id,
            dir.path(),
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(scope.join("cgroup.freeze")).unwrap(),
            "0",
            "retirement thaws before forgetting the entry"
        );
        assert!(crate::store::hibernate::read(&paths).is_empty());
    }
}

/// A hard stop or an agent's end retires the session; a swap releases the
/// session it replaced. Each takes the idle-stop request.
#[test]
fn retiring_or_releasing_a_session_removes_its_idle_stop_request() {
    for retire in [true, false] {
        let (dir, paths, runtime) = idle_stop_fixture();
        let dropped = retire_or_release(
            retire,
            &paths,
            &runtime,
            dir.path(),
            &crate::ids::AgentKind::new_unchecked("claude"),
            &"stopping".into(),
            Path::new("/sys/fs/cgroup"),
        )
        .expect("retire");
        assert_eq!(dropped, 0, "a request is not a wait row");
        assert_eq!(idle_stop_sessions(&paths), ["resting"], "retire={retire}");
    }
}

/// The rows go before the idle-stop request, so a withdrawal that fails
/// still reports the waits already dropped.
#[test]
fn a_failed_withdrawal_carries_the_waits_already_dropped() {
    let (dir, mut paths, runtime) = idle_stop_fixture();
    let entry = TaskEntry {
        wait: Some(TaskTarget {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            session: "stopping".into(),
            handle: "@stopping".to_owned(),
        }),
        root: dir.path().to_path_buf(),
        ..TaskEntry::default()
    };
    super::super::instances::insert_delivery(&paths, None, &entry, &Default::default())
        .expect("wait row");
    // A directory cannot be opened as the withdrawal's lock file.
    paths.workspace_lock = dir.path().to_path_buf();
    let now = Timestamp::UNIX_EPOCH;
    let mut ended = crate::testkit::agent_state("claude", "stopping", now);
    ended.ended_at = Some(now);

    let failure = retire_ended_sessions_in(&paths, &runtime, dir.path(), || vec![ended].into())
        .expect_err("the withdrawal fails");

    assert_eq!(failure.dropped(), 1, "{failure}");
    assert!(super::super::instances::pinned_sessions(&paths.root).is_empty());
    assert_eq!(idle_stop_sessions(&paths), ["stopping", "resting"]);
}

/// A session holding only an idle-stop request, no instance row, is still
/// reconciled once it durably ends.
#[test]
fn ended_session_reconcile_covers_a_request_only_session() {
    let (dir, paths, runtime) = idle_stop_fixture();
    let now = Timestamp::UNIX_EPOCH;
    let mut ended = crate::testkit::agent_state("claude", "stopping", now);
    ended.ended_at = Some(now);
    let agents = vec![ended, crate::testkit::agent_state("claude", "resting", now)];
    retire_ended_sessions_in(&paths, &runtime, dir.path(), || agents.into()).expect("reconcile");
    assert_eq!(idle_stop_sessions(&paths), ["resting"]);
}

/// A swap's replaced session ending leaves the agent's waits armed for the
/// holder and takes only what was the session's own; the holder's end
/// then retires them.
#[test]
fn a_replaced_sessions_end_keeps_its_agents_waits_until_the_agent_ends() {
    let (dir, paths, runtime) = idle_stop_fixture();
    let entry = TaskEntry {
        wait: Some(TaskTarget {
            kind: crate::ids::AgentKind::new_unchecked("claude"),
            session: "stopping".into(),
            handle: "@coder".to_owned(),
        }),
        root: dir.path().to_path_buf(),
        ..TaskEntry::default()
    };
    super::super::instances::insert_delivery(&paths, None, &entry, &Default::default())
        .expect("wait row");
    let now = Timestamp::UNIX_EPOCH;
    let session = |id: &str, epoch: u32| {
        let mut agent = crate::testkit::agent_state("claude", id, now);
        agent.launch_id = Some("launch_a".into());
        agent.epoch = epoch;
        agent
    };
    let mut old = session("stopping", 0);
    old.ended_at = Some(now);
    let mut holder = session("resting", 1);

    let dropped = retire_ended_sessions_in(&paths, &runtime, dir.path(), || {
        vec![old.clone(), holder.clone()].into()
    })
    .expect("reconcile");
    assert_eq!(dropped, 0);
    assert_eq!(
        super::super::instances::pinned_sessions(&paths.root).len(),
        1,
        "the agent lives on in the holder"
    );
    assert_eq!(idle_stop_sessions(&paths), ["resting"]);

    holder.ended_at = Some(now);
    let dropped =
        retire_ended_sessions_in(&paths, &runtime, dir.path(), || vec![old, holder].into())
            .expect("reconcile");
    assert_eq!(dropped, 1);
    assert!(super::super::instances::pinned_sessions(&paths.root).is_empty());
}

/// A replacement that fails after binding ends the agent, in either order
/// against the old session's end: the waits go with the replacement's
/// failure, without waiting for the replaced session to end.
#[test]
fn a_replacement_failing_after_bind_retires_the_agents_waits_in_either_order() {
    for old_ends_first in [true, false] {
        let (dir, paths, runtime) = idle_stop_fixture();
        let entry = TaskEntry {
            wait: Some(TaskTarget {
                kind: crate::ids::AgentKind::new_unchecked("claude"),
                session: "stopping".into(),
                handle: "@coder".to_owned(),
            }),
            root: dir.path().to_path_buf(),
            ..TaskEntry::default()
        };
        super::super::instances::insert_delivery(&paths, None, &entry, &Default::default())
            .expect("wait row");
        let now = Timestamp::UNIX_EPOCH;
        let mut old = crate::testkit::agent_state("claude", "stopping", now);
        old.launch_id = Some("launch_a".into());
        let mut replacement = crate::testkit::agent_state("claude", "launch_swap", now);
        replacement.launch_id = Some("launch_a".into());
        replacement.epoch = 1;
        replacement.launch_bound = true;
        let reconcile = |rows: Vec<crate::agents::AgentState>| {
            retire_ended_sessions_in(&paths, &runtime, dir.path(), || rows.into())
                .expect("reconcile")
        };
        if old_ends_first {
            old.ended_at = Some(now);
        } else {
            replacement.status = crate::agents::AgentStatus::Failed;
        }
        let retired = usize::from(!old_ends_first);
        assert_eq!(reconcile(vec![old.clone(), replacement.clone()]), retired);
        assert_eq!(
            super::super::instances::pinned_sessions(&paths.root).len(),
            1 - retired,
            "old_ends_first={old_ends_first}"
        );
        if !old_ends_first {
            continue;
        }
        replacement.status = crate::agents::AgentStatus::Failed;
        assert_eq!(
            reconcile(vec![old, replacement]),
            1,
            "old_ends_first={old_ends_first}"
        );
        assert!(super::super::instances::pinned_sessions(&paths.root).is_empty());
    }
}

/// An end appended by a producer that reached no hook (the reaper, the exec
/// wrapper, rebirth) is retired by the next frame that appends a tool use.
#[test]
fn a_tool_frame_retires_a_session_another_producer_ended() {
    let temp = tempfile::tempdir().unwrap();
    let state = crate::StatePaths::for_project_root(temp.path()).unwrap();
    let runtime = crate::RuntimePaths::for_state(&state).unwrap();
    let store = crate::Store::open(state.clone(), runtime).unwrap();
    let workspace = crate::workspace::ResolvedWorkspace {
        workspace_id: state.workspace_id.clone(),
        project_root: temp.path().to_path_buf(),
        cwd_project_root: None,
        root_class: crate::workspace::RootClass::Directory,
        worktree_root: temp.path().to_path_buf(),
        worktree_branch: None,
        session_name: "test".into(),
        mux_hint: None,
    };
    let kind = crate::ids::AgentKind::new_unchecked("claude");
    let append = |session: &str, signal: crate::agents::LifecycleSignal| {
        let observation =
            crate::agents::AgentLifecycleObservation::new(Some(session.into()), signal);
        store
            .for_hook_ingress(crate::EventId::new(), Timestamp::now())
            .append_agent_lifecycle(crate::store::writer::AgentLifecycleIntent {
                session_name: "test",
                agent_kind: kind.clone(),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap()
    };
    for session in ["ended", "worker"] {
        append(session, crate::agents::LifecycleSignal::Registered);
    }
    append("ended", crate::agents::LifecycleSignal::Ended);
    crate::store::idle_stop::arm(
        &state,
        crate::store::idle_stop::IdleStopRequest {
            kind: kind.clone(),
            agent_id: "ended".into(),
            stop: crate::agents::state::IdleStop {
                after_secs: 180,
                requested_at: Timestamp::now(),
                requested_by: None,
            },
        },
    )
    .unwrap();
    append(
        "worker",
        crate::agents::LifecycleSignal::TurnStarted { turn_id: None },
    );
    let tool = append(
        "worker",
        crate::agents::LifecycleSignal::ToolUsed {
            effect: crate::agents::lifecycle::ToolEffect::Mutating,
            name: Some("Bash".into()),
            native_key: None,
            turn_id: None,
        },
    );
    assert!(
        tool.primary_event_id.is_some(),
        "the tool use appended an event"
    );

    crate::harness::team_stage::react_to_lifecycle(&workspace, &store, &tool, None);

    assert!(crate::store::idle_stop::read(&state).is_empty());
}

/// The retirement predicate: a pinned session is retired on positive
/// evidence of its own durable end, and on nothing else.
#[test]
fn ended_pinned_sessions_selects_only_durably_ended_pinned_targets() {
    let now = Timestamp::UNIX_EPOCH;
    let session = crate::ids::AgentSessionId::from;
    let kind = crate::ids::AgentKind::new_unchecked;
    let agent = |agent_kind: &str, id: &str, ended: bool| {
        let mut agent = crate::testkit::agent_state(agent_kind, id, now);
        agent.ended_at = ended.then_some(now);
        agent
    };
    let pinned = [
        "ended",
        "revived",
        "subagent",
        "other-kind",
        "launch-gone",
        "launch-live",
        "molted",
        "launch-failed",
        "launch-held",
        "aliased",
        "launch-prior",
        "no-row",
    ]
    .map(|id| (kind("claude"), session(id)))
    .into();
    let member = |agent_kind: &str, id: &str, launch: &str, epoch: u32, ended: bool| {
        let mut agent = agent(agent_kind, id, ended);
        agent.launch_id = Some(crate::ids::AgentSessionId::from(launch));
        agent.epoch = epoch;
        agent
    };
    let failed_reservation = |id: &str, launch: &str, bound: bool| {
        let mut agent = member("claude", id, launch, 1, false);
        agent.launch_bound = bound;
        agent.status = crate::agents::AgentStatus::Failed;
        agent
    };
    let mut subagent = agent("claude", "subagent", true);
    subagent.parent_agent_id = Some(session("parent"));
    subagent.launch_depth = None;
    let agents = vec![
        agent("claude", "ended", true),
        // A later lifecycle event for the same session clears `ended_at`.
        agent("claude", "revived", false),
        subagent,
        // The pin names `claude`; this end is another provider's.
        agent("codex", "other-kind", true),
        // Ended, but nothing is pinned to it.
        agent("claude", "unpinned", true),
        // A launch pin ends with the agent, whichever kind it molted to.
        member("claude", "gone-0", "launch-gone", 0, true),
        member("codex", "gone-1", "launch-gone", 1, true),
        // A session pin ends with its session even when the agent molted on.
        member("claude", "molted", "launch-live", 0, true),
        member("codex", "live-1", "launch-live", 1, false),
        // A bound replacement that failed ends its agent while the
        // session it replaced is still open.
        member("claude", "failed-0", "launch-failed", 0, false),
        failed_reservation("launch_failed_1", "launch-failed", true),
        // One that failed before binding leaves the old session the agent.
        member("claude", "held-0", "launch-held", 0, false),
        failed_reservation("launch_held_1", "launch-held", false),
        // A session pin reaches its launch's later sessions through its row's
        // launch alias.
        member("claude", "aliased", "launch-alias", 0, false),
        member("claude", "alias-1", "launch-alias", 1, true),
        // A launch pin answers to an earlier launch id of its row.
        {
            let mut agent = member("claude", "prior-0", "launch-new", 0, true);
            agent.prior_launch_ids = vec![session("launch-prior")];
            agent
        },
    ];

    let selected = ended_pinned_sessions(&agents, &pinned);
    let per_pin = pinned
        .iter()
        .filter_map(|key| {
            let end = crate::agents::state::agent_end(&agents, &key.0, &key.1);
            let session_ended = agents.iter().any(|agent| {
                agent.ended_at.is_some()
                    && !agent.is_provider_subagent()
                    && agent.kind == key.0
                    && agent.agent_id == key.1
            });
            (session_ended || end == crate::agents::state::AgentEnd::Ended)
                .then_some((key, end != crate::agents::state::AgentEnd::Live))
        })
        .collect::<Vec<_>>();
    assert_eq!(
        selected, per_pin,
        "one pass answers each pin as agent_end does"
    );

    assert_eq!(
        selected
            .iter()
            .map(|((kind, session), agent_ended)| (kind.as_str(), session.as_str(), *agent_ended))
            .collect::<Vec<_>>(),
        vec![
            ("claude", "aliased", true),
            ("claude", "ended", true),
            ("claude", "launch-failed", true),
            ("claude", "launch-gone", true),
            ("claude", "launch-prior", true),
            ("claude", "molted", false),
        ]
    );
}
