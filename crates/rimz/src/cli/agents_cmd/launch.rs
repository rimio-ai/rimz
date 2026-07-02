use super::*;
use crate::cli::{
    agents_launch, build_sidebar_opts, machine_config, open_ledger, record_workspace,
};

pub(super) fn launch_layout(
    args: AgentsArgs,
    globals: &GlobalFlags,
    allow_in_place: bool,
) -> Result<()> {
    let spec = args.spec.as_deref();
    let workspace = WorkspaceResolver::resolve_participant(".", globals.root.clone())
        .context("resolving current workspace")?;
    let machine_config = machine_config();
    let profiles = effective_launch_profiles(&machine_config, &workspace)?;
    let teams = effective_launch_teams(&machine_config, &workspace)?;
    let mut layout = resolve_launch_layout(spec, &profiles, &teams, &machine_config, &workspace)?;
    let team_name = spec.and_then(|spec| rimz::harness::spec::spec_team(spec, &teams));
    reject_prompt_that_looks_like_spec(
        args.spec.as_deref(),
        args.prompt.as_deref(),
        &profiles,
        &machine_config.agents.commands,
        &teams,
    )?;
    ensure_profile_prompt_files(&layout)?;
    if args.name.is_some() && layout.agent_kinds().count() != 1 {
        bail!("--name requires a layout with exactly one agent cell");
    }
    apply_launch_mode_and_passthrough(
        &mut layout,
        interactive_permission_mode_from_flags(args.ask, args.yolo)?
            .map(LaunchModeApplication::explicit),
        &launch_override_preset(&args)?,
        &args.passthrough,
    )?;
    apply_default_launch_models(&mut layout)?;
    for kind in layout.agent_kinds() {
        agent_launch_env(&workspace.project_root, kind)?;
    }
    // Resolve where the launch lands before any side effect — the live-session
    // probe, worktree creation, the ledger append, the sidebar build — so an
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
    let backend = rimz::mux::backend_for(mux);
    agents_launch::ensure_live_session(backend.as_ref(), &workspace.session_name)?;
    record_workspace(&workspace)?;

    let mux_config = rimz::config::MultiplexerConfig::from(&machine_config);
    let width = rimz::mux::SidebarWidth::from_config(&machine_config.theme.display);
    let detected_size = rimz::mux::detect_terminal_size();
    let launch = agents_launch::resolve_cwd(
        &workspace,
        &machine_config.agents.worktree,
        args.worktree.as_deref(),
        args.from_pr.as_ref(),
    )?;
    let ledger = open_ledger(&workspace)?;
    if let Some(channel) = args.channel.as_deref() {
        crate::cli::channel::ensure_named_channel_available(&workspace, channel)?;
        rimz::channel::register(ledger.paths(), channel)?;
    }
    let launch_requests = launch_identity_requests(
        &layout,
        args.name.as_deref(),
        generated_worktree_name(&launch),
        team_name,
        args.channel.as_deref(),
    )?;
    let launch_identities = ledger.append_agent_launches_allocating(
        &launch_requests,
        &AgentLaunchAppend {
            workspace_id: workspace.workspace_id.clone(),
            session_name: workspace.session_name.clone(),
            cwd: launch.cwd.clone(),
            worktree_name: launch.worktree_name.clone(),
            channel: args.channel.clone(),
            prompt: args.prompt.clone(),
            description: args.description.clone(),
            state: rimz::schema::event::AgentLaunchState::Starting,
            pane_id: None,
        },
    )?;
    let worktree_name = launch.worktree_name.clone();
    let cwd = launch.cwd;
    let title = args.channel.as_deref().map_or_else(
        || {
            rimz::harness::spec::default_tab_title(
                &layout,
                &cwd,
                worktree_name.as_deref(),
                team_name,
            )
        },
        |channel| format!("#{channel}"),
    );
    let room = RoomTarget {
        workspace_id: &workspace.workspace_id,
        project_root: &workspace.project_root,
        session_name: &workspace.session_name,
        cwd: &cwd,
        mux_config: &mux_config,
        width,
        detected_size,
        refresh_ms: None,
    };
    let sidebar = build_sidebar_opts(&room, Vec::new())?;
    let panes = layout_panes_with_names(
        &layout,
        LayoutPaneParams {
            cwd: &cwd,
            prompt: args.prompt.as_deref(),
            cleanup_worktree: worktree_launch,
            in_place,
            team: team_name,
            channel: args.channel.as_deref(),
        },
        &launch_identities,
    )?;
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
                    target_pane_id: own_pane_id(mux),
                    cwd: Some(cwd.to_string_lossy().into_owned()),
                    command: Some(single_pane_argv(&panes)?),
                    env: agents_launch::launch_identity_env(
                        &workspace,
                        args.channel.as_deref(),
                        !worktree_launch,
                    ),
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
                agents_launch::launch_identity_env(
                    &workspace,
                    args.channel.as_deref(),
                    !worktree_launch,
                ),
                &cwd,
            );
            (Err(err), "running the agent in the current pane")
        }
    };
    if let Err(err) = open_result {
        let _ = append_launch_events(
            &ledger,
            &workspace,
            &launch_identities,
            LaunchEventParams {
                cwd: &cwd,
                worktree_name: worktree_name.as_deref(),
                channel: args.channel.as_deref(),
                prompt: args.prompt.as_deref(),
                state: rimz::schema::event::AgentLaunchState::Failed,
                pane_id: None,
            },
        );
        return Err(err).context(what);
    }
    Ok(())
}

