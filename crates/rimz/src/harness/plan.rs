//! Resolve launch layouts from effective config, then compile them into launch
//! identities and backend-neutral pane commands.

use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

use crate::config::{LaunchPlacement, RoleBinding};
use crate::harness::budget::BudgetSpec;
use crate::harness::resume::{CohortCell, CohortResumePlan, CohortSeed};
use crate::harness::run::PermissionMode;
use crate::harness::spec::{AgentCell, Cell, LayoutSpec};
use crate::ids::{AgentKind, AgentSessionId, EventId};
use crate::mux::{LayoutColumn, LayoutPanes, PaneCmd};
use crate::store::{AgentLaunchIdentity, AgentLaunchName, AgentLaunchRequest};

#[derive(Clone, Copy, Debug)]
pub struct LaunchFinalizeOptions<'a> {
    pub permission_mode: Option<PermissionMode>,
    pub preset: &'a crate::agents::LaunchPreset,
    pub passthrough: &'a [String],
    pub budget: Option<BudgetSpec>,
    pub max_turns: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct ResolvedLaunch {
    pub teams: crate::config::TeamsConfig,
    pub layout: LayoutSpec,
    pub team_name: Option<String>,
}

/// Flattened parent stamp for one pane-backed agent launched by another agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchAncestry {
    pub parent_agent_id: AgentSessionId,
    pub parent_agent_kind: AgentKind,
    pub launch_depth: u8,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LaunchAncestryError {
    #[error(
        "launch refused: RimZ could not resolve the calling agent's durable launch identity, so it cannot safely verify the configured nesting limit. Launching another agent from here is not permitted; do not retry this command."
    )]
    UnresolvedCaller,
    #[error(
        "launch refused: this agent is at agent-launch nesting depth {current_depth}, so another agent would exceed this workspace's maximum of {max_depth}. Launching another agent from here is not permitted; do not retry this command."
    )]
    DepthExceeded { current_depth: u8, max_depth: u8 },
}

/// Resolve the flattened display parent while retaining true launch depth.
pub fn resolve_launch_ancestry(
    caller: Option<&crate::agents::AgentState>,
    top_level: bool,
    max_depth: u8,
) -> std::result::Result<Option<LaunchAncestry>, LaunchAncestryError> {
    let Some(caller) = caller.filter(|_| !top_level) else {
        return Ok(None);
    };
    let current_depth = caller.launch_depth.unwrap_or(0);
    if current_depth >= max_depth {
        return Err(LaunchAncestryError::DepthExceeded {
            current_depth,
            max_depth,
        });
    }
    Ok(Some(LaunchAncestry {
        parent_agent_id: caller
            .parent_agent_id
            .clone()
            .unwrap_or_else(|| caller.agent_id.clone()),
        parent_agent_kind: caller
            .parent_agent_kind
            .clone()
            .unwrap_or_else(|| caller.kind.clone()),
        launch_depth: current_depth.saturating_add(1),
    }))
}

/// Whether this process identifies itself as an agent caller. Human launches
/// and the explicit top-level escape can skip the audit projection entirely.
pub fn launch_ancestry_required(top_level: bool) -> bool {
    !top_level
        && std::env::var(crate::harness::run::ENV_AGENT_KIND)
            .ok()
            .is_some_and(|value| !value.is_empty())
}

/// Resolve the launching process through its stable launch id. Kind
/// corroborates the match so stale cross-provider environment cannot attach a
/// child to the wrong durable row. An agent process already running across an
/// upgrade has no launch id; only that missing-id case may use an unambiguous
/// live pane stamp as legacy identity.
pub fn resolve_launch_ancestry_from_env(
    agents: &[crate::agents::AgentState],
    top_level: bool,
    max_depth: u8,
) -> std::result::Result<Option<LaunchAncestry>, LaunchAncestryError> {
    if top_level {
        return Ok(None);
    }
    let Some(kind) = std::env::var(crate::harness::run::ENV_AGENT_KIND)
        .ok()
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };
    let launch_id = std::env::var(crate::harness::run::ENV_AGENT_ID)
        .ok()
        .filter(|value| !value.is_empty());
    let pane_id = launch_id
        .is_none()
        .then(crate::mux::ambient_pane_id)
        .flatten();
    let caller = resolve_launch_caller(agents, &kind, launch_id.as_deref(), pane_id.as_ref())?;
    resolve_launch_ancestry(Some(caller), false, max_depth)
}

