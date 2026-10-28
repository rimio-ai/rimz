use super::*;
use clap::Parser;
use rimz::agents::{LaunchParams, PermissionMode};
use rimz::harness::launch::{ExecAction, ExecIdentity, ExecRequest, ProviderAccountState};
use rimz::harness::run_wake::ExpectedRunFrame;
use rimz::ids::{AgentKind, AgentSessionId, MuxName, WorkspaceId};
use rimz::store::run::{RunRecord, RunStatus};
use std::path::{Path, PathBuf};

/// A resumed session is bound to exactly one pane at a time, and the
/// replacement stamps that binding before it spawns its provider. The
/// exiting wrapper's argv-identity fallback therefore reads the binding to
/// tell itself apart from a replacement that already took the session
/// over: superseded, it ends nothing and retires nothing.
#[test]
fn a_session_bound_to_another_pane_supersedes_the_exiting_wrapper() {
    let kind = AgentKind::new_unchecked("claude");
    let session = AgentSessionId::from("resumed");
    let pane = |id: &str| rimz::ids::PaneId::parse(id).expect("normalized pane id");
    let bound = |id: Option<&str>| {
        let mut agent = rimz::testkit::agent_state("claude", "resumed", jiff::Timestamp::now());
        agent.pane = id.map(|id| rimz::pane::PaneRef::from_id(pane(id)));
        agent
    };
    let own = pane("tmux:%1");

    assert!(
        session_bound_to_another_pane(&[bound(Some("tmux:%2"))], &kind, &session, Some(&own)),
        "the replacement's pane supersedes this wrapper"
    );
    assert!(
        !session_bound_to_another_pane(&[bound(Some("tmux:%1"))], &kind, &session, Some(&own)),
        "the wrapper's own binding is not a supersession"
    );
    assert!(
        !session_bound_to_another_pane(&[bound(None)], &kind, &session, Some(&own)),
        "an unbound session leaves the fallback to decide"
    );
    assert!(
        !session_bound_to_another_pane(&[], &kind, &session, Some(&own)),
        "no row for the session is no evidence of a replacement"
    );
    assert!(
        session_bound_to_another_pane(&[bound(Some("tmux:%1"))], &kind, &session, None),
        "a wrapper with no pane of its own can claim no binding"
    );
    assert!(
        !session_bound_to_another_pane(
            &[bound(Some("tmux:%2"))],
            &AgentKind::new_unchecked("codex"),
            &session,
            Some(&own)
        ),
        "another provider's binding on the same session id is not this one"
    );
}

const OWN_PANE: &str = "tmux:%1";

fn pane_ref(id: &str) -> rimz::pane::PaneRef {
    rimz::pane::PaneRef::from_id(rimz::ids::PaneId::parse(id).expect("normalized pane id"))
}

/// A codex row stamped on `pane`, last touched `activity_secs` after the epoch.
fn pane_row(
    session: &str,
    launch: Option<&str>,
    pane: &str,
    activity_secs: i64,
    ended: bool,
) -> rimz::agents::AgentState {
    let at = jiff::Timestamp::from_second(activity_secs).expect("timestamp");
    let mut agent = rimz::testkit::agent_state("codex", session, at);
    agent.name = Some(format!("name-{session}"));
    agent.launch_id = launch.map(AgentSessionId::from);
    agent.pane = Some(pane_ref(pane));
    agent.ended_at = ended.then_some(at);
    agent
}

fn exit_request(launch: Option<&str>, resume: Option<&str>) -> ExecRequest {
    let mut request = bare_exec_args();
    request.identity.launch_id = launch.map(ToOwned::to_owned);
    if let Some(session) = resume {
        request.action = ExecAction::Resume {
            session_id: session.to_owned(),
            extra_args: Vec::new(),
        };
    }
    request
}

fn ends(agents: &[rimz::agents::AgentState], request: &ExecRequest) -> Option<String> {
    let own = rimz::ids::PaneId::parse(OWN_PANE).expect("normalized pane id");
    exit_end_target(agents, Some(&own), request).map(|ended| ended.agent_id.to_string())
}

/// The incident: the reaper ends an earlier occupant just after the resume
/// stamped its row, so the earlier occupant is the pane's most recently
/// touched row. The wrapper still ends the session it resumed.
#[test]
fn a_resumed_wrapper_ends_its_launch_row_not_a_later_touched_earlier_occupant() {
    let agents = [
        pane_row("resumed", Some("launch_a"), OWN_PANE, 10, false),
        pane_row("earlier", Some("launch_b"), OWN_PANE, 20, true),
    ];
    let request = exit_request(Some("launch_a"), Some("resumed"));
    assert_eq!(ends(&agents, &request).as_deref(), Some("resumed"));
}

#[test]
fn a_fresh_wrapper_ends_its_launch_row_not_a_later_touched_earlier_occupant() {
    let agents = [
        pane_row("fresh", Some("launch_a"), OWN_PANE, 10, false),
        pane_row("earlier", Some("launch_b"), OWN_PANE, 20, true),
    ];
    let request = exit_request(Some("launch_a"), None);
    assert_eq!(ends(&agents, &request).as_deref(), Some("fresh"));
}

/// A superseded wrapper's launch row is bound to the replacement's pane, so
/// it ends nothing, not even the older row still stamped on its own pane.
#[test]
fn a_superseded_wrapper_ends_no_older_row_on_its_own_pane() {
    let agents = [
        pane_row("moved", Some("launch_a"), "tmux:%2", 30, false),
        pane_row("older", Some("launch_b"), OWN_PANE, 20, true),
    ];
    let request = exit_request(Some("launch_a"), Some("moved"));
    assert_eq!(ends(&agents, &request), None);
    assert_eq!(
        exit_end_target(&agents, None, &request),
        None,
        "no pane of its own: any binding supersedes"
    );
}

#[test]
fn a_wrapper_with_no_launch_id_and_no_resume_target_ends_no_pane_occupant() {
    let agents = [pane_row("occupant", Some("launch_b"), OWN_PANE, 20, false)];
    assert_eq!(ends(&agents, &exit_request(None, None)), None);
}

/// A fork-on-resume successor inherits the launch id and replaces the
/// resumed row, which the wrapper's resume target still names.
#[test]
fn a_wrapper_ends_the_live_successor_of_its_resume_target() {
    let agents = [
        pane_row("original", Some("launch_a"), OWN_PANE, 10, true),
        pane_row("successor", Some("launch_a"), OWN_PANE, 15, false),
    ];
    let request = exit_request(Some("launch_a"), Some("original"));
    assert_eq!(ends(&agents, &request).as_deref(), Some("successor"));
}

