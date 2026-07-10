//! Backend-neutral agent layout IR plus team/profile/command resolution.
//!
//! Commas split columns, plus signs tile rows within a column, slashes stack
//! rows within a column, and each cell is a profile, a command, or a built-in
//! cell. Named teams compile to one column per role unless they declare an
//! explicit role-first layout shape. Built-ins provide `term`, every registered
//! agent kind, and `<kind>-<mode>` / `<kind>-ping` virtual variants; per-machine
//! `[agents.profiles]` entries can specialize agent cells and `[agents.commands]`
//! entries provide raw command panes.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::config::{CommandsConfig, Profile, ProfilesConfig, RoleBinding, Team, TeamsConfig};
use crate::harness::run::PermissionMode;
use crate::ids::AgentKind;

const BUILTIN_PEER: &str = "claude,codex";
const PERMISSION_MODE_NAMES: &[&str] = &["auto", "ask", "yolo", "plan"];
const PING_SUFFIX: &str = "ping";
const RESERVED_PROFILE_COMMAND_AND_TEAM_NAMES: &[&str] = &[
    "list", "ls", "show", "stop", "focus", "fork", "wait", "term", "exec",
];
pub const MAX_PROFILE_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSpec {
    pub columns: Vec<Column>,
}

impl LayoutSpec {
    pub fn single(cell: Cell) -> Self {
        Self {
            columns: vec![Column {
                rows: vec![cell],
                stacked: false,
            }],
        }
    }

    /// Every agent cell, in layout order (duplicates included).
    pub fn agent_cells(&self) -> impl Iterator<Item = &Cell> {
        self.columns
            .iter()
            .flat_map(|column| column.rows.iter())
            .filter(|cell| matches!(cell, Cell::Agent { .. }))
    }

    /// Every agent cell's kind, in layout order (duplicates included).
    pub fn agent_kinds(&self) -> impl Iterator<Item = &str> {
        self.agent_cells().filter_map(|cell| match cell {
            Cell::Agent { kind, .. } => Some(kind.as_str()),
            Cell::Command { .. } => None,
        })
    }

    pub fn first_agent_kind(&self) -> Option<&str> {
        self.agent_kinds().next()
    }

    pub fn has_agent(&self) -> bool {
        self.first_agent_kind().is_some()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub rows: Vec<Cell>,
    pub stacked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Agent {
        kind: AgentKind,
        args: Vec<String>,
        mode: Option<PermissionMode>,
        system_prompt_file: Option<PathBuf>,
        /// The profile or role prompt file whose contents append to the
        /// adapter's base system prompt. Launch pre-flight checks it before
        /// spawning the pane.
        append_system_prompt_file: Option<PathBuf>,
        /// The `[agents.profiles]` name this cell launched as, when it came
        /// from a named profile (`planner`) or a kind-default override
        /// (`claude`, `claude-auto`, `claude-ping`). Stamped onto the launch
        /// event so the agent answers to `@<profile>`; the wrapper also keeps
        /// `RIMZ_AGENT_PROFILE` for sender attribution from that pane. `None`
        /// for a bare built-in kind or virtual variant without an override.
        profile: Option<String>,
        /// The role this cell holds inside a named `[agents.teams]` launch. It
        /// is stamped onto the launch event so a team member answers to
        /// `@<role>`; the wrapper also keeps `RIMZ_AGENT_ROLE` for sender
        /// attribution from that pane.
        role: Option<String>,
        /// The launch model selected by the profile, role, CLI override, or
        /// adapter default.
        /// The wrapper keeps `RIMZ_AGENT_MODEL` so lifecycle hooks can stamp
        /// the card even when the agent reports the model only once.
        model: Option<String>,
        /// The launch reasoning effort selected by the profile, role, or CLI
        /// override. The wrapper keeps `RIMZ_AGENT_EFFORT` for card identity.
        effort: Option<String>,
        /// Canonical dollar cap carried into the launched session.
        budget: Option<String>,
    },
    Command {
        argv: Vec<String>,
    },
}

impl Cell {
    pub fn agent(kind: AgentKind) -> Self {
        Self::Agent {
            kind,
            args: Vec::new(),
            mode: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            profile: None,
            role: None,
            model: None,
            effort: None,
            budget: None,
        }
    }

    pub fn shell() -> Self {
        Self::Command { argv: Vec::new() }
    }
}

/// A profile chain flattened to the concrete adapter kind that can be executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub kind: AgentKind,
    pub mode: Option<PermissionMode>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub budget: Option<String>,
    pub system_prompt_file: Option<PathBuf>,
    pub append_system_prompt_file: Option<PathBuf>,
    pub args: Option<String>,
}

