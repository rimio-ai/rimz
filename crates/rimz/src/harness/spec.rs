//! Backend-neutral agent layout IR plus team/profile/command resolution.
//!
//! Commas split columns, plus signs tile rows within a column, slashes stack
//! rows within a column, and each cell is a profile, a configured command, a
//! PATH executable, or a built-in cell. Agent cells optionally carry a `:role`
//! suffix for an ad-hoc role handle. Named teams compile to one column per role
//! unless they declare an explicit role-first layout shape. Built-ins provide
//! `term`, every registered agent kind, and `<kind>-<mode>` virtual variants;
//! per-machine `[agents.profiles]` entries can specialize agent cells and
//! `[agents.commands]` entries provide raw command panes.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::config::{CommandsConfig, Profile, ProfilesConfig, RoleBinding, Team, TeamsConfig};
use crate::harness::petname;
use crate::harness::run::PermissionMode;
use crate::ids::AgentKind;

const BUILTIN_PEER: &str = "claude,codex";
const PERMISSION_MODE_NAMES: &[&str] = &["auto", "ask", "yolo", "plan"];
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
    pub fn agent_cells(&self) -> impl Iterator<Item = &AgentCell> {
        self.columns
            .iter()
            .flat_map(|column| column.rows.iter())
            .filter_map(|cell| match cell {
                Cell::Agent(agent) => Some(agent),
                Cell::Command { .. } => None,
            })
    }

    /// Every mutable agent cell, in layout order (duplicates included).
    pub fn agent_cells_mut(&mut self) -> impl Iterator<Item = &mut AgentCell> {
        self.columns
            .iter_mut()
            .flat_map(|column| column.rows.iter_mut())
            .filter_map(|cell| match cell {
                Cell::Agent(agent) => Some(agent),
                Cell::Command { .. } => None,
            })
    }

    /// Every agent cell's kind, in layout order (duplicates included).
    pub fn agent_kinds(&self) -> impl Iterator<Item = &str> {
        self.agent_cells().map(|cell| cell.kind.as_str())
    }
}

/// Borrowed structural layout grammar before cells acquire launch semantics.
pub(crate) struct RawLayout<'a> {
    pub(crate) columns: Vec<RawColumn<'a>>,
}

pub(crate) struct RawColumn<'a> {
    pub(crate) cells: Vec<&'a str>,
    pub(crate) stacked: bool,
}

