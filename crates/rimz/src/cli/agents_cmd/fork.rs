//! Provider-native conversation forks launched as fresh RimZ agent rows.

use super::*;
use crate::cli::machine_config;
use rimz::config::effective::ProfileScope;
use rimz::harness::ancestry::{LaunchFocus, resolve_caller, resolve_launch_caller};

use super::placement::{PlacementErrors, PlacementRequest};

#[derive(Debug, Args)]
pub(super) struct ForkArgs {
    /// Existing live or stopped agent to fork: @handle, @handle#lane, pet name, or pane id (tmux:%12).
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
    /// Run the fork under this isolation instead of the source agent's.
    #[arg(long, value_name = "host|scoped|sandbox")]
    pub(super) isolation: Option<rimz::config::Isolation>,
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
    let caller = resolve_caller(&snapshot.agents);
    let focus = LaunchFocus::resolve(args.bg, caller.as_ref());
    let launch_caller = caller
        .as_ref()
        .map(|caller| resolve_launch_caller(&snapshot.agents, caller))
        .transpose()?;
    let source = resolve_fork_source(store, workspace, ctx.runtime(), &snapshot, &args.reference)?;
    if source.checkout.is_some() {
        let cwd = source
            .worktree_path
            .as_deref()
            .context("empty-dir agent has no recorded working directory")?;
        rimz::agents::empty_dir::prepare_reentry(store.paths(), Path::new(cwd))?;
    }
    let logins = rimz::agents::room_accounts(
        &store.paths().workspace_record,
        Some(&workspace.project_root),
        &rimz::config::MachineConfig::load_lenient(),
    )?;
    let catalog = rimz::agents::machine_login_catalog();
    let mut seed = validate_fork_source(
        &source,
        launch_caller,
        &logins,
        &catalog,
        rimz::harness::resume::resume_session_present,
        Path::is_dir,
    )?;
    let source_name = source.name.as_deref().unwrap_or("unnamed").to_owned();
    let channel = seed
        .launch
        .lane
        .as_ref()
        .and_then(rimz::ids::Lane::pin_name)
        .map(String::from);

