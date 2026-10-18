//! Agent-only fresh-session succession at the caller's recorded generation.

use super::*;
use rimz::harness::ancestry::{LaunchFocus, resolve_caller, resolve_launch_caller};
use rimz::harness::launch::ExecAction;

use super::placement::{PlacementErrors, PlacementRequest};

#[derive(Debug, Args)]
pub(super) struct MoltArgs {
    /// Context and instructions for the fresh session.
    #[arg(value_name = "NOTE", allow_hyphen_values = true)]
    note: Option<String>,
    /// Read the note from stdin; an inline instruction goes first.
    #[arg(long)]
    stdin: bool,
    /// Re-base the same job onto this profile or provider.
    #[arg(long, value_name = "PROFILE|KIND")]
    agent: Option<String>,
}

pub(super) fn run_molt(args: MoltArgs, globals: &GlobalFlags) -> Result<()> {
    let user_shell = || {
        UsageError::new(
        "molt runs only inside an agent: only the session holding the context can write the note. Type /rimz-molt in the agent's pane, or send it with `rimz message @<agent> '/rimz-molt'`".into(),
    )
    };
    let ctx = match Ctx::open(globals) {
        Ok(ctx) => ctx,
        Err(_) if resolve_caller(&[]).is_none() => return Err(user_shell().into()),
        Err(error) => return Err(error),
    };
    let snapshot = ctx.store.runtime_projection(rimz::RuntimeScope::Runtime)?;
    let caller = resolve_caller(&snapshot.agents).ok_or_else(user_shell)?;
    let source = resolve_launch_caller(&snapshot.agents, &caller)?;
    if source.is_launched_child() {
        bail!("subagents cannot molt; finish and report to your caller; do not retry this command");
    }
    let piped = if args.stdin {
        crate::cli::send::read_stdin_prompt()?
    } else {
        None
    };
    let note = crate::cli::send::combine_text_prompt(args.note.as_deref(), piped.as_deref())
        .ok_or_else(|| UsageError::new("molt requires non-empty NOTE or --stdin".into()))?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let cwd = source
        .worktree_path
        .as_deref()
        .map_or_else(|| workspace.worktree_root.clone(), PathBuf::from);
    let machine = crate::cli::machine_config();
    let (kind, posture) = if let Some(alias) = args.agent.as_deref() {
        let effective = rimz::config::effective::load(&machine, &workspace.project_root)?;
        effective.block_set_failure()?;
        replacement_posture(
            source,
            &machine,
            &effective,
            alias,
            Some((store.runtime_paths(), store.paths())),
        )?
    } else {
        (
            source.kind.clone(),
            restart::restart_posture(source, workspace, &machine)?,
        )
    };
    let mut agent = source.clone();
    agent.kind = kind.clone();
    let adapter = rimz::agents::find_definition(kind.as_str())
        .with_context(|| format!("unknown agent kind `{kind}`"))?;
    let isolation = rimz::config::Isolation::resolve(
        source.isolation,
        posture.launch.isolation_default,
        machine.agents.isolation,
    );
    rimz::sandbox::preflight_launch(
        isolation,
        &kind,
        posture.launch.skills.is_some(),
        adapter.manual_skill(),
    )?;
    rimz::harness::launch::preflight_agent_kind(&workspace.project_root, kind.as_str(), &cwd)?;
    let logins = rimz::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &machine,
    )?;
    let catalog = rimz::agents::machine_login_catalog();
    let login = if args.agent.is_some() {
        Some(logins.name(&kind)?)
    } else {
        restart::relaunch_action(source, &logins, &catalog, &cwd)?.1
    };
    let provider_login = rimz::agents::session_login(&kind, login.as_ref(), &machine.accounts)?;
    provider_login.health(&catalog.native_ambient(&kind, &rimz::agents::ambient_env()))?;
    let repo_root = if source.checkout.is_none() {
        workspace.at_launch_dir(&cwd)?.cwd_project_root
    } else {
        None
    };
    crate::cli::folder_trust::preflight(
        kind.as_str(),
        &cwd,
        adapter.folder_trust(
            &cwd,
            repo_root.as_deref(),
            &provider_login.env(&rimz::agents::ambient_env()),
        ),
        None,
    )?;
    let action = ExecAction::Launch {
        prompt: Some(note.clone()),
        extra_args: Vec::new(),
    };
    if isolation == rimz::config::Isolation::Host {
        rimz::harness::launch::preflight_agent_process(
            &workspace.project_root,
            &restart::relaunch_request(&agent, &posture, action.clone(), login.clone(), None),
            &cwd,
            Some(store.runtime_paths()),
        )?;
    }
    let room = RoomContext::live_tab(workspace, machine, globals.mux)?;
    let mux = room.mux_name();
    let batch = store.begin_agent_launch_batch(
        &[molt_request(&agent, &posture, login.clone(), &note)?],
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.clone(),
            branch: None,
            description: None,
        },
    )?;
    let launch = batch.single_identity()?;
    let mut invocation = restart::relaunch_request(&agent, &posture, action, login, Some(launch));
    invocation.identity.params.clone_from(&launch.launch);
    let (title, pane_name) = molt_tab_names(&agent, &posture);
    let argv = rimz::harness::launch::exec_argv(
        &rimz::proc::rimz_exe(),
        store.runtime_paths(),
        &invocation,
    )
    .inspect_err(|_| {
        let _ = store.fail_agent_launch_batch(&batch);
    })?;
    placement::execute(
        room.backend(),
        store,
        &batch,
        PlacementRequest {
            placement: Placement::NewTab,
            mux,
            tab_anchor: placement::resolve_tab_anchor(
                Placement::NewTab,
                false,
                rimz::mux::own_pane_id(mux),
                crate::cli::pinned_room_root().as_deref(),
                &workspace.project_root,
            ),
            cwd: cwd.clone(),
            title,
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv,
                        name: Some(pane_name),
                    }],
                    stacked: false,
                }],
                focused_pane: 0,
            },
            sidebar: room.sidebar_options(&cwd),
            identity_env: rimz::room::pane_identity_env(
                workspace,
                &cwd,
                source.channel.as_deref(),
                false,
            ),
            focus: LaunchFocus::Keep,
            errors: PlacementErrors {
                new_tab: "opening molt successor tab",
                new_pane: "opening molt successor pane",
                same_pane: "opening molt successor",
            },
        },
        || rimz::harness::assist_log::record_tier_fallbacks(batch.identities()),
    )?;
    let predecessor = format!(
        "@{}",
        source.name.as_deref().unwrap_or(source.agent_id.as_str())
    );
    let successor = format!("@{}", launch.name);
    rimz::harness::assist_log::append(&rimz::harness::assist_log::AssistRecord {
        at: jiff::Timestamp::now(),
        assist: rimz::harness::assist_log::Assist::Molt {
            kind: source.kind.clone(),
            agent_id: source.agent_id.clone(),
            label: predecessor.clone(),
            successor_kind: kind,
            successor_id: launch.agent_id.clone(),
            successor_label: successor.clone(),
            generation: source.launch_depth,
        },
    });
    let audit = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    rimz::harness::run::rekey_team_run(store.paths(), source, launch, &audit.agents)
        .with_context(|| format!("{successor} is live in a new tab, but its team run could not move; do not retry this command"))?;
    let stop = rimz::store::idle_stop::IdleStopRequest {
        kind: source.kind.clone(),
        agent_id: source.agent_id.clone(),
        stop: rimz::agents::state::IdleStop {
            after_secs: 300,
            requested_at: jiff::Timestamp::now(),
            requested_by: Some(predecessor.clone()),
        },
    };
    rimz::store::idle_stop::arm(store.paths(), stop).with_context(|| {
        format!(
            "{successor} is live in a new tab, but {predecessor}'s idle stop could not be armed; \
             run `rimz agents stop {predecessor} --when-idle 5m`; do not retry this command"
        )
    })?;
    let idle = rimz::harness::schedule::arm::duration_label(Duration::from_secs(300));
    writeln!(
        crate::cli::render::out(),
        "molted {predecessor} into {successor} (new tab); {predecessor} stops once idle for {idle} with nothing owed\nnext: rimz agents show {successor}"
    )?;
    Ok(())
}