impl ResolvedProfile {
    fn bare(kind: &str) -> Self {
        Self {
            kind: AgentKind::new_unchecked(kind),
            mode: None,
            model: None,
            effort: None,
            budget: None,
            system_prompt_file: None,
            append_system_prompt_file: None,
            args: None,
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutErr {
    #[error("layout spec is empty")]
    Empty,
    #[error("empty layout cell in `{0}`")]
    EmptyCell(String),
    #[error("a layout column uses `+` (tile) or `/` (stack), not both: `{column}`")]
    MixedRowOperators { column: String },
    #[error(
        "unknown layout cell `{cell}`; define it under [agents.profiles] or [agents.commands], or use one of: {valid}"
    )]
    UnknownCell { cell: String, valid: String },
    #[error(
        "unknown team `{team}`; define it under [agents.teams] or pass an inline profile/command spec; valid teams: {valid_teams}; valid cells: {valid_cells}"
    )]
    UnknownTeam {
        team: String,
        valid_teams: String,
        valid_cells: String,
    },
    #[error("team `{team}` has no role `{role}`; declared roles: {valid_roles}")]
    UnknownRoleInTeam {
        team: String,
        role: String,
        valid_roles: String,
    },
    #[error("invalid team name `{name}`; team names cannot contain `.` or `/`")]
    InvalidTeamName { name: String },
    #[error(
        "team name `{0}` is reserved for an inline profile/command cell; choose another [agents.teams] name"
    )]
    ReservedTeamName(String),
    #[error("team `{team}` must declare at least one role")]
    EmptyTeam { team: String },
    #[error("team `{team}` role `{role}` references unknown profile `{profile}`")]
    UnknownRoleProfile {
        team: String,
        role: String,
        profile: String,
    },
    #[error(
        "invalid role name `{name}` in team `{team}`; roles cannot be empty or contain whitespace, `,`, `+`, `/`, `:`, or `#`"
    )]
    InvalidRoleName { team: String, name: String },
    #[error("duplicate role `{role}` in team `{team}`")]
    DuplicateRole { team: String, role: String },
    #[error("team `{team}` layout token `{role}` is not a declared role or valid roleless cell")]
    UnknownRoleInLayout { team: String, role: String },
    #[error("team `{team}` layout does not place role `{role}`")]
    RoleNotPlaced { team: String, role: String },
    #[error("team `{team}` layout places role `{role}` more than once")]
    DuplicateRoleInLayout { team: String, role: String },
    #[error("invalid profile `{profile}`: {reason}")]
    InvalidProfile { profile: String, reason: String },
    #[error("invalid command `{command}`: {reason}")]
    InvalidCommand { command: String, reason: String },
    #[error("profile `{profile}` references unknown base `{base}`")]
    UnknownProfileBase { profile: String, base: String },
    #[error("profile inheritance cycle: {chain}")]
    ProfileCycle { chain: String },
    #[error("profile `{profile}` inheritance chain is deeper than {MAX_PROFILE_DEPTH}")]
    ProfileChainTooDeep { profile: String },
    #[error(
        "repo profile `{profile}` references machine-only profile `{base}`; repo profiles may inherit only repo profiles or built-in agent kinds"
    )]
    RepoProfileEscapesTrust { profile: String, base: String },
    #[error(
        "invalid profile name `{name}`; profiles cannot be empty or contain whitespace, `,`, `+`, or `/`"
    )]
    InvalidProfileName { name: String },
    #[error("profile name `{name}` is reserved for `rimz agents`")]
    ReservedProfileName { name: String },
    #[error(
        "profile name `{name}` clashes with the agent-address grammar ({reason}); rename it so `@{name}` is unambiguous"
    )]
    ProfileShadowsAddress { name: String, reason: &'static str },
    #[error(
        "role name `{name}` in team `{team}` clashes with the agent-address grammar ({reason}); rename it so `@{name}` is unambiguous"
    )]
    RoleShadowsAddress {
        team: String,
        name: String,
        reason: &'static str,
    },
    #[error(
        "invalid command name `{name}`; commands cannot be empty or contain whitespace, `,`, `+`, or `/`"
    )]
    InvalidCommandName { name: String },
    #[error("command name `{name}` is reserved for `rimz agents`")]
    ReservedCommandName { name: String },
}

pub type Result<T> = std::result::Result<T, LayoutErr>;