    if let Some(name) = args.name.as_deref() {
        validate_agent_name(name)?;
    }
    let config = machine_config();
    let adapter = rimz::agents::find_definition(seed.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", seed.kind))?;
    let effective = rimz::config::effective::load(&config, &workspace.project_root)?;
    effective.block_set_failure()?;
    let posture = fork_posture(&seed, &effective)?;
    if args.isolation.is_some() {
        seed.launch.isolation = args.isolation;
    }
    let isolation = rimz::config::Isolation::resolve(
        seed.launch.isolation,
        posture.launch.isolation_default,
        config.agents.isolation,
    );
    rimz::sandbox::preflight_launch(
        isolation,
        &seed.kind,
        posture.launch.skills.is_some(),
        adapter.manual_skill(),
    )?;
    if let Some(reason) = &posture.degraded {
        writeln!(
            crate::cli::render::err(),
            "rimz: {reason}; forking bare {}",
            seed.kind
        )?;
    }
    seed.launch.mode = posture.launch.mode;
    seed.launch.model.clone_from(&posture.launch.model);
    seed.launch.effort.clone_from(&posture.launch.effort);
    seed.launch.fast = posture.launch.fast;
    seed.launch.budget.clone_from(&posture.launch.budget);
    seed.launch.tier.clone_from(&posture.launch.tier);
    seed.launch.record = Some(rimz::agents::LaunchRecord::replay_or_new(
        posture.launch.record.as_deref(),
        posture.launch.model.as_deref(),
        posture.launch.effort.as_deref(),
        posture.launch.fast,
    ));
    let mut preflight = posture.launch.exec_request(
        seed.kind.clone(),
        rimz::harness::launch::ExecAction::Fork {
            session_id: seed.source_session_id.to_string(),
            extra_args: Vec::new(),
        },
    );
    preflight.identity.params.clone_from(&seed.launch);
    rimz::harness::launch::preflight_agent_process(
        &workspace.project_root,
        &preflight,
        &seed.cwd,
        (!isolation.is_scoped()).then_some(store.runtime_paths()),
    )?;
    let login =
        rimz::agents::session_login(&seed.kind, seed.launch.login.as_ref(), &config.accounts)?;
    crate::cli::launch_interaction::preflight_hooks(
        login.health(&rimz::agents::ambient_env()),
        || {
            caller
                .as_ref()
                .map(|_| rimz::launcher::trust::Unattended::Agent)
        },
        || {
            rimz::config::definitions::hook_trust_fix(
                &source.kind,
                source.profile.as_deref(),
                Some((&source, "fork")),
            )
        },
        &mut Default::default(),
        &mut crate::cli::render::err(),
    )?;
    let repo_root = if seed.launch.checkout.is_none() {
        workspace.at_launch_dir(&seed.cwd)?.cwd_project_root
    } else {
        None
    };
    crate::cli::launch_interaction::preflight(
        seed.kind.as_str(),
        &seed.cwd,
        adapter.folder_trust(
            &seed.cwd,
            repo_root.as_deref(),
            &login.env(&rimz::agents::ambient_env()),
        ),
        None,
    )?;
    let placement = resolve_fork_placement(
        args.new_tab,
        args.new_pane,
        focus,
        rimz::mux::ambient_pane_id().is_some(),
    )?;
    let room = RoomContext::live_tab(workspace, config.clone(), globals.mux)?;
    let mux = room.mux_name();
    let backend = room.backend();

    let request = AgentLaunchRequest {
        login: rimz::store::writer::LaunchLogin::RoomDefault,
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
            branch: None,
            description: None,
        },
    )?;
    let launch = launch_batch.single_identity()?;
    let argv = rimz::harness::launch::exec_argv(
        &rimz::proc::rimz_exe(),
        store.runtime_paths(),
        &rimz::harness::launch::ExecRequest {
            close_pane_on_exit: placement != Placement::SamePane,
            identity: rimz::harness::launch::ExecIdentity {
                resume_model_override: false,
                name: Some(launch.name.clone()),
                name_explicit: launch.name_explicit,
                launch_id: Some(launch.agent_id.to_string()),
                params: launch.launch.clone(),
            },
            ..posture.launch.exec_request(
                seed.kind.clone(),
                rimz::harness::launch::ExecAction::Fork {
                    session_id: seed.source_session_id.to_string(),
                    extra_args: Vec::new(),
                },
            )
        },
    )?;
    let (title, pane_name) = fork_tab_names(&seed, channel.as_deref(), launch);
    let panes = LayoutPanes {
        columns: vec![LayoutColumn {
            panes: vec![PaneCmd {
                argv,
                name: Some(pane_name),
            }],
            stacked: false,
        }],
        focused_pane: 0,
    };
    let sidebar = room.sidebar_options(&seed.cwd);
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
            tab_anchor: super::placement::resolve_tab_anchor(
                placement,
                false,
                rimz::mux::own_pane_id(mux),
                crate::cli::pinned_room_root().as_deref(),
                &workspace.project_root,
            ),
            cwd: seed.cwd.clone(),
            title,
            panes,
            sidebar,
            identity_env: rimz::room::pane_identity_env(
                workspace,
                source.lane_path().map(Path::new).unwrap_or(&seed.cwd),
                channel.as_deref(),
            ),
            focus,
            errors: PlacementErrors {
                new_tab: "opening agent fork tab",
                new_pane: "splitting the agent fork into a new pane",
                same_pane: "running the agent fork in the current pane",
            },
        },
        // A fork carries its source's tier stamp; the fallback was recorded at that launch.
        || {},
    )?;
    if !in_place {
        report_fork(&seed, &source_name, &launch.name);
    }
    Ok(())
}

