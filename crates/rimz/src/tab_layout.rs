//! Backend-neutral tab layout IR and keyword-aware parser.
//!
//! Commas split columns, plus signs stack rows within a column, and each cell is
//! a keyword. Built-ins provide `term`, every registered agent kind, and
//! `<kind>-<mode>` permission variants; per-machine `[tab.keywords]` entries can
//! override them.

use std::collections::BTreeSet;
use std::path::Path;
use std::str::FromStr;

use crate::config::{Keyword, KeywordsConfig, LayoutsConfig};
use crate::ids::AgentKind;
use crate::run::PermissionMode;

const BUILTIN_PEER: &str = "claude,codex";
const PERMISSION_MODE_NAMES: &[&str] = &["auto", "ask", "yolo"];

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

    pub fn first_agent_kind(&self) -> Option<&str> {
        self.columns.iter().find_map(|column| {
            column.rows.iter().find_map(|cell| match cell {
                Cell::Agent { kind, .. } => Some(kind.as_str()),
                Cell::Command { .. } => None,
            })
        })
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
    Agent { kind: AgentKind, args: Vec<String> },
    Command { argv: Vec<String> },
}

impl Cell {
    pub fn agent(kind: AgentKind) -> Self {
        Self::Agent {
            kind,
            args: Vec::new(),
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
    #[error("unknown layout cell `{cell}`; define it under [tab.keywords] or use one of: {valid}")]
    UnknownCell { cell: String, valid: String },
    #[error(
        "unknown layout `{layout}`; define it under [tab.layouts] or pass an inline keyword spec; valid layouts: {valid_layouts}; valid cells: {valid_cells}"
    )]
    UnknownLayout {
        layout: String,
        valid_layouts: String,
        valid_cells: String,
    },
    #[error(
        "layout name `{0}` is reserved for an inline keyword cell; choose another [tab.layouts] name"
    )]
    ReservedLayoutName(String),
    #[error("invalid keyword `{keyword}`: {reason}")]
    InvalidKeyword { keyword: String, reason: String },
    #[error("keyword `{keyword}` names unknown agent `{agent}`")]
    UnknownKeywordAgent { keyword: String, agent: String },
    #[error(
        "invalid keyword name `{name}`; keyword names cannot be empty or contain whitespace, `,`, or `+`"
    )]
    InvalidKeywordName { name: String },
}

pub type Result<T> = std::result::Result<T, LayoutErr>;

pub fn parse_layout_spec(raw: &str, keywords: &KeywordsConfig) -> Result<LayoutSpec> {
    validate_keyword_names(keywords)?;
    parse_layout_spec_validated(raw, keywords)
}

pub fn resolve_layout(
    arg: Option<&str>,
    keywords: &KeywordsConfig,
    layouts: &LayoutsConfig,
) -> Result<LayoutSpec> {
    validate_keyword_names(keywords)?;
    let Some(raw) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(LayoutSpec::single(Cell::shell()));
    };
    if let Some(shape) = layouts.0.get(raw) {
        if is_cell_word(raw, keywords) {
            return Err(LayoutErr::ReservedLayoutName(raw.to_owned()));
        }
        return parse_layout_spec_validated(shape, keywords);
    }
    if is_inline_spec(raw, keywords) {
        return parse_layout_spec_validated(raw, keywords);
    }
    if raw == "peer" {
        return parse_layout_spec_validated(BUILTIN_PEER, keywords);
    }
    Err(LayoutErr::UnknownLayout {
        layout: raw.to_owned(),
        valid_layouts: valid_layouts(layouts),
        valid_cells: valid_cells(keywords),
    })
}

pub fn default_tab_title(spec: &LayoutSpec, worktree: &Path) -> String {
    let kind = spec.first_agent_kind().unwrap_or("term");
    crate::resume::build_label(kind, None, worktree)
}