/// An exact-id resume of a cleared conversation gives one launch two live
/// rows in two panes; each wrapper ends the one on its own pane.
#[test]
fn each_wrapper_of_a_launch_live_in_two_panes_ends_its_own_row() {
    for (here_secs, there_secs) in [(10, 20), (20, 10)] {
        let agents = [
            pane_row("here", Some("launch_a"), OWN_PANE, here_secs, false),
            pane_row("there", Some("launch_a"), "tmux:%2", there_secs, false),
        ];
        let request = exit_request(Some("launch_a"), Some("here"));
        assert_eq!(ends(&agents, &request).as_deref(), Some("here"));
    }
}

#[test]
fn a_wrapper_still_ends_its_launch_row_after_the_reaper_ended_it() {
    let agents = [
        pane_row("resumed", Some("launch_a"), OWN_PANE, 10, true),
        pane_row("earlier", Some("launch_b"), OWN_PANE, 20, true),
    ];
    let request = exit_request(Some("launch_a"), Some("resumed"));
    let own = rimz::ids::PaneId::parse(OWN_PANE).expect("normalized pane id");
    assert_eq!(
        exit_end_target(&agents, Some(&own), &request),
        Some(EndedSession {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: AgentSessionId::from("resumed"),
            name: Some("name-resumed".to_owned()),
            is_child_session: false,
        })
    );
}

/// The reporter's workspace, scoped to a fixture's own tempdir.
pub(super) fn test_workspace(root: &std::path::Path) -> rimz::ResolvedWorkspace {
    rimz::ResolvedWorkspace {
        workspace_id: rimz::WorkspaceId::from_project_root(root),
        project_root: root.to_owned(),
        cwd_project_root: None,
        root_class: rimz::workspace::RootClass::Directory,
        worktree_root: root.to_owned(),
        worktree_branch: None,
        session_name: "room".to_owned(),
        mux_hint: None,
    }
}

#[test]
fn only_terminal_subagent_resumes_await_reopening() {
    use rimz::harness::launch::{ExecAction, ExecRequest};
    use rimz::store::run::{RunRecord, RunStatus};

    let mut request = ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked("codex"),
        action: ExecAction::Resume {
            session_id: "child".into(),
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        no_skill_tool: false,
        allowed_tools: None,
        provider_account: Default::default(),
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: true,
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: Default::default(),
    };
    let mut record = RunRecord::new(
        rimz::WorkspaceId::from_project_root(Path::new("/project")),
        request.kind.clone(),
        PermissionMode::Auto,
        "work".into(),
        Path::new("/project").into(),
    );
    record.status = RunStatus::Completed;
    record.follow_ups = 7;
    assert_eq!(resumed_run_follow_ups(&request, &record), Some(7));
    request.subagent = false;
    assert_eq!(resumed_run_follow_ups(&request, &record), None);
    request.subagent = true;
    record.status = RunStatus::Running;
    assert_eq!(resumed_run_follow_ups(&request, &record), None);
    record.status = RunStatus::Completed;
    for action in [
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        ExecAction::Fork {
            session_id: "child".into(),
            extra_args: Vec::new(),
        },
    ] {
        request.action = action;
        assert_eq!(resumed_run_follow_ups(&request, &record), None);
    }
}

#[test]
fn parent_watchdog_covers_every_subagent_but_not_peers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = test_workspace(dir.path());
    let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
    let runtime =
        rimz::RuntimePaths::under(workspace.workspace_id.clone(), &dir.path().join("rt")).unwrap();
    let store = rimz::Store::open(paths, runtime).unwrap();
    let mut record = rimz::store::run::RunRecord::new(
        workspace.workspace_id.clone(),
        AgentKind::new_unchecked("codex"),
        PermissionMode::Auto,
        "work".into(),
        dir.path().into(),
    );
    let mut request = minimal_exec_request(
        "codex",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    request.subagent = true;
    let context = RunExecContext {
        run_id: record.run_id.clone(),
        store,
        session_name: "room".into(),
        workspace,
    };
    let identity = LaunchIdentity {
        kind: request.kind.clone(),
        agent_id: "child-launch".into(),
        name: "child".into(),
        name_explicit: false,
        launch: rimz::agents::LaunchParams::default(),
        run_id: None,
        prompt: None,
    };
    assert!(
        subagent_parent_watchdog(&request, Some(&context), Some(&identity)).is_some(),
        "an unresolved parent must keep its watchdog backstop"
    );
    for id in ["parent", "child-launch"] {
        let mut observation = rimz::agents::AgentLifecycleObservation::new(
            Some(id.into()),
            rimz::agents::LifecycleSignal::Registered,
        );
        if id == "child-launch" {
            observation.launch.parent_agent_id = Some("parent".into());
            observation.launch.parent_agent_kind = Some(request.kind.clone());
            observation.launch.launch_depth = Some(1);
        }
        context
            .store
            .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                session_name: "room",
                agent_kind: request.kind.clone(),
                event_name: "test",
                observation: &observation,
                spawned_subagents: &[],
            })
            .unwrap();
    }
    let admitted = |record: &rimz::store::run::RunRecord| {
        rimz::harness::run::create(context.store.paths(), record).unwrap();
        subagent_parent_watchdog(&request, Some(&context), Some(&identity)).is_some()
    };
    assert!(admitted(&record), "an attached child watches its parent");
    record.report_to = rimz::store::run::ReportTo::Nobody;
    assert!(
        admitted(&record),
        "a legacy nobody child watches its parent"
    );
    record.report_to = rimz::store::run::ReportTo::Launcher;
    record.keep = true;
    assert!(admitted(&record), "a kept child watches its parent");
    request.subagent = false;
    assert!(
        subagent_parent_watchdog(&request, Some(&context), Some(&identity)).is_none(),
        "a peer keeps its independent lifecycle"
    );
}