/// The fork's tab keeps its short label; the pane identifies the new agent.
fn fork_tab_names(
    seed: &ForkSeed,
    channel: Option<&str>,
    launch: &AgentLaunchIdentity,
) -> (String, rimz::pane::PaneTitle) {
    let title = channel.map_or_else(
        || {
            rimz::agents::pane_title::short_label(seed.launch.profile.as_deref(), &seed.kind)
                .to_owned()
        },
        |channel| format!("#{channel}"),
    );
    let pane_name = rimz::agents::pane_title::title(
        rimz::agents::pane_title::Seat::from_launch(
            &launch.kind,
            Some(&launch.name),
            &launch.launch,
        ),
        &[],
    );
    (title, pane_name)
}

/// The posture this fork replays, from the same seam restart and resume use.
///
/// A fork stays on the source session's provider, so a profile that now
/// resolves to a different provider refuses here. Every other degrade is
/// carried on `degraded`; the caller prints it and the fork proceeds bare.
fn fork_posture(
    seed: &ForkSeed,
    effective: &rimz::config::effective::LaunchAgents,
) -> Result<ResumePosture> {
    let scope = ProfileScope::for_launch(seed.launch.profile_scope, false);
    let profiles = effective.profiles_for_launch(scope, seed.launch.profile.as_deref());
    effective.block_failed_launch(
        &seed.kind,
        seed.launch.profile.as_deref(),
        seed.launch.record.as_deref(),
        profiles,
    )?;
    let posture = rimz::harness::resume::resolve_posture(
        rimz::harness::resume::PostureRequest {
            record: seed.launch.record.as_deref(),
            profile: seed.launch.profile.as_deref(),
            kind: &seed.kind,
            stamped_mode: seed.launch.mode,
            stamped_tier: seed.launch.tier.as_deref(),
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
    snapshot: &rimz::store::snapshot::SidebarSnapshot,
    reference: &str,
) -> Result<AgentState> {
    let current_channel = crate::cli::current_channel(workspace, Some(store));
    resolve_live_or_audit(
        store,
        workspace,
        runtime,
        snapshot,
        reference,
        &current_channel.address_context(),
    )
    .map(|(agent, _)| agent)
}

fn validate_fork_source(
    agent: &AgentState,
    caller: Option<&AgentState>,
    logins: &rimz::agents::RoomAccounts,
    catalog: &rimz::agents::LoginCatalog,
    session_backed: impl FnOnce(&AgentState) -> bool,
    worktree_exists: impl FnOnce(&Path) -> bool,
) -> Result<ForkSeed> {
    let login = rimz::harness::resume::relaunch_login(agent, logins, catalog)?;
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
    let scope = ProfileScope::for_launch(agent.profile_scope, agent.is_launched_child());
    let mut record = agent.record.clone();
    if let Some(record) = &mut record {
        record.notes = None;
        record.loop_section = None;
    }
    Ok(ForkSeed {
        kind: agent.kind.clone(),
        source_session_id: agent.agent_id.clone(),
        cwd,
        launch: rimz::agents::LaunchParams {
            checkout: agent.checkout.clone(),
            add_dirs: agent.add_dirs.clone(),
            untrusted_checkout: agent.untrusted_checkout,
            profile: agent.profile.clone(),
            profile_scope: (scope == ProfileScope::Subagents).then_some(scope),
            login,
            record,
            tier: agent.tier.clone(),
            mode: agent.mode,
            isolation: agent.isolation,
            automation: caller.and_then(|caller| caller.automation.clone()),
            // The fork continues the source's place in an agent-launch
            // chain: it copies the source's generation, whoever runs the
            // command, so a fork never restarts the chain at 0. It is not a
            // peer launch, so it neither counts a new generation nor names a
            // launcher.
            launch_depth: agent.launch_depth,
            lane: agent.lane.clone(),
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
mod tests;
