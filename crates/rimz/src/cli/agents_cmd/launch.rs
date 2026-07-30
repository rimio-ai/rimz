//! Interactive launch orchestration and presentation.

use std::borrow::Cow;

use super::*;
use crate::cli::ctx::Ctx;
use crate::cli::machine_config;

use super::placement::{PlacementErrors, PlacementRequest};

pub(super) fn launch_layout(
    mut args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
) -> Result<()> {
    let ctx = Ctx::open(globals)?;
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let machine_config = machine_config();
    let effective = rimz::config::effective::load(
        &machine_config.agents,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    // Inside a team's lane, a bare role names that team's role: in `#forge`,
    // `reviewer` means `forge.reviewer`. The lane's agents carry the team, since
    // the channel string alone does not name it. An explicit `--channel` picks
    // the lane to infer from, so the inference works from outside the tab too.
    let lane = args
        .launch
        .cohort
        .channel
        .as_deref()
        .or_else(|| ctx.channel());
    let mut inferred_lane = None;
    if let (Some(spec), Some(channel)) = (args.launch.spec.as_deref(), lane) {
        let snapshot = ctx.cached_snapshot()?;
        if let Some(team) = rimz::harness::target::channel_team(&snapshot.agents, channel) {
            let qualified = rimz::harness::spec::qualify_spec_in_channel(
                spec,
                channel,
                team,
                &effective.teams,
                &effective.profiles,
                &machine_config.agents.commands,
            )?;
            if let Cow::Owned(qualified) = qualified {
                args.launch.spec = Some(qualified);
                inferred_lane = Some(channel.to_owned());
            }
        }
    }
    let mut resolved = rimz::harness::plan::resolve_launch(
        &effective,
        &machine_config.agents.commands,
        args.launch.spec.as_deref(),
        rimz::harness::plan::normalized_preset_value(args.launch.agent.as_deref()).as_deref(),
    )?;
    let preset = validate_resolved_launch_inputs(
        &args,
        &effective,
        &machine_config.agents.commands,
        &resolved.layout,
        true,
    )?;
    let warnings = rimz::harness::plan::finalize_launch_layout(
        &mut resolved.layout,
        LaunchFinalizeOptions {
            permission_mode: interactive_permission_mode_from_flags(
                args.launch.ask,
                args.launch.yolo,
            )?,
            preset: &preset,
            passthrough: &args.launch.passthrough,
            budget: args.launch.cohort.budget,
            max_turns: args.launch.max_turns,
        },
    )
    .inspect_err(|err| {
        for warning in err.warnings() {
            let _ = writeln!(std::io::stderr(), "{warning}");
        }
    })?;
    for warning in &warnings {
        writeln!(std::io::stderr(), "{warning}")?;
    }
    let ResolvedLaunch {
        teams,
        layout,
        team_name,
    } = resolved;
    let ancestry = if rimz::harness::plan::launch_ancestry_required() {
        let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
        rimz::harness::plan::resolve_launch_ancestry_from_env(
            &projection.agents,
            false,
            machine_config.agents.max_chain_length,
        )?
    } else {
        None
    };
    let prompt = args
        .launch
        .prompt
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty());
    let prompt_agent_index = prompt
        .map(|_| {
            rimz::harness::spec::prompt_leader(
                &layout,
                team_name.as_deref().and_then(|name| teams.0.get(name)),
            )
        })
        .transpose()?;
    for kind in layout.agent_kinds() {
        rimz::harness::launch::preflight_agent_kind(
            &workspace.project_root,
            machine_config.harness.rtk,
            kind,
            &workspace.worktree_root,
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
            &ctx,
            &machine_config,
            &teams,
            layout,
            team_name,
            single_cell,
            worktree_filter.as_deref(),
            ancestry.as_ref(),
        );
    }
    let worktree_launch =
        args.launch.cohort.worktree.is_some() || args.launch.cohort.from_pr.is_some();
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
        args.launch.cohort.bg,
        allow_in_place,
    );
    let in_place = placement == Placement::SamePane;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let room = RoomContext::from_resolved(
        workspace,
        machine_config.clone(),
        mux,
        RoomSizing::OrdinaryTab,
    )?;
    let backend = room.backend();
    rimz::room::require_live_session(backend, &workspace.session_name)?;

    let explicit_worktree_name = args
        .launch
        .cohort
        .worktree
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| rimz::worktree::parse_requested_name(name).map(|requested| requested.name))
        .transpose()?;
    let cells = cohort_cells(&layout);
    if let Some(name) = explicit_worktree_name.as_deref()
        && args.launch.cohort.from_pr.is_none()
        && (team_name.is_some() || cells.len() >= 2)
    {
        let spec_display = args.launch.spec.as_deref().unwrap_or("<spec>");
        match reconcile::reconcile_cohort_launch(
            workspace,
            &machine_config,
            backend,
            store,
            name,
            spec_display,
            team_name.as_deref(),
            &cells,
        )? {
            reconcile::Reconciled::Done => return Ok(()),
            reconcile::Reconciled::Resume(path) => {
                return launch_resume_layout(
                    args,
                    globals,
                    allow_in_place,
                    &ctx,
                    &machine_config,
                    &teams,
                    layout,
                    team_name,
                    single_cell,
                    Some(&path),
                    ancestry.as_ref(),
                );
            }
            reconcile::Reconciled::Continue => {}
        }
    }

    let launch = rimz::worktree::resolve_launch_checkout(
        workspace,
        &machine_config.agents.worktree,
        args.launch.cohort.worktree.as_deref(),
        args.launch.cohort.from_pr.as_ref(),
    )?;
    if let Some(team) = team_name.as_deref().and_then(|name| teams.0.get(name)) {
        rimz::worktree::exclude_team_scratch(&launch.cwd, &team.scratch_files);
    }
    if let Some(reason) = launch.review_only_reason.as_deref() {
        writeln!(
            std::io::stderr(),
            "review-only checkout ({reason}); pushes are not configured — install gh/tea for a pushable checkout"
        )?;
    }
    if let Some(channel) = args.launch.cohort.channel.as_deref() {
        rimz::channel::register(workspace, store.paths(), channel)?;
    }
    // An inferred lane joins the exact channel it was inferred from, rather than
    // one recomputed from the caller's cwd — a shell pane that has `cd`'d into a
    // subdirectory would otherwise stamp that subdirectory's basename.
    let room_channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        team_name.as_deref(),
        args.launch
            .cohort
            .channel
            .as_deref()
            .or(inferred_lane.as_deref()),
    );
    let launch_requests = launch_identity_requests(
        &layout,
        args.launch.name.as_deref(),
        launch.generated_name(),
        team_name.as_deref(),
        team_name
            .as_deref()
            .and_then(|name| teams.0.get(name))
            .map(|team| team.roles.as_slice()),
        room_channel.as_deref(),
        prompt.zip(prompt_agent_index),
        None,
        ancestry.as_ref(),
    )?;
    let launch_batch = store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: launch.cwd.clone(),
            worktree_name: launch.worktree_name.clone(),
            channel: room_channel.clone(),
            description: args.launch.cohort.description.clone(),
        },
    )?;
    let worktree_name = launch.worktree_name.clone();
    let cwd = launch.cwd;
    let title = room_channel.as_deref().map_or_else(
        || {
            rimz::harness::spec::default_tab_title(
                &layout,
                &cwd,
                worktree_name.as_deref(),
                team_name.as_deref(),
            )
        },
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&cwd, Vec::new(), None);
    let panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: &cwd,
            cleanup_worktree: worktree_launch,
            in_place,
            resume_seeds: None,
            launch_identities: launch_batch.identities(),
            fallback_channel: None,
        },
    )?;
    super::placement::execute(
        backend,
        store,
        &launch_batch,
        PlacementRequest {
            placement,
            mux,
            cwd,
            title,
            panes,
            sidebar,
            identity_env: rimz::room::pane_identity_env(
                workspace,
                room_channel.as_deref(),
                !worktree_launch,
            ),
            background: args.launch.cohort.bg,
            errors: PlacementErrors {
                new_tab: "opening agent tab",
                new_pane: "splitting the agent into a new pane",
                same_pane: "running the agent in the current pane",
            },
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn launch_resume_layout(
    args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
    ctx: &Ctx,
    machine_config: &rimz::config::MachineConfig,
    teams: &rimz::config::TeamsConfig,
    layout: LayoutSpec,
    team_name: Option<String>,
    single_cell: bool,
    worktree_filter: Option<&Path>,
    ancestry: Option<&rimz::harness::plan::LaunchAncestry>,
) -> Result<()> {
    let workspace = &ctx.workspace;
    let store = &ctx.store;
    let projection = store.runtime_projection(rimz::RuntimeScope::Audit)?;
    let agents = match worktree_filter {
        Some(target) => {
            let target = rimz::worktree::normalize_path_lexical(target);
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
    let mut plan = rimz::harness::resume::plan_cohort_resume(
        &agents,
        rimz::store::runtime::agent_liveness,
        &cells,
        team_name.as_deref(),
        |path| path.is_dir(),
        rimz::harness::resume::resume_session_present,
    )
    .map_err(|err| cohort_resume_error(err, spec, scope.as_deref(), &agents, teams))?;
    let cwd = plan
        .cwd
        .clone()
        .context("cohort resume matched no working directory")?;
    if let Some(team) = team_name.as_deref().and_then(|name| teams.0.get(name)) {
        rimz::worktree::exclude_team_scratch(&cwd, &team.scratch_files);
    }
    let channel = rimz::harness::target::resolve_room_channel(
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
            args.launch.cohort.bg,
            allow_in_place,
        )
    } else {
        Placement::NewTab
    };
    let in_place = placement == Placement::SamePane;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let room = RoomContext::from_resolved(
        workspace,
        std::sync::Arc::new(machine_config.clone()),
        mux,
        RoomSizing::OrdinaryTab,
    )?;
    let backend = room.backend();
    rimz::room::require_live_session(backend, &workspace.session_name)?;
    let launch_requests = launch_identity_requests(
        &layout,
        None,
        None,
        team_name.as_deref(),
        team_name
            .as_deref()
            .and_then(|name| teams.0.get(name))
            .map(|team| team.roles.as_slice()),
        channel.as_deref(),
        None,
        Some(&plan),
        ancestry,
    )?;
    let launch_batch = store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: cwd.clone(),
            worktree_name: None,
            channel: channel.clone(),
            description: None,
        },
    )?;

    let title = channel.as_deref().map_or_else(
        || rimz::harness::spec::default_tab_title(&layout, &cwd, None, team_name.as_deref()),
        |channel| format!("#{channel}"),
    );
    let sidebar = room.sidebar_options(&cwd, Vec::new(), None);
    let panes = compile_layout_panes(
        &layout,
        LayoutPaneParams {
            cwd: &cwd,
            cleanup_worktree: false,
            in_place,
            resume_seeds: Some(&plan.seeds),
            launch_identities: launch_batch.identities(),
            fallback_channel: channel.as_deref(),
        },
    )?;
    if in_place {
        report_cohort_resume(&plan);
    }
    super::placement::execute(
        backend,
        store,
        &launch_batch,
        PlacementRequest {
            placement,
            mux,
            cwd,
            title,
            panes,
            sidebar,
            identity_env: rimz::room::pane_identity_env(workspace, channel.as_deref(), false),
            background: args.launch.cohort.bg,
            errors: PlacementErrors {
                new_tab: "opening agent tab",
                new_pane: "splitting the agent into a new pane",
                same_pane: "running the agent in the current pane",
            },
        },
    )?;
    if !in_place {
        report_cohort_resume(&plan);
    }
    Ok(())
}

fn agent_matches_worktree_filter(agent: &AgentState, target: &Path) -> bool {
    agent.worktree_path.as_deref().is_some_and(|worktree| {
        rimz::worktree::normalize_path_lexical(Path::new(worktree)) == target
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
        &workspace.project_root,
        |name| {
            if workspace.root_class != rimz::workspace::RootClass::Repo {
                bail!("--worktree requires a git repository-backed room");
            }
            rimz::worktree::worktree_path(
                &workspace.project_root,
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
    let target = rimz::worktree::normalize_path_lexical(cwd);
    if launch_dir.is_some_and(|dir| rimz::worktree::normalize_path_lexical(dir) == target) {
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
    }
}

fn cohort_resume_subject(spec: &str, scope: Option<&str>) -> String {
    match scope {
        Some(scope) => format!("`{spec}` in worktree `{scope}`"),
        None => format!("`{spec}`"),
    }
}

fn report_cohort_resume(plan: &rimz::harness::resume::CohortResumePlan) {
    let mut fresh = plan.fresh.iter();
    for seed in &plan.seeds {
        match seed {
            rimz::harness::resume::CohortSeed::Resume(agent) => {
                let name = agent.name.as_deref().unwrap_or("unnamed");
                let _ = writeln!(
                    std::io::stderr(),
                    "resumed {}:{} ({})",
                    agent.kind.as_str(),
                    name,
                    agent.agent_id
                );
            }
            rimz::harness::resume::CohortSeed::Fresh => {
                let label = fresh.next().map_or("agent", String::as_str);
                let _ = writeln!(std::io::stderr(), "started fresh {label}");
            }
        }
    }
}

pub(super) fn interactive_permission_mode_from_flags(
    ask: bool,
    yolo: bool,
) -> Result<Option<PermissionMode>> {
    if ask && yolo {
        bail!("choose at most one of --ask and --yolo");
    }
    Ok(if yolo {
        Some(PermissionMode::Yolo)
    } else if ask {
        Some(PermissionMode::Ask)
    } else {
        None
    })
}

pub(super) fn reject_launch_flags_without_spec(args: &AgentsArgs) -> Result<()> {
    if !args.launch.passthrough.is_empty() {
        bail!("missing agent spec before `--`");
    }
    if args.launch.cohort.worktree.is_some() {
        bail!(
            "--worktree requires an agent spec; use `rimz agents list --worktree <name>` to filter cards"
        );
    }
    if args.launch.cohort.channel.is_some() {
        bail!("--channel requires an agent spec; use `rimz channel list` to inspect channels");
    }
    if args.launch.cohort.from_pr.is_some() {
        bail!("--from-pr requires an agent spec");
    }
    if args.launch.name.is_some()
        || args.launch.cohort.bg
        || args.launch.new_pane
        || args.launch.cohort.new_tab
        || args.launch.cohort.resume
        || args.launch.ask
        || args.launch.yolo
        || args.launch.print
        || args.launch.effort.is_some()
        || args.launch.cohort.budget.is_some()
        || args.launch.model.is_some()
        || args.launch.agent.is_some()
        || args.launch.cohort.description.is_some()
        || args.launch.system_prompt_file.is_some()
        || !args.launch.append_system_prompt_files.is_empty()
        || args.launch.max_turns.is_some()
        || args.launch.retries.is_some()
    {
        bail!("agent launch options require an agent spec");
    }
    Ok(())
}

/// Build the launch-override preset from shared launch flags. Prompt files are
/// resolved to absolute paths and required to exist here, at the entry point,
/// rather than downstream in the agent.
pub(super) fn launch_override_preset(args: &AgentsArgs) -> Result<rimz::agents::LaunchPreset> {
    let system_prompt_file = resolve_launch_prompt_file(
        args.launch.system_prompt_file.as_deref(),
        "--system-prompt-file",
    )?;
    let append_system_prompt_files =
        resolve_launch_prompt_files(&args.launch.append_system_prompt_files)?;
    Ok(rimz::agents::LaunchPreset {
        model: rimz::harness::plan::normalized_preset_value(args.launch.model.as_deref()),
        effort: rimz::harness::plan::normalized_preset_value(args.launch.effort.as_deref()),
        system_prompt_file,
        append_system_prompt_files,
    })
}

pub(super) fn resolve_launch_prompt_file(
    path: Option<&Path>,
    flag: &str,
) -> Result<Option<PathBuf>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let resolved = path
        .canonicalize()
        .with_context(|| format!("reading {flag} `{}`", path.display()))?;
    if !resolved.is_file() {
        bail!("{flag} `{}` is not a regular file", path.display());
    }
    Ok(Some(resolved))
}

pub(super) fn resolve_launch_prompt_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>> {
    paths
        .iter()
        .map(|path| {
            resolve_launch_prompt_file(Some(path), "--append-system-prompt-file")
                .map(|path| path.expect("a supplied path resolves to one path"))
        })
        .collect()
}

/// Apply CLI-owned launch validation in its user-visible precedence order.
pub(super) fn validate_resolved_launch_inputs(
    args: &AgentsArgs,
    effective: &rimz::config::effective::LaunchAgents,
    commands: &rimz::config::CommandsConfig,
    layout: &LayoutSpec,
    enforce_name_cardinality: bool,
) -> Result<rimz::agents::LaunchPreset> {
    rimz::harness::plan::reject_prompt_that_looks_like_spec(
        args.launch.spec.as_deref(),
        args.launch.prompt.as_deref(),
        &effective.profiles,
        commands,
        &effective.teams,
    )?;
    if enforce_name_cardinality && args.launch.name.is_some() && layout.agent_kinds().count() != 1 {
        bail!("--name requires a layout with exactly one agent cell");
    }
    launch_override_preset(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resume_in_the_lane_directory_stays_in_the_origin_pane() {
        let project = Path::new("/repo");
        let worktree = Path::new("/repo-worktrees/single-card");

        // The dropped-to-shell origin pane: launch dir is the cohort cwd, so
        // in-place placement stays available despite the lane channel.
        assert!(!resume_outside_launch_dir(
            Some("single-card"),
            worktree,
            project,
            worktree,
            Some(worktree),
        ));

        // From anywhere else the lane resume still opens its own tab.
        assert!(resume_outside_launch_dir(
            Some("single-card"),
            worktree,
            project,
            project,
            Some(project),
        ));
        assert!(resume_outside_launch_dir(
            Some("single-card"),
            worktree,
            project,
            worktree,
            None,
        ));
    }

    #[test]
    fn resume_launch_dir_comparison_is_lexically_normalized() {
        let project = Path::new("/repo");
        let worktree = Path::new("/repo-worktrees/single-card");
        let launch_dir = Path::new("/repo-worktrees/../repo-worktrees/single-card");

        assert!(!resume_outside_launch_dir(
            Some("single-card"),
            worktree,
            project,
            worktree,
            Some(launch_dir),
        ));
    }

    #[test]
    fn worktree_filter_matches_normalized_agent_paths() {
        let target = rimz::worktree::normalize_path_lexical(Path::new("/repo-worktrees/demo"));
        let mut agent = test_agent("sess-demo");
        agent.worktree_path = Some("/repo/../repo-worktrees/demo".to_owned());

        assert!(agent_matches_worktree_filter(&agent, &target));

        agent.worktree_path = Some("/repo-worktrees/other".to_owned());
        assert!(!agent_matches_worktree_filter(&agent, &target));

        agent.worktree_path = None;
        assert!(!agent_matches_worktree_filter(&agent, &target));
    }

    #[test]
    fn resume_worktree_scope_resolves_named_worktree() {
        let expected = PathBuf::from("/repo-worktrees/restore-living-team");
        let called = std::cell::Cell::new(false);

        let scope = resume_worktree_scope_with(
            Some(" restore-living-team "),
            Path::new("/repo"),
            Path::new("/repo"),
            |name| {
                called.set(true);
                assert_eq!(name, "restore-living-team");
                Ok(expected.clone())
            },
        )
        .expect("named worktree scope");

        assert_eq!(scope, Some(expected));
        assert!(called.get());
    }

    #[test]
    fn resume_worktree_scope_uses_cwd_worktree_when_unnamed() {
        let worktree = Path::new("/repo-worktrees/restore-living-team");

        let scope = resume_worktree_scope_with(
            None,
            worktree,
            Path::new("/repo"),
            |_| -> Result<PathBuf> { panic!("unnamed scope must not resolve a worktree name") },
        )
        .expect("cwd worktree scope");

        assert_eq!(scope.as_deref(), Some(worktree));
    }

    #[test]
    fn resume_worktree_scope_keeps_repo_root_global_when_unnamed() {
        let scope = resume_worktree_scope_with(
            None,
            Path::new("/repo"),
            Path::new("/repo"),
            |_| -> Result<PathBuf> { panic!("repo-root scope must stay global") },
        )
        .expect("global resume scope");

        assert_eq!(scope, None);
    }

    #[test]
    fn resume_worktree_scope_treats_bare_worktree_flag_as_unnamed() {
        let worktree = Path::new("/repo-worktrees/restore-living-team");

        let scope = resume_worktree_scope_with(
            Some("  "),
            worktree,
            Path::new("/repo"),
            |_| -> Result<PathBuf> { panic!("bare -w must not resolve a generated worktree name") },
        )
        .expect("bare worktree flag scope");

        assert_eq!(scope.as_deref(), Some(worktree));
    }

    fn test_agent(id: &str) -> AgentState {
        rimz::testkit::agent_state("codex", id, jiff::Timestamp::UNIX_EPOCH)
    }
}