fn parse_exec_request(input: &ExecRequest) -> ExecRequest {
    let dir = tempfile::tempdir().expect("temp dir");
    let runtime = rimz::RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    let argv = rimz::harness::launch::exec_argv(Path::new("/bin/rimz"), &runtime, input)
        .expect("render exec argv");
    let parsed = crate::cli::Cli::try_parse_from(argv).expect("parse rendered exec argv");
    let Some(crate::cli::Subcmd::Agents(args)) = parsed.subcommand else {
        panic!("expected agents subcommand");
    };
    let Some(AgentsSubcmd::Exec(args)) = args.command else {
        panic!("expected exec subcommand");
    };
    decode_exec_envelope(&args)
        .expect("decode exec request")
        .materialize()
        .expect("materialize exec prompt")
}

pub(super) fn minimal_exec_request(kind: &str, action: ExecAction) -> ExecRequest {
    ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked(kind),
        action,
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        no_skill_tool: false,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity::default(),
    }
}

fn resume_or_fork_contract(action: &ExecAction) -> (&str, &str, &[String]) {
    use ExecAction::{Fork, Launch, Pick, Resume};

    match action {
        Resume {
            session_id,
            extra_args,
        } => ("resume", session_id, extra_args),
        Fork {
            session_id,
            extra_args,
        } => ("fork", session_id, extra_args),
        Launch { .. } | Pick { .. } => panic!("expected resume or fork"),
    }
}

#[test]
fn picker_consumes_launch_identity_without_attaching_a_session() {
    let action = serde_json::from_value::<ExecAction>(
        serde_json::json!({"action": "pick", "extra_args": []}),
    );
    assert!(action.is_ok(), "picker action must decode: {action:?}");
    let mut request = minimal_exec_request("claude", action.unwrap());
    request.identity.launch_id = Some("launch_picker".into());
    assert!(exec_launch_identity(&request).is_err());
    request.identity.name = Some("minted-handle".into());
    let identity = exec_launch_identity(&request).unwrap().unwrap();
    assert_eq!(identity.agent_id.as_str(), "launch_picker");
    assert_eq!(identity.name, "minted-handle");
    assert_eq!(identity.prompt, None);
    assert_eq!(exec_attach_target(&request), None);
}

#[test]
fn picker_binding_evidence_survives_session_end() {
    for bound in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let workspace = test_workspace(dir.path());
        let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let runtime =
            rimz::RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        let identity = LaunchIdentity {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: "launch_picker".into(),
            name: "picker".into(),
            name_explicit: false,
            launch: Default::default(),
            run_id: None,
            prompt: None,
        };
        let pane = rimz::ids::PaneId::parse("tmux:%1").unwrap();
        store
            .bind_agent_launch(&identity, "room", dir.path(), &pane, None)
            .unwrap();
        let invocation = ExecInvocationContext {
            workspace: &workspace,
            cwd: dir.path().to_owned(),
            store: RefCell::new(Some(store.clone())),
            effective_isolation: None,
        };
        assert_eq!(picker_session_bound(&invocation, &identity), Some(false));
        let session = if bound {
            AgentSessionId::from("provider-session")
        } else {
            identity.agent_id.clone()
        };
        let observe = |signal, event_name| {
            let mut observation =
                rimz::agents::AgentLifecycleObservation::new(Some(session.clone()), signal);
            observation.pane_id = Some(pane.clone());
            observation.agent_name = Some(identity.name.clone());
            store
                .append_agent_lifecycle(rimz::store::writer::AgentLifecycleIntent {
                    session_name: "room",
                    agent_kind: identity.kind.clone(),
                    event_name,
                    observation: &observation,
                    spawned_subagents: &[],
                })
                .unwrap();
        };
        if bound {
            observe(rimz::agents::LifecycleSignal::Registered, "SessionStart");
        }
        assert_eq!(picker_session_bound(&invocation, &identity), Some(bound));
        observe(rimz::agents::LifecycleSignal::Ended, "SessionEnd");
        assert!(
            !store
                .snapshot_cached()
                .unwrap()
                .agents
                .iter()
                .any(|agent| agent.agent_id == session),
            "ended roots are absent from the live snapshot"
        );
        assert_eq!(
            picker_session_bound(&invocation, &identity),
            Some(bound),
            "a durable end must not change whether a provider session bound"
        );
    }
}

fn bare_exec_args() -> ExecRequest {
    ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked("codex"),
        action: ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        no_skill_tool: false,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: None,
        worktree_path: None,
        close_pane_on_exit: false,
        exit_on_run_completion: false,
        subagent: false,
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity {
            name: Some("lucid-atlas".to_owned()),
            launch_id: Some("launch_0123456789abcdef0123456789abcdef".to_owned()),
            ..ExecIdentity::default()
        },
    }
}

#[test]
fn exec_accepts_one_legacy_or_file_request_and_decodes_the_same_envelope() {
    let dir = tempfile::tempdir().unwrap();
    let runtime =
        rimz::RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path()).unwrap();
    let request = bare_exec_args();
    let argv =
        rimz::harness::launch::exec_argv(Path::new("/bin/rimz"), &runtime, &request).unwrap();
    let path = argv.last().unwrap();
    let payload = std::fs::read_to_string(path).unwrap();
    let base = ["rimz", "agents", "exec", "codex"];
    let legacy =
        crate::cli::Cli::try_parse_from(base.into_iter().chain(["--request", payload.as_str()]));
    assert!(
        legacy.is_ok(),
        "a pre-upgrade wrapper must still parse: {legacy:?}"
    );
    let exec_args = |cli: crate::cli::Cli| {
        let Some(crate::cli::Subcmd::Agents(args)) = cli.subcommand else {
            panic!("expected agents subcommand");
        };
        let Some(AgentsSubcmd::Exec(args)) = args.command else {
            panic!("expected exec subcommand");
        };
        args
    };
    let legacy = exec_args(legacy.unwrap());
    let file = exec_args(crate::cli::Cli::try_parse_from(&argv).unwrap());
    assert_eq!(decode_exec_envelope(&legacy).unwrap().request(), &request);
    assert_eq!(decode_exec_envelope(&file).unwrap().request(), &request);
    assert!(crate::cli::Cli::try_parse_from(base).is_err());
    assert!(
        crate::cli::Cli::try_parse_from(base.into_iter().chain([
            "--request",
            payload.as_str(),
            "--request-file",
            path.as_str(),
        ]))
        .is_err()
    );
}

