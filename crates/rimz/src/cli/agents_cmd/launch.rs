//! Interactive launch orchestration and presentation.

use super::*;
use crate::cli::ctx::Ctx;
use crate::cli::{machine_config, render, report_unknown_config_keys};
use rimz::harness::ancestry::{self, LaunchFocus};
use std::collections::HashSet;

use super::placement::{PlacementErrors, PlacementRequest};

const LAUNCH_PLACEMENT_ERRORS: PlacementErrors = PlacementErrors {
    new_tab: "opening agent tab",
    new_pane: "splitting the agent into a new pane",
    same_pane: "running the agent in the current pane",
};

pub(super) enum ResumeEntrance {
    Flag,
    Reconcile,
}

pub(super) fn validate_resume_inputs(
    args: &AgentLaunchArgs,
    entrance: ResumeEntrance,
) -> Result<()> {
    let overrides = &args.overrides;
    let (input, message) = if overrides.system_prompt_file.is_some() {
        (
            "--system-prompt-file",
            "resume takes system-prompt files from the current profile; update the profile or launch fresh instead of passing `--system-prompt-file`",
        )
    } else if !overrides.append_system_prompt_files.is_empty() {
        (
            "--append-system-prompt-file",
            "resume takes system-prompt files from the current profile; update the profile or launch fresh instead of passing `--append-system-prompt-file`",
        )
    } else if !overrides.passthrough.is_empty() {
        (
            "passthrough arguments",
            "resume does not accept passthrough arguments after `--`; put supported settings in the profile or launch fresh",
        )
    } else {
        return Ok(());
    };
    match entrance {
        ResumeEntrance::Flag => bail!("{message}"),
        ResumeEntrance::Reconcile => {
            bail!("{message}; rerun without {input}, or choose fresh instead of resume")
        }
    }
}

pub(super) fn launch_layout(
    args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
) -> Result<()> {
    if args.launch.cohort.resume {
        validate_resume_inputs(&args.launch, ResumeEntrance::Flag)?;
    }
    if args.launch.cohort.fresh
        && args
            .launch
            .cohort
            .worktree
            .as_deref()
            .is_none_or(|name| name.trim().is_empty())
    {
        bail!("--fresh needs a named worktree (-w NAME) whose prior cohort it replaces");
    }
    let machine_config = machine_config();
    if args.launch.cohort.worktree.is_some() || args.launch.cohort.from_pr.is_some() {
        crate::cli::require_worktree_config(&machine_config)?;
    }
    crate::cli::check_launch_room(globals)?;
    let ctx = Ctx::open(globals)?;
    let cwd = crate::cli::resolve_launch_cwd(args.launch.cwd.as_deref(), ctx.store.paths())?;
    if let Some(launched) = launch_resolved(
        args.launch,
        globals,
        allow_in_place,
        &ctx,
        machine_config,
        cwd,
        None,
    )? {
        launched.write_receipt(&mut render::out(), &ctx.store, &ctx.workspace.project_root)?;
    }
    Ok(())
}

pub(in crate::cli) struct LaunchedLayout {
    pub(in crate::cli) identities: Vec<AgentLaunchIdentity>,
    pub(in crate::cli) leader_index: Option<usize>,
    team: Option<(String, rimz::config::Team)>,
    channel: Option<String>,
    cwd: PathBuf,
    in_place: bool,
    peer: Option<rimz::store::run::RunRecord>,
}

impl LaunchedLayout {
    fn write_receipt(
        &self,
        w: &mut impl Write,
        store: &rimz::Store,
        project_root: &Path,
    ) -> Result<()> {
        if self.in_place {
            return Ok(());
        }
        let paths = store.paths();
        let lane_label = launched_lane_label(self.channel.as_deref(), &self.cwd, project_root);
        write_launch_receipt(
            w,
            &LaunchReceipt {
                team: self.team.as_ref().map(|(name, team)| (name.as_str(), team)),
                channel: lane_label.as_deref(),
                cwd: &self.cwd,
                identities: &self.identities,
                leader_index: self.leader_index,
                terminal_width: render::terminal_columns(100),
                hints: reported_peer(self.peer.as_ref(), paths).is_none(),
            },
        )?;
        write_peer_receipt(
            w,
            &self.identities,
            self.peer.as_ref(),
            self.channel.as_ref().and(lane_label.as_deref()),
            paths,
        )
    }
}

fn launched_lane_label(channel: Option<&str>, cwd: &Path, project_root: &Path) -> Option<String> {
    if rimz::agents::AgentState::is_root_lane(channel, Some(cwd), Some(project_root)) {
        return Some("main".to_owned());
    }
    channel
        .filter(|channel| !channel.is_empty())
        .map(str::to_owned)
        .or_else(|| cwd.file_name()?.to_str().map(str::to_owned))
}