fn resolve_launch_caller<'a>(
    agents: &'a [crate::agents::AgentState],
    kind: &str,
    launch_id: Option<&str>,
    pane_id: Option<&crate::ids::PaneId>,
) -> std::result::Result<&'a crate::agents::AgentState, LaunchAncestryError> {
    let caller = if let Some(launch_id) = launch_id {
        agents.iter().find(|agent| {
            agent.kind == kind
                && agent
                    .launch_id
                    .as_ref()
                    .is_some_and(|candidate| candidate == launch_id)
        })
    } else {
        let pane_id = pane_id.ok_or(LaunchAncestryError::UnresolvedCaller)?;
        let mut matches = agents.iter().filter(|agent| {
            agent.kind == kind
                && !agent.is_provider_subagent()
                && agent.ended_at.is_none()
                && agent
                    .pane
                    .as_ref()
                    .is_some_and(|pane| &pane.pane_id == pane_id)
        });
        let caller = matches.next();
        if matches.next().is_some() {
            return Err(LaunchAncestryError::UnresolvedCaller);
        }
        caller
    }
    .ok_or(LaunchAncestryError::UnresolvedCaller)?;
    Ok(caller)
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveLaunchError {
    #[error(transparent)]
    Layout(#[from] crate::harness::spec::LayoutErr),
    #[error(transparent)]
    Effective(#[from] crate::config::effective::EffectiveConfigErr),
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "{origin} {field} `{}` not found; create it or fix the launch config",
    path.display()
)]
pub struct ProfilePromptFileError {
    origin: String,
    field: &'static str,
    path: PathBuf,
}

/// Resolve effective profiles/teams without applying runtime launch options.
pub fn resolve_launch(
    launch: &crate::config::effective::LaunchAgents,
    commands: &crate::config::CommandsConfig,
    spec: Option<&str>,
    agent_override: Option<&str>,
) -> std::result::Result<ResolvedLaunch, ResolveLaunchError> {
    let layout = match crate::harness::spec::resolve_spec_with_agent_override(
        spec,
        &launch.profiles,
        commands,
        &launch.teams,
        agent_override,
    ) {
        Ok(layout) => layout,
        Err(err @ crate::harness::spec::LayoutErr::UnknownTeam { .. })
        | Err(err @ crate::harness::spec::LayoutErr::UnknownCell { .. }) => {
            launch.block_untrusted_reference(spec, commands)?;
            return Err(err.into());
        }
        Err(err) => return Err(err.into()),
    };
    let team_name = spec
        .and_then(|spec| crate::harness::spec::spec_team(spec, &launch.teams))
        .map(str::to_owned);
    Ok(ResolvedLaunch {
        teams: launch.teams.clone(),
        layout,
        team_name,
    })
}

/// Require the prompt files carried by finalized launch cells.
pub fn validate_profile_prompt_files(
    layout: &LayoutSpec,
) -> std::result::Result<(), ProfilePromptFileError> {
    for cell in layout.agent_cells() {
        validate_agent_prompt_files(cell)?;
    }
    Ok(())
}

fn validate_agent_prompt_files(
    cell: &AgentCell,
) -> std::result::Result<(), ProfilePromptFileError> {
    let origin = || match (cell.launch.role.as_deref(), cell.launch.profile.as_deref()) {
        (Some(role), Some(profile)) => format!("role `{role}` profile `{profile}`"),
        (Some(role), None) => format!("role `{role}`"),
        (None, Some(profile)) => format!("profile `{profile}`"),
        (None, None) => "agent cell".to_owned(),
    };
    if let Some(path) = cell.system_prompt_file.as_ref()
        && !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
    {
        return Err(ProfilePromptFileError {
            origin: origin(),
            field: "system-prompt-file",
            path: path.clone(),
        });
    }
    for path in &cell.append_system_prompt_files {
        if !std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()) {
            return Err(ProfilePromptFileError {
                origin: origin(),
                field: "append-system-prompt-files entry",
                path: path.clone(),
            });
        }
    }
    Ok(())
}

