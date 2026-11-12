use super::*;
use crate as rimz;
use crate::agents::LaunchParams;
use crate::harness::plan::run_exit_policy;
use crate::{RuntimePaths, WorkspaceId};
#[test]
fn subagent_run_closes_its_pane_after_terminal_completion() {
    let dir = tempfile::tempdir().expect("temp dir");
    let runtime = RuntimePaths::under(WorkspaceId::from_project_root(dir.path()), dir.path())
        .expect("runtime");
    assert_eq!(run_exit_policy(true), (true, true));
    assert_eq!(run_exit_policy(false), (false, false));

    let run_id = rimz::RunId::new();
    let mut parent =
        crate::testkit::agent_state("claude", "parent-session", jiff::Timestamp::UNIX_EPOCH);
    parent.launch_id = Some("parent-launch".into());
    parent.name = Some("swift-otter".into());
    parent.team = Some("recon".into());
    parent.role = Some("planner".into());
    let launch = rimz::agents::LaunchParams {
        model: Some("gpt-5".to_owned()),
        profile: Some("reviewer".to_owned()),
        parent_agent_id: parent.launch_id.clone(),
        parent_agent_kind: Some(parent.kind.clone()),
        ..Default::default()
    };
    let launch_id = rimz::ids::AgentSessionId::from("child-id");
    let team_prompt = rimz::harness::team_prompt::TeamPrompt {
        consensus: rimz::harness::team_prompt::Consensus::BuiltIn,
        files: vec!["/team/pipeline.md".into()],
    };
    let system_prompt = rimz::config::PromptSource::File("/agents/child.md".into());
    let appended = [rimz::config::PromptSource::File("/agents/extra.md".into())];
    let skills = ["review".parse().unwrap()];
    let allowed_tools = ["Read".parse().unwrap()];
    let permission_args = ["--full-auto".to_owned()];
    let binding = rimz::agents::ProviderAccountBinding::decode(
        r#"{"scope":{"kind":"kind_wide"},"account_key":"acct"}"#,
    )
    .expect("provider binding");
    let worktree = Path::new("/tmp/child-worktree");
    let cell = rimz::harness::spec::AgentCell {
        notices: Vec::new(),
        empty_dir: None,
        timeout: None,
        resume_model_override: false,
        isolation_default: Some(rimz::config::Isolation::Scoped),
        kind: rimz::agents::definition_by_kind("codex")
            .unwrap()
            .spec()
            .kind_id(),
        args: permission_args.to_vec(),
        auto_compact: None,
        system_prompt_file: Some(system_prompt.clone()),
        append_system_prompt_files: appended.to_vec(),
        team_prompt: Some(team_prompt.clone()),
        skills: Some(skills.to_vec()),
        no_skill_tool: false,
        allowed_tools: Some(allowed_tools.to_vec()),
        launch: LaunchParams::default(),
    };
    let mut cell = cell;
    cell.launch.clone_from(&launch);
    let layout = crate::harness::spec::LayoutSpec {
        columns: vec![crate::harness::spec::Column {
            rows: vec![crate::harness::spec::Cell::Agent(cell)],
            stacked: false,
        }],
    };
    let identities = [crate::store::writer::AgentLaunchIdentity {
        kind: crate::ids::AgentKind::new_unchecked("codex"),
        agent_id: launch_id,
        name: "child".into(),
        name_explicit: true,
        launch: launch.clone(),
        run_id: Some(run_id.clone()),
        prompt: Some("work".into()),
    }];
    let managed = crate::agents::ManagedLaunchState::Bound(binding.clone());
    let mut panes = crate::harness::plan::compile_layout_panes(
        &layout,
        crate::harness::plan::LayoutPaneParams {
            runtime: &runtime,
            cwd: worktree,
            cleanup_worktree: true,
            in_place: false,
            pick_session: false,
            resume_seeds: None,
            launch_identities: &identities,
            agents: &[parent],
            reminder_section: None,
            leader_run: Some(crate::harness::plan::LayoutRunParams {
                run_id: &run_id,
                resume: None,
                leader_index: 0,
                managed_launch: Some(&managed),
                cleanup: true,
                subagent: true,
            }),
        },
    )
    .unwrap();
    let pane = panes.columns.remove(0).panes.remove(0);
    assert_eq!(
        pane.name.as_deref(),
        Some("recon · planner (@swift-otter) › reviewer subagent (@child)")
    );
    let payload = std::fs::read_to_string(pane.argv.last().expect("exec request file"))
        .expect("exec request contents");
    let request =
        rimz::harness::launch::decode_exec_request("codex", Some(worktree), &payload).unwrap();
    // Together `close_pane_on_exit`, `exit_on_run_completion`, `subagent` and
    // `team_prompt` select the wrapper's parent-receipt hold before it closes the pane.
    assert_eq!(
        request,
        rimz::harness::launch::ExecRequest {
            isolation_default: Some(rimz::config::Isolation::Scoped),
            kind: rimz::agents::definition_by_kind("codex")
                .unwrap()
                .spec()
                .kind_id(),
            action: rimz::harness::launch::ExecAction::Launch {
                prompt: Some("work".to_owned()),
                extra_args: permission_args.to_vec(),
            },
            system_prompt_file: Some(system_prompt),
            append_system_prompt_files: appended.to_vec(),
            team_prompt: Some(team_prompt),
            skills: Some(skills.to_vec()),
            no_skill_tool: false,
            allowed_tools: Some(allowed_tools.to_vec()),
            provider_account: rimz::harness::launch::ProviderAccountState::Pending { binding },
            run_id: Some(run_id),
            worktree_path: Some(worktree.to_path_buf()),
            close_pane_on_exit: true,
            exit_on_run_completion: true,
            subagent: true,
            continue_session: false,
            follow_up_message: None,
            reminder_section: None,
            headless: None,
            identity: rimz::harness::launch::ExecIdentity {
                resume_model_override: false,
                name: Some("child".to_owned()),
                name_explicit: true,
                launch_id: Some("child-id".to_owned()),
                params: launch,
            },
        }
    );
}

