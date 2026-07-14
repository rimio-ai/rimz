use super::binding_select::{
    BindingRejectReason, BindingSelectionMethod, BindingSession, PriorAgentPane,
    select_focused_pane_binding,
};
use super::lifecycle::append_lifecycle_event;
use super::lifecycle::fill_root_launch_identity;
use super::lifecycle::handle_lifecycle_hook;
use super::proctree::matches_agent_kind;
use BindingRejectReason::*;
use BindingSelectionMethod::{ClientFocus, OccupiedSoleCandidate, SingleCandidate, TabFocus};
use rimz::agents::AgentLifecycleObservation;
use rimz::agents::AgentStatus;
use rimz::agents::SessionOrigin;
use rimz::agents::lifecycle::{
    LifecycleSignal, LifecycleState, Transition, TransitionKind, TurnPhase,
};
use rimz::ids::AgentSessionId;
use rimz::ids::{MuxName, PaneId};
use rimz::pane::{PaneRef, RuntimeOwnerKind};
use rimz::store::runtime::process_owner;

struct Case {
    name: &'static str,
    panes: Vec<PaneRef>,
    client_focus: Option<Vec<PaneId>>,
    prior_stamps: Vec<(&'static str, jiff::Timestamp)>,
    expected_pane: Option<&'static str>,
    candidate_count: usize,
    method: BindingSelectionMethod,
    reject_reasons: Vec<(usize, BindingRejectReason)>,
}

fn id(raw: &str) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, raw)
}

fn incoming(
    origin: Option<SessionOrigin>,
    registered_at: Option<jiff::Timestamp>,
) -> BindingSession<'static> {
    BindingSession {
        kind: "codex",
        agent_id: "new",
        origin,
        registered_at,
    }
}

fn pane(raw: &str, command: &str, cwd: &str, focused: bool) -> PaneRef {
    PaneRef {
        pane_id: id(raw),
        session_name: "rimz-test".to_owned(),
        view_id: None,
        view_kind: None,
        view_name: None,
        is_focused: focused,
        is_floating: false,
        command: Some(command.to_owned()),
        foreground_cmdline: None,
        spawn_command: None,
        cwd: Some(cwd.to_owned()),
        pane_pid: None,
        pane_process_start: None,
        hosted_agent_kind: None,
        hosted_agent_process_start: None,
        resumed_session_id: None,
        elevated_agent: None,
        first_seen_at_ms: None,
    }
}

fn candidate(raw: &str, focused: bool) -> PaneRef {
    pane(raw, "codex", "/repo/main", focused)
}

fn hosted_candidate(raw: &str, focused: bool) -> PaneRef {
    PaneRef {
        hosted_agent_kind: Some(rimz::ids::AgentKind::new_unchecked("codex")),
        ..pane(raw, "chezmoi cd", "/repo/main", focused)
    }
}

fn started(raw: &str, start: jiff::Timestamp) -> PaneRef {
    PaneRef {
        pane_process_start: Some(start),
        elevated_agent: None,
        first_seen_at_ms: None,
        ..candidate(raw, true)
    }
}

fn transition(kind: TransitionKind, compaction_closed: bool) -> Transition {
    Transition {
        next: LifecycleState {
            status: AgentStatus::Running,
            phase: TurnPhase::Reasoning,
            compacting: false,
        },
        kind,
        compaction_closed,
        waiting_cleared: false,
        opened_turn: false,
    }
}

fn root_observation() -> AgentLifecycleObservation {
    AgentLifecycleObservation::new(
        Some(AgentSessionId::from("sess-1")),
        LifecycleSignal::Registered,
    )
}

fn hooks_test_store() -> (tempfile::TempDir, rimz::Store) {
    let dir = tempfile::TempDir::new().unwrap();
    let workspace_id =
        rimz::ids::WorkspaceId::from_project_root(std::path::Path::new("/tmp/hooks-test"));
    let paths = rimz::store::StatePaths::under(workspace_id.clone(), dir.path()).unwrap();
    let runtime = rimz::store::RuntimePaths::under(workspace_id, dir.path()).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    (dir, store)
}

