//! Static adapter coverage matrices. The report is workspace-independent:
//! adapter descriptors declare what they give the user and how it is wired, and
//! this command renders the registry from that declaration.
//!
//! The default report is the six user-facing capabilities — the same grid the
//! README and [agent support] print, with each cell's limit spelled out.
//! `--wiring` adds the mechanism beneath it: the integration-concern grid and
//! the lifecycle-hook grid, which is what an adapter author reads.
//!
//! [agent support]: ../../../../docs/reference/agent-support.md

use std::io::{self, Write};

use anyhow::Result;
use clap::Args;
use serde::Serialize;

use crate::cli::render::{Cell, Table, cell, paint, palette};
use rimz::agents::{
    AgentDefinition, CapabilityLevel, ConcernCoverage, HookCoverage, IntegrationConcern,
    LifecycleSignalKind, UserCapability,
};

use super::GlobalFlags;
use super::render as ui;

#[derive(Debug, Args)]
pub struct CoverageArgs {
    /// Emit machine-readable JSON instead of the human report.
    #[arg(long)]
    json: bool,
    /// Add the mechanism grids: integration concerns and the lifecycle hooks
    /// behind each capability.
    #[arg(long)]
    wiring: bool,
}

#[derive(Debug, Serialize)]
struct CoverageReport {
    capabilities: CoverageMatrix,
    #[serde(skip_serializing_if = "Option::is_none")]
    coverage: Option<CoverageMatrix>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hooks_matrix: Option<CoverageMatrix>,
}

/// A cross-adapter coverage grid: rows are concerns or lifecycle signals,
/// columns are agents in registry order.
#[derive(Debug, Serialize)]
struct CoverageMatrix {
    agents: Vec<String>,
    rows: Vec<MatrixRow>,
}

#[derive(Debug, Serialize)]
struct MatrixRow {
    label: String,
    cells: Vec<MatrixCell>,
}

#[derive(Debug, Serialize)]
struct MatrixCell {
    state: MatrixCellState,
    detail: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MatrixCellState {
    Ok,
    Partial,
    Absent,
}

impl MatrixCell {
    fn ok(detail: impl Into<String>) -> Self {
        Self {
            state: MatrixCellState::Ok,
            detail: detail.into(),
        }
    }

    fn partial(detail: impl Into<String>) -> Self {
        Self {
            state: MatrixCellState::Partial,
            detail: detail.into(),
        }
    }

