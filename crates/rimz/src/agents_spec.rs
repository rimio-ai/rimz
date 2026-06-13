//! Backend-neutral agent layout IR and alias-aware parser.
//!
//! Commas split columns, plus signs stack rows within a column, and each cell is
//! an alias. Built-ins provide `term`, every registered agent kind, and
//! `<kind>-<mode>` permission variants; per-machine `[agents.aliases]` entries can
//! override them.

use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;

use crate::config::{Alias, AliasesConfig, LayoutsConfig};
use crate::ids::AgentKind;
use crate::run::PermissionMode;

const BUILTIN_PEER: &str = "claude,codex";
/// Prefix marking a launch tab whose panes run in a Rimz-owned worktree.
const WORKTREE_TAB_PREFIX: &str = "⑂ ";
const PERMISSION_MODE_NAMES: &[&str] = &["auto", "ask", "yolo", "plan"];
const PING_SUFFIX: &str = "ping";
const RESERVED_ALIAS_AND_LAYOUT_NAMES: &[&str] = &[
    "list", "ls", "show", "stop", "focus", "wait", "term", "exec",
];

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
        }
    }

    pub fn shell() -> Self {
        Self::Command { argv: Vec::new() }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutErr {
    #[error("layout spec is empty")]
    Empty,
    #[error("empty layout cell in `{0}`")]
    EmptyCell(String),
    #[error(
        "unknown layout cell `{cell}`; define it under [agents.aliases] or use one of: {valid}"
    )]
    UnknownCell { cell: String, valid: String },
    #[error(
        "unknown layout `{layout}`; define it under [agents.layouts] or pass an inline alias spec; valid layouts: {valid_layouts}; valid cells: {valid_cells}"
    )]
    UnknownLayout {
        layout: String,
        valid_layouts: String,
        valid_cells: String,
    },
    #[error(
        "layout name `{0}` is reserved for an inline alias cell; choose another [agents.layouts] name"
    )]
    ReservedLayoutName(String),
    #[error("invalid alias `{alias}`: {reason}")]
    InvalidAlias { alias: String, reason: String },
    #[error("alias `{alias}` names unknown agent `{agent}`")]
    UnknownAliasAgent { alias: String, agent: String },
    #[error(
        "invalid alias name `{name}`; aliases cannot be empty or contain whitespace, `,`, or `+`"
    )]
    InvalidAliasName { name: String },
    #[error("alias name `{name}` is reserved for `rimz agents`")]
    ReservedAliasName { name: String },
}

pub type Result<T> = std::result::Result<T, LayoutErr>;

pub fn validate_config(aliases: &AliasesConfig, layouts: &LayoutsConfig) -> Result<()> {
    validate_alias_names(aliases)?;
    validate_layout_names(layouts)?;
    for (name, alias) in &aliases.0 {
        expand_alias(name, alias)?;
    }
    for name in layouts.0.keys() {
        if is_cell_word(name, aliases) {
            return Err(LayoutErr::ReservedLayoutName(name.clone()));
        }
    }
    for shape in layouts.0.values() {
        parse_layout_spec_validated(shape, aliases)?;
    }
    Ok(())
}

pub fn parse_layout_spec(raw: &str, aliases: &AliasesConfig) -> Result<LayoutSpec> {
    validate_alias_names(aliases)?;
    parse_layout_spec_validated(raw, aliases)
}

pub fn resolve_layout(
    arg: Option<&str>,
    aliases: &AliasesConfig,
    layouts: &LayoutsConfig,
) -> Result<LayoutSpec> {
    validate_alias_names(aliases)?;
    validate_layout_names(layouts)?;
    let Some(raw) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(LayoutSpec::single(Cell::shell()));
    };
    if let Some(shape) = layouts.0.get(raw) {
        if is_cell_word(raw, aliases) {
            return Err(LayoutErr::ReservedLayoutName(raw.to_owned()));
        }
        return parse_layout_spec_validated(shape, aliases);
    }
    if is_inline_spec(raw, aliases) {
        return parse_layout_spec_validated(raw, aliases);
    }
    if raw == "peer" {
        return parse_layout_spec_validated(BUILTIN_PEER, aliases);
    }
    Err(LayoutErr::UnknownLayout {
        layout: raw.to_owned(),
        valid_layouts: valid_layouts(layouts),
        valid_cells: valid_cells(aliases),
    })
}

pub fn default_tab_title(spec: &LayoutSpec, cwd: &Path, worktree_name: Option<&str>) -> String {
    if let Some(name) = worktree_name.filter(|name| !name.is_empty()) {
        return format!("{WORKTREE_TAB_PREFIX}{name}");
    }
    let kind = spec.first_agent_kind().unwrap_or("term");
    crate::resume::build_label(kind, None, cwd)
}

pub fn is_known_layout_token(raw: &str, aliases: &AliasesConfig, layouts: &LayoutsConfig) -> bool {
    let raw = raw.trim();
    !raw.is_empty()
        && (layouts.0.contains_key(raw)
            || raw == "peer"
            || aliases.0.contains_key(raw)
            || raw == "term"
            || crate::agents::find_adapter(raw).is_some()
            || virtual_agent_args(raw).is_some())
}