#[test]
fn exec_argv_round_trips_identity_actions_and_bindings() {
    let launch_extra = vec!["--dangerously-skip-permissions".to_owned()];
    let input_params = LaunchParams {
        profile: Some("planner".to_owned()),
        mode: Some(PermissionMode::Yolo),
        role: Some("coder".to_owned()),
        model: Some("opus".to_owned()),
        effort: Some("high".to_owned()),
        budget: Some("$12.50/day".to_owned()),
        team: Some("forge".to_owned()),
        launch_group: Some("launch_group_1".to_owned()),
        launch_ordinal: Some(2),
        channel: Some("design".to_owned()),
        kind_ordinal: None,
        ..LaunchParams::default()
    };
    let input = ExecRequest {
        isolation_default: None,
        kind: AgentKind::new_unchecked("claude"),
        action: ExecAction::Launch {
            prompt: Some("fix it".to_owned()),
            extra_args: launch_extra,
        },
        system_prompt_file: None,
        append_system_prompt_files: Vec::new(),
        team_prompt: None,
        skills: None,
        no_skill_tool: false,
        allowed_tools: None,
        provider_account: ProviderAccountState::Unbound,
        run_id: Some(
            "run_0123456789abcdef0123456789abcdef"
                .parse()
                .expect("run id"),
        ),
        worktree_path: Some(PathBuf::from("/repo/worktree")),
        close_pane_on_exit: true,
        exit_on_run_completion: true,
        subagent: true,
        follow_up_message: None,
        loop_reminder: None,
        headless: None,
        identity: ExecIdentity {
            resume_model_override: false,
            name: Some("swift-otter".to_owned()),
            name_explicit: true,
            launch_id: Some("launch_0123456789abcdef0123456789abcdef".to_owned()),
            params: input_params,
        },
    };

    let actual = parse_exec_request(&input);
    assert_eq!(
        (
            actual.kind.as_str(),
            actual.run_id.as_ref().map(ToString::to_string),
            actual.worktree_path.as_deref(),
            actual.close_pane_on_exit,
            actual.exit_on_run_completion,
        ),
        (
            "claude",
            Some("run_0123456789abcdef0123456789abcdef".to_owned()),
            Some(Path::new("/repo/worktree")),
            true,
            true,
        )
    );
    let ExecAction::Launch { prompt, extra_args } = &actual.action else {
        panic!("expected launch actions");
    };
    assert_eq!(
        (prompt.as_deref(), extra_args.as_slice()),
        (
            Some("fix it"),
            ["--dangerously-skip-permissions".to_owned()].as_slice()
        )
    );
    assert_eq!(
        (
            actual.identity.name.as_deref(),
            actual.identity.name_explicit,
            actual.identity.launch_id.as_deref(),
        ),
        (
            Some("swift-otter"),
            true,
            Some("launch_0123456789abcdef0123456789abcdef"),
        )
    );
    assert_eq!(
        actual.identity.params,
        LaunchParams {
            profile: Some("planner".to_owned()),
            mode: Some(PermissionMode::Yolo),
            role: Some("coder".to_owned()),
            model: Some("opus".to_owned()),
            effort: Some("high".to_owned()),
            budget: Some("$12.50/day".to_owned()),
            team: Some("forge".to_owned()),
            launch_group: Some("launch_group_1".to_owned()),
            launch_ordinal: Some(2),
            channel: Some("design".to_owned()),
            kind_ordinal: None,
            ..LaunchParams::default()
        }
    );

    let resume_extra = vec!["--verbose".to_owned()];
    let fork_extra = vec!["--branch".to_owned()];
    for input in [
        minimal_exec_request(
            "claude",
            ExecAction::Resume {
                session_id: "sess-1".to_owned(),
                extra_args: resume_extra,
            },
        ),
        minimal_exec_request(
            "codex",
            ExecAction::Fork {
                session_id: "sess-2".to_owned(),
                extra_args: fork_extra,
            },
        ),
    ] {
        let actual = parse_exec_request(&input);
        assert_eq!(actual.kind, input.kind);
        assert_eq!(
            resume_or_fork_contract(&actual.action),
            resume_or_fork_contract(&input.action)
        );
    }

    let mut resume = minimal_exec_request(
        "codex",
        ExecAction::Resume {
            session_id: "sess-resume".to_owned(),
            extra_args: Vec::new(),
        },
    );
    assert_eq!(
        exec_attach_target(&resume),
        Some((
            AgentKind::new_unchecked("codex"),
            AgentSessionId::from("sess-resume"),
        ))
    );
    resume.identity.launch_id = Some("sess-resume".to_owned());
    assert!(
        exec_launch_identity(&resume)
            .expect("resume-only launch identity")
            .is_none(),
        "a resume id stamps the existing session rather than creating a provisional launch"
    );
    for action in [
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
        ExecAction::Fork {
            session_id: "sess-source".to_owned(),
            extra_args: Vec::new(),
        },
    ] {
        assert!(exec_attach_target(&minimal_exec_request("codex", action)).is_none());
    }

    let mut orphan = minimal_exec_request(
        "claude",
        ExecAction::Launch {
            prompt: None,
            extra_args: Vec::new(),
        },
    );
    orphan.identity.launch_id = Some("launch_orphan".to_owned());
    assert_eq!(
        exec_launch_identity(&orphan)
            .expect_err("launch id requires a name")
            .to_string(),
        "--launch-id requires --agent-name"
    );

    let binding = rimz::agents::ProviderAccountBinding::decode(
        r#"{"scope":{"kind":"sub_provider","provider":"alibaba","variant":"international"},"account_key":"owner"}"#,
    )
    .expect("binding");
    for provider_account in [
        ProviderAccountState::Pending {
            binding: binding.clone(),
        },
        ProviderAccountState::Finalized { binding },
    ] {
        let mut request = minimal_exec_request(
            "qwen",
            ExecAction::Launch {
                prompt: None,
                extra_args: Vec::new(),
            },
        );
        request.provider_account = provider_account;
        assert_eq!(parse_exec_request(&request), request);
    }
}

mod pane_exec {
    use super::*;

    #[test]
    fn hold_requires_a_kept_subagent_with_a_live_watch_and_no_exit_signal() {
        for bits in 0..64 {
            let subagent = bits & 1 != 0;
            let keep = bits & 2 != 0;
            let live_watch = bits & 4 != 0;
            let parent_ended = bits & 8 != 0;
            let cleanup = bits & 16 != 0;
            let interrupted = bits & 32 != 0;
            assert_eq!(
                should_linger_subagent(
                    subagent,
                    keep,
                    live_watch,
                    parent_ended,
                    cleanup,
                    interrupted
                ),
                bits == 7,
                "subagent={subagent} keep={keep} watch={live_watch} parent_ended={parent_ended} cleanup={cleanup} interrupted={interrupted}"
            );
        }
    }