/// Normalize an optional launch preset override, dropping blank values.
pub fn normalized_preset_value(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

/// Reject a likely comma-separated spec typo parsed as a supervised prompt.
pub fn reject_prompt_that_looks_like_spec(
    spec: Option<&str>,
    prompt: Option<&str>,
    profiles: &crate::config::ProfilesConfig,
    commands: &crate::config::CommandsConfig,
    layouts: &crate::config::TeamsConfig,
) -> Result<()> {
    let Some(spec) = spec.map(str::trim).filter(|spec| !spec.is_empty()) else {
        return Ok(());
    };
    let Some(prompt) = prompt.map(str::trim).filter(|prompt| !prompt.is_empty()) else {
        return Ok(());
    };
    if crate::harness::spec::is_known_spec_token(prompt, profiles, commands, layouts) {
        bail!(
            "prompt `{prompt}` looks like another spec cell; did you mean `rimz agents {spec},{prompt}`?"
        );
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchFinalizeWarning {
    LaterModelWins {
        profile: String,
        setting: String,
        model: String,
    },
    DeclaredFieldWins {
        profile: String,
        setting: String,
        field: &'static str,
        value: String,
    },
    DeclaredPromptWins {
        profile: String,
        setting: String,
    },
}

impl fmt::Display for LaunchFinalizeWarning {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LaterModelWins {
                profile,
                setting,
                model,
            } => write!(
                formatter,
                "warning: profile `{profile}` args set {setting}; later model {model} wins"
            ),
            Self::DeclaredFieldWins {
                profile,
                setting,
                field,
                value,
            } => write!(
                formatter,
                "warning: profile `{profile}` args set {setting}; declared {field} {value} wins"
            ),
            Self::DeclaredPromptWins { profile, setting } => write!(
                formatter,
                "warning: profile `{profile}` args set {setting}; declared system prompt wins"
            ),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LaunchFinalizeError {
    #[error("unknown agent kind `{kind}`")]
    UnknownAdapter { kind: String },
    #[error(transparent)]
    PromptFile(#[from] ProfilePromptFileError),
    #[error(
        "{agent} does not support --{field}; remove it or put provider-specific flags in `args`"
    )]
    UnsupportedPresetField {
        agent: &'static str,
        field: &'static str,
    },
    #[error(
        "{agent} does not support config key `system-prompt-file` / flag `--system-prompt-file`; remove it or put provider-specific flags in `args`"
    )]
    UnsupportedSystemPrompt { agent: &'static str },
    #[error(
        "{agent} append-system-prompt-files requires a base system-prompt-file; add one to the profile, role, or launch command"
    )]
    MissingSystemPromptBase { agent: &'static str },
    #[error("{reason}")]
    PromptValidation { reason: String },
    #[error("{agent} does not support --max-turns")]
    UnsupportedMaxTurns {
        agent: &'static str,
        warnings: Vec<LaunchFinalizeWarning>,
    },
}

impl LaunchFinalizeError {
    pub fn warnings(&self) -> &[LaunchFinalizeWarning] {
        match self {
            Self::UnsupportedMaxTurns { warnings, .. } => warnings,
            Self::UnknownAdapter { .. }
            | Self::PromptFile(_)
            | Self::UnsupportedPresetField { .. }
            | Self::UnsupportedSystemPrompt { .. }
            | Self::MissingSystemPromptBase { .. }
            | Self::PromptValidation { .. } => &[],
        }
    }
}

/// Apply launch-wide CLI options and provider preset reconciliation to a
/// resolved layout. Command cells remain unchanged.
pub fn finalize_launch_layout(
    layout: &mut LayoutSpec,
    options: LaunchFinalizeOptions<'_>,
) -> std::result::Result<Vec<LaunchFinalizeWarning>, LaunchFinalizeError> {
    let mut warnings = Vec::new();
    for cell in layout.agent_cells_mut() {
        finalize_agent_cell(cell, options, &mut warnings)?;
    }

    if let Some(limit) = options.max_turns {
        for cell in layout.agent_cells_mut() {
            let adapter = crate::agents::find_definition(&cell.kind).ok_or_else(|| {
                LaunchFinalizeError::UnknownAdapter {
                    kind: cell.kind.to_string(),
                }
            })?;
            let Some(turn_args) = adapter.spec().launch.max_turns_args(limit) else {
                return Err(LaunchFinalizeError::UnsupportedMaxTurns {
                    agent: adapter.spec().kind,
                    warnings,
                });
            };
            cell.args.extend(turn_args);
        }
    }

    Ok(warnings)
}

fn finalize_agent_cell(
    cell: &mut AgentCell,
    options: LaunchFinalizeOptions<'_>,
    warnings: &mut Vec<LaunchFinalizeWarning>,
) -> std::result::Result<(), LaunchFinalizeError> {
    let adapter = crate::agents::find_definition(&cell.kind);
    if let Some(permission_mode) = options.permission_mode
        && cell.launch.mode.is_none()
        && let Some(adapter) = adapter
    {
        cell.args
            .extend(adapter.spec().launch.permission_args(permission_mode));
        cell.launch.mode = Some(permission_mode);
    }
    let mut overridden = Vec::new();
    if !options.preset.is_empty() {
        let adapter = adapter.ok_or_else(|| LaunchFinalizeError::UnknownAdapter {
            kind: cell.kind.to_string(),
        })?;
        if let Some(path) = options.preset.system_prompt_file.as_ref() {
            cell.system_prompt_file = Some(path.clone());
        }
        if !options.preset.append_system_prompt_files.is_empty() {
            cell.append_system_prompt_files
                .clone_from(&options.preset.append_system_prompt_files);
        }
        if options.preset.system_prompt_file.is_some()
            || !options.preset.append_system_prompt_files.is_empty()
        {
            overridden.push(crate::agents::PresetField::SystemPromptFile);
        }
        cell.args.extend(
            adapter
                .spec()
                .render_preset(options.preset)
                .map_err(unsupported_preset_error)?,
        );
        if let Some(model) = options
            .preset
            .model
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            cell.launch.model = Some(model.clone());
            overridden.push(crate::agents::PresetField::Model);
        }
        if let Some(effort) = options
            .preset
            .effort
            .as_ref()
            .filter(|value| !value.is_empty())
        {
            cell.launch.effort = Some(effort.clone());
            overridden.push(crate::agents::PresetField::Effort);
        }
    }
    validate_system_prompt_support(cell, adapter)?;
    validate_agent_prompt_files(cell)?;
    validate_system_prompt_text(cell)?;
    cell.args.extend(options.passthrough.iter().cloned());
    if let Some(budget) = options.budget {
        cell.launch.budget = Some(budget.to_string());
    }
    if let Some(adapter) = adapter {
        reconcile_preset_args(cell, adapter, &overridden, warnings)?;
        if cell.launch.model.is_none()
            && let Some(default) = adapter.default_launch_model()
        {
            cell.args.extend(
                adapter
                    .spec()
                    .render_preset(&crate::agents::LaunchPreset {
                        model: Some(default.clone()),
                        ..Default::default()
                    })
                    .map_err(unsupported_preset_error)?,
            );
            cell.launch.model = Some(default);
        }
    }
    Ok(())
}

