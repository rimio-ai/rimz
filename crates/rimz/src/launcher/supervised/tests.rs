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
    let launch = rimz::agents::LaunchParams {
        model: Some("gpt-5".to_owned()),
        profile: Some("reviewer".to_owned()),
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
        empty_dir: None,
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
            fallback_channel: None,
            loop_reminder: None,
            leader_run: Some(crate::harness::plan::LayoutRunParams {
                run_id: &run_id,
                leader_index: 0,
                managed_launch: Some(&managed),
                cleanup: true,
                subagent: true,
            }),
        },
    )
    .unwrap();
    let pane = panes.columns.remove(0).panes.remove(0);
    assert_eq!(pane.name.as_deref(), Some("codex"));
    let request = rimz::harness::launch::decode_exec_request(
        "codex",
        Some(worktree),
        pane.argv.last().expect("exec payload"),
    )
    .unwrap();
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
            allowed_tools: Some(allowed_tools.to_vec()),
            provider_account: rimz::harness::launch::ProviderAccountState::Pending { binding },
            run_id: Some(run_id),
            worktree_path: Some(worktree.to_path_buf()),
            close_pane_on_exit: true,
            exit_on_run_completion: true,
            subagent: true,
            follow_up_message: None,
            loop_reminder: None,
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