    #[test]
    fn exit_hint_policy_covers_every_interactive_wrapper() {
        let mut run_owned = bare_exec_args();
        run_owned.run_id = Some(rimz::RunId::new());
        let mut worktree_owned = bare_exec_args();
        worktree_owned.worktree_path = Some(PathBuf::from("/tmp/rimz-worktree"));
        let mut empty_dir_owned = worktree_owned.clone();
        empty_dir_owned.identity.params.checkout = Some("/repo".into());
        let mut completion_owned = bare_exec_args();
        completion_owned.run_id = Some(rimz::RunId::new());
        completion_owned.exit_on_run_completion = true;
        completion_owned.close_pane_on_exit = true;
        let mut subagent_completion_owned = completion_owned.clone();
        subagent_completion_owned.subagent = true;
        let mut close_owned = bare_exec_args();
        close_owned.close_pane_on_exit = true;
        let mut child_without_run = close_owned.clone();
        child_without_run.subagent = true;

        for (name, args, hint, record_end, drop_to_shell) in [
            ("bare", bare_exec_args(), true, true, false),
            ("supervised run", run_owned, false, true, false),
            ("worktree", worktree_owned, true, true, true),
            ("empty-dir", empty_dir_owned, true, true, false),
            ("completion", completion_owned, false, false, false),
            (
                "subagent completion",
                subagent_completion_owned,
                false,
                true,
                false,
            ),
            ("close", close_owned, true, true, true),
            ("child without run", child_without_run, true, true, false),
        ] {
            assert_eq!(should_print_exit_hint(&args, false), hint, "{name}");
            assert!(!should_print_exit_hint(&args, true), "{name}");
            assert_eq!(should_record_end_trace(&args), record_end, "{name}");
            assert_eq!(should_drop_to_shell(&args, false), drop_to_shell, "{name}");
            assert!(!should_drop_to_shell(&args, true), "{name}");
        }

        for action in [
            bare_exec_args().action,
            ExecAction::Resume {
                session_id: "sess-source".to_owned(),
                extra_args: Vec::new(),
            },
            ExecAction::Fork {
                session_id: "sess-source".to_owned(),
                extra_args: Vec::new(),
            },
        ] {
            let mut args = bare_exec_args();
            args.action = action;
            assert!(should_print_exit_hint(&args, false));
        }
    }

    #[test]
    fn abrupt_exit_of_an_agent_parked_for_recovery_is_not_deliberate() {
        for (abrupt, listed, pending, expected, expected_reads) in [
            (false, false, false, true, &[][..]),
            (false, true, true, true, &[]),
            (true, false, false, false, &["listing"]),
            (true, false, true, false, &["listing"]),
            (true, true, false, true, &["listing", "pending"]),
            (true, true, true, false, &["listing", "pending"]),
        ] {
            let reads = RefCell::new(Vec::new());
            let deliberate = close_is_deliberate(
                abrupt,
                || {
                    reads.borrow_mut().push("listing");
                    listed
                },
                || {
                    reads.borrow_mut().push("pending");
                    pending
                },
            );
            let case = format!("abrupt={abrupt} listed={listed} pending={pending}");
            assert_eq!(deliberate, expected, "{case}");
            assert_eq!(reads.into_inner(), expected_reads, "{case}");
        }
    }