fn replacement_posture(
    agent: &AgentState,
    machine: &rimz::config::MachineConfig,
    effective: &rimz::config::effective::LaunchAgents,
    alias: &str,
    room: Option<(&rimz::RuntimePaths, &rimz::StatePaths)>,
) -> Result<(AgentKind, ResumePosture)> {
    let spec = match (agent.team.as_deref(), agent.role.as_deref()) {
        (Some(team), Some(role)) => format!("{team}.{role}"),
        _ => agent
            .profile
            .clone()
            .unwrap_or_else(|| agent.kind.to_string()),
    };
    let finalized = launch_resolve::resolve_finalized_layout(
        None,
        machine,
        effective,
        Some(&spec),
        None,
        &LaunchOverrideArgs {
            agent: Some(alias.into()),
            isolation: agent.isolation,
            ..Default::default()
        },
        None,
        None,
        None,
        true,
        room,
    )?;
    for warning in &finalized.warnings {
        writeln!(crate::cli::render::err(), "{warning}")?;
    }
    let mut cell = finalized
        .resolved
        .layout
        .agent_cells()
        .next()
        .context("molt produced no agent cell")?
        .clone();
    if let Some(mode) = agent.mode {
        let adapter = rimz::agents::find_definition(cell.kind.as_str())
            .context("unknown replacement provider")?;
        adapter.spec().launch.strip_permission_args(&mut cell.args);
        cell.args
            .extend(adapter.spec().launch.permission_args(mode));
        cell.launch.mode = Some(mode);
    }
    cell.launch.isolation = agent.isolation;
    cell.launch
        .record
        .get_or_insert_with(Default::default)
        .agent = Some(alias.into());
    Ok((
        cell.kind.clone(),
        ResumePosture {
            launch: rimz::harness::plan::ResumeLaunchPosture::from(&cell),
            degraded: None,
        },
    ))
}