    fn absent(detail: impl Into<String>) -> Self {
        Self {
            state: MatrixCellState::Absent,
            detail: detail.into(),
        }
    }
}

pub fn run(args: CoverageArgs, _globals: &GlobalFlags) -> Result<()> {
    let report = CoverageReport {
        capabilities: collect_capabilities(),
        coverage: args.wiring.then(collect_coverage),
        hooks_matrix: args.wiring.then(collect_hook_matrix),
    };

    if args.json {
        return ui::json_pretty(&report);
    }
    render_human(&report, &mut ui::out())?;
    Ok(())
}

/// Cross-adapter user-facing capability coverage — the six marks the
/// compatibility matrix prints, in the language a person reads them in.
fn collect_capabilities() -> CoverageMatrix {
    let agents = matrix_agents();
    let mut rows = Vec::new();
    for capability in UserCapability::ALL {
        let mut cells = Vec::new();
        for agent in rimz::agents::all_definitions() {
            match agent.capability_level(capability) {
                CapabilityLevel::Full { note } => cells.push(MatrixCell::ok(note)),
                CapabilityLevel::Partial { shows, limit } => {
                    cells.push(MatrixCell::partial(format!("{shows} — {limit}")));
                }
                CapabilityLevel::Unsupported { reason } => cells.push(MatrixCell::absent(reason)),
            }
        }
        rows.push(MatrixRow {
            label: capability.short_label().to_owned(),
            cells,
        });
    }
    CoverageMatrix { agents, rows }
}

/// Cross-adapter integration-concern coverage.
fn collect_coverage() -> CoverageMatrix {
    let agents = matrix_agents();
    let mut rows = Vec::new();
    for concern in IntegrationConcern::ALL {
        let mut cells = Vec::new();
        for agent in rimz::agents::all_definitions() {
            let coverage = concern_coverage(agent, concern);
            match coverage {
                ConcernCoverage::Wired { via } => cells.push(MatrixCell::ok(via)),
                ConcernCoverage::Partial { via, gap } => {
                    cells.push(MatrixCell::partial(format!("{via} — {gap}")));
                }
                ConcernCoverage::Unsupported { reason } => cells.push(MatrixCell::absent(reason)),
            }
        }
        rows.push(MatrixRow {
            label: concern.short_label().to_owned(),
            cells,
        });
    }
    CoverageMatrix { agents, rows }
}

/// Cross-adapter lifecycle-hook coverage.
fn collect_hook_matrix() -> CoverageMatrix {
    let agents = matrix_agents();
    let mut rows = Vec::new();
    for signal_kind in LifecycleSignalKind::ALL {
        let mut cells = Vec::new();
        for agent in rimz::agents::all_definitions() {
            let coverage = hook_coverage(agent, signal_kind);
            match coverage {
                HookCoverage::Native { event } => cells.push(MatrixCell::ok(event)),
                HookCoverage::Derived { via, gap } => {
                    cells.push(MatrixCell::partial(format!("{via} — {gap}")));
                }
                HookCoverage::Absent { reason } => cells.push(MatrixCell::absent(reason)),
            }
        }
        rows.push(MatrixRow {
            label: signal_kind.short_label().to_owned(),
            cells,
        });
    }
    CoverageMatrix { agents, rows }
}

fn matrix_agents() -> Vec<String> {
    rimz::agents::all_definitions()
        .map(|agent| agent.spec().kind.to_owned())
        .collect()
}

fn concern_coverage(definition: &AgentDefinition, concern: IntegrationConcern) -> ConcernCoverage {
    definition.concern_coverage(concern)
}

fn hook_coverage(definition: &AgentDefinition, signal_kind: LifecycleSignalKind) -> HookCoverage {
    definition.lifecycle_coverage(signal_kind)
}

fn render_human(report: &CoverageReport, w: &mut impl Write) -> io::Result<()> {
    writeln!(w, "{}", paint(palette::header(), "RimZ coverage"))?;
    render_matrix(
        w,
        "WHAT EACH AGENT GIVES YOU",
        "CAPABILITY",
        ["full", "partial", "unsupported"],
        &report.capabilities,
    )?;
    if let Some(coverage) = &report.coverage {
        render_matrix(
            w,
            "WIRING — INTEGRATION CONCERNS",
            "CONCERN",
            ["wired", "partial", "unsupported"],
            coverage,
        )?;
    }
    if let Some(hooks) = &report.hooks_matrix {
        render_matrix(
            w,
            "WIRING — LIFECYCLE HOOKS",
            "SIGNAL",
            ["native", "derived", "absent"],
            hooks,
        )?;
    }
    Ok(())
}

fn section(w: &mut impl Write, title: &str) -> io::Result<()> {
    writeln!(w)?;
    writeln!(w, "{}", paint(palette::header(), title))
}

fn render_matrix(
    w: &mut impl Write,
    title: &str,
    detail_label_header: &str,
    legend: [&str; 3],
    matrix: &CoverageMatrix,
) -> io::Result<()> {
    section(w, title)?;
    render_grid(w, matrix)?;
    render_legend(w, legend)?;
    render_detail(w, detail_label_header, matrix)
}

fn render_grid(w: &mut impl Write, matrix: &CoverageMatrix) -> io::Result<()> {
    let headers =
        std::iter::once("AGENT".to_owned()).chain(matrix.rows.iter().map(|row| row.label.clone()));
    let mut table = Table::new(headers);
    for (agent_idx, agent) in matrix.agents.iter().enumerate() {
        let cells = std::iter::once(cell(agent.as_str()).fg(palette::identity(agent))).chain(
            matrix
                .rows
                .iter()
                .map(|row| matrix_cell(row.cells[agent_idx].state)),
        );
        table.row(cells);
    }
    table.render(w)
}

fn render_detail(
    w: &mut impl Write,
    label_header: &str,
    matrix: &CoverageMatrix,
) -> io::Result<()> {
    writeln!(w)?;
    writeln!(w, "{}", paint(palette::header(), "DETAIL"))?;
    let mut table = Table::new([label_header, "DETAIL"]);
    for (agent_idx, agent) in matrix.agents.iter().enumerate() {
        table.section(agent);
        for row in &matrix.rows {
            let entry = &row.cells[agent_idx];
            let (glyph, style) = matrix_parts(entry.state);
            table.row([
                cell(row.label.as_str()).fg(palette::accent()),
                cell(format!("{glyph} {}", entry.detail)).fg(style),
            ]);
        }
    }
    table.render(w)
}

fn render_legend(w: &mut impl Write, legend: [&str; 3]) -> io::Result<()> {
    let (ok_glyph, ok_style) = matrix_parts(MatrixCellState::Ok);
    let (partial_glyph, partial_style) = matrix_parts(MatrixCellState::Partial);
    let (absent_glyph, absent_style) = matrix_parts(MatrixCellState::Absent);
    writeln!(
        w,
        "  {} {} {}   {} {}   {} {}",
        paint(palette::faint(), "legend"),
        paint(ok_style, ok_glyph),
        paint(palette::faint(), legend[0]),
        paint(partial_style, partial_glyph),
        paint(palette::faint(), legend[1]),
        paint(absent_style, absent_glyph),
        paint(palette::faint(), legend[2])
    )
}

fn matrix_cell(value: MatrixCellState) -> Cell {
    let (glyph, style) = matrix_parts(value);
    cell(glyph).fg(style)
}

fn matrix_parts(value: MatrixCellState) -> (&'static str, anstyle::Style) {
    let role = match value {
        MatrixCellState::Ok => crate::cli::render::status::StateRole::Success,
        MatrixCellState::Partial => crate::cli::render::status::StateRole::Waiting,
        MatrixCellState::Absent => crate::cli::render::status::StateRole::Unavailable,
    };
    crate::cli::render::verdict(role)
}

#[cfg(test)]
mod tests;