/// Launch into an already-resolved host cwd without changing its lexical identity.
pub(in crate::cli) fn launch_resolved(
    launch: AgentLaunchArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
    ctx: &Ctx,
    machine_config: Arc<rimz::config::MachineConfig>,
    cwd: Option<PathBuf>,
    // The resident loop fire's task name and its `### Loop` reminder body.
    loop_fire: Option<(&str, &str)>,
) -> Result<Option<LaunchedLayout>> {
    let loop_task = loop_fire.map(|(task, _)| task);
    let mut args = AgentsArgs {
        launch,
        ..Default::default()
    };
    let explicit_worktree_name = args
        .launch
        .cohort
        .worktree
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| rimz::worktree::parse_requested_name(name).map(|requested| requested.name))
        .transpose()?;
    let worktree_launch =
        args.launch.cohort.worktree.is_some() || args.launch.cohort.from_pr.is_some();
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    report_unknown_config_keys(&machine_config)?;
    let effective = rimz::config::effective::load(&machine_config, &workspace.project_root)?;
    if !args.launch.cohort.resume {
        machine_config.agents.startup_relaunch_wait()?;
    }
    let default_lane = if loop_task.is_some() {
        crate::cli::directory_channel(
            workspace,
            cwd.as_deref().unwrap_or(&workspace.worktree_root),
        )
    } else {
        ctx.channel().map(str::to_owned)
    };
    let lane = args
        .launch
        .cohort
        .channel
        .as_deref()
        .or(default_lane.as_deref());
    let snapshot = if args.launch.spec.is_some() && lane.is_some() {
        Some(ctx.cached_snapshot()?)
    } else {
        None
    };
    let mut matching_overrides = args.launch.overrides.clone();
    matching_overrides.agent = None;
    matching_overrides.tier = None;
    let FinalizedLaunch {
        profiles: _,
        resolved,
        preset: _preset,
        warnings,
        inferred_lane,
        qualified_spec,
    } = resolve_finalized_layout(
        snapshot.as_ref(),
        &machine_config,
        &effective,
        args.launch.spec.as_deref(),
        args.launch
            .prompt
            .as_deref()
            .filter(|_| !args.launch.cohort.resume),
        if args.launch.cohort.resume {
            &matching_overrides
        } else {
            &args.launch.overrides
        },
        args.launch.cohort.budget,
        args.launch.max_turns,
        lane,
        args.launch.name.is_some(),
        Some((store.runtime_paths(), store.paths())),
    )
    .inspect_err(|err| {
        if let Some(err) = err.downcast_ref::<rimz::harness::plan::LaunchFinalizeError>() {
            for warning in err.warnings() {
                let _ = writeln!(std::io::stderr(), "{warning}");
            }
        }
    })?;
    if let Some(spec) = qualified_spec {
        args.launch.spec = Some(spec);
    }
    for warning in &warnings {
        writeln!(std::io::stderr(), "{warning}")?;
    }
    let ResolvedLaunch {
        teams,
        layout,
        team_name,
    } = resolved;
    let team = team_name.as_deref().and_then(|name| teams.0.get(name));
    if !args.launch.cohort.resume
        && args.launch.cohort.from_pr.is_none()
        && let Some(name) = team_name.as_deref()
        && let Some(team) = team
    {
        let mut launch_workspace = workspace.clone();
        if loop_task.is_some()
            && let Some(cwd) = &cwd
        {
            launch_workspace.worktree_root = cwd.clone();
        }
        rimz::harness::schedule::team::validate_launch(
            name,
            team,
            &launch_workspace,
            explicit_worktree_name.as_deref(),
        )?;
    }
    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let caller = if loop_task.is_some() {
        None
    } else {
        ancestry::resolve_caller(&projection.agents)
    };
    let focus = LaunchFocus::resolve(args.launch.cohort.bg, caller.as_ref());
    let ancestry = ancestry::resolve_launch_ancestry(
        caller
            .as_ref()
            .map(|caller| ancestry::resolve_launch_caller(&projection.agents, caller))
            .transpose()?,
        false,
        machine_config.agents.max_chain_length,
    )?;
    let prompt = args
        .launch
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty());
    let leader = rimz::harness::spec::prompt_leader(&layout, team);
    let receipt_leader_index = if prompt.is_some() {
        Some(leader?)
    } else {
        leader.ok()
    };
    let prompt_agent_index = prompt.and(receipt_leader_index);
    let mut checked_folder_trust = HashSet::new();
    let mut preflighted_logins = Vec::new();
    if !args.launch.cohort.resume {
        crate::cli::admit_launch_worktree_name(
            workspace,
            &machine_config.agents.worktree,
            &projection.agents,
            explicit_worktree_name.as_deref(),
        )?;
    }
    // Resolve where the launch lands before any side effect — the live-session
    // probe, worktree creation, the store append, the sidebar build — so an
    // invalid `--new-pane` (a multi-cell layout, or run outside a room) refuses
    // cleanly and leaves no provisional rows or worktree behind. Feasibility
    // reads the ambient pane; the split target is re-derived for the resolved
    // backend below.
    let single_cell = layout
        .columns
        .iter()
        .map(|column| column.rows.len())
        .sum::<usize>()
        == 1;
    if args.launch.cohort.resume {
        let worktree_filter = resume_worktree_scope(
            args.launch.cohort.worktree.as_deref(),
            workspace,
            &machine_config,
        )?;
        return launch_resume_layout(
            args,
            globals,
            allow_in_place,
            ctx,
            &machine_config,
            &teams,
            &effective.profiles,
            layout,
            team_name,
            single_cell,
            worktree_filter.as_deref(),
            ancestry.as_ref(),
            focus,
            checked_folder_trust,
        )
        .map(|()| None);
    }
    let channel_launch = args.launch.cohort.channel.is_some();
    let placement = apply_in_place_downgrade(
        resolve_placement(
            args.launch.cohort.new_tab,
            args.launch.new_pane,
            machine_config.agents.placement,
            worktree_launch || channel_launch,
            single_cell,
            rimz::mux::ambient_pane_id().is_some(),
        )?,
        focus,
        allow_in_place,
    );
    let in_place = placement == Placement::SamePane;
    if !args.launch.cohort.resume {
        for (index, cell) in layout.agent_cells().enumerate() {
            preflighted_logins.push(preflight_cell(
                workspace,
                store.runtime_paths(),
                cell,
                rimz::config::Isolation::resolve(
                    cell.launch.isolation,
                    cell.isolation_default,
                    machine_config.agents.isolation,
                ),
                prompt.filter(|_| Some(index) == prompt_agent_index),
                &mut checked_folder_trust,
            )?);
        }
    }
    let room = RoomContext::live_tab(workspace, machine_config.clone(), globals.mux)?;
    let mux = room.mux_name();
    let backend = room.backend();
    if worktree_launch && !crate::cli::confirm_cross_repo_worktree(workspace)? {
        return Ok(None);
    }

    let cells = cohort_cells(&layout);
    // A choice-less PR lookup discovers the holder without moving its local tip.
    let mut checkout = args.launch.cohort.from_pr.as_ref().map(|pr| {
        rimz::worktree::resolve_launch_checkout(
            workspace,
            &machine_config.agents.worktree,
            args.launch.cohort.worktree.as_deref(),
            Some(pr),
            None,
            cwd.as_deref(),
        )
    });
    let cohort_target = if team_name.is_some() || cells.len() >= 2 {
        match checkout.as_ref() {
            Some(Ok(launch)) if launch.reused => launch
                .worktree_name
                .as_ref()
                .map(|name| (name.clone(), launch.cwd.clone())),
            // Agents record the marker's lexical path; Git lists the holder realpath'd.
            Some(Err(rimz::worktree::WorktreeErr::PrBranchDiverged {
                holder: Some(path), ..
            })) => rimz::worktree::read_marker_for_worktree(path)?
                .map(|marker| (marker.name, marker.worktree_path)),
            Some(_) => None,
            None => explicit_worktree_name
                .as_deref()
                .map(|name| {
                    reconcile::cohort_worktree_path(
                        workspace,
                        &machine_config.agents.worktree,
                        name,
                    )
                    .map(|path| (name.to_owned(), path))
                })
                .transpose()?,
        }
    } else {
        None
    };
    // An inferred lane joins the exact channel it was inferred from, rather than
    // one recomputed from the caller's cwd — a shell pane that has `cd`'d into a
    // subdirectory would otherwise stamp that subdirectory's basename.
    let explicit_channel = args
        .launch
        .cohort
        .channel
        .as_deref()
        .or(inferred_lane.as_deref().filter(|_| cwd.is_none()));
    if let Some(team) = team_name.as_deref() {
        let target = if let Some(path) = cwd.as_deref() {
            Some((None, path))
        } else if let Some((name, path)) = cohort_target.as_ref() {
            Some((Some(name.as_str()), path.as_path()))
        } else {
            match checkout.as_ref() {
                Some(Ok(launch)) => Some((launch.worktree_name.as_deref(), launch.cwd.as_path())),
                Some(Err(rimz::worktree::WorktreeErr::PrBranchDiverged {
                    holder: Some(path),
                    ..
                })) => Some((None, path.as_path())),
                Some(Err(_)) => None,
                // A generated worktree has no prior occupants.
                None if args.launch.cohort.worktree.is_some() => None,
                None => Some((None, workspace.worktree_root.as_path())),
            }
        };
        if let Some((name, path)) = target
            && let Some(hold) = rimz::harness::resume::inspect_team_hold(&projection.agents, path)
            && hold.team != team
        {
            let (reason, release, resume) = team_hold_guidance(&hold, path, name);
            let place = match name {
                Some(name) => format!("worktree `{name}`"),
                None => format!("checkout `{}`", path.display()),
            };
            bail!(
                "{place} already holds team `{}` ({reason}); one team per checkout: resume it with {resume}, or launch into another worktree{release}",
                hold.team
            );
        }
        if let Some((_, path)) = target
            && let Some(channel) = rimz::harness::spec::resolve_room_channel(
                &workspace.project_root,
                path,
                Some(team),
                explicit_channel,
            )
            && let Some(hold) =
                rimz::harness::resume::inspect_channel_hold(&projection.agents, &channel)
        {
            let comparable_path = |path: &Path| {
                std::fs::canonicalize(path)
                    .unwrap_or_else(|_| rimz::utils::path::normalize_path_lexical(path))
            };
            if comparable_path(&hold.checkout) != comparable_path(path) {
                let marker = rimz::worktree::read_marker_for_worktree(&hold.checkout)?;
                let (reason, release, resume) = team_hold_guidance(
                    &hold,
                    &hold.checkout,
                    marker.as_ref().map(|marker| marker.name.as_str()),
                );
                bail!(
                    "channel `#{channel}` already carries team `{}` at checkout `{}` ({reason}); one team per channel: resume it with {resume}, or launch on another channel{release}",
                    hold.team,
                    hold.checkout.display()
                );
            }
        }
    }
    if let Some((name, path)) = cohort_target {
        let spec_display = args.launch.spec.as_deref().unwrap_or("<spec>");
        match reconcile::reconcile_cohort_launch(
            workspace,
            &machine_config,
            backend,
            store,
            &name,
            &path,
            spec_display,
            team_name.as_deref(),
            &cells,
            args.launch.cohort.fresh,
            focus,
        )? {
            reconcile::Reconciled::Done => return Ok(None),
            reconcile::Reconciled::Resume(path) => {
                validate_resume_inputs(&args.launch, ResumeEntrance::Reconcile)?;
                let layout = resolve_finalized_layout(
                    snapshot.as_ref(),
                    &machine_config,
                    &effective,
                    args.launch.spec.as_deref(),
                    None,
                    &matching_overrides,
                    args.launch.cohort.budget,
                    args.launch.max_turns,
                    lane,
                    false,
                    Some((store.runtime_paths(), store.paths())),
                )?
                .resolved
                .layout;
                if let Some(checkout) = checkout.take()
                    && crate::cli::settle_launch_checkout(
                        workspace,
                        &machine_config.agents.worktree,
                        args.launch.cohort.worktree.as_deref(),
                        args.launch.cohort.from_pr.as_ref(),
                        cwd.as_deref(),
                        checkout,
                    )?
                    .is_none()
                {
                    return Ok(None);
                }
                return launch_resume_layout(
                    args,
                    globals,
                    allow_in_place,
                    ctx,
                    &machine_config,
                    &teams,
                    &effective.profiles,
                    layout,
                    team_name,
                    single_cell,
                    Some(&path),
                    ancestry.as_ref(),
                    focus,
                    checked_folder_trust,
                )
                .map(|()| None);
            }
            reconcile::Reconciled::Continue => {}
            reconcile::Reconciled::Removed => checkout = None,
        }
    }

    let launch = match checkout {
        Some(checkout) => crate::cli::settle_launch_checkout(
            workspace,
            &machine_config.agents.worktree,
            args.launch.cohort.worktree.as_deref(),
            args.launch.cohort.from_pr.as_ref(),
            cwd.as_deref(),
            checkout,
        ),
        None => crate::cli::resolve_launch_checkout(
            workspace,
            &machine_config,
            args.launch.cohort.worktree.as_deref(),
            args.launch.cohort.from_pr.as_ref(),
            cwd.as_deref(),
        ),
    }?;
    let Some(launch) = launch else {
        return Ok(None);
    };
    if let Some(team) = team {
        rimz::worktree::exclude_team_scratch(&launch.cwd, &team.scratch_patterns());
    }
    crate::cli::lsp_admission::admit(&launch.cwd, &machine_config)?;
    if let Some(channel) = args.launch.cohort.channel.as_deref() {
        rimz::channel::admit_launch(workspace, channel)?;
    }
    let room_channel = rimz::harness::spec::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        team_name.as_deref(),
        explicit_channel,
    );
    let mut launch_requests = launch_identity_requests(
        &layout,
        args.launch.name.as_deref(),
        launch.generated_name(),
        team_name.as_deref(),
        team.map(|team| team.roles.as_slice()),
        room_channel.as_deref(),
        prompt.zip(prompt_agent_index),
        None,
        ancestry.as_ref(),
    )?;
    for (request, login) in launch_requests.iter_mut().zip(preflighted_logins) {
        request.login = rimz::store::writer::LaunchLogin::Pinned(login);
    }
    if let Some(index) = prompt_agent_index {
        launch_requests[index].launch.loop_task = loop_task.map(str::to_owned);
    }
    let launch_batch = store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: launch.cwd.clone(),
            branch: launch.branch.clone(),
            description: args.launch.cohort.description.clone(),
        },
    )?;
    let peer_prompt = prepare_peer_prompt(
        store,
        launch_batch.identities(),
        &launch.cwd,
        super::report_to(args.launch.cohort.detach),
    )
    .inspect_err(|_| {
        let _ = store.fail_agent_launch_batch(&launch_batch);
    })?;
    let fail_peer_prompt = || {
        if let Some((peer, _)) = &peer_prompt {
            match rimz::harness::run::fail_peer_run(store, peer, "peer launch failed") {
                Ok(Some(_)) => {
                    if let Some((_, launcher)) = peer.launcher() {
                        rimz::harness::orphan_sweep::spawn_digest_helper(
                            store.runtime_paths(),
                            launcher.clone(),
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "could not fail peer launch run"),
            }
        }
    };
    let cleanup_worktree = launch.owns_checkout_lifecycle();
    let worktree_name = launch.worktree_name.clone();
    let cwd = launch.cwd;
    let title = room_channel.as_deref().map_or_else(
        || {
            rimz::harness::spec::default_tab_title(
                &layout,
                worktree_name.as_deref(),
                team_name.as_deref(),
            )
        },
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&cwd);
    let mut panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            runtime: store.runtime_paths(),
            cwd: &cwd,
            cleanup_worktree,
            in_place,
            resume_seeds: None,
            launch_identities: launch_batch.identities(),
            fallback_channel: None,
            loop_reminder: loop_fire.map(|(_, reminder)| reminder),
        },
    )
    .inspect_err(|_| {
        let _ = store.fail_agent_launch_batch(&launch_batch);
        fail_peer_prompt();
    })?;
    panes.focused_pane = team_leader_pane(&layout, team);
    super::placement::execute(
        backend,
        store,
        &launch_batch,
        PlacementRequest {
            placement,
            mux,
            cwd: cwd.clone(),
            title,
            panes,
            sidebar,
            identity_env: rimz::room::pane_identity_env(
                workspace,
                &cwd,
                room_channel.as_deref(),
                !worktree_launch && loop_task.is_none(),
            ),
            focus,
            errors: LAUNCH_PLACEMENT_ERRORS,
        },
        || rimz::harness::assist_log::record_tier_fallbacks(launch_batch.identities()),
    )
    .inspect_err(|_| fail_peer_prompt())?;
    Ok(Some(LaunchedLayout {
        identities: launch_batch.identities().to_vec(),
        leader_index: receipt_leader_index,
        team: team_name
            .as_deref()
            .zip(team)
            .map(|(name, team)| (name.to_owned(), team.clone())),
        channel: room_channel,
        cwd,
        in_place,
        peer: peer_prompt.map(|(_, run)| run),
    }))
}