/// Reconcile declared launch fields against the args a profile carries. Fields
/// in `overridden` were named on this command line: the CLI value replaces the
/// profile's arg silently, since the override is the user's stated intent.
fn reconcile_preset_args(
    cell: &mut AgentCell,
    adapter: &crate::agents::AgentDefinition,
    overridden: &[crate::agents::PresetField],
    warnings: &mut Vec<LaunchFinalizeWarning>,
) -> std::result::Result<(), LaunchFinalizeError> {
    use crate::agents::PresetField;

    let label = cell
        .launch
        .profile
        .as_deref()
        .unwrap_or(cell.kind.as_str())
        .to_owned();
    let declared = [
        (PresetField::Model, cell.launch.model.clone()),
        (PresetField::Effort, cell.launch.effort.clone()),
    ];

    for (field, value) in declared {
        let Some(matcher) = adapter.spec().launch.preset_arg_matcher(field) else {
            continue;
        };
        let occurrences = matcher.occurrences(&cell.args);
        let Some(declared_value) = value else {
            if field == PresetField::Model
                && let Some(winner) = occurrences.last()
            {
                for occurrence in &occurrences[..occurrences.len() - 1] {
                    if occurrence.value != winner.value {
                        warnings.push(LaunchFinalizeWarning::LaterModelWins {
                            profile: label.clone(),
                            setting: matcher.display_setting(&occurrence.value),
                            model: winner.value.clone(),
                        });
                    }
                }
                cell.launch.model = Some(winner.value.clone());
                remove_occurrences(&mut cell.args, &occurrences[..occurrences.len() - 1]);
            }
            continue;
        };
        if occurrences.len() <= 1 {
            continue;
        }
        for occurrence in &occurrences {
            if occurrence.value != declared_value && !overridden.contains(&field) {
                warnings.push(LaunchFinalizeWarning::DeclaredFieldWins {
                    profile: label.clone(),
                    setting: matcher.display_setting(&occurrence.value),
                    field: field.flag_name(),
                    value: declared_value.clone(),
                });
            }
        }
        let canonical = adapter
            .spec()
            .render_preset(&field.launch_preset(declared_value))
            .map_err(unsupported_preset_error)?;
        remove_occurrences(&mut cell.args, &occurrences);
        cell.args.extend(canonical);
    }

    if cell.system_prompt_file.is_some() || !cell.append_system_prompt_files.is_empty() {
        let matcher = adapter
            .spec()
            .launch
            .preset_arg_matcher(PresetField::SystemPromptFile)
            .expect("prompt support was validated before preset reconciliation");
        let occurrences = matcher.occurrences(&cell.args);
        if !overridden.contains(&PresetField::SystemPromptFile) {
            for occurrence in &occurrences {
                let matches_declared_path = cell.append_system_prompt_files.is_empty()
                    && cell
                        .system_prompt_file
                        .as_ref()
                        .is_some_and(|path| path.to_string_lossy() == occurrence.value);
                if !matches_declared_path {
                    warnings.push(LaunchFinalizeWarning::DeclaredPromptWins {
                        profile: label.clone(),
                        setting: matcher.display_setting(&occurrence.value),
                    });
                }
            }
        }
        remove_occurrences(&mut cell.args, &occurrences);
    }
    Ok(())
}