/// The successor's tab title and pane name, named as a fresh launch of the
/// same seat would be.
fn molt_tab_names(agent: &AgentState, posture: &ResumePosture) -> (String, String) {
    let title = agent.channel.as_deref().map_or_else(
        || {
            rimz::harness::spec::default_tab_title(
                &LayoutSpec::single(restart::restart_cell(agent, posture)),
                None,
                agent.team.as_deref(),
            )
        },
        |channel| format!("#{channel}"),
    );
    let pane_name = agent
        .profile
        .clone()
        .unwrap_or_else(|| agent.kind.to_string());
    (title, pane_name)
}

fn molt_request(
    agent: &AgentState,
    posture: &ResumePosture,
    login: Option<rimz::ids::LoginName>,
    note: &str,
) -> Result<AgentLaunchRequest> {
    let mut request = restart::fresh_launch_request(
        agent,
        restart::restart_cell(agent, posture),
        posture.launch.mode,
    )?;
    request.launch.molted_from = Some(
        agent
            .launch_id
            .clone()
            .unwrap_or_else(|| agent.agent_id.clone()),
    );
    request.prompt = Some(note.into());
    request.login = rimz::store::writer::LaunchLogin::Pinned(login.unwrap_or_default());
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacement_rebases_the_same_role_without_replaying_the_old_record() {
        let dir = tempfile::tempdir().unwrap();
        let role_prompt = dir.path().join("role.md");
        std::fs::write(&role_prompt, "Keep the role's job").unwrap();
        let mut machine = rimz::config::MachineConfig::default();
        machine.agents.profiles = serde_json::from_value(serde_json::json!({
            "worker": {"agent": "claude", "model": "opus"},
            "replacement": {"agent": "codex", "model": "gpt-6.1-sol", "effort": "medium"}
        }))
        .unwrap();
        machine.agents.teams = serde_json::from_value(serde_json::json!({
            "forge": {"roles": [{"role": "coder", "profile": "worker", "system-prompt-file": role_prompt}]}
        })).unwrap();
        let effective = rimz::config::effective::load(&machine, dir.path()).unwrap();
        let mut agent = rimz::testkit::agent_state("claude", "old", jiff::Timestamp::UNIX_EPOCH);
        agent.profile = Some("worker".into());
        agent.team = Some("forge".into());
        agent.role = Some("coder".into());
        agent.mode = Some(PermissionMode::Ask);
        agent.isolation = Some(rimz::config::Isolation::Sandbox);
        agent.record = Some(Box::new(rimz::agents::LaunchRecord {
            model: Some("old-record".into()),
            effort: Some("xhigh".into()),
            fast: true,
            agent: None,
        }));
        let (kind, posture) =
            replacement_posture(&agent, &machine, &effective, "replacement", None).unwrap();
        assert_eq!(kind.as_str(), "codex");
        assert_eq!(posture.launch.model.as_deref(), Some("gpt-6.1-sol"));
        assert_eq!(posture.launch.effort.as_deref(), Some("medium"));
        assert!(!posture.launch.fast);
        assert_eq!(posture.launch.mode, agent.mode);
        assert_eq!(posture.launch.isolation, agent.isolation);
        assert_eq!(posture.launch.system_prompt_file, Some(role_prompt.into()));
        assert_eq!(
            posture.launch.record.as_ref().unwrap().agent.as_deref(),
            Some("replacement")
        );
    }

    #[test]
    fn molt_request_copies_the_seat_and_lineage_without_opening_a_peer_turn() {
        let mut agent =
            rimz::testkit::agent_state("claude", "session", jiff::Timestamp::UNIX_EPOCH);
        agent.name = Some("fox".into());
        agent.launch_id = Some("launch_old".into());
        agent.team = Some("forge".into());
        agent.role = Some("coder".into());
        agent.profile = Some("worker".into());
        agent.channel = Some("lane".into());
        agent.launch_group = Some("group".into());
        agent.launch_ordinal = Some(2);
        agent.launch_depth = Some(3);
        agent.loop_task = Some("fixer".into());
        agent.launched_by = Some(rimz::agents::LaunchedBy {
            kind: AgentKind::new_unchecked("codex"),
            agent_id: "launch_root".into(),
        });
        let posture = ResumePosture {
            launch: Default::default(),
            degraded: None,
        };
        let request =
            molt_request(&agent, &posture, Some("work".parse().unwrap()), "the note").unwrap();
        assert_eq!(request.launch.molted_from, agent.launch_id);
        assert_eq!(request.prompt.as_deref(), Some("the note"));
        assert_eq!(request.launch.launch_depth, agent.launch_depth);
        assert_eq!(
            request.launch.launched_by.as_deref(),
            agent.launched_by.as_ref()
        );
        assert_eq!(request.launch.parent_agent_id, agent.parent_agent_id);
        assert_eq!(request.launch.parent_agent_kind, agent.parent_agent_kind);
        assert_eq!(request.launch.team, agent.team);
        assert_eq!(request.launch.role, agent.role);
        assert_eq!(request.launch.profile, agent.profile);
        assert_eq!(request.launch.channel, agent.channel);
        assert_eq!(request.launch.launch_group, agent.launch_group);
        assert_eq!(request.launch.launch_ordinal, agent.launch_ordinal);
        assert_eq!(request.launch.loop_task, agent.loop_task);
        assert_eq!(
            request.login,
            rimz::store::writer::LaunchLogin::Pinned("work".parse().unwrap())
        );
        assert_eq!(request.name, AgentLaunchName::Soft("fox".into()));
        assert!(request.run_id.is_none());
    }

    #[test]
    fn molt_tab_names_follow_a_fresh_launch_of_the_seat() {
        let posture = ResumePosture {
            launch: Default::default(),
            degraded: None,
        };
        let names = |profile: Option<&str>, channel: Option<&str>, team: Option<&str>| {
            let mut agent =
                rimz::testkit::agent_state("claude", "session", jiff::Timestamp::UNIX_EPOCH);
            agent.profile = profile.map(Into::into);
            agent.channel = channel.map(Into::into);
            agent.team = team.map(Into::into);
            molt_tab_names(&agent, &posture)
        };
        let pair = |title: &str, pane: &str| (title.to_string(), pane.to_string());
        assert_eq!(
            names(Some("brainstormer"), None, None),
            pair("brainstormer", "brainstormer")
        );
        assert_eq!(
            names(Some("brainstormer"), Some("lane"), None),
            pair("#lane", "brainstormer")
        );
        assert_eq!(
            names(Some("worker"), None, Some("forge")),
            pair("team:forge", "worker")
        );
        assert_eq!(names(None, None, None), pair("claude", "claude"));
    }
}