impl<'a> RawLayout<'a> {
    pub(crate) fn cells(&self) -> impl Iterator<Item = &'a str> + '_ {
        self.columns
            .iter()
            .flat_map(|column| column.cells.iter().copied())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub rows: Vec<Cell>,
    pub stacked: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCell {
    pub kind: AgentKind,
    pub args: Vec<String>,
    pub system_prompt_file: Option<PathBuf>,
    /// Ordered prompt fragments composed into the replacement system prompt.
    pub append_system_prompt_files: Vec<PathBuf>,
    /// Canonical shared launch identity selected by profiles, roles, and CLI overlays.
    pub launch: crate::agents::LaunchParams,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[expect(
    clippy::large_enum_variant,
    reason = "layout IR keeps agent cells inline and avoids one allocation per launch"
)]
pub enum Cell {
    Agent(AgentCell),
    Command { argv: Vec<String> },
}

impl Cell {
    pub fn agent(kind: AgentKind) -> Self {
        Self::Agent(AgentCell {
            kind,
            args: Vec::new(),
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            launch: crate::agents::LaunchParams::default(),
        })
    }

    pub fn shell() -> Self {
        Self::Command { argv: Vec::new() }
    }
}

/// A profile chain flattened to the concrete adapter kind that can be executed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedProfile {
    pub kind: AgentKind,
    pub launch: crate::agents::LaunchParams,
    pub system_prompt_file: Option<PathBuf>,
    pub append_system_prompt_files: Vec<PathBuf>,
    pub args: Option<String>,
}

impl ResolvedProfile {
    fn bare(kind: &str) -> Self {
        Self {
            kind: AgentKind::new_unchecked(kind),
            launch: crate::agents::LaunchParams::default(),
            system_prompt_file: None,
            append_system_prompt_files: Vec::new(),
            args: None,
        }
    }

    fn fill_missing(&mut self, layer: &Profile) {
        self.launch.mode = self.launch.mode.or(layer.mode);
        if self.launch.model.is_none() {
            self.launch.model.clone_from(&layer.model);
        }
        if self.launch.effort.is_none() {
            self.launch.effort.clone_from(&layer.effort);
        }
        if self.launch.budget.is_none() {
            self.launch.budget.clone_from(&layer.budget);
        }
        if self.system_prompt_file.is_none() {
            self.system_prompt_file
                .clone_from(&layer.system_prompt_file);
        }
        if !layer.append_system_prompt_files.is_empty() {
            let mut fragments = layer.append_system_prompt_files.clone();
            fragments.append(&mut self.append_system_prompt_files);
            self.append_system_prompt_files = fragments;
        }
        if self.args.is_none() {
            self.args.clone_from(&layer.args);
        }
    }

    fn apply_role(&mut self, binding: &RoleBinding) {
        if let Some(mode) = binding.mode {
            self.launch.mode = Some(mode);
        }
        for (target, source) in [
            (&mut self.launch.model, &binding.model),
            (&mut self.launch.effort, &binding.effort),
            (&mut self.launch.budget, &binding.budget),
        ] {
            if source.is_some() {
                target.clone_from(source);
            }
        }
        if binding.system_prompt_file.is_some() {
            self.system_prompt_file
                .clone_from(&binding.system_prompt_file);
        }
        self.append_system_prompt_files
            .extend(binding.append_system_prompt_files.iter().cloned());
        if binding.args.is_some() {
            self.args.clone_from(&binding.args);
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
        "unknown layout cell `{cell}`; define it under [agents.profiles] or [agents.commands], install it on PATH, or use one of: {valid}"
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
    #[error(
        "team `{team}` leader `{leader}` is not a declared role; declared roles: {valid_roles}"
    )]
    UnknownLeaderRole {
        team: String,
        leader: String,
        valid_roles: String,
    },
    #[error(
        "the launch prompt leader `{token}` is ambiguous because this layout has several `{token}` cells; give the first cell an inline role (`{token}:lead,{token}`), use a unique profile plus `leader` on a named team, or launch without a prompt and `rimz message` the right agent"
    )]
    AmbiguousPromptLeader { token: String },
    #[error("this layout has no agent cell to receive a prompt")]
    NoPromptTarget,
    #[error("invalid team name `{name}`; team names cannot contain `.` or `/`")]
    InvalidTeamName { name: String },
    #[error(
        "team name `{0}` is reserved for an inline profile/command cell; choose another [agents.teams] name"
    )]
    ReservedTeamName(String),
    #[error(
        "`{spec}` is ambiguous in `#{channel}`: team `{team}` declares role `{spec}` and a `{spec}` profile or command resolves to a different agent; launch `{team}.{spec}` for the role, or rename one of them"
    )]
    AmbiguousInChannel {
        spec: String,
        channel: String,
        team: String,
    },
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
    #[error("invalid scratch-files pattern {pattern:?} in team `{team}`: {reason}")]
    InvalidScratchPattern {
        team: String,
        pattern: String,
        reason: &'static str,
    },
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
        "invalid inline role name `{name}`; roles cannot be empty or contain whitespace, `,`, `+`, `/`, `:`, or `#`"
    )]
    InvalidInlineRole { name: String },
    #[error(
        "inline role name `{name}` clashes with the agent-address grammar ({reason}); rename it so `@{name}` is unambiguous"
    )]
    InlineRoleShadowsAddress { name: String, reason: &'static str },
    #[error("duplicate inline role `{role}` in layout spec")]
    DuplicateInlineRole { role: String },
    #[error(
        "layout cell `{cell}` cannot have inline role `{role}`; roles apply only to agent cells"
    )]
    RoleOnCommandCell { cell: String, role: String },
    #[error(
        "invalid command name `{name}`; commands cannot be empty or contain whitespace, `,`, `+`, or `/`"
    )]
    InvalidCommandName { name: String },
    #[error("command name `{name}` is reserved for `rimz agents`")]
    ReservedCommandName { name: String },
}

pub type Result<T> = std::result::Result<T, LayoutErr>;

