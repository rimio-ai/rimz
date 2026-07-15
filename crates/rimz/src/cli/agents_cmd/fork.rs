//! Provider-native conversation forks launched as fresh Rimz agent rows.

use super::*;
use crate::cli::{machine_config, open_store};

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
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving the agent fork workspace")?;
    let store = open_store(&workspace)?;
    let runtime = rimz::RuntimePaths::for_workspace(workspace.workspace_id.clone())
        .context("preparing runtime paths")?;
    let snapshot = crate::cli::alive_snapshot(&store, &runtime, &workspace.session_name)?;
    let source = resolve_fork_source(&store, &workspace, &runtime, &snapshot, &args.reference)?;
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
    let adapter = rimz::agents::find_adapter(seed.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", seed.kind))?;
    rimz::harness::launch::preflight_agent_process(
        &workspace.project_root,
        config.harness.rtk,
        &rimz::harness::launch::ExecInvocation {
            kind: seed.kind.as_str(),
            action: rimz::harness::launch::ExecAction::Fork {
                session_id: seed.source_session_id.as_str(),
                extra_args: &[],
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: false,
            exit_on_run_completion: false,
            identity: rimz::harness::launch::ExecIdentity::default(),
        },
        &seed.cwd,
    )?;
    let scoped = channel.is_some()
        || (seed.cwd != workspace.project_root && seed.cwd != workspace.worktree_root);
    let placement = apply_in_place_downgrade(
        resolve_placement(
            args.new_tab,
            args.new_pane,
            config.agents.placement,
            scoped,
            true,
            rimz::mux::ambient_pane_id().is_some(),
        )?,
        args.bg,
        true,
    );
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let room =
        RoomContext::from_resolved(&workspace, config.clone(), mux, RoomSizing::OrdinaryTab)?;
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
    let permission_args = launch
        .launch
        .mode
        .map(|mode| adapter.permission_args(mode))
        .unwrap_or_default();
    let argv = rimz::harness::launch::exec_argv(
        &rimz::proc::rimz_exe(),
        &rimz::harness::launch::ExecInvocation {
            kind: seed.kind.as_str(),
            action: rimz::harness::launch::ExecAction::Fork {
                session_id: seed.source_session_id.as_str(),
                extra_args: &permission_args,
            },
            run_id: None,
            worktree_path: None,
            close_pane_on_exit: placement != Placement::SamePane,
            exit_on_run_completion: false,
            identity: rimz::harness::launch::ExecIdentity {
                name: Some(launch.name.as_str()),
                name_explicit: launch.name_explicit,
                launch_id: Some(launch.agent_id.as_str()),
                profile: launch.launch.profile.as_deref(),
                mode: launch.launch.mode,
                channel: channel.as_deref(),
                ..rimz::harness::launch::ExecIdentity::default()
            },
        },
    );
    let panes = LayoutPanes {
        columns: vec![LayoutColumn {
            panes: vec![PaneCmd { argv: argv.clone() }],
            stacked: false,
        }],
    };
    let title = channel.as_deref().map_or_else(
        || rimz::harness::resume::build_label(seed.kind.as_str(), None, &seed.cwd),
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&seed.cwd, Vec::new(), None);
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let (open_result, what): (Result<()>, &str) = match placement {
        Placement::NewTab => (
            backend
                .open_tab(&TabOptions {
                    session_name: workspace.session_name.clone(),
                    title,
                    cwd: seed.cwd.clone(),
                    panes,
                    focus: !args.bg,
                    dock_sidebar: true,
                    sidebar,
                })
                .map_err(Into::into),
            "opening agent fork tab",
        ),
        Placement::NewPane => (
            backend
                .split_pane(SplitPaneOptions {
                    session_name: None,
                    target_view_id: None,
                    target_pane_id: own_pane_id(mux),
                    cwd: Some(seed.cwd.to_string_lossy().into_owned()),
                    command: Some(argv.clone()),
                    title: None,
                    env: rimz::room::pane_identity_env(&workspace, channel.as_deref(), false),
                    stacked: false,
                    direction,
                    focus: !args.bg,
                })
                .map_err(Into::into),
            "splitting the agent fork into a new pane",
        ),
        Placement::SamePane => {
            report_fork(&seed, &source_name, &launch.name);
            let err = exec_wrapper_in_place(
                &argv,
                rimz::room::pane_identity_env(&workspace, channel.as_deref(), false),
                &seed.cwd,
            );
            (Err(err), "running the agent fork in the current pane")
        }
    };
    if let Err(err) = open_result {
        let _ = store.fail_agent_launch_batch(&launch_batch);
        return Err(err).context(what);
    }
    report_fork(&seed, &source_name, &launch.name);
    Ok(())
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
    if agent.parent_agent_id.is_some() {
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
                "agent `{}` has no recorded worktree; start it in a Rimz room before forking",
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

    fn source(id: &str) -> AgentState {
        let mut agent = AgentState::stub("codex", id, AgentStatus::Success);
        agent.worktree_path = Some("/repo/worktree".to_owned());
        agent
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