fn parse_layout_spec_validated(raw: &str, keywords: &KeywordsConfig) -> Result<LayoutSpec> {
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
            rows.push(parse_cell(cell_raw, keywords)?);
        }
        columns.push(Column { rows });
    }
    Ok(LayoutSpec { columns })
}

fn is_inline_spec(raw: &str, keywords: &KeywordsConfig) -> bool {
    raw.contains([',', '+']) || is_cell_word(raw, keywords)
}

fn is_cell_word(raw: &str, keywords: &KeywordsConfig) -> bool {
    keywords.0.contains_key(raw)
        || raw == "term"
        || crate::agents::find_adapter(raw).is_some()
        || virtual_agent_args(raw).is_some()
}

fn parse_cell(raw: &str, keywords: &KeywordsConfig) -> Result<Cell> {
    if let Some(keyword) = keywords.0.get(raw) {
        return expand_keyword(raw, keyword);
    }
    if raw == "term" {
        return Ok(Cell::shell());
    }
    if crate::agents::find_adapter(raw).is_some() {
        return Ok(Cell::agent(AgentKind::new_unchecked(raw)));
    }
    if let Some((kind, args)) = virtual_agent_args(raw) {
        return Ok(Cell::Agent {
            kind: AgentKind::new_unchecked(kind),
            args,
        });
    }
    Err(LayoutErr::UnknownCell {
        cell: raw.to_owned(),
        valid: valid_cells(keywords),
    })
}

fn expand_keyword(name: &str, keyword: &Keyword) -> Result<Cell> {
    match keyword {
        Keyword::Command(raw) => command_cell(name, raw),
        Keyword::CommandTable { command } => command_cell(name, command),
        Keyword::Agent { agent, mode, args } => {
            let adapter = crate::agents::find_adapter(agent).ok_or_else(|| {
                LayoutErr::UnknownKeywordAgent {
                    keyword: name.to_owned(),
                    agent: agent.to_owned(),
                }
            })?;
            let mut argv = mode
                .map(|mode| adapter.permission_args(mode))
                .unwrap_or_default();
            if let Some(raw) = args.as_deref().filter(|raw| !raw.trim().is_empty()) {
                let mut extra = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidKeyword {
                    keyword: name.to_owned(),
                    reason: "check shell quoting in `args`".to_owned(),
                })?;
                argv.append(&mut extra);
            }
            Ok(Cell::Agent {
                kind: AgentKind::new_unchecked(agent),
                args: argv,
            })
        }
    }
}

fn command_cell(name: &str, raw: &str) -> Result<Cell> {
    let argv = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidKeyword {
        keyword: name.to_owned(),
        reason: "check shell quoting in command".to_owned(),
    })?;
    if argv.is_empty() {
        return Err(LayoutErr::InvalidKeyword {
            keyword: name.to_owned(),
            reason: "command expands to no argv".to_owned(),
        });
    }
    Ok(Cell::Command { argv })
}