/// Resolve agent prompt files against the directory of the config source that
/// declared them. `~` expands to the home directory and a relative path roots
/// at that source directory, not the agent's launch cwd. Call this before
/// combining config sources so a drop-in `team.toml` keeps paths relative to
/// its own directory. Pure — the file's existence is checked at the launch
/// entry point, so a moved prompt never breaks an unrelated config read.
pub fn resolve_prompt_paths(
    profiles: &mut ProfilesConfig,
    teams: &mut TeamsConfig,
    source_dir: &Path,
) {
    for profile in profiles.0.values_mut() {
        if let Some(path) = profile.system_prompt_file.as_mut() {
            *path = resolve_prompt_path(path, source_dir);
        }
        for path in &mut profile.append_system_prompt_files {
            *path = resolve_prompt_path(path, source_dir);
        }
    }
    for team in teams.0.values_mut() {
        for binding in &mut team.roles {
            if let Some(path) = binding.system_prompt_file.as_mut() {
                *path = resolve_prompt_path(path, source_dir);
            }
            for path in &mut binding.append_system_prompt_files {
                *path = resolve_prompt_path(path, source_dir);
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
        let team = teams
            .0
            .get(name)
            .expect("team config key exists during validation");
        let prepared = prepare_team(name, team, profiles, None)?;
        if team.layout.is_some() {
            compile_team(name, prepared, profiles, commands, None)?;
        }
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

/// Qualify a bare role spec with the team that owns the current lane.
///
/// Inside `#forge`, `reviewer` means `forge.reviewer`. Returns the spec
/// unchanged when no team owns the lane, when the spec already names a team or
/// a multi-cell inline layout, or when the team declares no such role.
///
/// A role bound to its same-named profile refines that profile — same agent,
/// plus role and team identity — so it qualifies silently. A role bound to a
/// different profile than a same-named global cell is a genuine collision and
/// refuses.
pub fn qualify_spec_in_channel<'a>(
    raw: &'a str,
    channel: &str,
    channel_team: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<Cow<'a, str>> {
    let trimmed = raw.trim();
    if trimmed.is_empty()
        || trimmed.contains(['.', ',', '+', '/', ':'])
        || teams.0.contains_key(trimmed)
    {
        return Ok(Cow::Borrowed(raw));
    }
    // A stale stamp can name a team the config no longer declares.
    let Some(team) = teams.0.get(channel_team) else {
        return Ok(Cow::Borrowed(raw));
    };
    let Some(binding) = team.roles.iter().find(|binding| binding.role == trimmed) else {
        return Ok(Cow::Borrowed(raw));
    };
    if is_cell_word(trimmed, profiles, commands) && binding.profile != trimmed {
        return Err(LayoutErr::AmbiguousInChannel {
            spec: trimmed.to_owned(),
            channel: channel.to_owned(),
            team: channel_team.to_owned(),
        });
    }
    Ok(Cow::Owned(format!("{channel_team}.{trimmed}")))
}

pub fn parse_layout_spec(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<LayoutSpec> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    parse_layout_spec_validated(raw, profiles, commands, None)
}

pub fn resolve_spec(
    arg: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
) -> Result<LayoutSpec> {
    resolve_spec_with_kind_override(arg, profiles, commands, teams, None)
}

pub fn resolve_spec_with_kind_override(
    arg: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    teams: &TeamsConfig,
    kind_override: Option<&AgentKind>,
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
        return resolve_team_with_kind_override(raw, teams, profiles, commands, kind_override);
    }
    if let Some((team, role)) = raw
        .split_once('.')
        .filter(|(team, _)| teams.0.contains_key(*team))
    {
        return resolve_team_role(team, role, teams, profiles, commands, kind_override);
    }
    if is_inline_spec(raw, profiles, commands) {
        return parse_layout_spec_validated(raw, profiles, commands, kind_override);
    }
    if raw == "peer" {
        return parse_layout_spec_validated(BUILTIN_PEER, profiles, commands, kind_override);
    }
    if let Some(cell) = path_command_cell(raw) {
        return Ok(LayoutSpec::single(cell));
    }
    let (cell, role) = split_inline_role(raw, profiles, commands);
    if let Some(role) = role.filter(|_| path_command_cell(cell).is_some()) {
        return Err(LayoutErr::RoleOnCommandCell {
            cell: cell.to_owned(),
            role: role.to_owned(),
        });
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
    resolve_team_with_kind_override(name, teams, profiles, commands, None)
}

fn resolve_team_with_kind_override(
    name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    kind_override: Option<&AgentKind>,
) -> Result<LayoutSpec> {
    let team = teams
        .0
        .get(name)
        .expect("team resolution called with a known team name");
    Ok(compile_team(
        name,
        prepare_team(name, team, profiles, kind_override)?,
        profiles,
        commands,
        kind_override,
    )?
    .layout)
}

/// Resolve the one agent cell that receives a trailing launch prompt.
pub fn prompt_leader(layout: &LayoutSpec, team: Option<&Team>) -> Result<usize> {
    let agent_cells = layout.agent_cells().collect::<Vec<_>>();
    match agent_cells.as_slice() {
        [] => return Err(LayoutErr::NoPromptTarget),
        [_] => return Ok(0),
        _ => {}
    }

    if let Some(team) = team {
        if let Some(leader) = team.leader.as_deref() {
            let index = if team.roles.is_empty() {
                agent_cells
                    .iter()
                    .position(|cell| prompt_leader_token(cell) == Some(leader))
            } else {
                agent_cells
                    .iter()
                    .position(|cell| cell.launch.role.as_deref() == Some(leader))
            };
            // Team validation proves a configured leader is present exactly once.
            return Ok(index.expect("validated team leader is placed in the resolved layout"));
        }
        if let Some(first_role) = team.roles.first() {
            // Team validation proves every declared role is placed exactly once.
            return Ok(agent_cells
                .iter()
                .position(|cell| cell.launch.role.as_deref() == Some(first_role.role.as_str()))
                .expect("validated first team role is placed in the resolved layout"));
        }
    }

    let first = agent_cells[0];
    if first.launch.role.is_some() {
        return Ok(0);
    }
    let token = prompt_leader_token(first).expect("agent cells have a profile or kind token");
    if agent_cells
        .iter()
        .skip(1)
        .any(|cell| prompt_leader_token(cell) == Some(token))
    {
        return Err(LayoutErr::AmbiguousPromptLeader {
            token: token.to_owned(),
        });
    }
    Ok(0)
}

fn prompt_leader_token(cell: &AgentCell) -> Option<&str> {
    Some(cell.launch.profile.as_deref().unwrap_or(cell.kind.as_str()))
}

fn resolve_team_role(
    team_name: &str,
    role_name: &str,
    teams: &TeamsConfig,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    kind_override: Option<&AgentKind>,
) -> Result<LayoutSpec> {
    let team = teams
        .0
        .get(team_name)
        .expect("team role resolution called with a known team name");
    let compiled = compile_team(
        team_name,
        prepare_team(team_name, team, profiles, kind_override)?,
        profiles,
        commands,
        kind_override,
    )?;
    let Some(cell) = compiled.roles.get(role_name) else {
        return Err(LayoutErr::UnknownRoleInTeam {
            team: team_name.to_owned(),
            role: role_name.to_owned(),
            valid_roles: valid_team_roles(team),
        });
    };
    Ok(LayoutSpec::single(Cell::Agent(cell.clone())))
}

struct PreparedRole {
    role: String,
    profile: String,
    resolved: ResolvedProfile,
    args: Vec<String>,
}

struct PreparedTeam<'a> {
    team: &'a Team,
    roles: Vec<PreparedRole>,
}

struct CompiledTeam {
    layout: LayoutSpec,
    roles: BTreeMap<String, AgentCell>,
}

fn compile_team(
    team_name: &str,
    prepared: PreparedTeam<'_>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    kind_override: Option<&AgentKind>,
) -> Result<CompiledTeam> {
    let mut role_cells = BTreeMap::new();
    for mut role in prepared.roles {
        normalize_budget(&mut role.resolved.launch.budget, &role.profile)?;
        let cell = agent_cell_from(
            &role.resolved,
            role.args,
            Some(role.profile),
            Some(role.role.clone()),
            role.resolved.launch.mode,
        );
        role_cells.insert(role.role, cell);
    }
    let layout = if let Some(raw) = prepared.team.layout.as_deref() {
        compile_team_layout(
            team_name,
            raw,
            &role_cells,
            profiles,
            commands,
            kind_override,
        )?
    } else {
        LayoutSpec {
            columns: prepared
                .team
                .roles
                .iter()
                .map(|binding| Column {
                    rows: vec![Cell::Agent(
                        role_cells
                            .get(&binding.role)
                            .expect("prepared role has a compiled cell")
                            .clone(),
                    )],
                    stacked: false,
                })
                .collect(),
        }
    };
    if let Some(leader) = prepared.team.leader.as_deref()
        && prepared.team.roles.is_empty()
    {
        match layout
            .agent_cells()
            .filter(|cell| prompt_leader_token(cell) == Some(leader))
            .count()
        {
            1 => {}
            0 => {
                return Err(LayoutErr::UnknownLeaderRole {
                    team: team_name.to_owned(),
                    leader: leader.to_owned(),
                    valid_roles: valid_team_roles(prepared.team),
                });
            }
            _ => {
                return Err(LayoutErr::AmbiguousPromptLeader {
                    token: leader.to_owned(),
                });
            }
        }
    }
    Ok(CompiledTeam {
        layout,
        roles: role_cells,
    })
}

fn compile_team_layout(
    team_name: &str,
    raw: &str,
    role_cells: &BTreeMap<String, AgentCell>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    kind_override: Option<&AgentKind>,
) -> Result<LayoutSpec> {
    let structure = parse_layout_structure(raw)?;
    let mut placements: BTreeMap<String, usize> = role_cells
        .keys()
        .map(|role| (role.clone(), 0usize))
        .collect();
    let mut columns = Vec::new();
    for column in structure.columns {
        let mut rows = Vec::new();
        for cell_name in column.cells {
            if let Some(cell) = role_cells.get(cell_name) {
                *placements
                    .get_mut(cell_name)
                    .expect("placement map mirrors role cells") += 1;
                rows.push(Cell::Agent(cell.clone()));
                continue;
            }
            match parse_cell(cell_name, profiles, commands, kind_override) {
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
        columns.push(Column {
            rows,
            stacked: column.stacked,
        });
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

/// Resolve `name` through profile inheritance to a concrete built-in kind.
pub fn resolve_profile(name: &str, profiles: &ProfilesConfig) -> Result<ResolvedProfile> {
    let mut cur = name.to_owned();
    let mut seen = Vec::<String>::new();
    let mut layers = Vec::<&Profile>::new();
    let terminal_kind = loop {
        if layers.len() >= MAX_PROFILE_DEPTH {
            return Err(LayoutErr::ProfileChainTooDeep {
                profile: name.to_owned(),
            });
        }
        let Some(profile) = profiles.0.get(&cur) else {
            if layers.is_empty() && crate::agents::find_definition(&cur).is_some() {
                break cur;
            }
            return Err(LayoutErr::UnknownProfileBase {
                profile: name.to_owned(),
                base: cur,
            });
        };
        seen.push(cur.clone());
        layers.push(profile);
        let next = profile.agent.as_str();
        if next == cur && crate::agents::find_definition(next).is_some() {
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
        if crate::agents::find_definition(next).is_some() {
            break next.to_owned();
        }
        return Err(LayoutErr::UnknownProfileBase {
            profile: cur,
            base: next.to_owned(),
        });
    };

    let mut resolved = ResolvedProfile::bare(&terminal_kind);
    for layer in layers {
        resolved.fill_missing(layer);
    }
    let adapter = crate::agents::find_definition(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    adapter
        .spec()
        .render_preset(&profile_preset(&resolved))
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: err.to_string(),
        })?;
    normalize_budget(&mut resolved.launch.budget, name)?;
    Ok(resolved)
}

fn normalize_budget(budget: &mut Option<String>, profile: &str) -> Result<()> {
    let Some(raw) = budget.as_deref() else {
        return Ok(());
    };
    let spec = raw
        .parse::<crate::harness::budget::BudgetSpec>()
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: profile.to_owned(),
            reason: err.to_string(),
        })?;
    *budget = Some(spec.to_string());
    Ok(())
}

/// The default tab title for a launch. Worktree launches use the `#channel`
/// spelling shared with agent addresses; a named-team launch uses
/// `team:<name>`; otherwise the title lists up to three cells in layout order
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
    let tokens = spec
        .columns
        .iter()
        .flat_map(|column| &column.rows)
        .map(|cell| match cell {
            Cell::Agent(cell) => {
                Cow::Borrowed(cell.launch.profile.as_deref().unwrap_or(cell.kind.as_str()))
            }
            Cell::Command { argv } => argv.first().map_or(Cow::Borrowed("term"), |program| {
                Path::new(program).file_name().map_or_else(
                    || Cow::Borrowed(program.as_str()),
                    |name| name.to_string_lossy(),
                )
            }),
        })
        .collect::<Vec<_>>();
    let mut title = tokens
        .iter()
        .take(3)
        .map(Cow::as_ref)
        .collect::<Vec<_>>()
        .join("+");
    if title.is_empty() {
        title.push_str("term");
    } else if tokens.len() > 3 {
        title.push_str("+…");
    }
    crate::harness::resume::build_label(&title, None, cwd)
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
            || (raw.contains(':')
                && parse_layout_spec_validated(raw, profiles, commands, None).is_ok())
            || (raw.contains([',', '+', '/'])
                && parse_layout_spec_validated(raw, profiles, commands, None).is_ok()))
}

fn parse_layout_spec_validated(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    kind_override: Option<&AgentKind>,
) -> Result<LayoutSpec> {
    let structure = parse_layout_structure(raw)?;
    let mut seen_roles = BTreeSet::new();
    let mut columns = Vec::new();
    for column in structure.columns {
        let mut rows = Vec::new();
        for raw_cell in column.cells {
            let (cell_name, role) = split_inline_role(raw_cell, profiles, commands);
            if let Some(role) = role {
                validate_inline_role(role)?;
                if !seen_roles.insert(role.to_owned()) {
                    return Err(LayoutErr::DuplicateInlineRole {
                        role: role.to_owned(),
                    });
                }
            }
            let mut cell = parse_cell(cell_name, profiles, commands, kind_override)?;
            if let Some(role) = role {
                let Cell::Agent(agent) = &mut cell else {
                    return Err(LayoutErr::RoleOnCommandCell {
                        cell: cell_name.to_owned(),
                        role: role.to_owned(),
                    });
                };
                agent.launch.role = Some(role.to_owned());
            }
            rows.push(cell);
        }
        columns.push(Column {
            rows,
            stacked: column.stacked,
        });
    }
    Ok(LayoutSpec { columns })
}

pub(crate) fn parse_layout_structure(raw: &str) -> Result<RawLayout<'_>> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LayoutErr::Empty);
    }

    let mut columns = Vec::new();
    for column_raw in raw.split(',') {
        let column_raw = column_raw.trim();
        if column_raw.is_empty() {
            return Err(LayoutErr::EmptyCell(raw.to_owned()));
        }
        let tiled = column_raw.contains('+');
        let stacked = column_raw.contains('/');
        if tiled && stacked {
            return Err(LayoutErr::MixedRowOperators {
                column: column_raw.to_owned(),
            });
        }
        let separator = if stacked { '/' } else { '+' };
        let cells = column_raw
            .split(separator)
            .map(str::trim)
            .collect::<Vec<_>>();
        if cells.iter().any(|cell| cell.is_empty()) {
            return Err(LayoutErr::EmptyCell(raw.to_owned()));
        }
        columns.push(RawColumn { cells, stacked });
    }
    Ok(RawLayout { columns })
}