fn team_hold_guidance(
    hold: &rimz::harness::resume::TeamHold,
    checkout: &Path,
    name: Option<&str>,
) -> (String, String, String) {
    let (reason, release) = match &hold.reason {
        rimz::harness::resume::TeamHoldReason::LiveMember => {
            ("a member is live".to_owned(), String::new())
        }
        rimz::harness::resume::TeamHoldReason::BoardStage(stage) => (
            format!("its board is at `{stage}`"),
            format!(
                ", or mark `{}` `Stage: Done` to release it",
                checkout.join(rimz::harness::board::BOARD_FILE).display()
            ),
        ),
    };
    let resume = match name {
        Some(name) => format!("`rimz teams resume {} -w {name}`", hold.team),
        None => format!(
            "`rimz teams resume {}` from `{}`",
            hold.team,
            checkout.display()
        ),
    };
    (reason, release, resume)
}

fn prepare_peer_prompt(
    store: &rimz::Store,
    identities: &[AgentLaunchIdentity],
    cwd: &Path,
    report_to: rimz::store::run::ReportTo,
) -> Result<Option<(AgentState, rimz::store::run::RunRecord)>> {
    let Some(identity) = identities.iter().find(|identity| {
        identity.launch.launched_by.is_some()
            && identity
                .prompt
                .as_deref()
                .is_some_and(|prompt| !prompt.trim().is_empty())
    }) else {
        return Ok(None);
    };
    let adapter = rimz::agents::find_definition(identity.kind.as_str())
        .context("launched peer has no adapter")?;
    if !rimz::harness::run::peer_can_report(adapter) {
        return Ok(None);
    }
    let agents = store.runtime_projection(rimz::RuntimeScope::Audit)?.agents;
    let peer = agents
        .iter()
        .find(|peer| {
            peer.kind == identity.kind && peer.launch_id.as_ref() == Some(&identity.agent_id)
        })
        .context("launched peer has no provisional row")?;
    let launcher = peer
        .launcher()
        .and_then(|(kind, id)| rimz::address::launch_row(&agents, kind, id));
    if launcher.is_none() {
        tracing::warn!(
            "agent launched, but its launcher could not be resolved; its response lands under its own name"
        );
    }
    let run = rimz::harness::run::create_peer_prompt(
        store.paths(),
        peer,
        launcher.and_then(|launcher| launcher.name.as_deref()),
        adapter,
        identity.prompt.as_deref().unwrap_or_default(),
        cwd,
        report_to,
    )?;
    Ok(run.map(|run| (peer.clone(), run)))
}

