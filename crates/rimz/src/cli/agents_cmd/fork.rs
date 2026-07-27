//! Provider-native conversation forks launched as fresh RimZ agent rows.

use super::*;
use crate::cli::machine_config;

use super::placement::{PlacementErrors, PlacementRequest};

#[derive(Debug, Args)]
pub(super) struct ForkArgs {
    /// Existing live or stopped agent to fork.
    #[arg(
        value_name = "REFERENCE",
        add = clap_complete::ArgValueCandidates::new(crate::cli::complete::agent_refs)
    )]
    pub(super) reference: String,
    /// Durable name for the forked agent.
    #[arg(long, short = 'n', value_name = "NAME")]
    pub(super) name: Option<String>,
    /// Split the fork into a new pane in the current tab.
    #[arg(long)]
    pub(super) new_pane: bool,
    /// Open the fork in a new tab/window.
    #[arg(long, conflicts_with = "new_pane")]
    pub(super) new_tab: bool,
    /// Leave focus on the launching pane.
    #[arg(long)]
    pub(super) bg: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ForkSeed {
    kind: AgentKind,
    source_session_id: AgentSessionId,
    cwd: PathBuf,
    launch: rimz::agents::LaunchParams,
}

pub(super) fn run_fork(args: ForkArgs, globals: &GlobalFlags) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let snapshot = ctx.alive_snapshot()?;
    let source = resolve_fork_source(store, workspace, ctx.runtime(), &snapshot, &args.reference)?;
    let mut seed = validate_fork_source(
        &source,
        rimz::harness::resume::resume_session_present,
        Path::is_dir,
    )?;
    let source_name = source.name.as_deref().unwrap_or("unnamed").to_owned();
    let channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &seed.cwd,
        source.team.as_deref(),
        source.channel.as_deref(),
    );
    seed.launch.channel.clone_from(&channel);

    if let Some(name) = args.name.as_deref() {
        validate_agent_name(name)?;
    }
    let config = machine_config();
    rimz::agents::find_definition(seed.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", seed.kind))?;
    let effective = rimz::config::effective::load(
        &config.agents,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    let posture = fork_posture(&seed, &effective.profiles)?;
    if let Some(reason) = &posture.degraded {
        writeln!(
            crate::cli::render::err(),
            "rimz: {reason}; forking bare {}",
            seed.kind
        )?;
    }
    seed.launch.mode = posture.mode;
    seed.launch.model.clone_from(&posture.model);
    seed.launch.effort.clone_from(&posture.effort);
    seed.launch.budget.clone_from(&posture.budget);
    rimz::harness::launch::preflight_agent_process(
        &workspace.project_root,
        config.harness.rtk,
        &rimz::harness::launch::ExecRequest {
            kind: seed.kind.clone(),
            action: rimz::harness::launch::ExecAction::Fork {
                session_id: seed.source_session_id.to_string(),
                extra_args: posture.args.clone(),
            },
            system_prompt_file: posture.system_prompt_file.clone(),
            append_system_prompt_files: posture.append_system_prompt_files.clone(),
            provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            identity: rimz::harness::launch::ExecIdentity::default(),
        },
        &seed.cwd,
    )?;
    let placement = resolve_fork_placement(
        args.new_tab,
        args.new_pane,
        args.bg,
        rimz::mux::ambient_pane_id().is_some(),
    )?;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let room = RoomContext::from_resolved(workspace, config.clone(), mux, RoomSizing::OrdinaryTab)?;
    let backend = room.backend();
    rimz::room::require_live_session(backend, &workspace.session_name)?;

    let request = AgentLaunchRequest {
        kind: seed.kind.clone(),
        agent_id: mint_launch_id(),
        name: args
            .name
            .map_or(AgentLaunchName::Mint, AgentLaunchName::Explicit),
        launch: seed.launch.clone(),
        run_id: None,
        prompt: None,
    };
    let launch_batch = store.begin_agent_launch_batch(
        &[request],
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: seed.cwd.clone(),
            worktree_name: None,
            channel: channel.clone(),
            description: None,
        },
    )?;
    let launch = launch_batch.single_identity()?;
    let argv = rimz::harness::launch::exec_argv(
        &rimz::proc::rimz_exe(),
        &rimz::harness::launch::ExecRequest {
            kind: seed.kind.clone(),
            action: rimz::harness::launch::ExecAction::Fork {
                session_id: seed.source_session_id.to_string(),
                extra_args: posture.args,
            },
            system_prompt_file: posture.system_prompt_file.clone(),
            append_system_prompt_files: posture.append_system_prompt_files.clone(),
            provider_account: rimz::harness::launch::ProviderAccountState::Unbound,
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: placement != Placement::SamePane,
            exit_on_run_completion: false,
            identity: rimz::harness::launch::ExecIdentity {
                name: Some(launch.name.clone()),
                name_explicit: launch.name_explicit,
                launch_id: Some(launch.agent_id.to_string()),
                params: launch.launch.clone(),
            },
        },
    )?;
    let panes = LayoutPanes {
        columns: vec![LayoutColumn {
            panes: vec![PaneCmd { argv }],
            stacked: false,
        }],
    };
    let title = channel.as_deref().map_or_else(
        || rimz::harness::resume::build_label(seed.kind.as_str(), None, &seed.cwd),
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&seed.cwd, Vec::new(), None);
    let in_place = placement == Placement::SamePane;
    if in_place {
        report_fork(&seed, &source_name, &launch.name);
    }
    super::placement::execute(
        backend,
        store,
        &launch_batch,
        PlacementRequest {
            placement,
            mux,
            cwd: seed.cwd.clone(),
            title,
            panes,
            sidebar,
            identity_env: rimz::room::pane_identity_env(workspace, channel.as_deref(), false),
            background: args.bg,
            errors: PlacementErrors {
                new_tab: "opening agent fork tab",
                new_pane: "splitting the agent fork into a new pane",
                same_pane: "running the agent fork in the current pane",
            },
        },
    )?;
    if !in_place {
        report_fork(&seed, &source_name, &launch.name);
    }
    Ok(())
}

