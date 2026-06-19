//! Backend-neutral agent layout IR and profile/command-aware parser.
//!
//! Commas split columns, plus signs stack rows within a column, and each cell is
//! a profile, a command, or a built-in cell. Built-ins provide `term`, every
//! registered agent kind, and `<kind>-<mode>` / `<kind>-ping` virtual variants;
//! per-machine `[agents.profiles]` entries can specialize agent cells and
//! `[agents.commands]` entries provide raw command panes.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use crate::config::{CommandsConfig, LayoutsConfig, Profile, ProfilesConfig};
use crate::ids::AgentKind;
use crate::run::PermissionMode;

const BUILTIN_PEER: &str = "claude,codex";
const PERMISSION_MODE_NAMES: &[&str] = &["auto", "ask", "yolo", "plan"];
const PING_SUFFIX: &str = "ping";
const RESERVED_PROFILE_COMMAND_AND_LAYOUT_NAMES: &[&str] = &[
    "list", "ls", "show", "stop", "focus", "wait", "term", "exec",
];
pub const MAX_PROFILE_DEPTH: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutSpec {
    pub columns: Vec<Column>,
}

impl LayoutSpec {
    pub fn single(cell: Cell) -> Self {
        Self {
            columns: vec![Column { rows: vec![cell] }],
        }
    }

    /// Every agent cell's kind, in layout order (duplicates included).
    pub fn agent_kinds(&self) -> impl Iterator<Item = &str> {
        self.columns.iter().flat_map(|column| {
            column.rows.iter().filter_map(|cell| match cell {
                Cell::Agent { kind, .. } => Some(kind.as_str()),
                Cell::Command { .. } => None,
            })
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
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Agent {
        kind: AgentKind,
        args: Vec<String>,
        mode: Option<PermissionMode>,
        /// The `[agents.profiles]` name this cell launched as, when it came
        /// from a named profile (`planner`) or a kind-default override
        /// (`claude`, `claude-auto`, `claude-ping`). Stamped onto the agent as
        /// `RIMZ_AGENT_PROFILE` so it answers to `@<profile>`; `None` for a
        /// bare built-in kind or virtual variant without an override.
        profile: Option<String>,
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
            profile: None,
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
    pub system_prompt_file: Option<PathBuf>,
    pub args: Option<String>,
}

impl ResolvedProfile {
    fn bare(kind: &str) -> Self {
        Self {
            kind: AgentKind::new_unchecked(kind),
            mode: None,
            model: None,
            effort: None,
            system_prompt_file: None,
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
    #[error(
        "unknown layout cell `{cell}`; define it under [agents.profiles] or [agents.commands], or use one of: {valid}"
    )]
    UnknownCell { cell: String, valid: String },
    #[error(
        "unknown layout `{layout}`; define it under [agents.layouts] or pass an inline profile/command spec; valid layouts: {valid_layouts}; valid cells: {valid_cells}"
    )]
    UnknownLayout {
        layout: String,
        valid_layouts: String,
        valid_cells: String,
    },
    #[error(
        "layout name `{0}` is reserved for an inline profile/command cell; choose another [agents.layouts] name"
    )]
    ReservedLayoutName(String),
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
        "invalid profile name `{name}`; profiles cannot be empty or contain whitespace, `,`, or `+`"
    )]
    InvalidProfileName { name: String },
    #[error("profile name `{name}` is reserved for `rimz agents`")]
    ReservedProfileName { name: String },
    #[error(
        "profile name `{name}` clashes with the agent-address grammar ({reason}); rename it so `@{name}` is unambiguous"
    )]
    ProfileShadowsAddress { name: String, reason: &'static str },
    #[error(
        "invalid command name `{name}`; commands cannot be empty or contain whitespace, `,`, or `+`"
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
            let expanded = crate::agents::transcript_fs::expand_tilde(&path.to_string_lossy());
            *path = if expanded.is_absolute() {
                expanded
            } else {
                config_dir.join(expanded)
            };
        }
    }
}

pub fn validate_config(
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    layouts: &LayoutsConfig,
) -> Result<()> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    validate_layout_names(layouts)?;
    for name in layouts.0.keys() {
        if is_cell_word(name, profiles, commands) {
            return Err(LayoutErr::ReservedLayoutName(name.clone()));
        }
    }
    for shape in layouts.0.values() {
        validate_layout_shape(shape, profiles, commands)?;
    }
    Ok(())
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

pub fn resolve_layout(
    arg: Option<&str>,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    layouts: &LayoutsConfig,
) -> Result<LayoutSpec> {
    validate_profile_names(profiles)?;
    validate_command_names(commands)?;
    validate_layout_names(layouts)?;
    let Some(raw) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(LayoutSpec::single(Cell::shell()));
    };
    if let Some(shape) = layouts.0.get(raw) {
        if is_cell_word(raw, profiles, commands) {
            return Err(LayoutErr::ReservedLayoutName(raw.to_owned()));
        }
        return parse_layout_spec_validated(shape, profiles, commands);
    }
    if is_inline_spec(raw, profiles, commands) {
        return parse_layout_spec_validated(raw, profiles, commands);
    }
    if raw == "peer" {
        return parse_layout_spec_validated(BUILTIN_PEER, profiles, commands);
    }
    Err(LayoutErr::UnknownLayout {
        layout: raw.to_owned(),
        valid_layouts: valid_layouts(layouts),
        valid_cells: valid_cells(profiles, commands),
    })
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
        if resolved.system_prompt_file.is_none() {
            resolved.system_prompt_file = layer.system_prompt_file.clone();
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
    Ok(resolved)
}