pub fn validate_system_prompt_support(
    cell: &AgentCell,
    adapter: Option<&crate::agents::AgentDefinition>,
) -> std::result::Result<(), LaunchFinalizeError> {
    if cell.system_prompt_file.is_none() && cell.append_system_prompt_files.is_empty() {
        return Ok(());
    }
    let adapter = adapter.ok_or_else(|| LaunchFinalizeError::UnknownAdapter {
        kind: cell.kind.to_string(),
    })?;
    if adapter
        .spec()
        .launch
        .preset_arg_matcher(crate::agents::PresetField::SystemPromptFile)
        .is_none()
    {
        return Err(LaunchFinalizeError::UnsupportedSystemPrompt {
            agent: adapter.spec().kind,
        });
    }
    if cell.system_prompt_file.is_none() {
        return Err(LaunchFinalizeError::MissingSystemPromptBase {
            agent: adapter.spec().kind,
        });
    }
    Ok(())
}

pub fn validate_system_prompt_text(
    cell: &AgentCell,
) -> std::result::Result<(), LaunchFinalizeError> {
    if cell.system_prompt_file.is_none() && cell.append_system_prompt_files.is_empty() {
        return Ok(());
    }
    crate::harness::prompt_compose::validate_text_prompt_size(
        &cell.kind,
        &crate::harness::prompt_compose::SystemPromptSources::from_cell(cell),
    )
    .map_err(|err| LaunchFinalizeError::PromptValidation {
        reason: err.to_string(),
    })?;
    Ok(())
}

fn remove_occurrences(args: &mut Vec<String>, occurrences: &[crate::agents::PresetArgOccurrence]) {
    for occurrence in occurrences.iter().rev() {
        args.drain(occurrence.argv_range.clone());
    }
}

fn unsupported_preset_error(err: crate::agents::PresetErr) -> LaunchFinalizeError {
    match err {
        crate::agents::PresetErr::UnsupportedField { agent, field } => {
            LaunchFinalizeError::UnsupportedPresetField { agent, field }
        }
    }
}

/// Reduce a resolved layout to the agent cells used by cohort resume matching.
pub fn cohort_cells(layout: &LayoutSpec) -> Vec<CohortCell> {
    layout
        .agent_cells()
        .map(|cell| CohortCell {
            kind: cell.kind.clone(),
            role: cell.launch.role.clone(),
        })
        .collect()
}

/// Where a launch lands. The resolver derives it from the per-launch flags, the
/// `[agents] placement` default, and whether in-pane placement is feasible here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placement {
    SamePane,
    NewPane,
    NewTab,
}