fn hooks_test_workspace(worktree_branch: Option<&str>) -> rimz::ResolvedWorkspace {
    rimz::ResolvedWorkspace {
        workspace_id: rimz::ids::WorkspaceId::from_project_root(std::path::Path::new(
            "/tmp/hooks-test",
        )),
        project_root: std::path::PathBuf::from("/tmp/hooks-test"),
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: std::path::PathBuf::from("/tmp/hooks-test"),
        worktree_branch: worktree_branch.map(ToOwned::to_owned),
        session_name: "hooks-test".to_owned(),
        mux_hint: None,
    }
}

fn hooks_test_globals() -> crate::cli::GlobalFlags {
    crate::cli::GlobalFlags {
        mux: None,
        zellij: false,
        tmux: false,
        root: None,
        color: crate::cli::ColorWhen::Never,
    }
}

fn launch_identity_env(
    _observation: &AgentLifecycleObservation,
    var: &'static str,
) -> Option<String> {
    match var {
        rimz::harness::run::ENV_AGENT_ROLE => Some("coder".to_owned()),
        rimz::harness::run::ENV_TEAM => Some("forge".to_owned()),
        rimz::harness::run::ENV_LAUNCH_GROUP => Some("launch_group_1".to_owned()),
        rimz::harness::run::ENV_LAUNCH_ORDINAL => Some("2".to_owned()),
        rimz::harness::run::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
        rimz::harness::run::ENV_AGENT_MODEL => Some("env-model".to_owned()),
        rimz::harness::run::ENV_AGENT_EFFORT => Some("env-effort".to_owned()),
        _ => None,
    }
}

fn cwd_reject(path: &str) -> BindingRejectReason {
    CwdMismatch {
        got: Some(path.to_owned()),
    }
}

fn command_reject(command: &str) -> BindingRejectReason {
    CommandMismatch {
        got: Some(command.to_owned()),
    }
}

#[test]
fn agent_kind_matches_known_launch_shapes() {
    for (comm, source, expected) in [
        ("claude", "claude", true),
        ("codex", "codex", true),
        ("codex-aarch64-a", "codex", true),
        ("node", "codex", true),
        ("node", "claude", false),
        ("kiro-cli", "kiro", true),
        ("kiro-cli-chat", "kiro", true),
        ("kiro-cli-term", "kiro", false),
        ("zsh", "claude", false),
        ("bash", "codex", false),
    ] {
        assert_eq!(
            matches_agent_kind(comm, source),
            expected,
            "{comm}/{source}"
        );
    }
}

#[test]
fn lifecycle_append_gate_keeps_durable_truth_for_progress_signals() {
    let proof_of_work = LifecycleSignal::ToolUsed {
        mutates: false,
        edits: false,
    };
    let mutating_tool = LifecycleSignal::ToolUsed {
        mutates: true,
        edits: false,
    };

    assert!(
        append_lifecycle_event(&mutating_tool, None),
        "post-tool progress is durable even when transition inspection is unavailable"
    );
    assert!(
        !append_lifecycle_event(&proof_of_work, None),
        "tool proof-of-work drops when the prior rollup cannot be inspected"
    );
    assert!(
        !append_lifecycle_event(
            &proof_of_work,
            Some(transition(TransitionKind::Normal, false))
        ),
        "tool proof-of-work does not fill the durable log during normal running turns"
    );
    assert!(
        append_lifecycle_event(
            &proof_of_work,
            Some(transition(
                TransitionKind::Reconciled {
                    from: AgentStatus::Idle,
                    reason: "tool used outside a running turn",
                },
                false,
            )),
        ),
        "tool proof-of-work is durable when it reconciles a stale resting row"
    );
    assert!(
        append_lifecycle_event(
            &proof_of_work,
            Some(transition(TransitionKind::Normal, true))
        ),
        "tool proof-of-work is durable when it closes an open compaction bracket"
    );
    let mut clears_waiting = transition(TransitionKind::Normal, false);
    clears_waiting.waiting_cleared = true;
    assert!(
        append_lifecycle_event(&proof_of_work, Some(clears_waiting)),
        "tool proof-of-work is durable when it clears waiting"
    );
}