/// The launch-prompt run the peer receipt describes, with the peer's name and
/// response path; `None` when the launch created no such run.
fn reported_peer<'a>(
    run: Option<&'a rimz::store::run::RunRecord>,
    paths: &rimz::StatePaths,
) -> Option<(&'a rimz::store::run::RunRecord, &'a str, PathBuf)> {
    let run = run?;
    let name = run.agent_name.as_deref()?;
    let response = rimz::harness::run::response_path(paths, run)?;
    Some((run, name, response))
}

fn write_peer_receipt(
    w: &mut impl Write,
    identities: &[AgentLaunchIdentity],
    run: Option<&rimz::store::run::RunRecord>,
    channel: Option<&str>,
    paths: &rimz::StatePaths,
) -> Result<()> {
    if let Some((run, name, response)) = reported_peer(run, paths) {
        let response = response.display();
        let detached = run.report_to == rimz::store::run::ReportTo::Nobody;
        let handle = channel.map_or_else(
            || format!("@{name}"),
            |channel| format!("@{name}#{channel}"),
        );
        if let Some(team) = run.team.as_ref() {
            let instance = &team.instance;
            if detached {
                writeln!(
                    w,
                    "@{name} leads {instance}, detached: no TEAM_REPORT will reach you."
                )?;
                writeln!(
                    w,
                    "The leader's final response lands at {response}. To block until Done: rimz teams wait {instance}"
                )?;
            } else {
                writeln!(
                    w,
                    "@{name} leads {instance}. Keep working or end your turn: one TEAM_REPORT reaches you when its board reaches Done; stop it after: rimz teams stop {instance}"
                )?;
                writeln!(
                    w,
                    "The leader's final response lands at {response}. To block instead: rimz teams wait {instance}"
                )?;
            }
        } else if detached {
            writeln!(
                w,
                "Running detached in its own pane: no AGENT_REPORT will reach you for this launch turn."
            )?;
            writeln!(
                w,
                "Its response lands at {response} when this turn settles. To read it: rimz agents wait {handle}"
            )?;
            writeln!(
                w,
                "It stays open after this turn. A message of yours reports as usual only if it lands after this turn settles. To follow up: rimz message {handle} '<text>'"
            )?;
        } else {
            writeln!(
                w,
                "Running in its own pane. Keep working or end your turn: one AGENT_REPORT reaches you once every agent you launched has settled, and another after each turn a message of yours opens."
            )?;
            writeln!(
                w,
                "Its response lands at {response} when this turn settles. To block instead: rimz agents wait {handle}"
            )?;
            writeln!(
                w,
                "It stays open after this turn. To follow up: rimz message {handle} '<text>'"
            )?;
        }
    }
    // A team reports through its prompted leader alone, so only that seat's
    // missing hooks cost the launcher its report.
    for identity in identities.iter().filter(|identity| {
        identity.launch.launched_by.is_some()
            && (identity.launch.team.is_none() || identity.prompt.is_some())
    }) {
        if let Some(adapter) = rimz::agents::find_definition(identity.kind.as_str())
            && !rimz::harness::run::peer_can_report(adapter)
        {
            writeln!(
                w,
                "@{} will not report back: {} does not provide the turn hooks needed for reports.",
                identity.name, identity.kind
            )?;
        }
    }
    Ok(())
}