fn is_inline_spec(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    if raw.contains([',', '+', '/']) || is_cell_word(raw, profiles, commands) {
        return true;
    }
    let (cell, role) = split_inline_role(raw, profiles, commands);
    role.is_some() && is_cell_word(cell, profiles, commands)
}

fn is_cell_word(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    commands.0.contains_key(raw)
        || profiles.0.contains_key(raw)
        || raw == "term"
        || crate::agents::find_definition(raw).is_some()
        || virtual_agent_shape(raw)
}

pub(crate) fn split_inline_role<'a>(
    raw: &'a str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> (&'a str, Option<&'a str>) {
    if is_cell_word(raw, profiles, commands) {
        return (raw, None);
    }
    raw.split_once(':')
        .map_or((raw, None), |(cell, role)| (cell, Some(role)))
}

fn validate_inline_role(name: &str) -> Result<()> {
    if invalid_role_name(name) {
        return Err(LayoutErr::InvalidInlineRole {
            name: name.to_owned(),
        });
    }
    if let Some(reason) = address_grammar_clash(name) {
        return Err(LayoutErr::InlineRoleShadowsAddress {
            name: name.to_owned(),
            reason,
        });
    }
    if crate::agents::find_definition(name).is_some() {
        return Err(LayoutErr::InlineRoleShadowsAddress {
            name: name.to_owned(),
            reason: "it is a built-in kind handle like `@claude`",
        });
    }
    Ok(())
}