/// The default tab title for a launch. A worktree launch shows the worktree
/// name behind `branch_glyph` (the worktree header's branch glyph, resolved
/// from the sidebar glyph set so the tab tracks Unicode/Nerd Font); otherwise
/// the title is the first agent kind (or `term`) over the cwd basename.
pub fn default_tab_title(
    spec: &LayoutSpec,
    cwd: &Path,
    worktree_name: Option<&str>,
    branch_glyph: &str,
) -> String {
    if let Some(name) = worktree_name.filter(|name| !name.is_empty()) {
        return format!("{branch_glyph} {name}");
    }
    let kind = spec.first_agent_kind().unwrap_or("term");
    crate::resume::build_label(kind, None, cwd)
}

pub fn is_known_layout_token(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
    layouts: &LayoutsConfig,
) -> bool {
    let raw = raw.trim();
    !raw.is_empty()
        && (layouts.0.contains_key(raw) || raw == "peer" || is_cell_word(raw, profiles, commands))
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
        if column_raw.trim().is_empty() {
            return Err(LayoutErr::EmptyCell(raw.to_owned()));
        }
        let mut rows = Vec::new();
        for cell_raw in column_raw.split('+') {
            let cell_raw = cell_raw.trim();
            if cell_raw.is_empty() {
                return Err(LayoutErr::EmptyCell(raw.to_owned()));
            }
            rows.push(parse_cell(cell_raw, profiles, commands)?);
        }
        columns.push(Column { rows });
    }
    Ok(LayoutSpec { columns })
}

fn validate_layout_shape(
    raw: &str,
    profiles: &ProfilesConfig,
    commands: &CommandsConfig,
) -> Result<()> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(LayoutErr::Empty);
    }
    for column_raw in raw.split(',') {
        if column_raw.trim().is_empty() {
            return Err(LayoutErr::EmptyCell(raw.to_owned()));
        }
        for cell_raw in column_raw.split('+') {
            let cell_raw = cell_raw.trim();
            if cell_raw.is_empty() {
                return Err(LayoutErr::EmptyCell(raw.to_owned()));
            }
            if !is_cell_word(cell_raw, profiles, commands) {
                return Err(LayoutErr::UnknownCell {
                    cell: cell_raw.to_owned(),
                    valid: valid_cells(profiles, commands),
                });
            }
        }
    }
    Ok(())
}

fn is_inline_spec(raw: &str, profiles: &ProfilesConfig, commands: &CommandsConfig) -> bool {
    raw.contains([',', '+']) || is_cell_word(raw, profiles, commands)
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
        profile: Some(name.to_owned()),
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
    let mut args = match profile_name.as_deref() {
        Some(profile) => {
            let mut base = resolved.clone();
            base.mode = None;
            render_profile_args(profile, &base)?
        }
        None => Vec::new(),
    };
    args.extend(posture);
    Ok(Some(Cell::Agent {
        kind: resolved.kind,
        args,
        mode: Some(mode),
        profile: profile_name,
    }))
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
    let mut args = match profile_name.as_deref() {
        Some(profile) => {
            let mut base = resolved.clone();
            base.mode = None;
            render_profile_args(profile, &base)?
        }
        None => Vec::new(),
    };
    args.extend(ping_args);
    Ok(Some(Cell::Agent {
        kind: resolved.kind,
        args,
        mode: None,
        profile: profile_name,
    }))
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

fn virtual_ping_shape(raw: &str) -> bool {
    raw.strip_suffix("-ping")
        .and_then(crate::agents::find_adapter)
        .is_some_and(|adapter| adapter.ping_args().is_some())
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
                .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+')
        {
            return Err(LayoutErr::InvalidProfileName { name: name.clone() });
        }
        if RESERVED_PROFILE_COMMAND_AND_LAYOUT_NAMES.contains(&name.as_str()) {
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
                .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+')
        {
            return Err(LayoutErr::InvalidCommandName { name: name.clone() });
        }
        if RESERVED_PROFILE_COMMAND_AND_LAYOUT_NAMES.contains(&name.as_str()) {
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

fn validate_layout_names(layouts: &LayoutsConfig) -> Result<()> {
    if let Some(name) = layouts
        .0
        .keys()
        .find(|name| RESERVED_PROFILE_COMMAND_AND_LAYOUT_NAMES.contains(&name.as_str()))
    {
        return Err(LayoutErr::ReservedLayoutName(name.clone()));
    }
    Ok(())
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

fn valid_layouts(layouts: &LayoutsConfig) -> String {
    let mut values = BTreeSet::from(["peer".to_owned()]);
    values.extend(layouts.0.keys().cloned());
    values.into_iter().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests;