fn queue_resume_prompt(
    store: &rimz::Store,
    session_name: &str,
    seeds: &[rimz::harness::plan::CohortSeed],
    identities: &[AgentLaunchIdentity],
    prompt: &str,
    leader: usize,
) -> Result<()> {
    use rimz::harness::plan::CohortSeed;
    use rimz::store::message::{DeliveryGate, MessageRecord, MessageSender};
    let (kind, agent_id, name) = match &seeds[leader] {
        CohortSeed::Resume(agent) => (&agent.kind, &agent.agent_id, agent.name.clone()),
        CohortSeed::Fresh => {
            let index = seeds[..leader]
                .iter()
                .filter(|seed| matches!(seed, CohortSeed::Fresh))
                .count();
            let identity = &identities[index];
            (
                &identity.kind,
                &identity.agent_id,
                Some(identity.name.clone()),
            )
        }
    };
    let message = MessageRecord::new_for_card(
        store.paths().workspace_id.clone(),
        kind.clone(),
        agent_id.clone(),
        name,
        prompt.to_owned(),
        true,
        DeliveryGate::Done,
    )
    .with_sender(MessageSender::Human);
    store.queue_message(&message, session_name)?;
    Ok(())
}

fn preflight_cell(
    workspace: &rimz::ResolvedWorkspace,
    runtime: &rimz::RuntimePaths,
    cell: &rimz::harness::spec::AgentCell,
    isolation: rimz::config::Isolation,
    prompt: Option<&str>,
    checked_folder_trust: &mut HashSet<rimz::ids::LoginKey>,
) -> Result<rimz::ids::LoginName> {
    let adapter = rimz::agents::find_definition(cell.kind.as_str())
        .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", cell.kind))?;
    rimz::sandbox::preflight_launch(
        isolation,
        &cell.kind,
        cell.skills.is_some(),
        adapter.manual_skill(),
    )?;
    let mut request =
        rimz::harness::launch::ExecRequest::bare_launch(cell.kind.clone(), Vec::new());
    let state = rimz::StatePaths::for_workspace(runtime.workspace_id.clone())?;
    let machine = rimz::config::MachineConfig::load_lenient();
    let login = rimz::store::writer::LaunchLogin::RoomDefault.resolve(
        &cell.kind,
        &rimz::agents::room_accounts(&state.workspace_record, &machine)?,
        &machine.accounts,
    )?;
    request.identity.params.login = (!login.is_default()).then(|| login.name().clone());
    request.skills.clone_from(&cell.skills);
    request.isolation_default = cell.isolation_default;
    request.action = rimz::harness::launch::ExecAction::Launch {
        prompt: prompt.map(str::to_owned),
        extra_args: cell.args.clone(),
    };
    let process = rimz::harness::launch::preflight_agent_process(
        &workspace.project_root,
        &request,
        &workspace.worktree_root,
        (isolation == rimz::config::Isolation::Host).then_some(runtime),
    )?;
    login.health(
        &rimz::agents::machine_login_catalog()
            .native_ambient(&cell.kind, &rimz::agents::ambient_env()),
    )?;
    if adapter.min_version().is_some()
        && let Ok(Some(path)) = process.resolve_program_after_shell_rc()
    {
        rimz::agents::version::check_launch_version_floor(adapter, &path)?;
    }
    if checked_folder_trust.insert(login.key())
        && let Some(rimz::agents::FolderTrust::Undecided(gap)) = adapter.folder_trust(
            &workspace.worktree_root,
            Some(workspace.launch_repo_root()),
            &login.env(&rimz::agents::ambient_env()),
        )
    {
        writeln!(
            crate::cli::render::err(),
            "rimz: {} has no trust decision for `{}`; it will stop at its folder-trust prompt in the pane; {}",
            cell.kind,
            gap.key.display(),
            gap.fix(cell.kind.as_str())
        )?;
    }
    Ok(login.name().clone())
}