#[test]
fn stop_failure_records_turn_error_transcript_entry() {
    let (_dir, store) = hooks_test_store();
    let workspace = hooks_test_workspace(Some("main"));
    let globals = hooks_test_globals();

    handle_lifecycle_hook(
        &workspace,
        &store,
        &rimz::agents::ClaudeAdapter,
        "StopFailure",
        &serde_json::json!({
            "session_id": "sess-1",
            "error": "overloaded",
            "last_assistant_message": "API Error: Response stalled mid-stream. The response above may be incomplete."
        }),
        Some(std::process::id()),
        &globals,
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    let [entry] = entries.as_slice() else {
        panic!("expected one transcript entry, got {entries:?}");
    };
    assert_eq!(entry.entry, rimz::transcript::TranscriptKind::Error);
    assert_eq!(entry.kind.as_str(), "claude");
    assert_eq!(entry.agent_id.as_str(), "sess-1");
    assert_eq!(entry.channel.as_deref(), Some("hooks-test"));
    assert_eq!(
        entry.text,
        "API Error: Response stalled mid-stream. The response above may be incomplete."
    );
}

#[test]
fn canonical_droid_prompt_and_worker_stop_record_one_conversation() {
    let (dir, store) = hooks_test_store();
    let workspace = hooks_test_workspace(Some("main"));
    let globals = hooks_test_globals();
    let transcript_path = dir.path().join("droid-session.jsonl");
    std::fs::write(
        &transcript_path,
        concat!(
            "{\"type\":\"session_start\",\"version\":2}\n",
            "{\"type\":\"message\",\"id\":\"user\",\"timestamp\":\"2026-07-13T20:19:51.315Z\",\"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"ping\"}]}}\n",
            "{\"type\":\"message\",\"id\":\"assistant\",\"parentId\":\"user\",\"timestamp\":\"2026-07-13T20:19:54.616Z\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"pong\"}]}}\n",
        ),
    )
    .unwrap();
    let path = transcript_path.to_string_lossy();
    let owner_pid = std::process::id();

    handle_lifecycle_hook(
        &workspace,
        &store,
        &rimz::agents::DroidAdapter,
        "UserPromptSubmit",
        &serde_json::json!({
            "session_id": "droid-session",
            "transcript_path": path,
            "prompt": "ping"
        }),
        Some(owner_pid),
        &globals,
    )
    .unwrap();
    handle_lifecycle_hook(
        &workspace,
        &store,
        &rimz::agents::DroidAdapter,
        "Stop",
        &serde_json::json!({
            "session_id": "droid-session",
            "transcript_path": path
        }),
        Some(owner_pid),
        &globals,
    )
    .unwrap();

    let entries = rimz::transcript::read_all(store.paths()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].entry, rimz::transcript::TranscriptKind::Prompt);
    assert_eq!(entries[0].text, "ping");
    assert_eq!(
        entries[1].entry,
        rimz::transcript::TranscriptKind::Assistant
    );
    assert_eq!(entries[1].text, "pong");
    let user_inputs = rimz::agents::spending::user_input::load_in(dir.path());
    assert_eq!(user_inputs.len(), 1);
    assert_eq!(user_inputs[0].kind.as_str(), "droid");
    assert_eq!(
        user_inputs[0].origin.as_deref(),
        Some(std::path::Path::new("/tmp/hooks-test"))
    );
}