/// Resolve each agent profile's `system-prompt-file` against `config_dir` so
/// the path is correct wherever the profile later launches: `~` expands to the
/// home directory and a relative path roots at the config file's directory, not
/// the agent's launch cwd. Pure — the file's existence is checked at the launch
/// entry point, not here, so a moved prompt never breaks an unrelated config
/// read.
pub fn resolve_profile_prompt_paths(profiles: &mut ProfilesConfig, config_dir: &Path) {
    for profile in profiles.0.values_mut() {
        if let Some(path) = profile.system_prompt_file.as_mut() {
            *path = resolve_prompt_path(path, config_dir);
        }
        if let Some(path) = profile.append_system_prompt_file.as_mut() {
            *path = resolve_prompt_path(path, config_dir);
        }
    }
}

pub fn resolve_team_prompt_paths(teams: &mut TeamsConfig, config_dir: &Path) {
    for team in teams.0.values_mut() {
        for binding in &mut team.roles {
            if let Some(path) = binding.system_prompt_file.as_mut() {
                *path = resolve_prompt_path(path, config_dir);
            }
            if let Some(path) = binding.append_system_prompt_file.as_mut() {
                *path = resolve_prompt_path(path, config_dir);
            }
        }
    }
}

fn resolve_prompt_path(path: &Path, config_dir: &Path) -> PathBuf {
    let expanded = crate::agents::transcript_fs::expand_tilde(&path.to_string_lossy());
    if expanded.is_absolute() {
        expanded
    } else {
        config_dir.join(expanded)
    }
}

pub fn validate_config(
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Result<()> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    validate_team_names(teams)?;
    for name in teams.0.keys() {
        if is_cell_word(name, profiles, commands) {
            return Err(LayoutErr::ReservedTeamName(name.clone()));
        }
    }
    for name in teams.0.keys() {
        validate_team(name, teams, profiles, commands)?;
    }
    Ok(())
}

/// The team a launch spec names, for the whole-team form (`forge`) and the
/// single-role form (`forge.planner`). `None` when the spec names no team.
pub fn spec_team<'a>(spec: &'a str, teams: &TeamsConfig) -> Option<&'a str> {
    let spec = spec.trim();
    if teams.0.contains_key(spec) {
        return Some(spec);
    }
    let (team, _) = spec.split_once('.')?;
    teams.0.contains_key(team).then_some(team)
}

pub fn parse_layout_spec(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    parse_layout_spec_validated(raw, profiles, commands)
}

pub fn resolve_spec(
    arg: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Result<LayoutSpec> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    validate_team_names(teams)?;
    let Some(raw) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(LayoutSpec::single(Cell::shell()));
    };
    if teams.0.contains_key(raw) {
        if is_cell_word(raw, profiles, commands) {
            return Err(LayoutErr::ReservedTeamName(raw.to_owned()));
        }
        return resolve_team(raw, teams, profiles, commands);
    }
    if let Some((team, role)) = raw
        .split_once('.')
        .filter(|(team, _)| teams.0.contains_key(*team))
    {
        return resolve_team_role(team, role, teams, profiles, commands);
    }
    if is_inline_spec(raw, profiles, commands) {
        return parse_layout_spec_validated(raw, profiles, commands);
    }
    if raw == "peer" {
        return parse_layout_spec_validated(BUILTIN_PEER, profiles, commands);
    }
    Err(LayoutErr::UnknownTeam {
        team: raw.to_owned(),
        valid_teams: valid_teams(teams),
        valid_cells: valid_cells(profiles, commands),
    })
}

pub fn resolve_team(
    name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    validate_team(name, teams, profiles, commands)?;
    let team = teams
        .0
        .get(name)
        .expect("validated team name exists in teams config");
    let role_cells = team_role_cells(name, team, profiles)?;
    if let Some(layout) = team.layout.as_deref() {
        return parse_team_layout(name, layout, &role_cells, profiles, commands);
    }
    let columns = team
        .roles
        .iter()
        .map(|binding| Column {
            rows: vec![
                role_cells
                    .get(&binding.role)
                    .expect("validated role cell exists")
                    .clone(),
            ],
            stacked: false,
        })
        .collect();
    Ok(LayoutSpec { columns })
}

fn resolve_team_role(
    team_name: &str,
    role_name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    validate_team(team_name, teams, profiles, commands)?;
    let team = teams
        .0
        .get(team_name)
        .expect("validated team name exists in teams config");
    let Some(binding) = team.roles.iter().find(|binding| binding.role == role_name) else {
        return Err(LayoutErr::UnknownRoleInTeam {
            team: team_name.to_owned(),
            role: role_name.to_owned(),
            valid_roles: valid_team_roles(team),
        });
    };
    Ok(LayoutSpec::single(role_cell(team_name, binding, profiles)?))
}