#[allow(clippy::too_many_arguments)]
fn launch_resume_layout(
    args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
    ctx: &Ctx,
    machine_config: &rimz::config::MachineConfig,
    teams: &rimz::config::TeamsConfig,
    profiles: &rimz::config::ProfilesConfig,
    mut layout: LayoutSpec,
    team_name: Option<String>,
    single_cell: bool,
    worktree_filter: Option<&Path>,
    ancestry: Option<&rimz::harness::ancestry::LaunchAncestry>,
    focus: LaunchFocus,
    mut checked_folder_trust: HashSet<rimz::ids::LoginKey>,
) -> Result<()> {
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let agents = match worktree_filter {
        Some(target) => {
            let target = rimz::utils::path::normalize_path_lexical(target);
            projection
                .agents
                .into_iter()
                .filter(|agent| agent_matches_worktree_filter(agent, &target))
                .collect::<Vec<_>>()
        }
        None => projection.agents,
    };
    let cells = cohort_cells(&layout);
    let spec = args.launch.spec.as_deref().unwrap_or("<spec>");
    let scope = worktree_filter.and_then(worktree_scope_label);
    let logins = rimz::agents::room_accounts(
        &store.paths().workspace_record,
        &rimz::config::MachineConfig::load_lenient(),
    )?;
    let team = team_name.as_deref().and_then(|name| teams.0.get(name));
    let mut plan = rimz::harness::resume::plan_cohort_resume(
        &agents,
        &logins,
        &rimz::agents::machine_login_catalog(),
        rimz::store::runtime::agent_liveness,
        &cells,
        team_name.as_deref(),
        |path| path.is_dir(),
        rimz::harness::resume::resume_session_present,
    )
    .map_err(|err| cohort_resume_error(err, spec, scope.as_deref(), &agents, teams))?;
    let availability = rimz::harness::plan::LaunchAvailability::read(
        store.runtime_paths(),
        store.paths(),
        machine_config,
        jiff::Timestamp::now(),
    );
    let unavailable = |kind: &str, model: &str| availability.unavailable(kind, model);
    rimz::harness::resume::restore_routed_cells(
        &mut layout,
        &plan.seeds,
        profiles,
        &rimz::harness::resume::ResumeOverrides {
            team,
            permission_mode: interactive_permission_mode_from_flags(
                args.launch.overrides.ask,
                args.launch.overrides.yolo,
            )?,
            preset: launch_override_preset(&args.launch.overrides)?,
            agent: args.launch.overrides.agent.as_deref(),
            tier: args
                .launch
                .overrides
                .tier
                .map(|tier| (tier, &machine_config.tiers)),
            unavailable: Some(&unavailable),
        },
    )?;
    let mut preflighted_logins = Vec::new();
    for (cell, seed) in layout.agent_cells().zip(&plan.seeds) {
        let isolation = match seed {
            rimz::harness::plan::CohortSeed::Resume(agent) => {
                rimz::harness::plan::effective_resume_isolation(
                    cell.launch.isolation,
                    agent.isolation,
                )
            }
            rimz::harness::plan::CohortSeed::Fresh => cell.launch.isolation,
        };
        let login = preflight_cell(
            workspace,
            store.runtime_paths(),
            cell,
            rimz::config::Isolation::resolve(
                isolation,
                cell.isolation_default,
                machine_config.agents.isolation,
            ),
            None,
            &mut checked_folder_trust,
        )?;
        if matches!(seed, rimz::harness::plan::CohortSeed::Fresh) {
            preflighted_logins.push(login);
        }
    }
    let cwd = plan
        .cwd
        .clone()
        .context("cohort resume matched no working directory")?;
    crate::cli::lsp_admission::admit(&cwd, machine_config)?;
    if let Some(team) = team {
        rimz::worktree::exclude_team_scratch(&cwd, &team.scratch_patterns());
    }
    let channel = rimz::harness::spec::resolve_room_channel(
        &workspace.project_root,
        &cwd,
        team_name.as_deref(),
        plan.channel.as_deref(),
    );
    plan.channel = channel.clone();
    let scoped_resume = resume_outside_launch_dir(
        channel.as_deref(),
        &cwd,
        &workspace.project_root,
        &workspace.worktree_root,
        std::env::current_dir().ok().as_deref(),
    );
    let placement = if single_cell {
        apply_in_place_downgrade(
            resolve_placement(
                args.launch.cohort.new_tab,
                args.launch.new_pane,
                machine_config.agents.placement,
                scoped_resume,
                single_cell,
                rimz::mux::ambient_pane_id().is_some(),
            )?,
            focus,
            allow_in_place,
        )
    } else {
        Placement::NewTab
    };
    let in_place = placement == Placement::SamePane;
    let room = RoomContext::live_tab(
        workspace,
        std::sync::Arc::new(machine_config.clone()),
        globals.mux,
    )?;
    let mux = room.mux_name();
    let backend = room.backend();
    let mut launch_requests = launch_identity_requests(
        &layout,
        None,
        None,
        team_name.as_deref(),
        team.map(|team| team.roles.as_slice()),
        channel.as_deref(),
        None,
        Some(&plan),
        ancestry,
    )?;
    for (request, login) in launch_requests.iter_mut().zip(preflighted_logins) {
        request.login = rimz::store::writer::LaunchLogin::Pinned(login);
    }
    let launch_batch = store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.clone(),
            branch: None,
            description: None,
        },
    )?;

    let title = channel.as_deref().map_or_else(
        || rimz::harness::spec::default_tab_title(&layout, None, team_name.as_deref()),
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&cwd);
    let mut panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            runtime: store.runtime_paths(),
            cwd: &cwd,
            cleanup_worktree: false,
            in_place,
            resume_seeds: Some(&plan.seeds),
            launch_identities: launch_batch.identities(),
            fallback_channel: channel.as_deref(),
            loop_reminder: None,
        },
    )?;
    panes.focused_pane = team_leader_pane(&layout, team);
    let leader = team.and_then(|team| team.leader.as_deref());
    let lane_label = launched_lane_label(channel.as_deref(), &cwd, &workspace.project_root);
    let write_receipt = || {
        write_resume_receipt(
            &mut render::out(),
            &plan,
            team_name.as_deref(),
            lane_label.as_deref(),
            launch_batch.identities(),
            leader,
        )
    };
    if let Some(prompt) = args
        .launch
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
    {
        queue_resume_prompt(
            store,
            &workspace.session_name,
            &plan.seeds,
            launch_batch.identities(),
            prompt,
            rimz::harness::spec::prompt_leader(&layout, team)?,
        )?;
        rimz::message::deliver::register_message_wake(workspace, store);
    }
    if in_place {
        write_receipt()?;
    }
    super::placement::execute(
        backend,
        store,
        &launch_batch,
        PlacementRequest {
            placement,
            mux,
            cwd: cwd.clone(),
            title,
            panes,
            sidebar,
            identity_env: rimz::room::pane_identity_env(workspace, &cwd, channel.as_deref(), false),
            focus,
            errors: LAUNCH_PLACEMENT_ERRORS,
        },
        || {
            if let Err(err) = rimz::harness::rebirth::settle_refilled_seats(
                store,
                &workspace.session_name,
                &plan.refilled,
            ) {
                let _ = writeln!(
                    std::io::stderr(),
                    "rimz: replaced agents stay pending: {err}"
                );
            }
            rimz::harness::assist_log::record_tier_fallbacks(launch_batch.identities());
            if args.launch.overrides.tier.is_some() {
                for (cell, seed) in layout.agent_cells().zip(&plan.seeds) {
                    let rimz::harness::plan::CohortSeed::Resume(agent) = seed else {
                        continue;
                    };
                    rimz::harness::assist_log::record_tier_fallback(
                        &agent.kind,
                        &agent.agent_id,
                        agent.name.as_deref(),
                        &cell.launch,
                    );
                }
            }
        },
    )?;
    if !in_place {
        write_receipt()?;
    }
    Ok(())
}