#[test]
fn root_launch_identity_fills_from_env_then_config_without_clobbering_payload() {
    let mut observed = root_observation();
    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );
    assert_eq!(observed.launch.role.as_deref(), Some("coder"));
    assert_eq!(observed.launch.team.as_deref(), Some("forge"));
    assert_eq!(
        observed.launch.launch_group.as_deref(),
        Some("launch_group_1")
    );
    assert_eq!(observed.launch.launch_ordinal, Some(2));
    assert_eq!(observed.launch.profile.as_deref(), Some("codex-coder"));
    assert_eq!(observed.launch.model.as_deref(), Some("env-model"));
    assert_eq!(observed.launch.effort.as_deref(), Some("env-effort"));

    let mut payload = root_observation();
    payload.launch.role = Some("payload-role".to_owned());
    payload.launch.team = Some("payload-team".to_owned());
    payload.launch.launch_group = Some("payload-group".to_owned());
    payload.launch.launch_ordinal = Some(7);
    payload.launch.profile = Some("payload-profile".to_owned());
    payload.launch.model = Some("payload-model".to_owned());
    payload.launch.effort = Some("payload-effort".to_owned());
    fill_root_launch_identity(
        &mut payload,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );
    assert_eq!(payload.launch.role.as_deref(), Some("payload-role"));
    assert_eq!(payload.launch.team.as_deref(), Some("payload-team"));
    assert_eq!(
        payload.launch.launch_group.as_deref(),
        Some("payload-group")
    );
    assert_eq!(payload.launch.launch_ordinal, Some(7));
    assert_eq!(payload.launch.profile.as_deref(), Some("payload-profile"));
    assert_eq!(payload.launch.model.as_deref(), Some("payload-model"));
    assert_eq!(payload.launch.effort.as_deref(), Some("payload-effort"));

    let mut configured = root_observation();
    fill_root_launch_identity(
        &mut configured,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        |_observation, var| match var {
            rimz::harness::run::ENV_AGENT_ROLE => Some("coder".to_owned()),
            rimz::harness::run::ENV_TEAM => Some("forge".to_owned()),
            rimz::harness::run::ENV_LAUNCH_ORDINAL => Some("not-a-number".to_owned()),
            rimz::harness::run::ENV_AGENT_PROFILE => Some("codex-coder".to_owned()),
            _ => None,
        },
    );
    assert_eq!(configured.launch.model.as_deref(), Some("cfg-model"));
    assert_eq!(configured.launch.effort.as_deref(), Some("cfg-effort"));
    assert_eq!(configured.launch.launch_ordinal, None);
}

#[test]
fn subagent_launch_identity_is_not_inherited_from_parent_env() {
    let mut observed = root_observation();
    observed.parent_agent_id = Some(AgentSessionId::from("parent-1"));

    fill_root_launch_identity(
        &mut observed,
        (Some("cfg-model".to_owned()), Some("cfg-effort".to_owned())),
        launch_identity_env,
    );

    assert_eq!(observed.launch.role, None);
    assert_eq!(observed.launch.team, None);
    assert_eq!(observed.launch.launch_group, None);
    assert_eq!(observed.launch.launch_ordinal, None);
    assert_eq!(observed.launch.profile, None);
    assert_eq!(observed.launch.model, None);
    assert_eq!(observed.launch.effort, None);
}

#[test]
fn focused_pane_recovery_selects_or_rejects_by_focus_and_stamp_state() {
    for case in focus_recovery_cases() {
        let prior_ids: Vec<PaneId> = case.prior_stamps.iter().map(|(raw, _)| id(raw)).collect();
        let prior: Vec<PriorAgentPane<'_>> = case
            .prior_stamps
            .iter()
            .zip(&prior_ids)
            .map(|((_, last_activity), pane_id)| PriorAgentPane {
                kind: "codex",
                agent_id: "old",
                is_provisional: false,
                pane_id: Some(pane_id),
                status: AgentStatus::Idle,
                origin: Some(SessionOrigin::Fresh),
                last_activity: *last_activity,
            })
            .collect();
        let selected = select_focused_pane_binding(
            incoming(Some(SessionOrigin::Fresh), None),
            "/repo/main",
            &prior,
            &case.panes,
            case.client_focus.as_deref(),
            true,
        );

        assert_eq!(
            selected.pane_id.as_ref().map(|pane| pane.raw()),
            case.expected_pane,
            "{} selected pane",
            case.name,
        );
        assert_eq!(
            selected.candidate_count, case.candidate_count,
            "{} candidate count",
            case.name,
        );
        assert_eq!(
            selected.method, case.method,
            "{} selection method",
            case.name
        );
        for (index, reason) in case.reject_reasons {
            assert!(
                selected.candidates[index].reject_reasons.contains(&reason),
                "{} candidate {index} missing {reason:?}: {:?}",
                case.name,
                selected.candidates[index].reject_reasons,
            );
        }
    }
}