fn team_role_cells(
    team_name: &str,
    team: &Team,
    profiles: &ProfilesConfig,
) -> Result<BTreeMap<String, Cell>> {
    let mut cells = BTreeMap::new();
    for binding in &team.roles {
        cells.insert(
            binding.role.clone(),
            role_cell(team_name, binding, profiles)?,
        );
    }
    Ok(cells)
}

fn role_cell(team_name: &str, binding: &RoleBinding, profiles: &ProfilesConfig) -> Result<Cell> {
    let mut resolved = resolve_profile(&binding.profile, profiles).map_err(|err| match err {
        LayoutErr::UnknownProfileBase { profile, base } if profile == binding.profile => {
            LayoutErr::UnknownRoleProfile {
                team: team_name.to_owned(),
                role: binding.role.clone(),
                profile: base,
            }
        }
        other => other,
    })?;
    apply_role_overrides(&mut resolved, binding);
    if let Some(raw) = resolved.budget.as_deref() {
        let spec = raw
            .parse::<crate::harness::budget::BudgetSpec>()
            .map_err(|err| LayoutErr::InvalidProfile {
                profile: binding.profile.clone(),
                reason: err.to_string(),
            })?;
        resolved.budget = Some(spec.to_string());
    }
    Ok(Cell::Agent {
        kind: resolved.kind.clone(),
        args: render_profile_args(&binding.profile, &resolved)?,
        mode: resolved.mode,
        system_prompt_file: resolved.system_prompt_file.clone(),
        append_system_prompt_file: resolved.append_system_prompt_file.clone(),
        profile: Some(binding.profile.clone()),
        role: Some(binding.role.clone()),
        model: resolved.model.clone(),
        effort: resolved.effort.clone(),
        budget: resolved.budget.clone(),
    })
}

fn parse_team_layout(
    team_name: &str,
    raw: &str,
    role_cells: &BTreeMap<String, Cell>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LayoutErr::Empty);
    }

    let mut placements: BTreeMap<String, usize> = role_cells
        .keys()
        .map(|role| (role.clone(), 0usize))
        .collect();
    let mut columns = Vec::new();
    for column_raw in raw.split(',') {
        let (cell_names, stacked) = split_column_rows(raw, column_raw)?;
        let mut rows = Vec::new();
        for cell_name in cell_names {
            if let Some(cell) = role_cells.get(cell_name) {
                *placements
                    .get_mut(cell_name)
                    .expect("placement map mirrors role cells") += 1;
                rows.push(cell.clone());
                continue;
            }
            match parse_cell(cell_name, profiles, commands) {
                Ok(cell) => rows.push(cell),
                Err(LayoutErr::UnknownCell { .. }) => {
                    return Err(LayoutErr::UnknownRoleInLayout {
                        team: team_name.to_owned(),
                        role: cell_name.to_owned(),
                    });
                }
                Err(err) => return Err(err),
            }
        }
        columns.push(Column { rows, stacked });
    }

    for (role, count) in placements {
        match count {
            0 => {
                return Err(LayoutErr::RoleNotPlaced {
                    team: team_name.to_owned(),
                    role,
                });
            }
            1 => {}
            _ => {
                return Err(LayoutErr::DuplicateRoleInLayout {
                    team: team_name.to_owned(),
                    role,
                });
            }
        }
    }

    Ok(LayoutSpec { columns })
}

fn apply_role_overrides(resolved: &mut ResolvedProfile, binding: &RoleBinding) {
    if let Some(mode) = binding.mode {
        resolved.mode = Some(mode);
    }
    if let Some(model) = binding.model.as_ref() {
        resolved.model = Some(model.clone());
    }
    if let Some(effort) = binding.effort.as_ref() {
        resolved.effort = Some(effort.clone());
    }
    if let Some(budget) = binding.budget.as_ref() {
        resolved.budget = Some(budget.clone());
    }
    if let Some(path) = binding.system_prompt_file.as_ref() {
        resolved.system_prompt_file = Some(path.clone());
    }
    if let Some(path) = binding.append_system_prompt_file.as_ref() {
        resolved.append_system_prompt_file = Some(path.clone());
    }
    if let Some(args) = binding.args.as_ref() {
        resolved.args = Some(args.clone());
    }
}