fn parse_cell(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    kind_override: Option<&AgentKind>,
) -> Result<Cell> {
    if let Some(command) = commands.0.get(raw) {
        return command_cell(raw, command);
    }
    if profiles.0.contains_key(raw) {
        return Ok(Cell::Agent(profile_cell_with_kind_override(
            raw,
            profiles,
            kind_override,
        )?));
    }
    if raw == "term" {
        return Ok(Cell::shell());
    }
    if crate::agents::find_definition(raw).is_some() {
        return Ok(Cell::agent(
            kind_override
                .cloned()
                .unwrap_or_else(|| AgentKind::new_unchecked(raw)),
        ));
    }
    if let Some(cell) = virtual_agent_cell(raw, profiles, kind_override)? {
        return Ok(cell);
    }
    if let Some(cell) = path_command_cell(raw) {
        return Ok(cell);
    }
    Err(LayoutErr::UnknownCell {
        cell: raw.to_owned(),
        valid: valid_cells(profiles, commands),
    })
}

/// Last-resort cell resolution: a bare word that is executable on PATH
/// launches as a raw command pane.
fn path_command_cell(raw: &str) -> Option<Cell> {
    which::which(raw).ok()?;
    Some(Cell::Command {
        argv: vec![raw.to_owned()],
    })
}