/// Where a launch lands. The resolver derives it from the per-launch flags, the
/// `[agents] placement` default, and whether in-pane placement is feasible here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Placement {
    SamePane,
    NewPane,
    NewTab,
}

/// Resolve launch placement. Explicit flags win; otherwise the config policy
/// decides, with `auto` running a single non-worktree cell in the current pane
/// and opening a new tab for a worktree or multi-cell layout. In-pane placement
/// needs a single cell and a launching pane; an explicit `--new-pane` that
/// cannot be honored fails fast, while a defaulted one falls back to a new tab.
pub(super) fn resolve_placement(
    new_tab: bool,
    new_pane: bool,
    policy: LaunchPlacement,
    is_worktree: bool,
    single_cell: bool,
    has_launching_pane: bool,
) -> Result<Placement> {
    if new_tab {
        return Ok(Placement::NewTab);
    }
    if new_pane {
        if !single_cell {
            bail!(
                "--new-pane opens a single agent cell; a multi-cell layout opens a new tab — drop --new-pane or pass --new-tab"
            );
        }
        if !has_launching_pane {
            bail!(
                "--new-pane splits the current pane, so run it from inside the room; drop it to open a new tab"
            );
        }
        return Ok(Placement::NewPane);
    }
    Ok(match policy {
        LaunchPlacement::Tab => Placement::NewTab,
        LaunchPlacement::Pane if is_worktree => Placement::NewTab,
        LaunchPlacement::Pane => {
            feasible_or_new(Placement::NewPane, single_cell, has_launching_pane)
        }
        LaunchPlacement::Auto if is_worktree => Placement::NewTab,
        LaunchPlacement::Auto => {
            feasible_or_new(Placement::SamePane, single_cell, has_launching_pane)
        }
    })
}

pub(super) fn apply_in_place_downgrade(
    placement: Placement,
    bg: bool,
    allow_in_place: bool,
) -> Placement {
    // In-place takes over the launching pane: it cannot honor --bg, and
    // create-on-miss must never replace the caller's pane. Downgrade to a split.
    if placement == Placement::SamePane && (bg || !allow_in_place) {
        Placement::NewPane
    } else {
        placement
    }
}