/// Resolve `name` through profile inheritance to a concrete built-in kind.
pub fn resolve_profile(name: &str, profiles: &ProfilesConfig) -> Result<ResolvedProfile> {
    let mut cur = name.to_owned();
    let mut seen = Vec::<String>::new();
    let mut layers = Vec::<Profile>::new();
    let terminal_kind = loop {
        if layers.len() >= MAX_PROFILE_DEPTH {
            return Err(LayoutErr::ProfileChainTooDeep {
                profile: name.to_owned(),
            });
        }
        let Some(profile) = profiles.0.get(&cur) else {
            return Err(LayoutErr::UnknownProfileBase {
                profile: name.to_owned(),
                base: cur,
            });
        };
        seen.push(cur.clone());
        layers.push(profile.clone());
        let next = profile.agent.as_str();
        if next == cur && crate::agents::find_adapter(next).is_some() {
            break next.to_owned();
        }
        if profiles.0.contains_key(next) {
            if seen.iter().any(|visited| visited == next) {
                let mut chain = seen;
                chain.push(next.to_owned());
                return Err(LayoutErr::ProfileCycle {
                    chain: chain.join(" -> "),
                });
            }
            cur = next.to_owned();
            continue;
        }
        if crate::agents::find_adapter(next).is_some() {
            break next.to_owned();
        }
        return Err(LayoutErr::UnknownProfileBase {
            profile: cur,
            base: next.to_owned(),
        });
    };

    let mut resolved = ResolvedProfile::bare(&terminal_kind);
    for layer in &layers {
        if resolved.mode.is_none() {
            resolved.mode = layer.mode;
        }
        if resolved.model.is_none() {
            resolved.model = layer.model.clone();
        }
        if resolved.effort.is_none() {
            resolved.effort = layer.effort.clone();
        }
        if resolved.budget.is_none() {
            resolved.budget = layer.budget.clone();
        }
        if resolved.system_prompt_file.is_none() {
            resolved.system_prompt_file = layer.system_prompt_file.clone();
        }
        if resolved.append_system_prompt_file.is_none() {
            resolved.append_system_prompt_file = layer.append_system_prompt_file.clone();
        }
        if resolved.args.is_none() {
            resolved.args = layer.args.clone();
        }
    }
    let adapter = crate::agents::find_adapter(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    adapter
        .render_preset(&profile_preset(&resolved))
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: err.to_string(),
        })?;
    if let Some(raw) = resolved.budget.as_deref() {
        let spec = raw
            .parse::<crate::harness::budget::BudgetSpec>()
            .map_err(|err| LayoutErr::InvalidProfile {
                profile: name.to_owned(),
                reason: err.to_string(),
            })?;
        resolved.budget = Some(spec.to_string());
    }
    Ok(resolved)
}

/// The default tab title for a launch. Worktree launches use the `#channel`
/// spelling shared with agent addresses; a named-team launch uses
/// `team:<name>`; otherwise the title is the first agent kind (or `term`)
/// over the cwd basename.
pub fn default_tab_title(
    spec: &LayoutSpec,
    cwd: &Path,
    worktree_name: Option<&str>,
    team: Option<&str>,
) -> String {
    if let Some(name) = worktree_name.filter(|name| !name.is_empty()) {
        return format!("#{name}");
    }
    if let Some(team) = team.filter(|team| !team.is_empty()) {
        return format!("team:{team}");
    }
    let kind = spec.first_agent_kind().unwrap_or("term");
    crate::harness::resume::build_label(kind, None, cwd)
}

pub fn is_known_spec_token(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> bool {
    let raw = raw.trim();
    !raw.is_empty()
        && (spec_team(raw, teams).is_some()
            || raw == "peer"
            || is_cell_word(raw, profiles, commands)
            || (raw.contains([',', '+', '/'])
                && parse_layout_spec_validated(raw, profiles, commands).is_ok()))
}

fn parse_layout_spec_validated(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LayoutErr::Empty);
    }

    let mut columns = Vec::new();
    for column_raw in raw.split(',') {
        let (cell_names, stacked) = split_column_rows(raw, column_raw)?;
        let mut rows = Vec::new();
        for cell_name in cell_names {
            rows.push(parse_cell(cell_name, profiles, commands)?);
        }
        columns.push(Column { rows, stacked });
    }
    Ok(LayoutSpec { columns })
}

fn split_column_rows<'a>(layout_raw: &str, column_raw: &'a str) -> Result<(Vec<&'a str>, bool)> {
    let column_raw = column_raw.trim();
    if column_raw.is_empty() {
        return Err(LayoutErr::EmptyCell(layout_raw.to_owned()));
    }
    let tiled = column_raw.contains('+');
    let stacked = column_raw.contains('/');
    if tiled && stacked {
        return Err(LayoutErr::MixedRowOperators {
            column: column_raw.to_owned(),
        });
    }
    let separator = if stacked { '/' } else { '+' };
    let mut rows = Vec::new();
    for cell_raw in column_raw.split(separator) {
        let cell_raw = cell_raw.trim();
        if cell_raw.is_empty() {
            return Err(LayoutErr::EmptyCell(layout_raw.to_owned()));
        }
        rows.push(cell_raw);
    }
    Ok((rows, stacked))
}

