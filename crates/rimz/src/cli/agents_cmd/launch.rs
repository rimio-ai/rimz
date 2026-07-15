//! Interactive launch orchestration and presentation.

use super::*;
use crate::cli::{machine_config, open_store, record_workspace};

pub(super) fn launch_layout(
    args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
) -> Result<()> {
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let machine_config = machine_config();
    let effective = rimz::config::effective::load(
        &machine_config.agents,
        &workspace.project_root,
        &rimz::store::paths::config_home(),
    )?;
    reject_prompt_that_looks_like_spec(
        args.spec.as_deref(),
        args.prompt.as_deref(),
        &effective.profiles,
        &machine_config.agents.commands,
        &effective.teams,
    )?;
    let preset = launch_override_preset(&args)?;
    let prepared = rimz::harness::plan::prepare_launch(
        &effective,
        &machine_config.agents.commands,
        args.spec.as_deref(),
        PrepareLaunchOptions {
            permission_mode: interactive_permission_mode_from_flags(args.ask, args.yolo)?,
            preset: &preset,
            passthrough: &args.passthrough,
            budget: args.budget,
            max_turns: args.max_turns,
        },
    )
    .inspect_err(|err| {
        for warning in err.warnings() {
            let _ = writeln!(std::io::stderr(), "{warning}");
        }
    })?;
    for warning in &prepared.warnings {
        writeln!(std::io::stderr(), "{warning}")?;
    }
    if args.name.is_some() && prepared.layout.agent_kinds().count() != 1 {
        bail!("--name requires a layout with exactly one agent cell");
    }
    let PreparedLaunch {
        teams,
        layout,
        team_name,
        warnings: _,
    } = prepared;
    let prompt = args
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
    if args.resume {
        let worktree_filter =
            resume_worktree_scope(args.worktree.as_deref(), &workspace, &machine_config)?;
        return launch_resume_layout(
            args,
            globals,
            allow_in_place,
            &workspace,
            &machine_config,
            &teams,
            layout,
            team_name,
            single_cell,
            worktree_filter.as_deref(),
        );
    }
    let worktree_launch = args.worktree.is_some() || args.from_pr.is_some();
    let channel_launch = args.channel.is_some();
    let placement = apply_in_place_downgrade(
        resolve_placement(
            args.new_tab,
            args.new_pane,
            machine_config.agents.placement,
            worktree_launch || channel_launch,
            single_cell,
            rimz::mux::ambient_pane_id().is_some(),
        )?,
        args.bg,
        allow_in_place,
    );
    let in_place = placement == Placement::SamePane;
    let mux = rimz::mux::auto_detect_backend(globals.mux)?;
    let room = RoomContext::from_resolved(
        &workspace,
        machine_config.clone(),
        mux,
        RoomSizing::OrdinaryTab,
    )?;
    let backend = room.backend();
    rimz::room::require_live_session(backend, &workspace.session_name)?;
    let store = open_store(&workspace)?;

    let explicit_worktree_name = args
        .worktree
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| rimz::worktree::parse_requested_name(name).map(|requested| requested.name))
        .transpose()?;
    let cells = cohort_cells(&layout);
    if let Some(name) = explicit_worktree_name.as_deref()
        && args.from_pr.is_none()
        && (team_name.is_some() || cells.len() >= 2)
    {
        let spec_display = args.spec.as_deref().unwrap_or("<spec>");
        match reconcile::reconcile_cohort_launch(
            &workspace,
            &machine_config,
            backend,
            &store,
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
                    &workspace,
                    &machine_config,
                    &teams,
                    layout,
                    team_name,
                    single_cell,
                    Some(&path),
                );
            }
            reconcile::Reconciled::Continue => {}
        }
    }

    let launch = rimz::worktree::resolve_launch_checkout(
        &workspace,
        &machine_config.agents.worktree,
        args.worktree.as_deref(),
        args.from_pr.as_ref(),
    )?;
    if let Some(channel) = args.channel.as_deref() {
        crate::cli::channel::ensure_named_channel_available(&workspace, channel)?;
        rimz::channel::register(store.paths(), channel)?;
    }
    let room_channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &launch.cwd,
        team_name.as_deref(),
        args.channel.as_deref(),
    );
    let launch_requests = launch_identity_requests(
        &layout,
        args.name.as_deref(),
        launch.generated_name(),
        team_name.as_deref(),
        team_name
            .as_deref()
            .and_then(|name| teams.0.get(name))
            .map(|team| team.roles.as_slice()),
        room_channel.as_deref(),
        prompt.zip(prompt_agent_index),
    )?;
    let launch_batch = store.begin_agent_launch_batch(
        &launch_requests,
        AgentLaunchScope {
            session_name: workspace.session_name.clone(),
            cwd: launch.cwd.clone(),
            worktree_name: launch.worktree_name.clone(),
            channel: room_channel.clone(),
            description: args.description.clone(),
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
    let panes = layout_panes_with_names(
        &layout,
        LayoutPaneParams {
            cwd: &cwd,
            prompt,
            prompt_agent_index,
            cleanup_worktree: worktree_launch,
            in_place,
            team: team_name.as_deref(),
            channel: room_channel.as_deref(),
            resume_seeds: None,
        },
        launch_batch.identities(),
    )?;
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let (open_result, what): (Result<()>, &str) = match placement {
        Placement::NewTab => (
            backend
                .open_tab(&TabOptions {
                    session_name: workspace.session_name.clone(),
                    title,
                    cwd: cwd.clone(),
                    panes,
                    focus: !args.bg,
                    dock_sidebar: true,
                    sidebar,
                })
                .map_err(Into::into),
            "opening agent tab",
        ),
        Placement::NewPane => (
            backend
                .split_pane(SplitPaneOptions {
                    session_name: None,
                    target_view_id: None,
                    target_pane_id: own_pane_id(mux),
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    command: Some(single_pane_argv(&panes)?),
                    title: None,
                    env: rimz::room::pane_identity_env(
                        &workspace,
                        room_channel.as_deref(),
                        !worktree_launch,
                    ),
                    stacked: false,
                    direction,
                    focus: !args.bg,
                })
                .map_err(Into::into),
            "splitting the agent into a new pane",
        ),
        Placement::SamePane => {
            // exec replaces this process with the wrapper, which binds the pane
            // and direct-execs the agent in place; returns only on failure.
            let argv = single_pane_argv(&panes)?;
            let err = exec_wrapper_in_place(
                &argv,
                rimz::room::pane_identity_env(
                    &workspace,
                    room_channel.as_deref(),
                    !worktree_launch,
                ),
                &cwd,
            );
            (Err(err), "running the agent in the current pane")
        }
    };
    if let Err(err) = open_result {
        let _ = store.fail_agent_launch_batch(&launch_batch);
        return Err(err).context(what);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn launch_resume_layout(
    args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
    workspace: &rimz::ResolvedWorkspace,
    machine_config: &rimz::config::MachineConfig,
    teams: &rimz::config::TeamsConfig,
    layout: LayoutSpec,
    team_name: Option<String>,
    single_cell: bool,
    worktree_filter: Option<&Path>,
) -> Result<()> {
    let store = open_store(workspace)?;
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
    let spec = args.spec.as_deref().unwrap_or("<spec>");
    let scope = worktree_filter.and_then(worktree_scope_label);
    let mut plan = rimz::harness::resume::plan_cohort_resume(
        &agents,
        rimz::store::runtime::agent_liveness,
        &cells,
        team_name.as_deref(),
        |path| path.is_dir(),
        rimz::harness::resume::resume_session_present,
    )
    .map_err(|err| cohort_resume_error(err, spec, scope.as_deref()))?;
    let cwd = plan
        .cwd
        .clone()
        .context("cohort resume matched no working directory")?;
    let channel = rimz::harness::target::resolve_room_channel(
        &workspace.project_root,
        &cwd,
        team_name.as_deref(),
        plan.channel.as_deref(),
    );
    plan.channel = channel.clone();
    let scoped_resume =
        channel.is_some() || (cwd != workspace.project_root && cwd != workspace.worktree_root);
    let placement = if single_cell {
        apply_in_place_downgrade(
            resolve_placement(
                args.new_tab,
                args.new_pane,
                machine_config.agents.placement,
                scoped_resume,
                single_cell,
                rimz::mux::ambient_pane_id().is_some(),
            )?,
            args.bg,
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
    record_workspace(workspace)?;

    let launch_requests = fresh_resume_launch_requests(
        &layout,
        &plan,
        team_name.as_deref(),
        team_name
            .as_deref()
            .and_then(|name| teams.0.get(name))
            .map(|team| team.roles.as_slice()),
        channel.as_deref(),
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
    let panes = layout_panes_with_names(
        &layout,
        LayoutPaneParams {
            cwd: &cwd,
            prompt: None,
            prompt_agent_index: None,
            cleanup_worktree: false,
            in_place,
            team: team_name.as_deref(),
            channel: channel.as_deref(),
            resume_seeds: Some(&plan.seeds),
        },
        launch_batch.identities(),
    )?;
    let direction = rimz::mux::detect_terminal_size()
        .map(|(cols, rows)| rimz::mux::split_along_longer_edge(cols, rows))
        .unwrap_or_default();
    let (open_result, what): (Result<()>, &str) = match placement {
        Placement::NewTab => (
            backend
                .open_tab(&TabOptions {
                    session_name: workspace.session_name.clone(),
                    title,
                    cwd: cwd.clone(),
                    panes,
                    focus: !args.bg,
                    dock_sidebar: true,
                    sidebar,
                })
                .map_err(Into::into),
            "opening agent tab",
        ),
        Placement::NewPane => (
            backend
                .split_pane(SplitPaneOptions {
                    session_name: None,
                    target_view_id: None,
                    target_pane_id: own_pane_id(mux),
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    command: Some(single_pane_argv(&panes)?),
                    title: None,
                    env: rimz::room::pane_identity_env(workspace, channel.as_deref(), false),
                    stacked: false,
                    direction,
                    focus: !args.bg,
                })
                .map_err(Into::into),
            "splitting the agent into a new pane",
        ),
        Placement::SamePane => {
            report_cohort_resume(&plan);
            let argv = single_pane_argv(&panes)?;
            let err = exec_wrapper_in_place(
                &argv,
                rimz::room::pane_identity_env(workspace, channel.as_deref(), false),
                &cwd,
            );
            (Err(err), "running the agent in the current pane")
        }
    };
    if let Err(err) = open_result {
        let _ = store.fail_agent_launch_batch(&launch_batch);
        return Err(err).context(what);
    }
    report_cohort_resume(&plan);
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

fn worktree_scope_label(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
}

fn cohort_resume_error(
    err: rimz::harness::resume::CohortResumeErr,
    spec: &str,
    scope: Option<&str>,
) -> anyhow::Error {
    let subject = cohort_resume_subject(spec, scope);
    match err {
        rimz::harness::resume::CohortResumeErr::NothingToResume { .. } => {
            anyhow::anyhow!("nothing to resume for {subject}; launch without `--resume`")
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

/// The one pane command of an in-pane launch (the resolver guarantees a single
/// cell before this is reached).
fn single_pane_argv(panes: &LayoutPanes) -> Result<Vec<String>> {
    panes
        .columns
        .first()
        .and_then(|column| column.panes.first())
        .map(|pane| pane.argv.clone())
        .context("in-pane launch produced no pane command")
}

#[cfg(unix)]
pub(super) fn exec_wrapper_in_place(
    argv: &[String],
    env: BTreeMap<String, String>,
    cwd: &Path,
) -> anyhow::Error {
    use std::os::unix::process::CommandExt;

    let Some((program, rest)) = argv.split_first() else {
        return anyhow::anyhow!("in-place launch produced no command");
    };
    let mut command = Command::new(program);
    command.args(rest).envs(&env).current_dir(cwd);
    command.exec().into()
}

#[cfg(not(unix))]
pub(super) fn exec_wrapper_in_place(
    _argv: &[String],
    _env: BTreeMap<String, String>,
    _cwd: &Path,
) -> anyhow::Error {
    anyhow::anyhow!("in-place launch is only supported on Unix")
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

pub(super) fn supervised_permission_mode_from_flags(
    ask: bool,
    yolo: bool,
) -> Result<PermissionMode> {
    if ask && yolo {
        bail!("choose at most one of --ask and --yolo");
    }
    Ok(if yolo {
        PermissionMode::Yolo
    } else if ask {
        PermissionMode::Ask
    } else {
        PermissionMode::Auto
    })
}

pub(super) fn reject_launch_flags_without_spec(args: &AgentsArgs) -> Result<()> {
    if !args.passthrough.is_empty() {
        bail!("missing agent spec before `--`");
    }
    if args.worktree.is_some() {
        bail!(
            "--worktree requires an agent spec; use `rimz agents list --worktree <name>` to filter cards"
        );
    }
    if args.channel.is_some() {
        bail!("--channel requires an agent spec; use `rimz channel list` to inspect channels");
    }
    if args.from_pr.is_some() {
        bail!("--from-pr requires an agent spec");
    }
    if args.name.is_some()
        || args.bg
        || args.new_pane
        || args.new_tab
        || args.resume
        || args.ask
        || args.yolo
        || args.print
        || args.effort.is_some()
        || args.budget.is_some()
        || args.model.is_some()
        || args.description.is_some()
        || args.system_prompt_file.is_some()
        || args.append_system_prompt_file.is_some()
        || args.max_turns.is_some()
        || args.retries.is_some()
    {
        bail!("agent launch options require an agent spec");
    }
    Ok(())
}

/// Build the launch-override preset from shared launch flags. Prompt files are
/// resolved to absolute paths and required to exist here, at the entry point,
/// rather than downstream in the agent.
pub(super) fn launch_override_preset(args: &AgentsArgs) -> Result<rimz::agents::LaunchPreset> {
    let system_prompt_file =
        resolve_launch_prompt_file(args.system_prompt_file.as_deref(), "--system-prompt-file")?;
    let append_system_prompt_file = resolve_launch_prompt_file(
        args.append_system_prompt_file.as_deref(),
        "--append-system-prompt-file",
    )?;
    Ok(rimz::agents::LaunchPreset {
        model: args
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .map(ToOwned::to_owned),
        effort: args
            .effort
            .as_deref()
            .map(str::trim)
            .filter(|effort| !effort.is_empty())
            .map(ToOwned::to_owned),
        system_prompt_file,
        append_system_prompt_file,
    })
}

fn resolve_launch_prompt_file(path: Option<&Path>, flag: &str) -> Result<Option<PathBuf>> {
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

pub(super) fn reject_prompt_that_looks_like_spec(
    spec: Option<&str>,
    prompt: Option<&str>,
    profiles: &rimz::config::ProfilesConfig,
    commands: &rimz::config::CommandsConfig,
    layouts: &rimz::config::TeamsConfig,
) -> Result<()> {
    let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
        return Ok(());
    };
    let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) else {
        return Ok(());
    };
    if rimz::harness::spec::is_known_spec_token(prompt, profiles, commands, layouts) {
        bail!(
            "prompt `{prompt}` looks like another spec cell; did you mean `rimz agents {spec},{prompt}`?"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