/// The posture this fork replays, from the same seam restart and resume use.
///
/// A fork stays on the source session's provider, so a profile that now
/// resolves to a different provider refuses here. Every other degrade is
/// carried on `degraded`; the caller prints it and the fork proceeds bare.
fn fork_posture(seed: &ForkSeed, profiles: &rimz::config::ProfilesConfig) -> Result<ResumePosture> {
    let posture = rimz::harness::resume::resolve_posture(
        rimz::harness::resume::PostureRequest {
            profile: seed.launch.profile.as_deref(),
            kind: &seed.kind,
            stamped_mode: seed.launch.mode,
        },
        profiles,
    );
    if let Some(reason @ PostureDegrade::KindChanged { .. }) = &posture.degraded {
        bail!("{reason}; a fork stays on the source provider — fix the profile and retry");
    }
    if let Some(reason @ PostureDegrade::PromptUnsupported { .. }) = &posture.degraded {
        bail!("{reason}");
    }
    Ok(posture)
}

fn resolve_fork_source(
    store: &rimz::Store,
    workspace: &rimz::ResolvedWorkspace,
    runtime: &rimz::RuntimePaths,
    snapshot: &rimz::SidebarSnapshot,
    reference: &str,
) -> Result<AgentState> {
    let current_channel = crate::cli::current_channel(workspace);
    match crate::cli::resolve_agent_one(snapshot, reference, None, current_channel.as_deref()) {
        Ok(agent) => Ok(agent.clone()),
        Err(live_err) => {
            match super::show::resolve_audit_agent(store, workspace, runtime, reference) {
                Ok(Some(agent)) => Ok(agent),
                Ok(None) => Err(live_err),
                Err(audit_err) => Err(audit_err),
            }
        }
    }
}