fn parse_layout_spec_validated(raw: &str, aliases: &AliasesConfig) -> Result<LayoutSpec> {
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
            rows.push(parse_cell(cell_raw, aliases)?);
        }
        columns.push(Column { rows });
    }
    Ok(LayoutSpec { columns })
}

fn is_inline_spec(raw: &str, aliases: &AliasesConfig) -> bool {
    raw.contains([',', '+']) || is_cell_word(raw, aliases)
}

fn is_cell_word(raw: &str, aliases: &AliasesConfig) -> bool {
    aliases.0.contains_key(raw)
        || raw == "term"
        || crate::agents::find_adapter(raw).is_some()
        || virtual_agent_args(raw).is_some()
        || virtual_ping_cell(raw).is_some()
}

fn parse_cell(raw: &str, aliases: &AliasesConfig) -> Result<Cell> {
    if let Some(alias) = aliases.0.get(raw) {
        return expand_alias(raw, alias);
    }
    if raw == "term" {
        return Ok(Cell::shell());
    }
    if crate::agents::find_adapter(raw).is_some() {
        return Ok(Cell::agent(AgentKind::new_unchecked(raw)));
    }
    if let Some((kind, mode, args)) = virtual_agent_args(raw) {
        return Ok(Cell::Agent {
            kind: AgentKind::new_unchecked(kind),
            args,
            mode: Some(mode),
        });
    }
    if let Some(cell) = virtual_ping_cell(raw) {
        return Ok(cell);
    }
    Err(LayoutErr::UnknownCell {
        cell: raw.to_owned(),
        valid: valid_cells(aliases),
    })
}

fn expand_alias(name: &str, alias: &Alias) -> Result<Cell> {
    match alias {
        Alias::Command(raw) => command_cell(name, raw),
        Alias::CommandTable { command } => command_cell(name, command),
        Alias::Agent {
            agent,
            mode,
            model,
            effort,
            args,
        } => {
            let adapter =
                crate::agents::find_adapter(agent).ok_or_else(|| LayoutErr::UnknownAliasAgent {
                    alias: name.to_owned(),
                    agent: agent.to_owned(),
                })?;
            let mut argv = adapter
                .render_preset(&crate::agents::LaunchPreset {
                    model: model.clone(),
                    effort: effort.clone(),
                    system_prompt_file: None,
                })
                .map_err(|err| LayoutErr::InvalidAlias {
                    alias: name.to_owned(),
                    reason: err.to_string(),
                })?;
            argv.extend(
                mode.map(|mode| adapter.permission_args(mode))
                    .unwrap_or_default(),
            );
            if let Some(raw) = args.as_deref().filter(|raw| !raw.trim().is_empty()) {
                let mut extra = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidAlias {
                    alias: name.to_owned(),
                    reason: "check shell quoting in `args`".to_owned(),
                })?;
                argv.append(&mut extra);
            }
            Ok(Cell::Agent {
                kind: AgentKind::new_unchecked(agent),
                args: argv,
                mode: *mode,
            })
        }
    }
}

fn command_cell(name: &str, raw: &str) -> Result<Cell> {
    let argv = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidAlias {
        alias: name.to_owned(),
        reason: "check shell quoting in command".to_owned(),
    })?;
    if argv.is_empty() {
        return Err(LayoutErr::InvalidAlias {
            alias: name.to_owned(),
            reason: "command expands to no argv".to_owned(),
        });
    }
    Ok(Cell::Command { argv })
}

fn virtual_agent_args(raw: &str) -> Option<(&str, PermissionMode, Vec<String>)> {
    let (kind, mode) = raw.rsplit_once('-')?;
    let mode = PermissionMode::from_str(mode).ok()?;
    supported_virtual_agent_args(kind, mode).map(|args| (kind, mode, args))
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

fn virtual_ping_cell(raw: &str) -> Option<Cell> {
    let kind = raw.strip_suffix("-ping")?;
    let adapter = crate::agents::find_adapter(kind)?;
    let args = adapter.ping_args()?;
    Some(Cell::Agent {
        kind: AgentKind::new_unchecked(kind),
        args,
        mode: None,
    })
}

fn validate_alias_names(aliases: &AliasesConfig) -> Result<()> {
    for name in aliases.0.keys() {
        if name.is_empty()
            || name
                .chars()
                .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+')
        {
            return Err(LayoutErr::InvalidAliasName { name: name.clone() });
        }
        if RESERVED_ALIAS_AND_LAYOUT_NAMES.contains(&name.as_str()) {
            return Err(LayoutErr::ReservedAliasName { name: name.clone() });
        }
    }
    Ok(())
}

fn validate_layout_names(layouts: &LayoutsConfig) -> Result<()> {
    if let Some(name) = layouts
        .0
        .keys()
        .find(|name| RESERVED_ALIAS_AND_LAYOUT_NAMES.contains(&name.as_str()))
    {
        return Err(LayoutErr::ReservedLayoutName(name.clone()));
    }
    Ok(())
}

fn valid_cells(aliases: &AliasesConfig) -> String {
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
    values.extend(aliases.0.keys().cloned());
    values.into_iter().collect::<Vec<_>>().join(", ")
}

fn valid_layouts(layouts: &LayoutsConfig) -> String {
    let mut values = BTreeSet::from(["peer".to_owned()]);
    values.extend(layouts.0.keys().cloned());
    values.into_iter().collect::<Vec<_>>().join(", ")
}

#[cfg(test)]
mod tests;