fn is_inline_spec(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    raw.contains([',', '+', '/']) || is_cell_word(raw, profiles, commands)
}

fn is_cell_word(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    commands.0.contains_key(raw)
        || profiles.0.contains_key(raw)
        || raw == "term"
        || crate::agents::find_adapter(raw).is_some()
        || virtual_agent_shape(raw)
        || virtual_ping_shape(raw)
}

fn parse_cell(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> Result<Cell> {
    if let Some(command) = commands.0.get(raw) {
        return command_cell(raw, command);
    }
    if profiles.0.contains_key(raw) {
        let resolved = resolve_profile(raw, profiles)?;
        return cell_from_profile(raw, &resolved);
    }
    if raw == "term" {
        return Ok(Cell::shell());
    }
    if crate::agents::find_adapter(raw).is_some() {
        return Ok(Cell::agent(AgentKind::new_unchecked(raw)));
    }
    if let Some(cell) = virtual_agent_cell(raw, profiles)? {
        return Ok(cell);
    }
    if let Some(cell) = virtual_ping_cell(raw, profiles)? {
        return Ok(cell);
    }
    Err(LayoutErr::UnknownCell {
        cell: raw.to_owned(),
        valid: valid_cells(profiles, commands),
    })
}

fn cell_from_profile(name: &str, resolved: &ResolvedProfile) -> Result<Cell> {
    Ok(Cell::Agent {
        kind: resolved.kind.clone(),
        args: render_profile_args(name, resolved)?,
        mode: resolved.mode,
        system_prompt_file: resolved.system_prompt_file.clone(),
        append_system_prompt_file: resolved.append_system_prompt_file.clone(),
        profile: Some(name.to_owned()),
        role: None,
        model: resolved.model.clone(),
        effort: resolved.effort.clone(),
        budget: resolved.budget.clone(),
    })
}

fn render_profile_args(name: &str, resolved: &ResolvedProfile) -> Result<Vec<String>> {
    let adapter = crate::agents::find_adapter(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    let mut argv = adapter
        .render_preset(&profile_preset(resolved))
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: err.to_string(),
        })?;
    if let Some(mode) = resolved.mode {
        argv.extend(adapter.permission_args(mode));
    }
    if let Some(raw) = resolved
        .args
        .as_deref()
        .filter(|raw| !raw.trim().is_empty())
    {
        let mut extra = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: "check shell quoting in `args`".to_owned(),
        })?;
        argv.append(&mut extra);
    }
    Ok(argv)
}

fn profile_preset(resolved: &ResolvedProfile) -> crate::agents::LaunchPreset {
    crate::agents::LaunchPreset {
        model: resolved.model.clone(),
        effort: resolved.effort.clone(),
        system_prompt_file: resolved.system_prompt_file.clone(),
        append_system_prompt_file: resolved.append_system_prompt_file.clone(),
    }
}

fn command_cell(name: &str, raw: &str) -> Result<Cell> {
    let argv = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidCommand {
        command: name.to_owned(),
        reason: "check shell quoting in command".to_owned(),
    })?;
    if argv.is_empty() {
        return Err(LayoutErr::InvalidCommand {
            command: name.to_owned(),
            reason: "command expands to no argv".to_owned(),
        });
    }
    Ok(Cell::Command { argv })
}