/// The agent cell a named profile launches as: its rendered provider argv plus
/// the typed launch params it declares. Layout resolution builds profile cells
/// through this, and relaunch replays a stored profile name through it to
/// recover the same posture.
pub fn profile_cell(name: &str, profiles: &ProfilesConfig) -> Result<AgentCell> {
    profile_cell_with_kind_override(name, profiles, None)
}

fn profile_cell_with_kind_override(
    name: &str,
    profiles: &ProfilesConfig,
    kind_override: Option<&AgentKind>,
) -> Result<AgentCell> {
    let resolved = resolve_profile(name, profiles)?;
    let resolved = with_kind_override(resolved, kind_override);
    cell_from_profile(name, &resolved)
}

fn cell_from_profile(name: &str, resolved: &ResolvedProfile) -> Result<AgentCell> {
    Ok(agent_cell_from(
        resolved,
        render_profile_args(name, resolved)?,
        Some(name.to_owned()),
        None,
        resolved.launch.mode,
    ))
}

fn agent_cell_from(
    resolved: &ResolvedProfile,
    args: Vec<String>,
    profile: Option<String>,
    role: Option<String>,
    mode: Option<PermissionMode>,
) -> AgentCell {
    AgentCell {
        kind: resolved.kind.clone(),
        args,
        system_prompt_file: resolved.system_prompt_file.clone(),
        append_system_prompt_files: resolved.append_system_prompt_files.clone(),
        launch: crate::agents::LaunchParams {
            profile,
            mode,
            role,
            ..resolved.launch.clone()
        },
    }
}

