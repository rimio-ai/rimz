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
const PERMISSION_MODE_NAMES: &[&str] = &["auto", "ask", "yolo"];
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

pub fn default_tab_title(spec: &LayoutSpec, worktree: &Path) -> String {
    let kind = spec.first_agent_kind().unwrap_or("term");
    crate::resume::build_label(kind, None, worktree)
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
    if mode == PermissionMode::Ask || !args.is_empty() {
        Some(args)
    } else {
        None
    }
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
mod tests {
    use super::*;
    use crate::config::Alias;
    use std::collections::BTreeMap;

    fn aliases(entries: impl IntoIterator<Item = (&'static str, Alias)>) -> AliasesConfig {
        AliasesConfig(
            entries
                .into_iter()
                .map(|(name, alias)| (name.to_owned(), alias))
                .collect(),
        )
    }

    #[test]
    fn parses_columns_and_stacked_rows() {
        let spec =
            parse_layout_spec("claude,codex+term", &AliasesConfig::default()).expect("parse");
        assert_eq!(
            spec,
            LayoutSpec {
                columns: vec![
                    Column {
                        rows: vec![Cell::agent(AgentKind::new_unchecked("claude"))]
                    },
                    Column {
                        rows: vec![
                            Cell::agent(AgentKind::new_unchecked("codex")),
                            Cell::shell()
                        ]
                    }
                ]
            }
        );
    }

    #[test]
    fn rejects_empty_and_unknown_cells() {
        let aliases = AliasesConfig::default();
        assert_eq!(parse_layout_spec("", &aliases), Err(LayoutErr::Empty));
        assert_eq!(
            parse_layout_spec("claude,,term", &aliases),
            Err(LayoutErr::EmptyCell("claude,,term".to_owned()))
        );
        assert!(matches!(
            parse_layout_spec("claude,bogus", &aliases),
            Err(LayoutErr::UnknownCell { cell, .. }) if cell == "bogus"
        ));
    }

    #[test]
    fn resolves_default_inline_builtin_and_named_layouts() {
        let aliases = AliasesConfig::default();
        let mut layouts = LayoutsConfig::default();
        layouts
            .0
            .insert("stacked".to_owned(), "claude,codex+term".to_owned());

        assert_eq!(
            resolve_layout(None, &aliases, &layouts).expect("default"),
            LayoutSpec::single(Cell::shell())
        );
        assert_eq!(
            resolve_layout(Some("claude"), &aliases, &layouts).expect("inline"),
            LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")))
        );
        layouts
            .0
            .insert("claude".to_owned(), "claude,codex".to_owned());
        assert_eq!(
            resolve_layout(Some("claude"), &aliases, &layouts),
            Err(LayoutErr::ReservedLayoutName("claude".to_owned()))
        );
        assert_eq!(
            resolve_layout(Some("peer"), &aliases, &layouts)
                .expect("builtin")
                .columns
                .len(),
            2
        );
        assert!(matches!(
            resolve_layout(Some("dual"), &aliases, &layouts),
            Err(LayoutErr::UnknownLayout { layout, .. }) if layout == "dual"
        ));
        assert_eq!(
            resolve_layout(Some("stacked"), &aliases, &layouts)
                .expect("named")
                .columns
                .len(),
            2
        );
        assert!(matches!(
            resolve_layout(Some("missing"), &aliases, &layouts),
            Err(LayoutErr::UnknownLayout { layout, .. }) if layout == "missing"
        ));
    }

    #[test]
    fn command_keywords_parse_to_raw_argv_cells() {
        let aliases = aliases([
            ("vim", Alias::Command("nvim -p".to_owned())),
            ("htop", Alias::Command("htop".to_owned())),
            (
                "zsh",
                Alias::CommandTable {
                    command: "zsh".to_owned(),
                },
            ),
        ]);

        let spec = parse_layout_spec("vim,htop+zsh", &aliases).expect("parse commands");

        assert_eq!(
            spec.columns[0].rows[0],
            Cell::Command {
                argv: vec!["nvim".to_owned(), "-p".to_owned()]
            }
        );
        assert_eq!(
            spec.columns[1].rows,
            vec![
                Cell::Command {
                    argv: vec!["htop".to_owned()]
                },
                Cell::Command {
                    argv: vec!["zsh".to_owned()]
                }
            ]
        );
    }

    #[test]
    fn agent_keyword_mode_precedes_extra_args() {
        let aliases = aliases([(
            "codex-deep",
            Alias::Agent {
                agent: "codex".to_owned(),
                mode: Some(PermissionMode::Auto),
                model: None,
                effort: None,
                args: Some("--model gpt-5-codex -c model_reasoning_effort=high".to_owned()),
            },
        )]);
        let Cell::Agent { args, .. } = parse_layout_spec("codex-deep", &aliases)
            .expect("parse agent keyword")
            .columns[0]
            .rows[0]
            .clone()
        else {
            panic!("agent cell");
        };
        let mut expected = crate::agents::find_adapter("codex")
            .expect("codex")
            .permission_args(PermissionMode::Auto);
        expected.extend([
            "--model".to_owned(),
            "gpt-5-codex".to_owned(),
            "-c".to_owned(),
            "model_reasoning_effort=high".to_owned(),
        ]);

        assert_eq!(args, expected);
    }

    #[test]
    fn agent_alias_model_and_effort_render_before_extra_args() {
        let aliases = aliases([(
            "codex-deep",
            Alias::Agent {
                agent: "codex".to_owned(),
                mode: None,
                model: Some("gpt-5-codex".to_owned()),
                effort: Some("high".to_owned()),
                args: Some("--profile reviewer".to_owned()),
            },
        )]);
        let Cell::Agent { args, .. } = parse_layout_spec("codex-deep", &aliases)
            .expect("parse agent alias")
            .columns[0]
            .rows[0]
            .clone()
        else {
            panic!("agent cell");
        };

        assert_eq!(
            args,
            vec![
                "--model".to_owned(),
                "gpt-5-codex".to_owned(),
                "-c".to_owned(),
                "model_reasoning_effort=high".to_owned(),
                "--profile".to_owned(),
                "reviewer".to_owned(),
            ]
        );
    }

    #[test]
    fn unsupported_agent_alias_preset_field_errors() {
        let aliases = aliases([(
            "pi-deep",
            Alias::Agent {
                agent: "pi".to_owned(),
                mode: None,
                model: Some("large".to_owned()),
                effort: None,
                args: None,
            },
        )]);

        assert!(matches!(
            parse_layout_spec("pi-deep", &aliases),
            Err(LayoutErr::InvalidAlias { alias, reason })
                if alias == "pi-deep"
                    && reason.contains("does not support alias field `model`")
        ));
    }

    #[test]
    fn user_alias_overrides_agent_and_virtual_cell_words() {
        let aliases = aliases([
            ("claude", Alias::Command("nvim".to_owned())),
            (
                "codex-yolo",
                Alias::Command("codex --profile reviewer".to_owned()),
            ),
        ]);

        assert_eq!(
            parse_layout_spec("claude", &aliases)
                .expect("agent override")
                .columns[0]
                .rows[0],
            Cell::Command {
                argv: vec!["nvim".to_owned()]
            }
        );
        assert_eq!(
            parse_layout_spec("codex-yolo", &aliases)
                .expect("virtual override")
                .columns[0]
                .rows[0],
            Cell::Command {
                argv: vec![
                    "codex".to_owned(),
                    "--profile".to_owned(),
                    "reviewer".to_owned()
                ]
            }
        );
    }

    #[test]
    fn virtual_agent_modes_work_without_config() {
        let spec = parse_layout_spec("claude-auto,codex-yolo+pi-ask", &AliasesConfig::default())
            .expect("virtual modes");

        assert_eq!(
            spec.columns[0].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("claude"),
                args: crate::agents::find_adapter("claude")
                    .expect("claude")
                    .permission_args(PermissionMode::Auto),
                mode: Some(PermissionMode::Auto),
            }
        );
        assert_eq!(
            spec.columns[1].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("codex"),
                args: crate::agents::find_adapter("codex")
                    .expect("codex")
                    .permission_args(PermissionMode::Yolo),
                mode: Some(PermissionMode::Yolo),
            }
        );
        assert_eq!(
            spec.columns[1].rows[1],
            Cell::Agent {
                kind: AgentKind::new_unchecked("pi"),
                args: Vec::new(),
                mode: Some(PermissionMode::Ask),
            }
        );
    }

    #[test]
    fn virtual_non_ask_modes_without_adapter_flags_are_unknown() {
        assert!(matches!(
            parse_layout_spec("pi-yolo", &AliasesConfig::default()),
            Err(LayoutErr::UnknownCell { cell, valid })
                if cell == "pi-yolo"
                    && !valid.split(", ").any(|candidate| candidate == "pi-yolo")
                    && valid.split(", ").any(|candidate| candidate == "pi-ask")
        ));
    }

    #[test]
    fn keyword_errors_are_specific() {
        let mut map = BTreeMap::new();
        map.insert(
            "bad-agent".to_owned(),
            Alias::Agent {
                agent: "ghost".to_owned(),
                mode: None,
                model: None,
                effort: None,
                args: None,
            },
        );
        map.insert(
            "bad-command".to_owned(),
            Alias::Command("nvim 'unterminated".to_owned()),
        );
        let config = AliasesConfig(map);

        assert_eq!(
            parse_layout_spec("bad-agent", &config),
            Err(LayoutErr::UnknownAliasAgent {
                alias: "bad-agent".to_owned(),
                agent: "ghost".to_owned()
            })
        );
        assert_eq!(
            parse_layout_spec("bad-command", &config),
            Err(LayoutErr::InvalidAlias {
                alias: "bad-command".to_owned(),
                reason: "check shell quoting in command".to_owned()
            })
        );

        let invalid = aliases([("bad,name", Alias::Command("nvim".to_owned()))]);
        assert_eq!(
            parse_layout_spec("term", &invalid),
            Err(LayoutErr::InvalidAliasName {
                name: "bad,name".to_owned()
            })
        );
    }

    #[test]
    fn reserved_agent_verbs_reject_alias_and_layout_names() {
        let invalid_alias = aliases([("term", Alias::Command("zsh".to_owned()))]);
        assert_eq!(
            parse_layout_spec("claude", &invalid_alias),
            Err(LayoutErr::ReservedAliasName {
                name: "term".to_owned()
            })
        );

        let layouts = LayoutsConfig(BTreeMap::from([("wait".to_owned(), "claude".to_owned())]));
        assert_eq!(
            resolve_layout(Some("wait"), &AliasesConfig::default(), &layouts),
            Err(LayoutErr::ReservedLayoutName("wait".to_owned()))
        );
    }

    #[test]
    fn layout_name_collision_with_keyword_is_reserved() {
        let aliases = aliases([("review", Alias::Command("nvim".to_owned()))]);
        let layouts = LayoutsConfig(BTreeMap::from([(
            "review".to_owned(),
            "claude,codex".to_owned(),
        )]));

        assert_eq!(
            resolve_layout(Some("review"), &aliases, &layouts),
            Err(LayoutErr::ReservedLayoutName("review".to_owned()))
        );
    }

    #[test]
    fn named_layouts_compose_keywords() {
        let aliases = aliases([
            (
                "claude-plan",
                Alias::Agent {
                    agent: "claude".to_owned(),
                    mode: None,
                    model: None,
                    effort: None,
                    args: Some("--permission-mode plan".to_owned()),
                },
            ),
            ("vim", Alias::Command("nvim".to_owned())),
        ]);
        let layouts = LayoutsConfig(BTreeMap::from([(
            "review".to_owned(),
            "claude-plan,codex+vim".to_owned(),
        )]));

        let spec = resolve_layout(Some("review"), &aliases, &layouts).expect("layout");

        assert_eq!(
            spec.columns[0].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("claude"),
                args: vec!["--permission-mode".to_owned(), "plan".to_owned()],
                mode: None,
            }
        );
        assert_eq!(
            spec.columns[1].rows[1],
            Cell::Command {
                argv: vec!["nvim".to_owned()]
            }
        );
    }

    #[test]
    fn headline_examples_resolve_end_to_end() {
        let aliases = aliases([
            ("vim", Alias::Command("nvim".to_owned())),
            ("htop", Alias::Command("htop".to_owned())),
            ("zsh", Alias::Command("zsh".to_owned())),
        ]);

        assert_eq!(
            parse_layout_spec("vim,htop+zsh", &aliases)
                .expect("command layout")
                .columns
                .len(),
            2
        );
        let spec = parse_layout_spec("pi,claude-auto+codex-yolo", &AliasesConfig::default())
            .expect("agent mode layout");
        assert_eq!(spec.columns.len(), 2);
        assert_eq!(
            spec.columns[0].rows[0],
            Cell::agent(AgentKind::new_unchecked("pi"))
        );
        assert!(
            matches!(spec.columns[1].rows[0], Cell::Agent { ref args, .. } if !args.is_empty())
        );
        assert!(
            matches!(spec.columns[1].rows[1], Cell::Agent { ref args, .. } if !args.is_empty())
        );
    }

    #[test]
    fn title_uses_first_agent_or_terminal_and_worktree_name() {
        let agent = parse_layout_spec("term,codex", &AliasesConfig::default()).expect("parse");
        assert_eq!(
            default_tab_title(&agent, Path::new("/code/query-engine")),
            "codex:query-engine"
        );
        assert_eq!(
            default_tab_title(&LayoutSpec::single(Cell::shell()), Path::new("/code/main")),
            "term:main"
        );
    }
}
