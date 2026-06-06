//! Backend-neutral tab layout IR and parser.
//!
//! The DSL is intentionally small: commas split columns, plus signs stack rows
//! within a column, and each cell is either a registered agent kind or `term`.

use std::path::Path;

use crate::config::LayoutsConfig;
use crate::ids::AgentKind;

const BUILTIN_DUAL: &str = "claude,codex";

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
                Cell::Agent(kind) => Some(kind.as_str()),
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
    Agent(AgentKind),
    Term,
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
    if is_inline_spec(raw) {
        return parse_layout_spec(raw);
    }
    if let Some(spec) = layouts.0.get(raw) {
        return parse_layout_spec(spec);
    }
    if raw == "dual" {
        return parse_layout_spec(BUILTIN_DUAL);
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
        return Ok(Cell::Agent(AgentKind::new_unchecked(raw)));
    }
    Err(LayoutErr::UnknownCell(raw.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_columns_and_stacked_rows() {
        let spec = parse_layout_spec("claude,codex+term").expect("parse");
        assert_eq!(
            spec,
            LayoutSpec {
                columns: vec![
                    Column {
                        rows: vec![Cell::Agent(AgentKind::new_unchecked("claude"))]
                    },
                    Column {
                        rows: vec![Cell::Agent(AgentKind::new_unchecked("codex")), Cell::Term]
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
        layouts
            .0
            .insert("review".to_owned(), "claude,codex+term".to_owned());

        assert_eq!(
            resolve_layout(None, &layouts).expect("default"),
            LayoutSpec::single(Cell::Term)
        );
        assert_eq!(
            resolve_layout(Some("claude"), &layouts).expect("inline"),
            LayoutSpec::single(Cell::Agent(AgentKind::new_unchecked("claude")))
        );
        assert_eq!(
            resolve_layout(Some("dual"), &layouts)
                .expect("builtin")
                .columns
                .len(),
            2
        );
        assert_eq!(
            resolve_layout(Some("review"), &layouts)
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