fn validate_fork_source(
    agent: &AgentState,
    session_backed: impl FnOnce(&AgentState) -> bool,
    worktree_exists: impl FnOnce(&Path) -> bool,
) -> Result<ForkSeed> {
    if agent.is_provider_subagent() {
        bail!(
            "agent `{}` is a subagent; fork its parent instead",
            agent.agent_id
        );
    }
    if agent.agent_id.is_empty() || agent.agent_id.is_provisional() {
        bail!(
            "agent `{}` has not registered a provider session yet; wait for it to start and retry",
            agent.agent_id
        );
    }
    let cwd = agent
        .worktree_path
        .as_deref()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "agent `{}` has no recorded worktree; start it in a RimZ room before forking",
                agent.agent_id
            )
        })?;
    if !worktree_exists(&cwd) {
        bail!(
            "agent `{}` worktree `{}` is gone; restore it or launch a fresh agent",
            agent.agent_id,
            cwd.display()
        );
    }
    if !session_backed(agent) {
        bail!(
            "agent `{}` conversation file is gone; restore it or launch a fresh agent",
            agent.agent_id
        );
    }
    Ok(ForkSeed {
        kind: agent.kind.clone(),
        source_session_id: agent.agent_id.clone(),
        cwd,
        launch: rimz::agents::LaunchParams {
            profile: agent.profile.clone(),
            mode: agent.mode,
            channel: agent.channel.clone(),
            ..rimz::agents::LaunchParams::default()
        },
    })
}

