//! Backend-neutral tab layout IR and parser.
//!
//! The DSL is intentionally small: commas split columns, plus signs stack rows
//! within a column, and each cell is either a registered agent kind or `term`.
//! Named layouts may also carry per-agent launch flags from their
//! `[agents.layouts.<name>.flags]` table; inline specs stay shape-only.

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::{LayoutEntry, LayoutsConfig};
use crate::ids::AgentKind;

const BUILTIN_PEER: &str = "claude,codex";

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
                Cell::Term => None,
            })
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Column {
    pub rows: Vec<Cell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cell {
    Agent { kind: AgentKind, args: Vec<String> },
    Term,
}

impl Cell {
    pub fn agent(kind: AgentKind) -> Self {
        Self::Agent {
            kind,
            args: Vec::new(),
        }
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LayoutErr {
    #[error("layout spec is empty")]
    Empty,
    #[error("empty layout cell in `{0}`")]
    EmptyCell(String),
    #[error("unknown layout cell `{0}`; expected `term` or a known agent kind")]
    UnknownCell(String),
    #[error("unknown layout `{0}`; define it under [agents.layouts] or pass an inline spec")]
    UnknownLayout(String),
    #[error(
        "layout name `{0}` is reserved for an inline cell; choose another [agents.layouts] name"
    )]
    ReservedLayoutName(String),
    #[error("invalid flags for layout `{layout}` agent `{kind}`; check shell quoting")]
    InvalidFlags { layout: String, kind: String },
    #[error("layout `{layout}` defines flags for `{kind}`, but its shape has no `{kind}` cell")]
    UnusedFlags { layout: String, kind: String },
}

pub type Result<T> = std::result::Result<T, LayoutErr>;

pub fn parse_layout_spec(raw: &str) -> Result<LayoutSpec> {
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
            rows.push(parse_cell(cell_raw)?);
        }
        columns.push(Column { rows });
    }
    Ok(LayoutSpec { columns })
}

pub fn resolve_layout(arg: Option<&str>, layouts: &LayoutsConfig) -> Result<LayoutSpec> {
    let Some(raw) = arg.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(LayoutSpec::single(Cell::Term));
    };
    if let Some(entry) = layouts.0.get(raw) {
        if is_cell_word(raw) {
            return Err(LayoutErr::ReservedLayoutName(raw.to_owned()));
        }
        return resolve_named_layout(raw, entry);
    }
    if is_inline_spec(raw) {
        return parse_layout_spec(raw);
    }
    if raw == "peer" {
        return parse_layout_spec(BUILTIN_PEER);
    }
    Err(LayoutErr::UnknownLayout(raw.to_owned()))
}

pub fn default_tab_title(spec: &LayoutSpec, worktree: &Path) -> String {
    let kind = spec.first_agent_kind().unwrap_or("term");
    crate::resume::build_label(kind, None, worktree)
}

fn is_inline_spec(raw: &str) -> bool {
    raw.contains([',', '+']) || is_cell_word(raw)
}

fn is_cell_word(raw: &str) -> bool {
    raw == "term" || crate::agents::find_adapter(raw).is_some()
}

fn parse_cell(raw: &str) -> Result<Cell> {
    if raw == "term" {
        return Ok(Cell::Term);
    }
    if crate::agents::find_adapter(raw).is_some() {
        return Ok(Cell::agent(AgentKind::new_unchecked(raw)));
    }
    Err(LayoutErr::UnknownCell(raw.to_owned()))
}

fn resolve_named_layout(name: &str, entry: &LayoutEntry) -> Result<LayoutSpec> {
    let mut spec = parse_layout_spec(entry.shape())?;
    if let Some(flags) = entry.flags() {
        apply_agent_flags(name, &mut spec, flags)?;
    }
    Ok(spec)
}