fn virtual_agent_cell(raw: &str, profiles: &ProfilesConfig) -> Result<Option<Cell>> {
    let Some((kind_name, mode)) = virtual_agent_parts(raw) else {
        return Ok(None);
    };
    let (resolved, profile_name) = virtual_base(kind_name, profiles)?;
    let Some(resolved) = resolved else {
        return Ok(None);
    };
    let adapter = crate::agents::find_adapter(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    let posture = adapter.permission_args(mode);
    if mode != PermissionMode::Ask && mode != PermissionMode::Plan && posture.is_empty() {
        return Ok(None);
    }
    Ok(Some(virtual_cell_from(
        resolved,
        profile_name,
        posture,
        Some(mode),
    )?))
}

fn virtual_ping_cell(raw: &str, profiles: &ProfilesConfig) -> Result<Option<Cell>> {
    let Some(kind_name) = raw.strip_suffix("-ping") else {
        return Ok(None);
    };
    if crate::agents::find_adapter(kind_name).is_none() {
        return Ok(None);
    }
    let (resolved, profile_name) = virtual_base(kind_name, profiles)?;
    let Some(resolved) = resolved else {
        return Ok(None);
    };
    let adapter = crate::agents::find_adapter(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    let Some(ping_args) = adapter.ping_args() else {
        return Ok(None);
    };
    Ok(Some(virtual_cell_from(
        resolved,
        profile_name,
        ping_args,
        None,
    )?))
}

fn virtual_cell_from(
    resolved: ResolvedProfile,
    profile_name: Option<String>,
    extra_args: Vec<String>,
    mode: Option<PermissionMode>,
) -> Result<Cell> {
    let mut args = match profile_name.as_deref() {
        Some(profile) => {
            let mut base = resolved.clone();
            base.mode = None;
            render_profile_args(profile, &base)?
        }
        None => Vec::new(),
    };
    args.extend(extra_args);
    Ok(Cell::Agent {
        kind: resolved.kind,
        args,
        mode,
        system_prompt_file: resolved.system_prompt_file,
        append_system_prompt_file: resolved.append_system_prompt_file,
        profile: profile_name,
        role: None,
        model: resolved.model,
        effort: resolved.effort,
        budget: resolved.budget,
    })
}

fn virtual_base(
    kind_name: &str,
    profiles: &ProfilesConfig,
) -> Result<(Option<ResolvedProfile>, Option<String>)> {
    if crate::agents::find_adapter(kind_name).is_none() {
        return Ok((None, None));
    }
    if profiles.0.contains_key(kind_name) {
        let resolved = resolve_profile(kind_name, profiles)?;
        return Ok((Some(resolved), Some(kind_name.to_owned())));
    }
    Ok((Some(ResolvedProfile::bare(kind_name)), None))
}

fn virtual_agent_parts(raw: &str) -> Option<(&str, PermissionMode)> {
    let (kind, mode) = raw.rsplit_once('-')?;
    let mode = PermissionMode::from_str(mode).ok()?;
    (crate::agents::find_adapter(kind).is_some()).then_some((kind, mode))
}

fn virtual_agent_shape(raw: &str) -> bool {
    let Some((kind, mode)) = virtual_agent_parts(raw) else {
        return false;
    };
    supported_virtual_agent_args(kind, mode).is_some()
}

pub fn virtual_ping_shape(raw: &str) -> bool {
    ping_kind(raw).is_some()
}

pub fn ping_kind(raw: &str) -> Option<&str> {
    let kind = raw.strip_suffix("-ping")?;
    crate::agents::find_adapter(kind)?
        .ping_args()
        .is_some()
        .then_some(kind)
}

fn supported_virtual_agent_args(kind: &str, mode: PermissionMode) -> Option<Vec<String>> {
    let adapter = crate::agents::find_adapter(kind)?;
    let args = adapter.permission_args(mode);
    if mode == PermissionMode::Ask || mode == PermissionMode::Plan || !args.is_empty() {
        Some(args)
    } else {
        None
    }
}

fn validate_profile_names(profiles: &ProfilesConfig) -> Result<()> {
    for name in profiles.0.keys() {
        if name.is_empty()
            || name
                .chars()
                .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+' || ch == '/')
        {
            return Err(LayoutErr::InvalidProfileName { name: name.clone() });
        }
        if RESERVED_PROFILE_COMMAND_AND_TEAM_NAMES.contains(&name.as_str()) {
            return Err(LayoutErr::ReservedProfileName { name: name.clone() });
        }
        if let Some(reason) = address_grammar_clash(name) {
            return Err(LayoutErr::ProfileShadowsAddress {
                name: name.clone(),
                reason,
            });
        }
    }
    Ok(())
}

fn validate_command_names(commands: &CommandsConfig) -> Result<()> {
    for name in commands.0.keys() {
        if name.is_empty()
            || name
                .chars()
                .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+' || ch == '/')
        {
            return Err(LayoutErr::InvalidCommandName { name: name.clone() });
        }
        if RESERVED_PROFILE_COMMAND_AND_TEAM_NAMES.contains(&name.as_str()) {
            return Err(LayoutErr::ReservedCommandName { name: name.clone() });
        }
    }
    Ok(())
}

/// Why an agent profile name would collide with the agent-address grammar, so
/// the rendered `@<profile>` handle could never name the profile
/// unambiguously: it shadows the broadcast handle (`@all`), a kind ordinal
/// (`@claude-2`), or a pane/channel address (`zellij:%1`, `@x#chan`).
fn address_grammar_clash(name: &str) -> Option<&'static str> {
    if name == "all" {
        return Some("`@all` is the broadcast handle");
    }
    if is_kind_ordinal_shape(name) {
        return Some("it reads as a kind ordinal like `@claude-2`");
    }
    if name.contains(':') || name.contains('#') {
        return Some("`:` and `#` are reserved for pane and channel addresses");
    }
    None
}

/// Whether `name` reads as a `<kind>-<n>` ordinal handle (a known kind, then a
/// positive integer) — the shape `@claude-2` parses as.
fn is_kind_ordinal_shape(name: &str) -> bool {
    let Some((kind, ordinal)) = name.rsplit_once('-') else {
        return false;
    };
    crate::agents::find_adapter(kind).is_some() && ordinal.parse::<u32>().is_ok_and(|n| n > 0)
}

fn validate_team_names(teams: &TeamsConfig) -> Result<()> {
    if let Some(name) = teams.0.keys().find(|name| name.contains(['.', '/'])) {
        return Err(LayoutErr::InvalidTeamName { name: name.clone() });
    }
    if let Some(name) = teams
        .0
        .keys()
        .find(|name| RESERVED_PROFILE_COMMAND_AND_TEAM_NAMES.contains(&name.as_str()))
    {
        return Err(LayoutErr::ReservedTeamName(name.clone()));
    }
    Ok(())
}

fn validate_team(
    name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    let team = teams
        .0
        .get(name)
        .expect("team validation called with a known team name");
    if team.roles.is_empty() && team.layout.is_none() {
        return Err(LayoutErr::EmptyTeam {
            team: name.to_owned(),
        });
    }
    let mut seen = BTreeSet::new();
    for binding in &team.roles {
        if invalid_role_name(&binding.role) {
            return Err(LayoutErr::InvalidRoleName {
                team: name.to_owned(),
                name: binding.role.clone(),
            });
        }
        if let Some(reason) = address_grammar_clash(&binding.role) {
            return Err(LayoutErr::RoleShadowsAddress {
                team: name.to_owned(),
                name: binding.role.clone(),
                reason,
            });
        }
        if crate::agents::find_adapter(&binding.role).is_some() {
            return Err(LayoutErr::RoleShadowsAddress {
                team: name.to_owned(),
                name: binding.role.clone(),
                reason: "it is a built-in kind handle like `@claude`",
            });
        }
        if !seen.insert(binding.role.clone()) {
            return Err(LayoutErr::DuplicateRole {
                team: name.to_owned(),
                role: binding.role.clone(),
            });
        }
        if !profiles.0.contains_key(&binding.profile) {
            return Err(LayoutErr::UnknownRoleProfile {
                team: name.to_owned(),
                role: binding.role.clone(),
                profile: binding.profile.clone(),
            });
        }
        let mut resolved = resolve_profile(&binding.profile, profiles)?;
        apply_role_overrides(&mut resolved, binding);
        render_profile_args(&binding.profile, &resolved)?;
    }
    if let Some(layout) = team.layout.as_deref() {
        let role_cells = team_role_cells(name, team, profiles)?;
        parse_team_layout(name, layout, &role_cells, profiles, commands)?;
    }
    Ok(())
}

fn invalid_role_name(name: &str) -> bool {
    name.is_empty()
        || name.chars().any(|ch| {
            ch.is_whitespace() || ch == ',' || ch == '+' || ch == '/' || ch == ':' || ch == '#'
        })
}

fn valid_cells(profiles: &ProfilesConfig, commands: &CommandsConfig) -> String {
    let mut values = BTreeSet::new();
    values.insert("term".to_owned());
    for kind in crate::agents::known_kinds() {
        values.insert(kind.to_owned());
        for mode in PERMISSION_MODE_NAMES {
            let parsed = PermissionMode::from_str(mode).expect("permission mode constant is valid");
            if supported_virtual_agent_args(kind, parsed).is_some() {
                values.insert(format!("{kind}-{mode}"));
            }
        }
        if crate::agents::find_adapter(kind).is_some_and(|a| a.ping_args().is_some()) {
            values.insert(format!("{kind}-{PING_SUFFIX}"));
        }
    }
    values.extend(commands.0.keys().cloned());
    values.extend(profiles.0.keys().cloned());
    values.into_iter().collect::<Vec<_>>().join(", ")
}

fn valid_teams(teams: &TeamsConfig) -> String {
    let mut values = BTreeSet::from(["peer".to_owned()]);
    values.extend(teams.0.keys().cloned());
    values.into_iter().collect::<Vec<_>>().join(", ")
}

fn valid_team_roles(team: &Team) -> String {
    let values = team
        .roles
        .iter()
        .map(|binding| binding.role.clone())
        .collect::<Vec<_>>();
    if values.is_empty() {
        "none".to_owned()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests;