    /// The respawn decision is the consumer's: whatever ended the relaunch
    /// during the wait (a late first hook, a terminal or timed-out run, an
    /// ended parent, a stop) is read by the last ask before the spawn.
    /// A rebirth parks the agent after its wrapper took the pane; the wrapper's
    /// abrupt exit then reads the record through a real store and stamps nothing.
    #[test]
    fn wrapper_of_an_agent_parked_after_its_attach_stamps_no_end() {
        const CHILD: &str = "RIMZ_TEST_PARKED_WRAPPER";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    concat!(
                        module_path!(),
                        "::wrapper_of_an_agent_parked_after_its_attach_stamps_no_end"
                    )
                    .split_once("::")
                    .expect("test module has a crate prefix")
                    .1,
                    "--nocapture",
                ])
                .env_remove("ZELLIJ_PANE_ID")
                .env("TMUX_PANE", "%7")
                .env(CHILD, "1")
                .output()
                .expect("run with the wrapper's pane");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let workspace = test_workspace(dir.path());
        let store = rimz::Store::open(
            rimz::StatePaths::under(workspace.workspace_id.clone(), dir.path()).expect("paths"),
            rimz::RuntimePaths::under(workspace.workspace_id.clone(), dir.path()).expect("runtime"),
        )
        .expect("store");
        let kind = AgentKind::new_unchecked("claude");
        let session = AgentSessionId::from("alpha");
        store
            .attach_agent_pane(
                &kind,
                &session,
                None,
                &rimz::ids::LoginName::default(),
                &workspace.session_name,
                &rimz::ids::PaneId::from_parts(MuxName::Tmux, "%7"),
                rimz::pane::RuntimeOwner::new(
                    rimz::pane::RuntimeOwnerKind::Agent,
                    "alpha",
                    std::process::id(),
                    None,
                ),
                None,
                None,
                None,
            )
            .expect("the wrapper's attach");
        let park = |agents: serde_json::Value| {
            let record = &store.paths().pending_recovery;
            std::fs::create_dir_all(record.parent().expect("records dir")).expect("records dir");
            std::fs::write(
                record,
                serde_json::json!({"version": 1, "agents": agents}).to_string(),
            )
            .expect("write pending record");
        };
        let invocation = ExecInvocationContext {
            workspace: &workspace,
            cwd: dir.path().to_owned(),
            store: RefCell::new(Some(store.clone())),
            effective_isolation: None,
        };
        let request = minimal_exec_request(
            "claude",
            ExecAction::Resume {
                session_id: "alpha".to_owned(),
                extra_args: Vec::new(),
            },
        );
        let ends = || {
            store
                .read_events()
                .expect("read events")
                .iter()
                .filter(|event| {
                    matches!(
                        event.kind(),
                        rimz::store::event::EventKind::AgentLifecycle(payload)
                            if payload.event_name.as_deref() == Some(AGENT_ENDED_EVENT)
                    )
                })
                .count()
        };

        park(serde_json::json!([["claude", "alpha"]]));
        let parked = stamp_own_end_if_deliberate(&invocation, &request, true, || true);
        assert_eq!(
            parked,
            (false, None),
            "a parked agent's close is not deliberate"
        );
        assert_eq!(ends(), 0, "no end stamp for a parked agent");

        park(serde_json::json!([]));
        let closed = stamp_own_end_if_deliberate(&invocation, &request, true, || true);
        assert_eq!(
            closed,
            (
                true,
                Some(EndedSession {
                    kind,
                    agent_id: session,
                    name: None,
                    is_child_session: false,
                })
            ),
            "a pane close ends"
        );
        assert_eq!(ends(), 1);
    }

    #[test]
    fn exit_hints_use_best_relaunch_identity() {
        let mut team = bare_exec_args();
        team.identity.params.team = Some("trim".to_owned());
        team.identity.params.role = Some("pruner".to_owned());
        team.identity.params.profile = Some("codex-plan".to_owned());
        let mut profile = bare_exec_args();
        profile.identity.params.profile = Some("codex-plan".to_owned());
        assert_eq!(relaunch_command(&team), "rimz agents trim.pruner");
        assert_eq!(relaunch_command(&profile), "rimz agents codex-plan");
        assert_eq!(relaunch_command(&bare_exec_args()), "rimz agents codex");

        let status = exit_status(0);
        let message = exit_hint(&profile, &status, false, false, None, None);
        assert_eq!(
            message,
            format!(
                "rimz: agent `codex` exited ({status}); relaunch with `rimz agents codex-plan`\r\n"
            )
        );
    }

    #[test]
    fn exit_hint_teaches_resume_for_a_redeemable_session() {
        let mut request = bare_exec_args();
        request.identity.params.team = Some("forge".into());
        request.identity.params.role = Some("coder".into());
        let status = exit_status(0);
        let message = exit_hint(&request, &status, false, true, None, None);
        assert_eq!(
            message,
            format!(
                "rimz: agent `codex` exited ({status}); resume with `rimz agents forge.coder --continue`\r\n"
            )
        );

        // A startup failure never advertises resume: there is no conversation.
        let failed = exit_status(1);
        let message = exit_hint(&request, &failed, true, true, None, None);
        assert_eq!(
            message,
            format!(
                "rimz: agent `codex` failed to start ({failed}); relaunch with `rimz agents forge.coder`\r\n"
            )
        );

        // A refused fresh launch left no conversation; a refused model switch
        // left the resumed one intact, so its hint resumes it.
        let refused = exit_status(rimz::agents::capabilities::SESSION_STEP_REFUSED);
        let mut solo = bare_exec_args();
        solo.identity.name = Some("plain-fig".into());
        for startup_failure in [false, true] {
            for (request, resumable, rerun) in [
                (&request, false, "relaunch with `rimz agents forge.coder`"),
                (
                    &request,
                    true,
                    "resume with `rimz agents forge.coder --continue` and a corrected model",
                ),
                (&solo, false, "relaunch with `rimz agents codex`"),
                (
                    &solo,
                    true,
                    "resume with `rimz agents resume @plain-fig` and a corrected model",
                ),
            ] {
                assert_eq!(
                    exit_hint(request, &refused, startup_failure, resumable, None, None),
                    format!(
                        "rimz: agent `codex` launch was refused; see the reason above; {rerun}\r\n"
                    )
                );
            }
        }
    }

    #[test]
    fn solo_exit_hint_prefers_stamped_name_then_request_then_session() {
        let mut request = bare_exec_args();
        request.identity.name = Some("requested-pet".into());
        let session = AgentSessionId::from("019f796b-f60b-7ab0-9adb-35be6e6904b7");
        let status = exit_status(0);
        for (stamped, requested, expected) in [
            (Some("folded-pet"), Some("requested-pet"), "folded-pet"),
            (None, Some("requested-pet"), "requested-pet"),
            (None, None, session.as_str()),
        ] {
            request.identity.name = requested.map(str::to_owned);
            assert_eq!(
                exit_hint(&request, &status, false, true, stamped, Some(&session)),
                format!(
                    "rimz: agent `codex` exited ({status}); resume with `rimz agents resume @{expected}`\r\n"
                )
            );
        }
    }

    #[test]
    fn exit_hint_stays_one_line_in_a_worktree() {
        let path = Path::new("/code/query-engine-worktrees/feat-a");
        let mut request = bare_exec_args();
        request.identity.params.profile = Some("codex-plan".into());
        request.identity.name = Some("bright-otter".into());
        request.worktree_path = Some(path.to_owned());
        for (startup_failure, resumable, code, action) in [
            (false, false, 0, "exited"),
            (false, true, 0, "exited"),
            (true, true, 1, "failed to start"),
        ] {
            let status = exit_status(code);
            let command = if resumable && !startup_failure {
                "resume with `rimz agents resume @bright-otter`"
            } else {
                "relaunch with `rimz agents codex-plan`"
            };
            assert_eq!(
                exit_hint(&request, &status, startup_failure, resumable, None, None,),
                format!("rimz: agent `codex` {action} ({status}); {command}\r\n")
            );
        }
    }

    #[test]
    fn exit_hints_offer_session_resume_for_peers_not_children() {
        let session = "019f796b-f60b-7ab0-9adb-35be6e6904b7";
        let own = rimz::ids::PaneId::parse(OWN_PANE).unwrap();
        let status = exit_status(0);
        for launch in [Some("launch_a"), None] {
            let request = exit_request(launch, Some(session));
            for (parent, depth) in [
                (None, Some(3)),
                (Some("parent"), Some(1)),
                (Some("parent"), None),
            ] {
                let mut agent = pane_row(session, launch, OWN_PANE, 10, true);
                agent.name = Some("swift-otter".into());
                agent.parent_agent_id = parent.map(AgentSessionId::from);
                agent.launch_depth = depth;
                let ended = exit_end_target(&[agent], Some(&own), &request).unwrap();
                let resumable = exited_session_resumable(Some(&ended), Path::new("/code/feature"));
                let hint = exit_hint(
                    &request,
                    &status,
                    false,
                    resumable,
                    ended.name.as_deref(),
                    Some(&ended.agent_id),
                );
                let command = if parent.is_none() {
                    "resume with `rimz agents resume @swift-otter`"
                } else {
                    "relaunch with `rimz agents codex`"
                };
                assert_eq!(
                    hint,
                    format!("rimz: agent `codex` exited ({status}); {command}\r\n")
                );
            }
        }
    }

    #[test]
    fn exited_session_resumable_requires_real_id_and_resume_cli() {
        let cwd = Path::new("/code/feature");
        let codex = EndedSession {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            agent_id: rimz::ids::AgentSessionId::from("019f796b-f60b-7ab0-9adb-35be6e6904b7"),
            name: None,
            is_child_session: false,
        };
        assert!(exited_session_resumable(Some(&codex), cwd));

        let provisional = EndedSession {
            kind: rimz::ids::AgentKind::new_unchecked("codex"),
            agent_id: rimz::ids::AgentSessionId::from("launch_019f2cecea067320b667c5946d266e64"),
            name: None,
            is_child_session: false,
        };
        assert!(!exited_session_resumable(Some(&provisional), cwd));
        assert!(!exited_session_resumable(None, cwd));
    }

    fn exit_status(code: i32) -> std::process::ExitStatus {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;

            std::process::ExitStatus::from_raw(code << 8)
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", &format!("exit {code}")])
                .status()
                .expect("exit status")
        }
    }
}