#[test]
fn recovered_binding_stamps_full_pane_and_reowns_to_in_pane_agent_process() {
    let mut observation = root_observation();
    let mut pane = candidate("terminal_30", true);
    pane.view_id = Some("tab_4".to_owned());
    pane.cwd = Some("/repo/main".to_owned());
    pane.pane_pid = Some(1234);
    let child_pid = 5678;

    super::binding::apply_recovered_pane_binding_with(
        &mut observation,
        "sess-1",
        pane.clone(),
        |root_pid| {
            assert_eq!(root_pid, 1234);
            Some(child_pid)
        },
    );

    assert_eq!(observation.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(observation.pane_stamp.as_ref(), Some(&pane));
    let owner = observation.runtime_owner.as_ref().expect("runtime owner");
    assert_eq!(owner.kind, RuntimeOwnerKind::Agent);
    assert_eq!(owner.pid, child_pid);
}

#[test]
fn recovered_binding_preserves_prior_owner_when_agent_process_is_unknown() {
    let mut observation = root_observation();
    let existing_owner = process_owner(RuntimeOwnerKind::Daemon, "sess-1", std::process::id());
    observation.runtime_owner = Some(existing_owner.clone());
    let mut pane = candidate("terminal_30", true);
    pane.view_id = Some("tab_4".to_owned());
    pane.cwd = Some("/repo/main".to_owned());
    pane.pane_pid = Some(1234);

    super::binding::apply_recovered_pane_binding_with(
        &mut observation,
        "sess-1",
        pane.clone(),
        |root_pid| {
            assert_eq!(root_pid, 1234);
            None
        },
    );

    assert_eq!(observation.pane_id.as_ref(), Some(&pane.pane_id));
    assert_eq!(observation.pane_stamp.as_ref(), Some(&pane));
    assert_eq!(observation.runtime_owner.as_ref(), Some(&existing_owner));
}

fn focus_recovery_cases() -> Vec<Case> {
    let epoch = jiff::Timestamp::UNIX_EPOCH;
    let later = jiff::Timestamp::from_second(60).unwrap();
    vec![
        Case {
            name: "unique client focus",
            panes: vec![
                candidate("terminal_4", true),
                candidate("terminal_30", true),
            ],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![],
            expected_pane: Some("terminal_30"),
            candidate_count: 2,
            method: ClientFocus,
            reject_reasons: vec![(0, NotInClientFocus)],
        },
        Case {
            name: "single candidate without focus",
            panes: vec![
                candidate("terminal_4", false),
                pane("terminal_30", "codex", "/repo/other", true),
            ],
            client_focus: None,
            prior_stamps: vec![],
            expected_pane: Some("terminal_4"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![(1, cwd_reject("/repo/other"))],
        },
        Case {
            name: "hosted agent under wrapper command",
            panes: vec![hosted_candidate("terminal_176", true)],
            client_focus: Some(vec![id("terminal_176")]),
            prior_stamps: vec![],
            expected_pane: Some("terminal_176"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "ambiguous client focus",
            panes: vec![
                candidate("terminal_4", true),
                candidate("terminal_30", true),
            ],
            client_focus: Some(vec![id("terminal_4"), id("terminal_30")]),
            prior_stamps: vec![],
            expected_pane: None,
            candidate_count: 2,
            method: ClientFocus,
            reject_reasons: vec![(0, Ambiguous { n: 2 }), (1, Ambiguous { n: 2 })],
        },
        Case {
            name: "tab focus fallback",
            panes: vec![
                candidate("terminal_4", false),
                candidate("terminal_30", true),
            ],
            client_focus: None,
            prior_stamps: vec![],
            expected_pane: Some("terminal_30"),
            candidate_count: 2,
            method: TabFocus,
            reject_reasons: vec![(0, NotTabFocused)],
        },
        Case {
            name: "codex can share occupied pane when no free candidate",
            panes: vec![candidate("terminal_30", true)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", epoch)],
            expected_pane: Some("terminal_30"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "stale foreign stamp",
            panes: vec![started("terminal_30", later)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", epoch)],
            expected_pane: Some("terminal_30"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "codex can share current occupied pane",
            panes: vec![started("terminal_30", epoch)],
            client_focus: Some(vec![id("terminal_30")]),
            prior_stamps: vec![("terminal_30", later)],
            expected_pane: Some("terminal_30"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![],
        },
        Case {
            name: "codex occupied fallback records surrounding reasons",
            panes: vec![
                pane("terminal_4", "claude", "/repo/main", false),
                pane("terminal_30", "codex", "/repo/other", false),
                candidate("terminal_42", true),
            ],
            client_focus: None,
            prior_stamps: vec![("terminal_42", epoch)],
            expected_pane: Some("terminal_42"),
            candidate_count: 1,
            method: SingleCandidate,
            reject_reasons: vec![
                (0, command_reject("claude")),
                (1, cwd_reject("/repo/other")),
            ],
        },
    ]
}

#[test]
fn provisional_launch_stamp_allows_recovery_for_known_session() {
    let pane_id = id("terminal_30");
    let provisional = PriorAgentPane {
        kind: "codex",
        agent_id: "launch_abc",
        is_provisional: true,
        pane_id: Some(&pane_id),
        status: AgentStatus::Idle,
        origin: None,
        last_activity: jiff::Timestamp::UNIX_EPOCH,
    };
    let known_session = PriorAgentPane {
        kind: "codex",
        agent_id: "new",
        is_provisional: false,
        pane_id: None,
        status: AgentStatus::Running,
        origin: Some(SessionOrigin::Fresh),
        last_activity: jiff::Timestamp::UNIX_EPOCH,
    };

    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), None),
        "/repo/main",
        &[provisional, known_session],
        &[candidate("terminal_30", true)],
        Some(std::slice::from_ref(&pane_id)),
        true,
    );

    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(selected.method, SingleCandidate);
    assert_eq!(
        selected.candidates[0].occupied_by_agent_id.as_deref(),
        Some("launch_abc")
    );
    assert!(selected.candidates[0].reject_reasons.is_empty());

    let mut observation = AgentLifecycleObservation::new(
        Some(AgentSessionId::from("new")),
        LifecycleSignal::TurnStarted,
    );
    super::binding::apply_recovered_pane_binding_with(
        &mut observation,
        "new",
        selected.pane.expect("selected pane"),
        |_| None,
    );
    assert_eq!(observation.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(
        observation.pane_stamp.as_ref().map(|pane| &pane.pane_id),
        Some(&pane_id)
    );
}

#[test]
fn occupied_pane_fallback_stays_daemon_hooked_and_first_event_only() {
    let epoch = jiff::Timestamp::UNIX_EPOCH;
    let occupied_pane_id = id("terminal_30");

    let old_codex = PriorAgentPane {
        kind: "codex",
        agent_id: "old",
        is_provisional: false,
        pane_id: Some(&occupied_pane_id),
        status: AgentStatus::Idle,
        origin: Some(SessionOrigin::Fresh),
        last_activity: epoch,
    };
    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), None),
        "/repo/main",
        &[old_codex],
        &[candidate("terminal_30", true)],
        Some(std::slice::from_ref(&occupied_pane_id)),
        true,
    );
    assert_eq!(
        selected.pane_id.as_ref().map(|pane| pane.raw()),
        Some("terminal_30")
    );
    assert_eq!(
        selected.candidates[0].occupied_by_agent_id.as_deref(),
        Some("old"),
        "selected occupied pane still records the prior owner"
    );
    assert!(
        selected.candidates[0].reject_reasons.is_empty(),
        "accepted occupied Codex candidate is no longer logged as rejected"
    );

    let old_codex = PriorAgentPane {
        kind: "codex",
        agent_id: "old",
        is_provisional: false,
        pane_id: Some(&occupied_pane_id),
        status: AgentStatus::Idle,
        origin: Some(SessionOrigin::Fresh),
        last_activity: epoch,
    };
    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), None),
        "/repo/main",
        &[old_codex],
        &[candidate("terminal_30", true)],
        Some(std::slice::from_ref(&occupied_pane_id)),
        false,
    );
    assert_eq!(
        selected.pane_id, None,
        "occupied fallback is limited to prompt-start recovery"
    );
    assert_eq!(selected.candidate_count, 0);

    let old_codex = PriorAgentPane {
        kind: "codex",
        agent_id: "old",
        is_provisional: false,
        pane_id: Some(&occupied_pane_id),
        status: AgentStatus::Idle,
        origin: Some(SessionOrigin::Fresh),
        last_activity: epoch,
    };
    let selected = select_focused_pane_binding(
        incoming(None, None),
        "/repo/main",
        &[old_codex],
        &[candidate("terminal_30", false)],
        Some(&[]),
        true,
    );
    assert_eq!(
        selected.pane_id, None,
        "occupied fallback without focus requires fresh incoming lineage"
    );
    assert_eq!(selected.candidate_count, 0);

    let old_codex = PriorAgentPane {
        kind: "codex",
        agent_id: "old",
        is_provisional: false,
        pane_id: Some(&occupied_pane_id),
        status: AgentStatus::Idle,
        origin: Some(SessionOrigin::Fresh),
        last_activity: epoch,
    };
    let known_new_codex = PriorAgentPane {
        kind: "codex",
        agent_id: "new",
        is_provisional: false,
        pane_id: None,
        status: AgentStatus::Running,
        origin: Some(SessionOrigin::Fresh),
        last_activity: epoch,
    };
    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), None),
        "/repo/main",
        &[old_codex, known_new_codex],
        &[candidate("terminal_30", true)],
        Some(std::slice::from_ref(&occupied_pane_id)),
        true,
    );
    assert_eq!(
        selected.pane_id, None,
        "an already-known unstamped session cannot later claim a sibling's occupied pane"
    );
    assert_eq!(selected.candidate_count, 0);
    assert_eq!(
        selected.candidates[0].occupied_by_agent_id.as_deref(),
        Some("old"),
        "blocked occupied pane still records the owner"
    );
    assert!(
        selected.candidates[0]
            .reject_reasons
            .contains(&StampedToOther {
                agent_id: "old".to_owned()
            })
    );

    let old_claude = PriorAgentPane {
        kind: "claude",
        agent_id: "old",
        is_provisional: false,
        pane_id: Some(&occupied_pane_id),
        status: AgentStatus::Idle,
        origin: Some(SessionOrigin::Fresh),
        last_activity: epoch,
    };
    let selected = select_focused_pane_binding(
        BindingSession {
            kind: "claude",
            agent_id: "new",
            origin: Some(SessionOrigin::Fresh),
            registered_at: None,
        },
        "/repo/main",
        &[old_claude],
        &[pane("terminal_30", "claude", "/repo/main", true)],
        Some(std::slice::from_ref(&occupied_pane_id)),
        true,
    );
    assert_eq!(
        selected.pane_id, None,
        "non-daemon-hooked recovery keeps the one-owner pane stamp rule"
    );
    assert_eq!(selected.candidate_count, 0);
}