/// A team tab opens focused on its leader's pane; any other layout keeps the
/// leading pane. The leader is an agent-cell index, so command cells before it
/// still count toward its pane position.
fn team_leader_pane(layout: &LayoutSpec, team: Option<&rimz::config::Team>) -> usize {
    let Some(leader) =
        team.and_then(|team| rimz::harness::spec::prompt_leader(layout, Some(team)).ok())
    else {
        return 0;
    };
    layout
        .columns
        .iter()
        .flat_map(|column| column.rows.iter())
        .enumerate()
        .filter(|(_, cell)| matches!(cell, Cell::Agent(_)))
        .nth(leader)
        .map_or(0, |(position, _)| position)
}

fn agent_matches_worktree_filter(agent: &AgentState, target: &Path) -> bool {
    agent.worktree_path.as_deref().is_some_and(|worktree| {
        rimz::utils::path::normalize_path_lexical(Path::new(worktree)) == target
    })
}

fn resume_worktree_scope(
    worktree_arg: Option<&str>,
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
) -> Result<Option<PathBuf>> {
    resume_worktree_scope_with(
        worktree_arg,
        &workspace.worktree_root,
        workspace.launch_repo_root(),
        |name| {
            if workspace.cwd_project_root.is_none()
                && workspace.root_class != rimz::workspace::RootClass::Repo
            {
                bail!("--worktree requires a git repository-backed room");
            }
            rimz::worktree::worktree_path(
                workspace.launch_repo_root(),
                &machine_config.agents.worktree,
                name,
            )
            .map_err(Into::into)
        },
    )
}

fn resume_worktree_scope_with(
    worktree_arg: Option<&str>,
    worktree_root: &Path,
    project_root: &Path,
    resolve_named: impl FnOnce(&str) -> Result<PathBuf>,
) -> Result<Option<PathBuf>> {
    match worktree_arg.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) => resolve_named(name).map(Some),
        None if worktree_root != project_root => Ok(Some(worktree_root.to_owned())),
        None => Ok(None),
    }
}

/// Whether a resolved resume lands outside the pane the command runs in. A
/// launch pane already sitting in the cohort's working directory is the origin
/// pane — an agent that dropped to a shell there resumes in place instead of
/// opening a lane tab.
fn resume_outside_launch_dir(
    channel: Option<&str>,
    cwd: &Path,
    project_root: &Path,
    worktree_root: &Path,
    launch_dir: Option<&Path>,
) -> bool {
    let target = rimz::utils::path::normalize_path_lexical(cwd);
    if launch_dir.is_some_and(|dir| rimz::utils::path::normalize_path_lexical(dir) == target) {
        return false;
    }
    channel.is_some() || (cwd != project_root && cwd != worktree_root)
}

fn worktree_scope_label(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn cohort_resume_error(
    err: rimz::harness::resume::CohortResumeErr,
    spec: &str,
    scope: Option<&str>,
    agents: &[AgentState],
    teams: &rimz::config::TeamsConfig,
) -> anyhow::Error {
    let subject = cohort_resume_subject(spec, scope);
    match err {
        rimz::harness::resume::CohortResumeErr::NothingToResume { .. } => {
            let resumable = rimz::harness::resume::closed_cohort_specs(
                agents,
                rimz::store::runtime::agent_liveness,
            );
            match resumable.first() {
                Some(first) => {
                    let retry = if teams.0.contains_key(first) {
                        format!("rimz teams resume {first}")
                    } else {
                        format!("rimz agents {first} --resume")
                    };
                    anyhow::anyhow!(
                        "nothing to resume for {subject}; resumable here: {} — retry with `{retry}`",
                        resumable.join(", ")
                    )
                }
                None => {
                    anyhow::anyhow!("nothing to resume for {subject}; launch without `--resume`")
                }
            }
        }
        rimz::harness::resume::CohortResumeErr::MembersStillLive { labels } => {
            anyhow::anyhow!(
                "cannot resume {subject}; still live: {}; close them first or drop `--resume`",
                labels.join(", ")
            )
        }
        rimz::harness::resume::CohortResumeErr::LoginMismatch(mismatch) => mismatch.into(),
    }
}

fn cohort_resume_subject(spec: &str, scope: Option<&str>) -> String {
    match scope {
        Some(scope) => format!("`{spec}` in worktree `{scope}`"),
        None => format!("`{spec}`"),
    }
}

struct LaunchReceipt<'a> {
    team: Option<(&'a str, &'a rimz::config::Team)>,
    channel: Option<&'a str>,
    cwd: &'a Path,
    identities: &'a [AgentLaunchIdentity],
    leader_index: Option<usize>,
    terminal_width: usize,
    /// Off when the peer receipt prints, since it names the same two commands.
    hints: bool,
}