fn apply_agent_flags(
    layout: &str,
    spec: &mut LayoutSpec,
    flags: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    let mut matched = BTreeSet::new();
    for cell in spec
        .columns
        .iter_mut()
        .flat_map(|column| column.rows.iter_mut())
    {
        let Cell::Agent { kind, args } = cell else {
            continue;
        };
        let Some(raw) = flags.get(kind.as_str()) else {
            continue;
        };
        *args = shlex::split(raw).ok_or_else(|| LayoutErr::InvalidFlags {
            layout: layout.to_owned(),
            kind: kind.as_str().to_owned(),
        })?;
        matched.insert(kind.as_str().to_owned());
    }
    for kind in flags.keys() {
        if !matched.contains(kind) {
            return Err(LayoutErr::UnusedFlags {
                layout: layout.to_owned(),
                kind: kind.to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn parses_columns_and_stacked_rows() {
        let spec = parse_layout_spec("claude,codex+term").expect("parse");
        assert_eq!(
            spec,
            LayoutSpec {
                columns: vec![
                    Column {
                        rows: vec![Cell::agent(AgentKind::new_unchecked("claude"))]
                    },
                    Column {
                        rows: vec![Cell::agent(AgentKind::new_unchecked("codex")), Cell::Term]
                    }
                ]
            }
        );
    }

    #[test]
    fn rejects_empty_and_unknown_cells() {
        assert_eq!(parse_layout_spec(""), Err(LayoutErr::Empty));
        assert_eq!(
            parse_layout_spec("claude,,term"),
            Err(LayoutErr::EmptyCell("claude,,term".to_owned()))
        );
        assert_eq!(
            parse_layout_spec("claude,bogus"),
            Err(LayoutErr::UnknownCell("bogus".to_owned()))
        );
    }

    #[test]
    fn resolves_default_inline_builtin_and_named_layouts() {
        let mut layouts = LayoutsConfig::default();
        layouts.0.insert(
            "stacked".to_owned(),
            LayoutEntry::Shape("claude,codex+term".to_owned()),
        );

        assert_eq!(
            resolve_layout(None, &layouts).expect("default"),
            LayoutSpec::single(Cell::Term)
        );
        assert_eq!(
            resolve_layout(Some("claude"), &layouts).expect("inline"),
            LayoutSpec::single(Cell::agent(AgentKind::new_unchecked("claude")))
        );
        layouts.0.insert(
            "claude".to_owned(),
            LayoutEntry::Shape("claude,codex".to_owned()),
        );
        assert_eq!(
            resolve_layout(Some("claude"), &layouts),
            Err(LayoutErr::ReservedLayoutName("claude".to_owned()))
        );
        assert_eq!(
            resolve_layout(Some("peer"), &layouts)
                .expect("builtin")
                .columns
                .len(),
            2
        );
        assert_eq!(
            resolve_layout(Some("dual"), &layouts),
            Err(LayoutErr::UnknownLayout("dual".to_owned()))
        );
        assert_eq!(
            resolve_layout(Some("stacked"), &layouts)
                .expect("named")
                .columns
                .len(),
            2
        );
        assert_eq!(
            resolve_layout(Some("missing"), &layouts),
            Err(LayoutErr::UnknownLayout("missing".to_owned()))
        );
    }

    #[test]
    fn detailed_named_layout_applies_agent_flags() {
        let mut layouts = LayoutsConfig::default();
        layouts.0.insert(
            "peer".to_owned(),
            LayoutEntry::Detailed {
                shape: "claude,codex+term".to_owned(),
                flags: BTreeMap::from([
                    ("claude".to_owned(), "--permission-mode plan".to_owned()),
                    (
                        "codex".to_owned(),
                        "--model 'gpt 5 codex' -c model_reasoning_effort=high".to_owned(),
                    ),
                ]),
            },
        );

        let spec = resolve_layout(Some("peer"), &layouts).expect("detailed layout");
        assert_eq!(
            spec.columns[0].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("claude"),
                args: vec!["--permission-mode".to_owned(), "plan".to_owned()]
            }
        );
        assert_eq!(
            spec.columns[1].rows[0],
            Cell::Agent {
                kind: AgentKind::new_unchecked("codex"),
                args: vec![
                    "--model".to_owned(),
                    "gpt 5 codex".to_owned(),
                    "-c".to_owned(),
                    "model_reasoning_effort=high".to_owned(),
                ]
            }
        );
        assert_eq!(spec.columns[1].rows[1], Cell::Term);
    }

    #[test]
    fn detailed_named_layout_rejects_bad_or_unused_flags() {
        let mut bad_quote = LayoutsConfig::default();
        bad_quote.0.insert(
            "peer".to_owned(),
            LayoutEntry::Detailed {
                shape: "claude,codex".to_owned(),
                flags: BTreeMap::from([("codex".to_owned(), "--model 'unterminated".to_owned())]),
            },
        );
        assert_eq!(
            resolve_layout(Some("peer"), &bad_quote),
            Err(LayoutErr::InvalidFlags {
                layout: "peer".to_owned(),
                kind: "codex".to_owned()
            })
        );

        let mut typo = LayoutsConfig::default();
        typo.0.insert(
            "peer".to_owned(),
            LayoutEntry::Detailed {
                shape: "claude,codex".to_owned(),
                flags: BTreeMap::from([("codx".to_owned(), "--model gpt-5-codex".to_owned())]),
            },
        );
        assert_eq!(
            resolve_layout(Some("peer"), &typo),
            Err(LayoutErr::UnusedFlags {
                layout: "peer".to_owned(),
                kind: "codx".to_owned()
            })
        );
    }

    #[test]
    fn title_uses_first_agent_or_terminal_and_worktree_name() {
        let agent = parse_layout_spec("term,codex").expect("parse");
        assert_eq!(
            default_tab_title(&agent, Path::new("/code/query-engine")),
            "codex:query-engine"
        );
        assert_eq!(
            default_tab_title(&LayoutSpec::single(Cell::Term), Path::new("/code/main")),
            "term:main"
        );
    }
}