#[test]
fn sole_resting_fresh_occupied_pane_binds_without_focus() {
    let pane_id = id("terminal_30");
    let owner = PriorAgentPane {
        kind: "codex",
        agent_id: "old",
        is_provisional: false,
        pane_id: Some(&pane_id),
        status: AgentStatus::Idle,
        origin: Some(SessionOrigin::Fresh),
        last_activity: jiff::Timestamp::UNIX_EPOCH,
    };

    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), None),
        "/repo/main",
        &[owner],
        &[candidate("terminal_30", false)],
        Some(&[]),
        true,
    );

    assert_eq!(selected.pane_id.as_ref(), Some(&pane_id));
    assert_eq!(selected.method, OccupiedSoleCandidate);
    assert_eq!(selected.candidate_count, 1);
    assert!(selected.candidates[0].reject_reasons.is_empty());
}

#[test]
fn occupied_pane_without_focus_requires_clear_lineage_and_resting_owner() {
    let pane_id = id("terminal_30");
    for (label, status, owner_origin, incoming_origin) in [
        (
            "running owner",
            AgentStatus::Running,
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Fresh),
        ),
        (
            "waiting owner",
            AgentStatus::Waiting,
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Fresh),
        ),
        (
            "forked owner",
            AgentStatus::Idle,
            Some(SessionOrigin::Forked),
            Some(SessionOrigin::Fresh),
        ),
        (
            "unknown owner",
            AgentStatus::Idle,
            None,
            Some(SessionOrigin::Fresh),
        ),
        (
            "forked incoming",
            AgentStatus::Idle,
            Some(SessionOrigin::Fresh),
            Some(SessionOrigin::Forked),
        ),
        (
            "unknown incoming",
            AgentStatus::Idle,
            Some(SessionOrigin::Fresh),
            None,
        ),
    ] {
        let owner = PriorAgentPane {
            kind: "codex",
            agent_id: "old",
            is_provisional: false,
            pane_id: Some(&pane_id),
            status,
            origin: owner_origin,
            last_activity: jiff::Timestamp::UNIX_EPOCH,
        };
        let selected = select_focused_pane_binding(
            incoming(incoming_origin, None),
            "/repo/main",
            &[owner],
            &[candidate("terminal_30", false)],
            Some(&[]),
            true,
        );

        assert_eq!(selected.pane_id, None, "{label}");
        assert_eq!(selected.candidate_count, 0, "{label}");
    }
}