fn render_profile_args(name: &str, resolved: &ResolvedProfile) -> Result<Vec<String>> {
    let adapter = crate::agents::find_definition(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    let mut argv = adapter
        .spec()
        .render_preset(&profile_preset(resolved))
        .map_err(|err| LayoutErr::InvalidProfile {
            profile: name.to_owned(),
            reason: err.to_string(),
        })?;
    if let Some(mode) = resolved.launch.mode {
        argv.extend(adapter.spec().launch.permission_args(mode));
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
        model: resolved.launch.model.clone(),
        effort: resolved.launch.effort.clone(),
        ..Default::default()
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

fn virtual_agent_cell(
    raw: &str,
    profiles: &ProfilesConfig,
    kind_override: Option<&AgentKind>,
) -> Result<Option<Cell>> {
    let Some((kind_name, mode)) = virtual_agent_parts(raw) else {
        return Ok(None);
    };
    let (resolved, profile_name) = virtual_base(kind_name, profiles, kind_override)?;
    let Some(resolved) = resolved else {
        return Ok(None);
    };
    let adapter = crate::agents::find_definition(resolved.kind.as_str())
        .expect("resolved profile terminal kind is known");
    let posture = adapter.spec().launch.permission_args(mode);
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

fn virtual_cell_from(
    resolved: ResolvedProfile,
    profile_name: Option<String>,
    extra_args: Vec<String>,
    mode: Option<PermissionMode>,
) -> Result<Cell> {
    let mut args = match profile_name.as_deref() {
        Some(profile) => {
            let mut base = resolved.clone();
            base.launch.mode = None;
            render_profile_args(profile, &base)?
        }
        None => Vec::new(),
    };
    args.extend(extra_args);
    Ok(Cell::Agent(agent_cell_from(
        &resolved,
        args,
        profile_name,
        None,
        mode,
    )))
}

fn virtual_base(
    kind_name: &str,
    profiles: &ProfilesConfig,
    kind_override: Option<&AgentKind>,
) -> Result<(Option<ResolvedProfile>, Option<String>)> {
    if crate::agents::find_definition(kind_name).is_none() {
        return Ok((None, None));
    }
    if profiles.0.contains_key(kind_name) {
        let resolved = resolve_profile(kind_name, profiles)?;
        return Ok((
            Some(with_kind_override(resolved, kind_override)),
            Some(kind_name.to_owned()),
        ));
    }
    Ok((
        Some(with_kind_override(
            ResolvedProfile::bare(kind_name),
            kind_override,
        )),
        None,
    ))
}

fn virtual_agent_parts(raw: &str) -> Option<(&str, PermissionMode)> {
    let (kind, mode) = raw.rsplit_once('-')?;
    let mode = PermissionMode::from_str(mode).ok()?;
    (crate::agents::find_definition(kind).is_some()).then_some((kind, mode))
}

fn virtual_agent_shape(raw: &str) -> bool {
    let Some((kind, mode)) = virtual_agent_parts(raw) else {
        return false;
    };
    supported_virtual_agent_args(kind, mode).is_some()
}

fn supported_virtual_agent_args(kind: &str, mode: PermissionMode) -> Option<Vec<String>> {
    let adapter = crate::agents::find_definition(kind)?;
    let args = adapter.spec().launch.permission_args(mode);
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
        if petname::RESERVED_AGENT_WORDS.contains(&name.as_str()) {
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
        if petname::RESERVED_AGENT_WORDS.contains(&name.as_str()) {
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
    crate::agents::find_definition(kind).is_some() && ordinal.parse::<u32>().is_ok_and(|n| n > 0)
}

fn validate_team_names(teams: &TeamsConfig) -> Result<()> {
    if let Some(name) = teams.0.keys().find(|name| name.contains(['.', '/'])) {
        return Err(LayoutErr::InvalidTeamName { name: name.clone() });
    }
    if let Some(name) = teams
        .0
        .keys()
        .find(|name| petname::RESERVED_AGENT_WORDS.contains(&name.as_str()))
    {
        return Err(LayoutErr::ReservedTeamName(name.clone()));
    }
    Ok(())
}

fn prepare_team<'a>(
    name: &str,
    team: &'a Team,
    profiles: &ProfilesConfig,
    kind_override: Option<&AgentKind>,
) -> Result<PreparedTeam<'a>> {
    for pattern in &team.scratch_files {
        if let Some(reason) = invalid_scratch_pattern(pattern) {
            return Err(LayoutErr::InvalidScratchPattern {
                team: name.to_owned(),
                pattern: pattern.clone(),
                reason,
            });
        }
    }
    if team.roles.is_empty() && team.layout.is_none() {
        return Err(LayoutErr::EmptyTeam {
            team: name.to_owned(),
        });
    }
    let mut seen = BTreeSet::new();
    let mut roles = Vec::with_capacity(team.roles.len());
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
        if crate::agents::find_definition(&binding.role).is_some() {
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
        if !profiles.0.contains_key(&binding.profile)
            && crate::agents::find_definition(&binding.profile).is_none()
        {
            return Err(LayoutErr::UnknownRoleProfile {
                team: name.to_owned(),
                role: binding.role.clone(),
                profile: binding.profile.clone(),
            });
        }
        let mut resolved = resolve_profile(&binding.profile, profiles)?;
        resolved.apply_role(binding);
        let resolved = with_kind_override(resolved, kind_override);
        let args = render_profile_args(&binding.profile, &resolved)?;
        roles.push(PreparedRole {
            role: binding.role.clone(),
            profile: binding.profile.clone(),
            resolved,
            args,
        });
    }
    if let Some(leader) = team.leader.as_deref()
        && !team.roles.is_empty()
        && !team.roles.iter().any(|binding| binding.role == leader)
    {
        return Err(LayoutErr::UnknownLeaderRole {
            team: name.to_owned(),
            leader: leader.to_owned(),
            valid_roles: valid_team_roles(team),
        });
    }
    Ok(PreparedTeam { team, roles })
}

fn with_kind_override(
    mut resolved: ResolvedProfile,
    kind_override: Option<&AgentKind>,
) -> ResolvedProfile {
    if let Some(kind) = kind_override {
        resolved.kind = kind.clone();
    }
    resolved
}

fn invalid_scratch_pattern(pattern: &str) -> Option<&'static str> {
    if pattern.trim().is_empty() {
        Some("patterns cannot be empty")
    } else if pattern != pattern.trim() {
        Some("leading or trailing whitespace is not allowed")
    } else if pattern.starts_with('!') {
        Some("negation patterns are not allowed")
    } else if pattern.starts_with('#') {
        Some("comment lines are not patterns")
    } else if pattern.chars().any(|ch| ch.is_ascii_control()) {
        Some("ASCII control characters are not allowed")
    } else if pattern.split('/').any(|segment| segment == "..") {
        Some("`..` path segments are not allowed")
    } else {
        None
    }
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