/// Resolve launch placement. Explicit flags win; otherwise the config policy
/// decides, with `auto` running a single non-worktree cell in the current pane
/// and opening a new tab for a worktree or multi-cell layout. In-pane placement
/// needs a single cell and a launching pane; an explicit `--new-pane` that
/// cannot be honored fails fast, while a defaulted one falls back to a new tab.
pub fn resolve_placement(
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

/// Resolve provider-native fork placement. A plain fork takes over the
/// launching pane; only explicit placement flags or `--bg` open another pane
/// or tab. The global launch-placement preference applies to fresh and resumed
/// layout launches, not this targeted agent operation.
pub fn resolve_fork_placement(
    new_tab: bool,
    new_pane: bool,
    bg: bool,
    has_launching_pane: bool,
) -> Result<Placement> {
    let placement = resolve_placement(
        new_tab,
        new_pane,
        LaunchPlacement::Auto,
        false,
        true,
        has_launching_pane,
    )?;
    Ok(apply_in_place_downgrade(placement, bg, true))
}

pub fn apply_in_place_downgrade(placement: Placement, bg: bool, allow_in_place: bool) -> Placement {
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

/// Compile one launch request per agent cell in layout order.
#[expect(
    clippy::too_many_arguments,
    reason = "one explicit identity-allocation boundary without a duplicate launch DTO"
)]
pub fn launch_identity_requests(
    layout: &LayoutSpec,
    explicit_name: Option<&str>,
    generated_worktree_name: Option<&str>,
    team: Option<&str>,
    team_roles: Option<&[RoleBinding]>,
    channel: Option<&str>,
    prompt: Option<(&str, usize)>,
    resume: Option<&CohortResumePlan>,
    ancestry: Option<&LaunchAncestry>,
) -> Result<Vec<AgentLaunchRequest>> {
    let agent_cells: Vec<&AgentCell> = layout.agent_cells().collect();
    let agent_count = agent_cells.len();
    if let Some(resume) = resume
        && resume.seeds.len() != agent_count
    {
        bail!(
            "resume plan has {} seeds for {agent_count} agent cells",
            resume.seeds.len()
        );
    }
    let inline_launch_group = (team.is_none() && agent_count >= 2).then(|| {
        resume
            .and_then(|plan| plan.launch_group.clone())
            .unwrap_or_else(mint_launch_group)
    });
    let mut requests = Vec::with_capacity(agent_cells.len());
    for (index, cell) in agent_cells.into_iter().enumerate() {
        if resume.is_some_and(|plan| matches!(plan.seeds[index], CohortSeed::Resume(_))) {
            continue;
        }
        let launch_ordinal = match team {
            Some(_) => cell
                .launch
                .role
                .as_deref()
                .and_then(|role| team_role_ordinal(team_roles, role)),
            None if inline_launch_group.is_some() => Some(index_to_launch_ordinal(index)),
            None => None,
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
        let mut launch = cell.launch.clone();
        launch.team = team.map(ToOwned::to_owned);
        launch.launch_group.clone_from(&inline_launch_group);
        launch.launch_ordinal = launch_ordinal;
        launch.channel = channel.map(ToOwned::to_owned);
        launch.kind_ordinal = None;
        if let Some(ancestry) = ancestry {
            launch.parent_agent_id = Some(ancestry.parent_agent_id.clone());
            launch.parent_agent_kind = Some(ancestry.parent_agent_kind.clone());
            launch.launch_depth = Some(ancestry.launch_depth);
        }
        requests.push(AgentLaunchRequest {
            kind: cell.kind.clone(),
            agent_id: mint_launch_id(),
            name,
            launch,
            run_id: None,
            prompt: prompt
                .filter(|(_, leader_index)| *leader_index == index)
                .map(|(prompt, _)| prompt.to_owned()),
        });
    }
    Ok(requests)
}

pub fn mint_launch_id() -> AgentSessionId {
    let raw = EventId::new();
    let suffix = raw.as_str().strip_prefix("evt_").unwrap_or(raw.as_str());
    AgentSessionId::from(format!("launch_{suffix}"))
}

fn mint_launch_group() -> String {
    mint_launch_id().to_string()
}

fn team_role_ordinal(team_roles: Option<&[RoleBinding]>, role: &str) -> Option<u32> {
    let index = team_roles?
        .iter()
        .position(|binding| binding.role == role)?;
    Some(index_to_launch_ordinal(index))
}

fn index_to_launch_ordinal(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX)
}

#[derive(Clone, Copy)]
pub struct LayoutPaneParams<'a> {
    pub cwd: &'a Path,
    pub cleanup_worktree: bool,
    pub in_place: bool,
    pub resume_seeds: Option<&'a [CohortSeed]>,
    pub launch_identities: &'a [AgentLaunchIdentity],
    pub fallback_channel: Option<&'a str>,
}