#[test]
fn several_resting_fresh_occupied_panes_stay_unbound_without_focus() {
    let first_id = id("terminal_30");
    let second_id = id("terminal_42");
    let first = PriorAgentPane {
        kind: "codex",
        agent_id: "old-1",
        is_provisional: false,
        pane_id: Some(&first_id),
        status: AgentStatus::Idle,
        origin: Some(SessionOrigin::Fresh),
        last_activity: jiff::Timestamp::UNIX_EPOCH,
    };
    let second = PriorAgentPane {
        kind: "codex",
        agent_id: "old-2",
        is_provisional: false,
        pane_id: Some(&second_id),
        status: AgentStatus::Success,
        origin: Some(SessionOrigin::Fresh),
        last_activity: jiff::Timestamp::UNIX_EPOCH,
    };

    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), None),
        "/repo/main",
        &[first, second],
        &[
            candidate("terminal_30", false),
            candidate("terminal_42", false),
        ],
        Some(&[]),
        true,
    );

    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.candidate_count, 2);
}

#[test]
fn pane_started_after_session_is_rejected_unless_it_resumes_that_session() {
    let registered_at = jiff::Timestamp::from_second(30).unwrap();
    let pane_start = jiff::Timestamp::from_second(60).unwrap();
    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), Some(registered_at)),
        "/repo/main",
        &[],
        &[started("terminal_30", pane_start)],
        None,
        true,
    );
    assert_eq!(selected.pane_id, None);
    assert_eq!(selected.candidate_count, 0);
    assert!(
        selected.candidates[0]
            .reject_reasons
            .contains(&StartedAfterSession)
    );

    let resumed = PaneRef {
        resumed_session_id: Some(AgentSessionId::from("new")),
        ..started("terminal_30", pane_start)
    };
    let selected = select_focused_pane_binding(
        incoming(Some(SessionOrigin::Fresh), Some(registered_at)),
        "/repo/main",
        &[],
        &[resumed],
        None,
        true,
    );
    assert_eq!(
        selected.pane_id.as_ref().map(|pane| pane.raw()),
        Some("terminal_30")
    );
    assert_eq!(selected.method, SingleCandidate);
}