/// In-pane placement (same pane or new pane) needs a single cell and a
/// launching pane to take over or split; otherwise fall back to a new tab.
fn feasible_or_new(target: Placement, single_cell: bool, has_launching_pane: bool) -> Placement {
    if single_cell && has_launching_pane {
        target
    } else {
        Placement::NewTab
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
fn exec_wrapper_in_place(
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
fn exec_wrapper_in_place(
    _argv: &[String],
    _env: BTreeMap<String, String>,
    _cwd: &Path,
) -> anyhow::Error {
    anyhow::anyhow!("in-place launch is only supported on Unix")
}

/// Trust-gated `[[agents]]` env for an agent launch, ready to inject into the
/// agent process. A closed trust gate fails here — at the entry point — with
/// the fix, so an agent never launches without the env the project declares.
pub(super) fn agent_launch_env(
    project_root: &Path,
    kind: &str,
) -> Result<BTreeMap<String, String>> {
    use rimz::trust::{AgentEnv, TrustState};
    match rimz::trust::agent_env(project_root, kind)? {
        AgentEnv::Apply(env) => {
            validate_agent_launch_env(kind, &env)?;
            Ok(env)
        }
        AgentEnv::Unconfigured => Ok(BTreeMap::new()),
        AgentEnv::Blocked(state) => {
            let fix = match state {
                TrustState::Stale => {
                    "the executable surface changed since the grant; review it and rerun `rimz trust grant`"
                }
                _ => "run `rimz trust grant` to apply it",
            };
            bail!(
                "agent `{kind}` env is configured in {root}/.rimz/config.toml but the project is {state}; {fix}",
                root = project_root.display(),
                state = state.as_str(),
            )
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(super) struct AgentLaunchEnvIdentity<'a> {
    pub(super) run_id: Option<&'a rimz::RunId>,
    pub(super) agent_name: Option<&'a str>,
    pub(super) agent_profile: Option<&'a str>,
    pub(super) agent_role: Option<&'a str>,
    pub(super) agent_team: Option<&'a str>,
    pub(super) agent_channel: Option<&'a str>,
    pub(super) agent_model: Option<&'a str>,
    pub(super) agent_effort: Option<&'a str>,
}

pub(super) fn full_agent_launch_env(
    project_root: &Path,
    adapter: &dyn AgentAdapter,
    rtk: rimz::config::RtkMode,
    transcript_file_days: u32,
    identity: AgentLaunchEnvIdentity<'_>,
) -> Result<BTreeMap<String, String>> {
    let kind = adapter.descriptor().kind;
    let mut env = agent_launch_env(project_root, kind)?;
    for (key, value) in adapter.launch_env() {
        env.insert(key.to_owned(), value.to_owned());
    }
    env.insert(
        rimz::harness::run::ENV_AGENT_KIND.to_owned(),
        kind.to_owned(),
    );
    if let Some(run_id) = identity.run_id {
        env.insert(
            rimz::harness::run::ENV_RUN_ID.to_owned(),
            run_id.as_str().to_owned(),
        );
    }
    if let Some(agent_name) = identity.agent_name {
        env.insert(
            rimz::harness::run::ENV_AGENT_NAME.to_owned(),
            agent_name.to_owned(),
        );
    }
    if let Some(agent_profile) = identity.agent_profile {
        env.insert(
            rimz::harness::run::ENV_AGENT_PROFILE.to_owned(),
            agent_profile.to_owned(),
        );
    }
    if let Some(agent_role) = identity.agent_role {
        env.insert(
            rimz::harness::run::ENV_AGENT_ROLE.to_owned(),
            agent_role.to_owned(),
        );
    }
    if let Some(agent_team) = identity.agent_team {
        env.insert(
            rimz::harness::run::ENV_TEAM.to_owned(),
            agent_team.to_owned(),
        );
    }
    if let Some(agent_channel) = identity.agent_channel {
        env.insert(
            rimz::harness::run::ENV_CHANNEL.to_owned(),
            agent_channel.to_owned(),
        );
    }
    if let Some(agent_model) = identity.agent_model {
        env.insert(
            rimz::harness::run::ENV_AGENT_MODEL.to_owned(),
            agent_model.to_owned(),
        );
    }
    if let Some(agent_effort) = identity.agent_effort {
        env.insert(
            rimz::harness::run::ENV_AGENT_EFFORT.to_owned(),
            agent_effort.to_owned(),
        );
    }
    env.insert(
        rimz::harness::run::ENV_RTK.to_owned(),
        rtk.as_str().to_owned(),
    );
    env.insert(
        rimz::harness::run::ENV_TRANSCRIPT_FILE_DAYS.to_owned(),
        transcript_file_days.to_string(),
    );
    validate_agent_launch_env(kind, &env)?;
    Ok(env)
}

pub(super) fn validate_agent_launch_env(kind: &str, env: &BTreeMap<String, String>) -> Result<()> {
    if let Some(key) = rimz::harness::launch::invalid_env_key(env) {
        bail!(
            "agent `{kind}` launch env key `{key}` is invalid; environment variable names must be non-empty, cannot contain `=`, and cannot start with `-`",
        );
    }
    Ok(())
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
) -> Result<LaunchModeApplication> {
    if ask && yolo {
        bail!("choose at most one of --ask and --yolo");
    }
    let mode = if yolo {
        LaunchModeApplication::explicit(PermissionMode::Yolo)
    } else if ask {
        LaunchModeApplication::explicit(PermissionMode::Ask)
    } else {
        LaunchModeApplication::implicit_default(PermissionMode::Auto)
    };
    Ok(mode)
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
        || args.ask
        || args.yolo
        || args.print
        || args.effort.is_some()
        || args.model.is_some()
        || args.description.is_some()
        || args.system_prompt_file.is_some()
        || args.append_system_prompt_file.is_some()
        || args.max_turns.is_some()
    {
        bail!("agent launch options require an agent spec");
    }
    Ok(())
}

pub(super) fn effective_launch_profiles(
    machine_config: &rimz::config::MachineConfig,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<rimz::config::ProfilesConfig> {
    rimz::config::effective::effective_profiles(
        &machine_config.agents.profiles,
        &workspace.project_root,
        &rimz::ledger::paths::config_home(),
    )
    .map_err(Into::into)
}

pub(super) fn effective_launch_teams(
    machine_config: &rimz::config::MachineConfig,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<rimz::config::TeamsConfig> {
    rimz::config::effective::effective_teams(
        &machine_config.agents.teams,
        &workspace.project_root,
        &rimz::ledger::paths::config_home(),
    )
    .map_err(Into::into)
}

pub(super) fn resolve_launch_layout(
    spec: Option<&str>,
    profiles: &rimz::config::ProfilesConfig,
    teams: &rimz::config::TeamsConfig,
    machine_config: &rimz::config::MachineConfig,
    workspace: &rimz::ResolvedWorkspace,
) -> Result<LayoutSpec> {
    match rimz::harness::spec::resolve_spec(spec, profiles, &machine_config.agents.commands, teams)
    {
        Ok(layout) => Ok(layout),
        Err(err @ rimz::harness::spec::LayoutErr::UnknownTeam { .. })
        | Err(err @ rimz::harness::spec::LayoutErr::UnknownCell { .. }) => {
            rimz::config::effective::block_untrusted_profile_reference(
                spec,
                profiles,
                &machine_config.agents.commands,
                teams,
                &workspace.project_root,
                &rimz::ledger::paths::config_home(),
            )?;
            Err(err.into())
        }
        Err(err) => Err(err.into()),
    }
}

/// Confirm every profile the resolved layout launches has its prompt files
/// present, so a missing prompt fails here — at the launch entry point, with
/// the absolute path to fix — rather than reaching the agent. This mirrors the
/// explicit prompt-file checks; the profile paths are already resolved against
/// the config file at load, so unrelated config reads stay IO-free.
pub(super) fn ensure_profile_prompt_files(layout: &LayoutSpec) -> Result<()> {
    for cell in layout.columns.iter().flat_map(|column| &column.rows) {
        let Cell::Agent {
            profile,
            role,
            system_prompt_file,
            append_system_prompt_file,
            ..
        } = cell
        else {
            continue;
        };
        for (field, path) in [
            ("system-prompt-file", system_prompt_file.as_ref()),
            (
                "append-system-prompt-file",
                append_system_prompt_file.as_ref(),
            ),
        ] {
            let Some(path) = path else {
                continue;
            };
            if path.is_file() {
                continue;
            }
            let source = match (role.as_deref(), profile.as_deref()) {
                (Some(role), Some(profile)) => {
                    format!("role `{role}` profile `{profile}`")
                }
                (Some(role), None) => format!("role `{role}`"),
                (None, Some(profile)) => format!("profile `{profile}`"),
                (None, None) => "agent cell".to_owned(),
            };
            bail!(
                "{source} {field} `{}` not found; create it or fix the launch config",
                path.display()
            );
        }
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

pub(super) fn apply_launch_mode_and_passthrough(
    layout: &mut LayoutSpec,
    mode: Option<LaunchModeApplication>,
    preset: &rimz::agents::LaunchPreset,
    passthrough: &[String],
) -> Result<()> {
    for column in &mut layout.columns {
        for cell in &mut column.rows {
            let Cell::Agent {
                kind,
                args,
                mode: cell_mode,
                model,
                effort,
                ..
            } = cell
            else {
                continue;
            };
            let adapter = rimz::agents::find_adapter(kind);
            if let Some(application) = mode
                && application.applies_to(*cell_mode)
                && let Some(adapter) = adapter
            {
                args.extend(adapter.permission_args(application.mode));
                *cell_mode = Some(application.mode);
            }
            if !preset.is_empty() {
                let adapter = adapter
                    .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", kind.as_str()))?;
                args.extend(adapter.render_preset(preset).map_err(launch_option_error)?);
                if let Some(preset_model) = preset.model.as_ref().filter(|model| !model.is_empty())
                {
                    *model = Some(preset_model.clone());
                }
                if let Some(preset_effort) =
                    preset.effort.as_ref().filter(|effort| !effort.is_empty())
                {
                    *effort = Some(preset_effort.clone());
                }
            }
            args.extend(passthrough.iter().cloned());
        }
    }
    Ok(())
}

pub(super) fn apply_supervised_turn_limit(layout: &mut LayoutSpec, limit: u32) -> Result<()> {
    for column in &mut layout.columns {
        for cell in &mut column.rows {
            let Cell::Agent { kind, args, .. } = cell else {
                continue;
            };
            let adapter = rimz::agents::find_adapter(kind)
                .ok_or_else(|| anyhow::anyhow!("unknown agent kind `{}`", kind.as_str()))?;
            let turn_args = adapter.max_turns_args(limit).ok_or_else(|| {
                anyhow::anyhow!("{} does not support --max-turns", adapter.descriptor().kind)
            })?;
            args.extend(turn_args);
        }
    }
    Ok(())
}

/// Fill each agent cell's launch model with the adapter's default when the spec
/// left it unset. The default is rendered as a real provider argv preset and
/// carried as the Rimz identity model, so a fresh card names a model
/// immediately and the agent runs that model.
pub(super) fn apply_default_launch_models(layout: &mut LayoutSpec) -> Result<()> {
    for column in &mut layout.columns {
        for cell in &mut column.rows {
            let Cell::Agent {
                kind, args, model, ..
            } = cell
            else {
                continue;
            };
            if model.is_some() {
                continue;
            }
            let Some(adapter) = rimz::agents::find_adapter(kind) else {
                continue;
            };
            let Some(default) = adapter.default_launch_model() else {
                continue;
            };
            let preset = rimz::agents::LaunchPreset {
                model: Some(default.clone()),
                ..Default::default()
            };
            args.extend(
                adapter
                    .render_preset(&preset)
                    .map_err(launch_option_error)?,
            );
            *model = Some(default);
        }
    }
    Ok(())
}

/// Map an unsupported-preset failure onto a CLI-shaped message naming the flag.
fn launch_option_error(err: rimz::agents::PresetErr) -> anyhow::Error {
    match err {
        rimz::agents::PresetErr::UnsupportedField { agent, field } => {
            anyhow::anyhow!("{agent} does not support --{field}")
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct LaunchModeApplication {
    pub(super) mode: PermissionMode,
}

impl LaunchModeApplication {
    pub(super) fn explicit(mode: PermissionMode) -> Self {
        Self { mode }
    }

    pub(super) fn implicit_default(mode: PermissionMode) -> Self {
        Self { mode }
    }

    pub(super) fn applies_to(self, cell_mode: Option<PermissionMode>) -> bool {
        cell_mode.is_none()
    }
}

pub(super) fn generated_worktree_name(launch: &agents_launch::ResolvedCwd) -> Option<&str> {
    launch
        .generated_worktree
        .then_some(launch.worktree_name.as_deref())?
}

pub(super) fn launch_identity_requests(
    layout: &LayoutSpec,
    explicit_name: Option<&str>,
    generated_worktree_name: Option<&str>,
    team: Option<&str>,
    channel: Option<&str>,
) -> Result<Vec<AgentLaunchRequest>> {
    let agent_cells: Vec<&Cell> = layout.agent_cells().collect();
    let agent_count = agent_cells.len();
    let mut requests = Vec::with_capacity(agent_cells.len());
    for (index, cell) in agent_cells.into_iter().enumerate() {
        let Cell::Agent {
            kind,
            profile,
            role,
            model,
            effort,
            ..
        } = cell
        else {
            continue;
        };
        let name = if agent_count == 1 && index == 0 {
            match explicit_name {
                Some(name) => {
                    validate_agent_name(name)?;
                    AgentLaunchName::Explicit(name.to_owned())
                }
                None => generated_worktree_name
                    .map(|name| AgentLaunchName::Soft(name.to_owned()))
                    .unwrap_or(AgentLaunchName::Mint),
            }
        } else {
            AgentLaunchName::Mint
        };
        requests.push(AgentLaunchRequest {
            kind: (*kind).clone(),
            agent_id: mint_launch_id(),
            name,
            profile: profile.clone(),
            role: role.clone(),
            model: model.clone(),
            effort: effort.clone(),
            team: team.map(ToOwned::to_owned),
            channel: channel.map(ToOwned::to_owned),
            run_id: None,
        });
    }
    Ok(requests)
}

pub(super) fn mint_launch_id() -> AgentSessionId {
    let raw = EventId::new();
    let suffix = raw.as_str().strip_prefix("evt_").unwrap_or(raw.as_str());
    AgentSessionId::from(format!("launch_{suffix}"))
}

pub(super) fn append_launch_events(
    ledger: &rimz::Ledger,
    workspace: &rimz::ResolvedWorkspace,
    identities: &[LaunchIdentity],
    params: LaunchEventParams<'_>,
) -> Result<()> {
    for identity in identities {
        append_launch_event(ledger, workspace, identity, params.clone())?;
    }
    Ok(())
}

pub(super) fn append_launch_event(
    ledger: &rimz::Ledger,
    workspace: &rimz::ResolvedWorkspace,
    identity: &LaunchIdentity,
    params: LaunchEventParams<'_>,
) -> Result<()> {
    let runtime_owner = params.pane_id.as_ref().map(|_| {
        rimz::ledger::runtime::current_process_owner(
            rimz::pane::RuntimeOwnerKind::Agent,
            identity.agent_id.as_str(),
        )
    });
    let event = rimz::schema::event::EventEnvelope::agent_launched(
        workspace.workspace_id.clone(),
        workspace.session_name.clone(),
        &identity.kind,
        rimz::schema::event::AgentLaunchPayload {
            agent_id: identity.agent_id.clone(),
            agent_name: identity.name.clone(),
            profile: identity.profile.clone(),
            role: identity.role.clone(),
            model: identity.model.clone(),
            effort: identity.effort.clone(),
            team: identity.team.clone(),
            channel: identity
                .channel
                .clone()
                .or_else(|| params.channel.map(ToOwned::to_owned)),
            kind_ordinal: None,
            state: params.state,
            run_id: identity.run_id.clone(),
            pane_id: params.pane_id,
            runtime_owner,
            worktree_path: Some(params.cwd.to_string_lossy().into_owned()),
            worktree_branch: params.worktree_name.map(ToOwned::to_owned),
            prompt: params
                .prompt
                .filter(|prompt| !prompt.trim().is_empty())
                .map(ToOwned::to_owned),
            description: None,
        },
    );
    ledger.append_event(&event)?;
    Ok(())
}

pub(super) fn layout_panes_with_names(
    layout: &LayoutSpec,
    params: LayoutPaneParams<'_>,
    launch_identities: &[LaunchIdentity],
) -> Result<LayoutPanes> {
    let rimz_bin = std::env::current_exe().context("locating the rimz executable")?;
    let mut agent_index = 0usize;
    let columns = layout
        .columns
        .iter()
        .map(|column| {
            let panes = column
                .rows
                .iter()
                .map(|cell| {
                    let launch = if matches!(cell, Cell::Agent { .. }) {
                        let launch = launch_identities.get(agent_index);
                        agent_index = agent_index.saturating_add(1);
                        launch
                    } else {
                        None
                    };
                    pane_cmd_with_name(
                        cell,
                        PaneCmdOptions {
                            rimz_bin: &rimz_bin,
                            cwd: params.cwd,
                            prompt: params.prompt,
                            cleanup_worktree: params.cleanup_worktree,
                            in_place: params.in_place,
                            team: params.team,
                            channel: params.channel,
                            launch,
                        },
                    )
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(LayoutColumn {
                panes,
                stacked: column.stacked,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(LayoutPanes { columns })
}

#[derive(Clone, Copy)]
pub(super) struct LayoutPaneParams<'a> {
    pub cwd: &'a Path,
    pub prompt: Option<&'a str>,
    pub cleanup_worktree: bool,
    pub in_place: bool,
    pub team: Option<&'a str>,
    pub channel: Option<&'a str>,
}

pub(super) struct PaneCmdOptions<'a> {
    pub rimz_bin: &'a Path,
    pub cwd: &'a Path,
    pub prompt: Option<&'a str>,
    pub cleanup_worktree: bool,
    pub in_place: bool,
    pub team: Option<&'a str>,
    pub channel: Option<&'a str>,
    pub launch: Option<&'a LaunchIdentity>,
}

pub(super) fn pane_cmd_with_name(cell: &Cell, options: PaneCmdOptions<'_>) -> Result<PaneCmd> {
    let argv = match cell {
        Cell::Command { argv } if argv.is_empty() => {
            vec![rimz::harness::launch::user_shell_program()]
        }
        Cell::Command { argv } => argv.clone(),
        Cell::Agent {
            kind,
            args,
            profile,
            role,
            model,
            effort,
            ..
        } => {
            let mut argv = vec![
                options.rimz_bin.to_string_lossy().into_owned(),
                "agents".to_owned(),
                "exec".to_owned(),
                kind.as_str().to_owned(),
            ];
            if !options.cleanup_worktree && !options.in_place {
                argv.push("--close-pane-on-exit".to_owned());
            }
            if let Some(profile) = profile {
                argv.extend(["--agent-profile".to_owned(), profile.clone()]);
            }
            if let Some(role) = role {
                argv.extend(["--agent-role".to_owned(), role.clone()]);
            }
            if let Some(team) = options.team {
                argv.extend(["--agent-team".to_owned(), team.to_owned()]);
            }
            if let Some(channel) = options.channel {
                argv.extend(["--agent-channel".to_owned(), channel.to_owned()]);
            }
            if let Some(model) = model {
                argv.extend(["--agent-model".to_owned(), model.clone()]);
            }
            if let Some(effort) = effort {
                argv.extend(["--agent-effort".to_owned(), effort.clone()]);
            }
            if let Some(launch) = options.launch {
                validate_agent_name(&launch.name)?;
                argv.extend(["--agent-name".to_owned(), launch.name.clone()]);
                argv.extend([
                    "--launch-id".to_owned(),
                    launch.agent_id.as_str().to_owned(),
                ]);
            }
            if options.cleanup_worktree {
                argv.extend([
                    "--worktree-path".to_owned(),
                    options.cwd.to_string_lossy().into_owned(),
                ]);
            }
            if let Some(prompt) = options.prompt.filter(|value| !value.is_empty()) {
                argv.extend(["--prompt".to_owned(), prompt.to_owned()]);
            }
            if !args.is_empty() {
                argv.push("--".to_owned());
                argv.extend(args.iter().cloned());
            }
            argv
        }
    };
    Ok(PaneCmd { argv })
}

pub(super) fn validate_agent_name(name: &str) -> Result<()> {
    if !valid_agent_name_candidate(name) {
        bail!("invalid agent name `{name}`; use ASCII letters, numbers, and `-`");
    }
    Ok(())
}

pub(super) fn valid_agent_name_candidate(name: &str) -> bool {
    rimz::harness::petname::valid_name(name)
        && !rimz::harness::petname::collides_with_reserved_prefix(name, rimz::agents::known_kinds())
}