mod runs {
    use super::*;

    #[test]
    fn session_provider_exit_settles_without_waiting_for_terminal_hooks() {
        let state = tempfile::tempdir().unwrap();
        let runtime = tempfile::Builder::new()
            .prefix("rr")
            .tempdir_in("/tmp")
            .unwrap();
        let workspace = test_workspace(state.path());
        let paths = rimz::StatePaths::under(workspace.workspace_id.clone(), state.path()).unwrap();
        let runtime =
            rimz::RuntimePaths::under(workspace.workspace_id.clone(), runtime.path()).unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        let mut record = RunRecord::new(
            workspace.workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "go".into(),
            state.path().to_owned(),
        );
        record.lifecycle = rimz::store::run::RunLifecycle::Session;
        let context = RunExecContext {
            run_id: record.run_id.clone(),
            store,
            session_name: workspace.session_name.clone(),
            workspace,
        };
        let globals = GlobalFlags {
            mux: Some(MuxName::Tmux),
            tmux: false,
            zellij: false,
            root: None,
            color: crate::cli::ColorWhen::Auto,
        };
        let grace = Duration::from_secs(1);
        let mut outcomes = Vec::new();
        for status in [RunStatus::Pending, RunStatus::Running, RunStatus::Canceled] {
            record.status = status;
            rimz::harness::run::create(context.store.paths(), &record).unwrap();
            let start = Instant::now();
            fail_run_if_child_exited_first(&context, &globals, grace);
            let stored = rimz::harness::run::load(context.store.paths(), &record.run_id).unwrap();
            outcomes.push((stored.status, start.elapsed() < grace));
        }
        assert_eq!(
            outcomes,
            [
                (RunStatus::Failed, true),
                (RunStatus::Completed, true),
                (RunStatus::Canceled, true)
            ]
        );
    }

    #[test]
    fn child_exit_drains_an_already_appended_stop_before_failing_the_run() {
        queued_stop_at_child_exit(false);
    }

    #[test]
    fn child_exit_waits_for_a_resident_drain_beyond_the_terminal_hook_grace() {
        queued_stop_at_child_exit(true);
    }