fn report_fork(seed: &ForkSeed, source_name: &str, new_name: &str) {
    let _ = writeln!(
        std::io::stderr(),
        "forked {}:{} ({}) → @{}",
        seed.kind,
        source_name,
        seed.source_session_id,
        new_name
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use rimz::agents::AgentStatus;
    use rimz::config::{Profile, ProfilesConfig};

    fn source(id: &str) -> AgentState {
        let mut agent = AgentState::stub("codex", id, AgentStatus::Success);
        agent.worktree_path = Some("/repo/worktree".to_owned());
        agent
    }

    fn seed(kind: &str, profile: Option<&str>, mode: Option<PermissionMode>) -> ForkSeed {
        ForkSeed {
            kind: AgentKind::new_unchecked(kind),
            source_session_id: AgentSessionId::from("session-1"),
            cwd: PathBuf::from("/repo/worktree"),
            launch: rimz::agents::LaunchParams {
                profile: profile.map(ToOwned::to_owned),
                mode,
                ..Default::default()
            },
        }
    }

    fn profiles(name: &str, profile: Profile) -> ProfilesConfig {
        ProfilesConfig(BTreeMap::from([(name.to_owned(), profile)]))
    }

    fn profile(agent: &str) -> Profile {
        toml::from_str(&format!("agent = {agent:?}")).expect("profile fixture")
    }

    #[test]
    fn fork_posture_replays_full_profile_argv() {
        let prompt = tempfile::NamedTempFile::new().expect("temp prompt file");
        let profiles = profiles(
            "planner",
            Profile {
                agent: "claude".to_owned(),
                mode: Some(PermissionMode::Yolo),
                model: Some("opus".to_owned()),
                effort: Some("high".to_owned()),
                system_prompt_file: Some(prompt.path().to_path_buf()),
                args: Some("--plugin-dir '/tmp/plugin dir'".to_owned()),
                ..profile("claude")
            },
        );

        let posture =
            fork_posture(&seed("claude", Some("planner"), None), &profiles).expect("posture");

        assert_eq!(
            posture.args,
            vec![
                "--model",
                "opus",
                "--effort",
                "high",
                "--dangerously-skip-permissions",
                "--plugin-dir",
                "/tmp/plugin dir",
            ]
        );
        assert_eq!(posture.system_prompt_file.as_deref(), Some(prompt.path()));
        assert_eq!(posture.mode, Some(PermissionMode::Yolo));
        assert_eq!(posture.model.as_deref(), Some("opus"));
        assert_eq!(posture.effort.as_deref(), Some("high"));
    }

    #[test]
    fn fork_posture_refuses_provider_change() {
        let profiles = profiles("planner", profile("codex"));

        let err = fork_posture(&seed("claude", Some("planner"), None), &profiles)
            .expect_err("provider change");

        assert!(
            err.to_string()
                .contains("a fork stays on the source provider")
        );
    }

    #[test]
    fn fork_posture_degrades_missing_profile_to_bare() {
        let seed = seed("claude", Some("retired"), Some(PermissionMode::Yolo));

        let posture =
            fork_posture(&seed, &ProfilesConfig::default()).expect("bare degraded posture");

        assert_eq!(posture.args, vec!["--dangerously-skip-permissions"]);
        assert!(matches!(
            posture.degraded,
            Some(PostureDegrade::Unresolved { .. })
        ));
    }

    #[test]
    fn fork_posture_keeps_bare_source_permission_argv() {
        let seed = seed("claude", None, Some(PermissionMode::Yolo));

        let posture = fork_posture(&seed, &ProfilesConfig::default()).expect("bare posture");

        assert_eq!(posture.args, vec!["--dangerously-skip-permissions"]);
        assert_eq!(posture.mode, Some(PermissionMode::Yolo));
        assert_eq!(posture.degraded, None);
    }

    #[test]
    fn fork_validation_refuses_subagents() {
        let mut agent = source("session-1");
        agent.parent_agent_id = Some(AgentSessionId::from("parent-1"));

        let err = validate_fork_source(&agent, |_| true, |_| true).expect_err("subagent");

        assert!(err.to_string().contains("fork its parent"));
    }

    #[test]
    fn fork_validation_refuses_provisional_sessions() {
        let agent = source("launch_123");

        let err = validate_fork_source(&agent, |_| true, |_| true).expect_err("provisional");

        assert!(err.to_string().contains("has not registered"));
    }

    #[test]
    fn fork_validation_refuses_missing_session_file() {
        let agent = source("session-1");

        let err = validate_fork_source(&agent, |_| false, |_| true).expect_err("session file");

        assert!(err.to_string().contains("conversation file is gone"));
    }

    #[test]
    fn fork_validation_refuses_missing_worktree() {
        let mut unrecorded = source("session-1");
        unrecorded.worktree_path = None;
        let err =
            validate_fork_source(&unrecorded, |_| true, |_| true).expect_err("recorded worktree");
        assert!(err.to_string().contains("no recorded worktree"));

        let missing = source("session-1");
        let err = validate_fork_source(&missing, |_| true, |_| false).expect_err("worktree path");
        assert!(
            err.to_string()
                .contains("worktree `/repo/worktree` is gone")
        );
    }

    #[test]
    fn fork_validation_inherits_lane_identity_and_drops_cohort_identity() {
        let mut agent = source("session-1");
        agent.profile = Some("planner".to_owned());
        agent.channel = Some("auth".to_owned());
        agent.team = Some("forge".to_owned());
        agent.role = Some("coder".to_owned());
        agent.mode = Some(rimz::harness::run::PermissionMode::Yolo);

        let seed = validate_fork_source(&agent, |_| true, |_| true).expect("valid fork");

        assert_eq!(seed.source_session_id, AgentSessionId::from("session-1"));
        assert_eq!(seed.cwd, PathBuf::from("/repo/worktree"));
        assert_eq!(seed.launch.profile.as_deref(), Some("planner"));
        assert_eq!(
            seed.launch.mode,
            Some(rimz::harness::run::PermissionMode::Yolo)
        );
        assert_eq!(seed.launch.channel.as_deref(), Some("auth"));
        assert_eq!(seed.launch.team, None);
        assert_eq!(seed.launch.role, None);
        assert_eq!(seed.launch.model, None);
        assert_eq!(seed.launch.effort, None);
    }
}