fn write_launch_receipt(w: &mut impl Write, receipt: &LaunchReceipt<'_>) -> Result<()> {
    let team = receipt.team.map(|(name, _)| name);
    let channel = receipt.channel;
    let identities = receipt.identities;
    let subject = match (team, identities) {
        (Some(team), _) => team.to_owned(),
        (None, [identity]) => format!("@{}", identity.name),
        (None, identities) => format!("{} agents", identities.len()),
    };
    let lane = channel.map_or_else(String::new, |channel| {
        if team.is_some() {
            format!(" in worktree #{channel}")
        } else {
            format!(" in #{channel}")
        }
    });
    let cwd = rimz::utils::path::normalize_path_lexical(receipt.cwd);
    if team.is_some() {
        writeln!(w, "launched {subject}{lane}")?;
        writeln!(w, "  path      {}", cwd.display())?;
        writeln!(w, "  board     {}", rimz::harness::board::BOARD_FILE)?;
    } else {
        writeln!(w, "launched {subject}{lane} ({})", cwd.display())?;
    }
    let leader = receipt.leader_index.and_then(|index| identities.get(index));
    if let Some(identity) = leader
        && let Some(prompt) = identity.prompt.as_deref()
    {
        let prompt = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
        let prompt = prompt.replace('\\', "\\\\").replace('"', "\\\"");
        let line = format!(
            "  prompt    → @{}  \"{prompt}\"",
            launch_identity_handle(identity)
        );
        writeln!(
            w,
            "{}",
            render::clip_to_width(&line, receipt.terminal_width)
        )?;
    }
    writeln!(w)?;

    let rows = identities
        .iter()
        .enumerate()
        .map(|(index, identity)| render::RosterRow {
            handle: launch_identity_handle(identity).to_owned(),
            kind: identity.kind.as_str().to_owned(),
            model: identity.launch.model.clone(),
            leader: team.is_some() && receipt.leader_index == Some(index),
        })
        .collect();
    let signals = receipt
        .team
        .into_iter()
        .flat_map(|(_, team)| &team.roles)
        .flat_map(|role| {
            role.signals.iter().map(|binding| render::RosterSignal {
                signal: binding.signal.clone(),
                matches: binding.matches.clone(),
                role: role.role.clone(),
            })
        })
        .collect();
    render::Roster::new(rows)
        .signals(signals)
        .signal_width(receipt.terminal_width)
        .indent(2)
        .render(w)?;
    if team.is_some() {
        return Ok(());
    }
    writeln!(w)?;
    if !receipt.hints {
        return Ok(());
    }
    write_launch_hints(
        w,
        team,
        channel,
        identities.first().map(launch_identity_handle),
        leader.map(launch_identity_handle),
    )
}

fn launch_identity_handle(identity: &AgentLaunchIdentity) -> &str {
    identity.launch.role.as_deref().unwrap_or(&identity.name)
}

fn resume_hint_handle<'a>(
    plan: &'a rimz::harness::plan::CohortResumePlan,
    fresh_identities: &'a [AgentLaunchIdentity],
) -> Option<&'a str> {
    match plan.seeds.first()? {
        rimz::harness::plan::CohortSeed::Resume(agent) => agent
            .role
            .as_deref()
            .or(agent.name.as_deref())
            .or(agent.profile.as_deref())
            .or(Some(agent.kind.as_str())),
        rimz::harness::plan::CohortSeed::Fresh => {
            fresh_identities.first().map(launch_identity_handle)
        }
    }
}

fn write_launch_hints(
    w: &mut impl Write,
    team: Option<&str>,
    channel: Option<&str>,
    fallback_handle: Option<&str>,
    leader: Option<&str>,
) -> Result<()> {
    if let (Some(team), Some(channel)) = (team, channel) {
        writeln!(w, "Check: rimz teams show {team}#{channel}")?;
    }
    if let Some(handle) = leader.or(fallback_handle) {
        let lane = channel.map_or_else(String::new, |channel| format!("#{channel}"));
        writeln!(w, "Reach: rimz message @{handle}{lane} '<text>'")?;
        if team.is_none() {
            writeln!(w, "Wait:  rimz agents wait @{handle}{lane}")?;
        }
    }
    if let (Some(team), Some(channel)) = (team, channel) {
        writeln!(
            w,
            "Wait:  rimz loop add team-idle --wait @me --signal team.idle --match instance={team}#{channel} --once"
        )?;
    }
    Ok(())
}

fn write_resume_receipt(
    w: &mut impl Write,
    plan: &rimz::harness::plan::CohortResumePlan,
    team: Option<&str>,
    channel: Option<&str>,
    fresh_identities: &[AgentLaunchIdentity],
    leader: Option<&str>,
) -> Result<()> {
    report_cohort_resume(w, plan)?;
    writeln!(w)?;
    write_launch_hints(
        w,
        team,
        channel,
        resume_hint_handle(plan, fresh_identities),
        leader,
    )
}

fn report_cohort_resume(
    w: &mut impl Write,
    plan: &rimz::harness::plan::CohortResumePlan,
) -> Result<()> {
    let mut fresh = plan.fresh.iter();
    for seed in &plan.seeds {
        match seed {
            rimz::harness::plan::CohortSeed::Resume(agent) => {
                let name = agent.name.as_deref().unwrap_or("unnamed");
                writeln!(
                    w,
                    "resumed {}:{} ({})",
                    agent.kind.as_str(),
                    name,
                    agent.agent_id
                )?;
            }
            rimz::harness::plan::CohortSeed::Fresh => {
                let label = fresh.next().map_or("agent", String::as_str);
                writeln!(w, "started fresh {label}")?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