    fn queued_stop_at_child_exit(slow_resident: bool) {
        let state = tempfile::tempdir().unwrap();
        let root = state.path().canonicalize().unwrap();
        let workspace_id = WorkspaceId::from_project_root(&root);
        let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).unwrap();
        let runtime_root = tempfile::Builder::new()
            .prefix("rr")
            .tempdir_in("/tmp")
            .unwrap();
        let runtime = rimz::RuntimePaths::under(workspace_id.clone(), runtime_root.path()).unwrap();
        let store = rimz::Store::open(paths, runtime).unwrap();
        let mut workspace = test_workspace(&root);
        workspace.workspace_id = workspace_id.clone();
        workspace.project_root = root.clone();
        workspace.worktree_root = root.clone();
        store.record_workspace(&workspace).unwrap();
        let mut run = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("claude"),
            PermissionMode::Auto,
            "task".into(),
            root.clone(),
        );
        run.status = RunStatus::Running;
        run.agent_id = Some("exit-stop".into());
        rimz::harness::run::create(store.paths(), &run).unwrap();
        let frame = rimz::store::ingress::HookIngress {
            boot_id: None,
            schema_version: "1".into(),
            event_id: rimz::EventId::new(),
            ts: jiff::Timestamp::now(),
            source: AgentKind::new_unchecked("claude"),
            event: Some("Stop".into()),
            payload:
                serde_json::json!({"session_id": "exit-stop", "last_assistant_message": "done"})
                    .to_string(),
            cwd: root.clone(),
            hook_pid: std::process::id(),
            env: std::collections::BTreeMap::from([
                ("RIMZ_RUN_ID".into(), run.run_id.to_string()),
                ("RIMZ_PROJECT_ROOT".into(), root.display().to_string()),
                ("RIMZ_WORKSPACE_ID".into(), workspace_id.to_string()),
                ("RIMZ_AGENT_PID".into(), std::process::id().to_string()),
            ]),
        };
        {
            let lock =
                rimz::disk::lock::SharedAppendLock::acquire(&store.paths().hook_ingress_lock)
                    .unwrap();
            rimz::store::ingress::append(store.paths(), &frame, &lock).unwrap();
        }
        crate::cli::hooks::register_test_drainer();
        let context = RunExecContext {
            run_id: run.run_id.clone(),
            store,
            session_name: workspace.session_name.clone(),
            workspace,
        };
        let globals = GlobalFlags {
            mux: None,
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Never,
        };
        std::thread::scope(|scope| {
            if slow_resident {
                let lifetime = rimz::disk::lock::WorkspaceLock::acquire(
                    &context.store.runtime_paths().hook_drainer_lock(),
                )
                .unwrap();
                let listener = std::os::unix::net::UnixListener::bind(
                    context.store.runtime_paths().hook_drainer_socket_path(),
                )
                .unwrap();
                let store = &context.store;
                scope.spawn(move || {
                    use std::io::{BufRead, Write};
                    let _lifetime = lifetime;
                    let (mut stream, _) = listener.accept().unwrap();
                    let mut request = String::new();
                    std::io::BufReader::new(&stream)
                        .read_line(&mut request)
                        .unwrap();
                    let request: serde_json::Value = serde_json::from_str(&request).unwrap();
                    assert_eq!(request["through"], 0);
                    std::thread::sleep(RUN_EXIT_TERMINAL_GRACE * 2 + Duration::from_millis(200));
                    run.status = RunStatus::Completed;
                    run.last_message = Some("done".into());
                    rimz::harness::run::create(store.paths(), &run).unwrap();
                    let _ = stream.write_all(b"{\"applied\":0,\"reply\":null}\n");
                });
            }
            fail_run_if_child_exited_first(&context, &globals, RUN_EXIT_TERMINAL_GRACE);
            let completed =
                rimz::harness::run::load(context.store.paths(), &context.run_id).unwrap();
            assert_eq!(
                completed.status,
                RunStatus::Completed,
                "the terminal decision must see the queued Stop"
            );
            assert_eq!(completed.last_message.as_deref(), Some("done"));
        });
    }

    #[tokio::test(flavor = "current_thread")]
    async fn child_exit_marks_nonterminal_run_failed_and_wakes_waiter() {
        #[cfg(unix)]
        if std::env::var_os("RIMZ_TEST_EXIT_CAPTURE_LOG").is_none() {
            use std::os::unix::fs::PermissionsExt;

            let bin = tempfile::tempdir().expect("fake mux dir");
            let tmux = bin.path().join("tmux");
            std::fs::write(
                &tmux,
                "#!/bin/sh\n[ \"$1\" = '-S' ] || exit 1\nshift 3\n[ \"$*\" = 'capture-pane -p -t %7 -S -60' ] || exit 1\nprintf 'capture\\n' >> \"$RIMZ_TEST_EXIT_CAPTURE_LOG\"\nprintf 'provider failure evidence\\n'\n",
            )
            .expect("fake tmux");
            std::fs::set_permissions(&tmux, std::fs::Permissions::from_mode(0o755))
                .expect("executable tmux");
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    concat!(
                        module_path!(),
                        "::child_exit_marks_nonterminal_run_failed_and_wakes_waiter"
                    )
                    .split_once("::")
                    .expect("test module has a crate prefix")
                    .1,
                    "--nocapture",
                ])
                .env("PATH", bin.path())
                .env("XDG_RUNTIME_DIR", bin.path())
                .env("TMUX_PANE", "%7")
                .env("RIMZ_TEST_EXIT_CAPTURE_LOG", bin.path().join("captures"))
                .output()
                .expect("isolated capture test");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(bin.path().join("captures")).expect("capture log"),
                "capture\ncapture\ncapture\ncapture\ncapture\ncapture\n"
            );
            return;
        }

        let state = tempfile::tempdir().expect("state dir");
        let runtime_root = tempfile::Builder::new()
            .prefix("rr")
            .tempdir_in("/tmp")
            .expect("runtime dir");
        let workspace_id = WorkspaceId::from_project_root(Path::new("/tmp/rimz-run"));
        let paths = rimz::StatePaths::under(workspace_id.clone(), state.path()).expect("paths");
        let runtime =
            rimz::RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
        paths.ensure_dirs().expect("state dirs");
        runtime.ensure_dirs().expect("runtime dirs");
        let record = RunRecord::new(
            workspace_id.clone(),
            AgentKind::new_unchecked("codex"),
            PermissionMode::Auto,
            "summarize".to_owned(),
            Path::new("/tmp/rimz-run").to_path_buf(),
        );
        let run_id = record.run_id.clone();
        rimz::harness::run::create(&paths, &record).expect("create run");
        let context = RunExecContext {
            run_id: run_id.clone(),
            store: rimz::Store::open(paths.clone(), runtime).expect("store"),
            session_name: "rimz-test".to_owned(),
            workspace: rimz::ResolvedWorkspace {
                workspace_id: workspace_id.clone(),
                project_root: Path::new("/tmp/rimz-run").to_path_buf(),
                cwd_project_root: None,
                root_class: rimz::workspace::RootClass::Directory,
                worktree_root: Path::new("/tmp/rimz-run").to_path_buf(),
                worktree_branch: None,
                session_name: "rimz-test".to_owned(),
                mux_hint: None,
            },
        };
        let waiter = rimz::harness::run_wake::RunWaiter::bind(
            context.store.runtime_paths(),
            ExpectedRunFrame {
                workspace_id,
                run_id: run_id.clone(),
            },
            rimz::harness::run::RunCancellation::new(),
        )
        .expect("bind run");

        let globals = GlobalFlags {
            mux: Some(MuxName::Tmux),
            zellij: false,
            tmux: false,
            root: None,
            color: crate::cli::ColorWhen::Auto,
        };

        fail_run_if_child_exited_first(&context, &globals, Duration::ZERO);

        let failed = rimz::harness::run::load(&paths, &run_id).expect("load failed run");
        assert_eq!(failed.status, RunStatus::Failed);
        let terminal = waiter
            .wait_terminal(&context.store, Some(Duration::from_secs(1)), None)
            .await
            .expect("run wait");
        assert_eq!(terminal.status, RunStatus::Failed);
        #[cfg(unix)]
        {
            assert_eq!(
                terminal.failure_tail.as_deref(),
                Some("provider failure evidence")
            );
            for status in [
                RunStatus::Failed,
                RunStatus::VerifyFailed,
                RunStatus::TimedOut,
                RunStatus::BudgetExceeded,
                RunStatus::Canceled,
                RunStatus::Completed,
            ] {
                let mut record = terminal.clone();
                record.status = status;
                record.failure_tail = None;
                rimz::harness::run::create(&paths, &record).expect("terminal run without evidence");
                assert!(
                    rimz::store::run::run_waiter_is_live(context.store.runtime_paths(), &run_id)
                        .expect("probe waiter")
                );

                fail_run_if_child_exited_first(&context, &globals, Duration::ZERO);

                let captured = rimz::harness::run::load(&paths, &run_id).expect("captured run");
                assert_eq!(captured.status, status);
                assert_eq!(
                    captured.failure_tail.as_deref(),
                    (status != RunStatus::Completed).then_some("provider failure evidence")
                );
                fail_run_if_child_exited_first(&context, &globals, Duration::ZERO);
            }
        }
    }
}