fn virtual_agent_args(raw: &str) -> Option<(&str, Vec<String>)> {
    let (kind, mode) = raw.rsplit_once('-')?;
    let mode = PermissionMode::from_str(mode).ok()?;
    supported_virtual_agent_args(kind, mode).map(|args| (kind, args))
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

fn validate_keyword_names(keywords: &KeywordsConfig) -> Result<()> {
    for name in keywords.0.keys() {
        if name.is_empty()
            || name
                .chars()
                .any(|ch| ch.is_whitespace() || ch == ',' || ch == '+')
        {
            return Err(LayoutErr::InvalidKeywordName { name: name.clone() });
        }
    }
    Ok(())
}

fn valid_cells(keywords: &KeywordsConfig) -> String {
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
    values.extend(keywords.0.keys().cloned());
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
    use crate::config::Keyword;
    use std::collections::BTreeMap;

    fn keywords(entries: impl IntoIterator<Item = (&'static str, Keyword)>) -> KeywordsConfig {
        KeywordsConfig(
            entries
                .into_iter()
                .map(|(name, keyword)| (name.to_owned(), keyword))
                .collect(),
        )
    }

    #[test]
    fn parses_columns_and_stacked_rows() {
        let spec =
            parse_layout_spec("claude,codex+term", &KeywordsConfig::default()).expect("parse");
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
        let keywords = KeywordsConfig::default();
        assert_eq!(parse_layout_spec("", &keywords), Err(LayoutErr::Empty));
        assert_eq!(
            parse_layout_spec("claude,,term", &keywords),
            Err(LayoutErr::EmptyCell("claude,,term".to_owned()))
        );
        assert!(matches!(
            parse_layout_spec("claude,bogus", &keywords),
            Err(LayoutErr::UnknownCell { cell, .. }) if cell == "bogus"
        ));
    }

    #[test]
    fn resolves_default_inline_builtin_and_named_layouts() {
        let keywords = KeywordsConfig::default();
        let mut layouts = LayoutsConfig::default();
        layouts
            .0
            .insert("stacked".to_owned(), "claude,codex+term".to_owned());

        assert_eq!(
            resolve_layout(None, &keywords, &layouts).expect("default"),
            LayoutSpec::single(Cell::shell())
        );
        assert_eq!(
            resolve_layout(Some("claude"), &keywords, &layouts).expect("inline"),
            LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")))
        );
        layouts
            .0
            .insert("claude".to_owned(), "claude,codex".to_owned());
        assert_eq!(
            resolve_layout(Some("claude"), &keywords, &layouts),
            Err(LayoutErr::ReservedLayoutName("claude".to_owned()))
        );
        assert_eq!(
            resolve_layout(Some("peer"), &keywords, &layouts)
                .expect("builtin")
                .columns
                .len(),
            2
        );
        assert!(matches!(
            resolve_layout(Some("dual"), &keywords, &layouts),
            Err(LayoutErr::UnknownLayout { layout, .. }) if layout == "dual"
        ));
        assert_eq!(
            resolve_layout(Some("stacked"), &keywords, &layouts)
                .expect("named")
                .columns
                .len(),
            2
        );
        assert!(matches!(
            resolve_layout(Some("missing"), &keywords, &layouts),
            Err(LayoutErr::UnknownLayout { layout, .. }) if layout == "missing"
        ));
    }

    #[test]
    fn command_keywords_parse_to_raw_argv_cells() {
        let keywords = keywords([
            ("vim", Keyword::Command("nvim -p".to_owned())),
            ("htop", Keyword::Command("htop".to_owned())),
            (
                "zsh",
                Keyword::CommandTable {
                    command: "zsh".to_owned(),
                },
            ),
        ]);

        let spec = parse_layout_spec("vim,htop+zsh", &keywords).expect("parse commands");

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
        let keywords = keywords([(
            "codex-deep",
            Keyword::Agent {
                agent: "codex".to_owned(),
                mode: Some(PermissionMode::Auto),
                args: Some("--model gpt-5-codex -c model_reasoning_effort=high".to_owned()),
            },
        )]);
        let Cell::Agent { args, .. } = parse_layout_spec("codex-deep", &keywords)
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
    fn user_keyword_overrides_builtin_cell_words() {
        let keywords = keywords([
            ("term", Keyword::Command("zsh -l".to_owned())),
            ("claude", Keyword::Command("nvim".to_owned())),
            (
                "codex-yolo",
                Keyword::Command("codex --profile reviewer".to_owned()),
            ),
        ]);

        assert_eq!(
            parse_layout_spec("term", &keywords)
                .expect("term override")
                .columns[0]
                .rows[0],
            Cell::Command {
                argv: vec!["zsh".to_owned(), "-l".to_owned()]
            }
        );
        assert_eq!(
            parse_layout_spec("claude", &keywords)
                .expect("agent override")
                .columns[0]
                .rows[0],
            Cell::Command {
                argv: vec!["nvim".to_owned()]
            }
        );
        assert_eq!(
            parse_layout_spec("codex-yolo", &keywords)
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
        let spec = parse_layout_spec("claude-auto,codex-yolo+pi-ask", &KeywordsConfig::default())
            .expect("virtual modes");

        assert_eq!(
            spec.columns[0].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("claude"),
                args: crate::agents::find_adapter("claude")
                    .expect("claude")
                    .permission_args(PermissionMode::Auto)
            }
        );
        assert_eq!(
            spec.columns[1].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("codex"),
                args: crate::agents::find_adapter("codex")
                    .expect("codex")
                    .permission_args(PermissionMode::Yolo)
            }
        );
        assert_eq!(
            spec.columns[1].rows[1],
            Cell::Agent {
                kind: AgentKind::new_unchecked("pi"),
                args: Vec::new()
            }
        );
    }

    #[test]
    fn virtual_non_ask_modes_without_adapter_flags_are_unknown() {
        assert!(matches!(
            parse_layout_spec("pi-yolo", &KeywordsConfig::default()),
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
            Keyword::Agent {
                agent: "ghost".to_owned(),
                mode: None,
                args: None,
            },
        );
        map.insert(
            "bad-command".to_owned(),
            Keyword::Command("nvim 'unterminated".to_owned()),
        );
        let config = KeywordsConfig(map);

        assert_eq!(
            parse_layout_spec("bad-agent", &config),
            Err(LayoutErr::UnknownKeywordAgent {
                keyword: "bad-agent".to_owned(),
                agent: "ghost".to_owned()
            })
        );
        assert_eq!(
            parse_layout_spec("bad-command", &config),
            Err(LayoutErr::InvalidKeyword {
                keyword: "bad-command".to_owned(),
                reason: "check shell quoting in command".to_owned()
            })
        );

        let invalid = keywords([("bad,name", Keyword::Command("nvim".to_owned()))]);
        assert_eq!(
            parse_layout_spec("term", &invalid),
            Err(LayoutErr::InvalidKeywordName {
                name: "bad,name".to_owned()
            })
        );
    }

    #[test]
    fn layout_name_collision_with_keyword_is_reserved() {
        let keywords = keywords([("review", Keyword::Command("nvim".to_owned()))]);
        let layouts = LayoutsConfig(BTreeMap::from([(
            "review".to_owned(),
            "claude,codex".to_owned(),
        )]));

        assert_eq!(
            resolve_layout(Some("review"), &keywords, &layouts),
            Err(LayoutErr::ReservedLayoutName("review".to_owned()))
        );
    }

    #[test]
    fn named_layouts_compose_keywords() {
        let keywords = keywords([
            (
                "claude-plan",
                Keyword::Agent {
                    agent: "claude".to_owned(),
                    mode: None,
                    args: Some("--permission-mode plan".to_owned()),
                },
            ),
            ("vim", Keyword::Command("nvim".to_owned())),
        ]);
        let layouts = LayoutsConfig(BTreeMap::from([(
            "review".to_owned(),
            "claude-plan,codex+vim".to_owned(),
        )]));

        let spec = resolve_layout(Some("review"), &keywords, &layouts).expect("layout");

        assert_eq!(
            spec.columns[0].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("claude"),
                args: vec!["--permission-mode".to_owned(), "plan".to_owned()]
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
        let keywords = keywords([
            ("vim", Keyword::Command("nvim".to_owned())),
            ("htop", Keyword::Command("htop".to_owned())),
            ("zsh", Keyword::Command("zsh".to_owned())),
        ]);

        assert_eq!(
            parse_layout_spec("vim,htop+zsh", &keywords)
                .expect("command layout")
                .columns
                .len(),
            2
        );
        let spec = parse_layout_spec("pi,claude-auto+codex-yolo", &KeywordsConfig::default())
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
        let agent = parse_layout_spec("term,codex", &KeywordsConfig::default()).expect("parse");
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