/// Compile backend-neutral pane commands for a resolved layout.
///
/// This is the single layout-to-pane boundary: it validates launch inputs,
/// keeps resumed and fresh agents aligned with their cells, and preserves
/// command cells in layout order.
pub fn compile_layout_panes(
    layout: &LayoutSpec,
    params: LayoutPaneParams<'_>,
) -> Result<LayoutPanes> {
    for cell in layout.agent_cells() {
        validate_system_prompt_support(cell, crate::agents::find_definition(cell.kind.as_str()))?;
    }
    validate_profile_prompt_files(layout)?;
    for cell in layout.agent_cells() {
        validate_system_prompt_text(cell)?;
    }
    let agent_count = layout.agent_cells().count();
    if let Some(seeds) = params.resume_seeds
        && seeds.len() != agent_count
    {
        bail!(
            "resume plan has {} seeds for {agent_count} agent cells",
            seeds.len()
        );
    }
    let rimz_bin = crate::proc::rimz_exe();
    let mut agent_index = 0usize;
    let mut launches = params.launch_identities.iter();
    let columns = layout
        .columns
        .iter()
        .map(|column| {
            let panes = column
                .rows
                .iter()
                .map(|cell| {
                    let pane = match cell {
                        Cell::Command { argv } if argv.is_empty() => PaneCmd {
                            argv: vec![crate::harness::launch::user_shell_program()],
                        },
                        Cell::Command { argv } => PaneCmd { argv: argv.clone() },
                        Cell::Agent(cell) => {
                            let seed = params.resume_seeds.map(|seeds| &seeds[agent_index]);
                            let pane = match seed {
                                Some(CohortSeed::Resume(agent)) => PaneCmd {
                                    // The layout already resolved this cell from its profile or
                                    // role binding, so the posture to replay is right here.
                                    argv: crate::harness::resume::resume_command(
                                        &rimz_bin,
                                        agent,
                                        params.fallback_channel,
                                        &crate::harness::resume::ResumePosture::from_cell(cell),
                                    ),
                                },
                                Some(CohortSeed::Fresh) | None => {
                                    let Some(launch) = launches.next() else {
                                        bail!(
                                            "launch plan missing identity for agent cell {agent_index}"
                                        );
                                    };
                                    fresh_agent_pane(cell, launch, &rimz_bin, params)?
                                }
                            };
                            agent_index = agent_index.saturating_add(1);
                            pane
                        }
                    };
                    Ok(pane)
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(LayoutColumn {
                panes,
                stacked: column.stacked,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    if launches.next().is_some() {
        bail!("launch plan has more identities than fresh agent cells");
    }
    Ok(LayoutPanes { columns })
}

fn fresh_agent_pane(
    cell: &AgentCell,
    launch: &AgentLaunchIdentity,
    rimz_bin: &Path,
    params: LayoutPaneParams<'_>,
) -> Result<PaneCmd> {
    validate_agent_name(&launch.name)?;
    Ok(PaneCmd {
        argv: crate::harness::launch::exec_argv(
            rimz_bin,
            &crate::harness::launch::ExecRequest {
                kind: cell.kind.clone(),
                action: crate::harness::launch::ExecAction::Launch {
                    prompt: launch.prompt.clone(),
                    extra_args: cell.args.clone(),
                },
                system_prompt_file: cell.system_prompt_file.clone(),
                append_system_prompt_files: cell.append_system_prompt_files.clone(),
                provider_account: crate::harness::launch::ProviderAccountState::Unbound,
                run_id: None,
                worktree_path: params.cleanup_worktree.then(|| params.cwd.to_path_buf()),
                close_pane_on_exit: !params.cleanup_worktree && !params.in_place,
                exit_on_run_completion: false,
                identity: crate::harness::launch::ExecIdentity {
                    name: Some(launch.name.clone()),
                    name_explicit: launch.name_explicit,
                    launch_id: Some(launch.agent_id.to_string()),
                    params: launch.launch.clone(),
                },
            },
        )?,
    })
}

pub fn validate_agent_name(name: &str) -> Result<()> {
    if !crate::harness::petname::valid_agent_name(name) {
        bail!("invalid agent name `{name}`; use ASCII letters, numbers, and `-`");
    }
    Ok(())
}

#[cfg(test)]
mod tests;