fn ladder_request(
    timeout: Option<Duration>,
    warn: Option<Vec<Duration>>,
    grace: Option<Duration>,
) -> SupervisedRunRequest {
    let mut request = SupervisedRunRequest::new(
        "worker".into(),
        "Read the brief.".into(),
        None,
        crate::agents::ManagedLaunchState::PendingResolution,
    );
    request.subagent = true;
    request.timeout = timeout;
    request.warn = warn;
    request.grace = grace;
    request
}

fn minutes(values: &[u64]) -> Vec<Duration> {
    values.iter().map(|m| Duration::from_secs(m * 60)).collect()
}

fn ladder(
    request: &SupervisedRunRequest,
    definition: Option<u64>,
    defaults: &crate::config::SubagentsConfig,
) -> Result<DeadlineLadder> {
    resolve_deadline_ladder(
        request,
        definition.map(|m| Duration::from_secs(m * 60)),
        defaults,
        jiff::Timestamp::UNIX_EPOCH,
    )
}

#[test]
fn deadline_ladder_takes_the_launch_then_the_definition_then_the_config() {
    let defaults = crate::config::SubagentsConfig::default();
    let unset = ladder_request(None, None, None);
    let explicit = ladder_request(Some(Duration::from_secs(20 * 60)), None, None);
    let timeouts = [(&unset, Some(90)), (&explicit, Some(90)), (&unset, None)]
        .map(|(request, definition)| ladder(request, definition, &defaults).unwrap().timeout);
    assert_eq!(timeouts.to_vec(), minutes(&[90, 20, 30]));
    assert_eq!(
        ladder(&unset, Some(90), &defaults).unwrap(),
        DeadlineLadder {
            timeout: Duration::from_secs(90 * 60),
            warn: minutes(&[10, 5]),
            grace: Duration::from_secs(5 * 60),
        }
    );
}

#[test]
fn deadline_ladder_filters_warnings_against_the_final_timeout() {
    let defaults = crate::config::SubagentsConfig::default();
    let unset = ladder_request(None, None, None);
    let warn = |request: &SupervisedRunRequest, definition| {
        ladder(request, definition, &defaults).unwrap().warn
    };
    assert_eq!(warn(&unset, Some(10)), minutes(&[5]));
    assert_eq!(warn(&unset, Some(4)), Vec::<Duration>::new());
    let given = ladder_request(
        Some(Duration::from_secs(5 * 60)),
        Some(
            [0, 300, 360, 180, 60, 180]
                .map(Duration::from_secs)
                .to_vec(),
        ),
        None,
    );
    assert_eq!(warn(&given, Some(90)), minutes(&[3, 1]));
    let disabled = ladder_request(None, Some(Vec::new()), Some(Duration::ZERO));
    assert_eq!(
        ladder(&disabled, Some(10), &defaults).unwrap(),
        DeadlineLadder {
            timeout: Duration::from_secs(10 * 60),
            warn: Vec::new(),
            grace: Duration::ZERO,
        }
    );
}

#[test]
fn deadline_ladder_refuses_a_malformed_config_or_an_overflow() {
    let unset = ladder_request(None, None, None);
    let config = |key: &str, value: &str| {
        let mut defaults = crate::config::SubagentsConfig::default();
        match key {
            "timeout" => defaults.timeout = value.into(),
            "warn" => defaults.warn = vec![value.into()],
            _ => defaults.grace = value.into(),
        }
        defaults
    };
    for key in ["timeout", "warn", "grace"] {
        let error = ladder(&unset, None, &config(key, "0")).unwrap_err();
        assert!(
            format!("{error:#}").contains(&format!("parsing agents.subagents.{key}")),
            "{error:#}"
        );
    }
    let error = resolve_deadline_ladder(
        &unset,
        Some(Duration::from_secs(90 * 60)),
        &crate::config::SubagentsConfig::default(),
        jiff::Timestamp::MAX,
    )
    .unwrap_err();
    assert_eq!(error.to_string(), "computing subagent kill deadline");
}
