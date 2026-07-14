//! Static adapter coverage matrices. The report is workspace-independent:
//! adapter descriptors declare integration-concern and lifecycle-hook coverage,
//! and this command renders the registry as a developer-facing checklist.

use std::io::{self, Write};

use anyhow::Result;
use clap::Args;
use serde::Serialize;

use crate::cli::render::{Cell, Table, cell, paint, palette};
use rimz::agents::{
    AgentDescriptor, ConcernCoverage, HookCoverage, IntegrationConcern, LifecycleSignalKind,
};

use super::GlobalFlags;
use super::render as ui;

#[derive(Debug, Args)]
pub struct CoverageArgs {
    /// Emit machine-readable JSON instead of the human report.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Serialize)]
struct CoverageReport {
    coverage: CoverageMatrix,
    hooks_matrix: CoverageMatrix,
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
        coverage: collect_coverage(),
        hooks_matrix: collect_hook_matrix(),
    };

    let mut out = ui::out();
    if args.json {
        let rendered = serde_json::to_string_pretty(&report).expect("CoverageReport serializes");
        writeln!(out, "{rendered}")?;
    } else {
        render_human(&report, &mut out)?;
    }
    Ok(())
}

/// Cross-adapter integration-concern coverage.
fn collect_coverage() -> CoverageMatrix {
    let agents = matrix_agents();
    let mut rows = Vec::new();
    for concern in IntegrationConcern::ALL {
        let mut cells = Vec::new();
        for agent in rimz::agents::all_adapters() {
            let descriptor = agent.descriptor();
            let coverage = concern_coverage(descriptor, concern);
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
        for agent in rimz::agents::all_adapters() {
            let descriptor = agent.descriptor();
            let coverage = hook_coverage(descriptor, signal_kind);
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
    rimz::agents::all_adapters()
        .map(|agent| agent.descriptor().kind.to_owned())
        .collect()
}

fn concern_coverage(descriptor: &AgentDescriptor, concern: IntegrationConcern) -> ConcernCoverage {
    descriptor
        .coverage
        .iter()
        .find(|(declared, _)| *declared == concern)
        .map(|(_, coverage)| *coverage)
        .unwrap_or(ConcernCoverage::Unsupported {
            reason: "coverage row missing",
        })
}

fn hook_coverage(descriptor: &AgentDescriptor, signal_kind: LifecycleSignalKind) -> HookCoverage {
    descriptor
        .lifecycle_hooks
        .iter()
        .find(|(declared, _)| *declared == signal_kind)
        .map(|(_, coverage)| *coverage)
        .unwrap_or(HookCoverage::Absent {
            reason: "lifecycle hook row missing",
        })
}

fn render_human(report: &CoverageReport, w: &mut impl Write) -> io::Result<()> {
    writeln!(w, "{}", paint(palette::ACCENT.bold(), "Rimz coverage"))?;
    render_matrix(
        w,
        "AGENT COVERAGE",
        "CONCERN",
        ["wired", "partial", "unsupported"],
        &report.coverage,
    )?;
    render_matrix(
        w,
        "HOOKS MATRIX",
        "SIGNAL",
        ["native", "derived", "absent"],
        &report.hooks_matrix,
    )
}

fn section(w: &mut impl Write, title: &str) -> io::Result<()> {
    writeln!(w)?;
    writeln!(w, "{}", paint(palette::ACCENT.bold(), title))
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
        let cells = std::iter::once(cell(agent.as_str()).fg(palette::ACCENT)).chain(
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
    writeln!(w, "{}", paint(palette::ACCENT.bold(), "DETAIL"))?;
    let mut table = Table::new([label_header, "DETAIL"]);
    for (agent_idx, agent) in matrix.agents.iter().enumerate() {
        table.section(agent);
        for row in &matrix.rows {
            let entry = &row.cells[agent_idx];
            let (glyph, style) = matrix_parts(entry.state);
            table.row([
                cell(row.label.as_str()).fg(palette::ACCENT),
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
        paint(palette::FAINT, "legend"),
        paint(ok_style, ok_glyph),
        paint(palette::FAINT, legend[0]),
        paint(partial_style, partial_glyph),
        paint(palette::FAINT, legend[1]),
        paint(absent_style, absent_glyph),
        paint(palette::FAINT, legend[2])
    )
}

fn matrix_cell(value: MatrixCellState) -> Cell {
    let (glyph, style) = matrix_parts(value);
    cell(glyph).fg(style)
}

fn matrix_parts(value: MatrixCellState) -> (&'static str, anstyle::Style) {
    match value {
        MatrixCellState::Ok => ("✓", palette::GOOD),
        MatrixCellState::Partial => ("◐", palette::WARN),
        MatrixCellState::Absent => ("✗", palette::MUTED),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strip(
        render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> io::Result<()>,
    ) -> String {
        let mut stream = anstream::StripStream::new(Vec::new());
        render_one(&mut stream).expect("render to in-memory buffer");
        String::from_utf8(stream.into_inner()).expect("utf-8")
    }

    fn agent_cells(matrix: &CoverageMatrix, agent: &str) -> Vec<MatrixCellState> {
        let idx = matrix
            .agents
            .iter()
            .position(|kind| kind == agent)
            .expect("agent column");
        matrix.rows.iter().map(|row| row.cells[idx].state).collect()
    }

    fn agent_labels<'a>(
        matrix: &'a CoverageMatrix,
        agent: &str,
        state: MatrixCellState,
    ) -> Vec<&'a str> {
        let idx = matrix
            .agents
            .iter()
            .position(|kind| kind == agent)
            .expect("agent column");
        matrix
            .rows
            .iter()
            .filter(|row| row.cells[idx].state == state)
            .map(|row| row.label.as_str())
            .collect()
    }

    fn row<'a>(matrix: &'a CoverageMatrix, label: &str) -> &'a MatrixRow {
        matrix
            .rows
            .iter()
            .find(|row| row.label == label)
            .expect("matrix row")
    }

    fn cell_detail<'a>(matrix: &CoverageMatrix, row: &'a MatrixRow, agent: &str) -> &'a str {
        let idx = matrix
            .agents
            .iter()
            .position(|kind| kind == agent)
            .expect("agent column");
        row.cells[idx].detail.as_str()
    }

    fn states(row: &MatrixRow) -> Vec<MatrixCellState> {
        row.cells.iter().map(|cell| cell.state).collect()
    }

    fn count(cells: &[MatrixCellState], needle: MatrixCellState) -> usize {
        cells.iter().filter(|cell| **cell == needle).count()
    }

    #[test]
    fn coverage_pins_agent_matrix() {
        let matrix = collect_coverage();
        assert_eq!(
            matrix.agents,
            [
                "claude",
                "codex",
                "amp",
                "copilot",
                "kimi",
                "pi",
                "opencode",
                "antigravity",
                "cursor",
                "droid",
                "kiro",
                "qwen"
            ]
        );
        assert_eq!(matrix.rows.len(), IntegrationConcern::ALL.len());

        let claude = agent_cells(&matrix, "claude");
        assert_eq!(
            count(&claude, MatrixCellState::Ok),
            IntegrationConcern::ALL.len()
        );
        assert_eq!(count(&claude, MatrixCellState::Partial), 0);
        assert_eq!(count(&claude, MatrixCellState::Absent), 0);

        let codex = agent_cells(&matrix, "codex");
        assert_eq!(count(&codex, MatrixCellState::Ok), 13);
        assert_eq!(count(&codex, MatrixCellState::Partial), 2);
        assert_eq!(count(&codex, MatrixCellState::Absent), 1);
        // `end` and `idle` have no native hook, but pane liveness/the reaper and
        // the turn-boundary/stall path reconstruct them — partial, not absent.
        assert_eq!(
            agent_labels(&matrix, "codex", MatrixCellState::Partial),
            ["end", "idle"]
        );
        assert_eq!(
            agent_labels(&matrix, "codex", MatrixCellState::Absent),
            ["bg"]
        );
        assert!(cell_detail(&matrix, row(&matrix, "end"), "codex").contains("SessionEnd"));

        let amp = agent_cells(&matrix, "amp");
        assert_eq!(count(&amp, MatrixCellState::Ok), 3);
        assert_eq!(count(&amp, MatrixCellState::Partial), 5);
        assert_eq!(count(&amp, MatrixCellState::Absent), 8);
        assert_eq!(
            agent_labels(&matrix, "amp", MatrixCellState::Partial),
            ["end", "idle", "usage", "live$", "spend"]
        );
        assert_eq!(
            agent_labels(&matrix, "amp", MatrixCellState::Absent),
            [
                "plan", "ask", "answer", "compact", "sub", "bg", "rich", "remote"
            ]
        );

        let copilot = agent_cells(&matrix, "copilot");
        assert_eq!(count(&copilot, MatrixCellState::Ok), 5);
        assert_eq!(count(&copilot, MatrixCellState::Partial), 4);
        assert_eq!(count(&copilot, MatrixCellState::Absent), 7);
        assert_eq!(
            agent_labels(&matrix, "copilot", MatrixCellState::Partial),
            ["compact", "idle", "usage", "rich"]
        );
        assert_eq!(
            agent_labels(&matrix, "copilot", MatrixCellState::Absent),
            ["plan", "answer", "sub", "bg", "live$", "spend", "remote"]
        );

        let kimi = agent_cells(&matrix, "kimi");
        assert_eq!(count(&kimi, MatrixCellState::Ok), 8);
        assert_eq!(count(&kimi, MatrixCellState::Partial), 4);
        assert_eq!(count(&kimi, MatrixCellState::Absent), 4);
        assert_eq!(
            agent_labels(&matrix, "kimi", MatrixCellState::Partial),
            ["sub", "idle", "usage", "spend"]
        );
        assert_eq!(
            agent_labels(&matrix, "kimi", MatrixCellState::Absent),
            ["answer", "bg", "rich", "remote"]
        );

        let pi = agent_cells(&matrix, "pi");
        assert_eq!(count(&pi, MatrixCellState::Ok), 9);
        assert_eq!(count(&pi, MatrixCellState::Partial), 1);
        assert_eq!(count(&pi, MatrixCellState::Absent), 6);
        // Pi's `agent_settled` marks final idle, while the stall window
        // reconstructs the missing idle-timeout nudge — partial, like Codex,
        // not absent. `live$` is wired: the extension pushes a running dollar
        // reconciled to the authoritative session spend sum every turn, so the
        // figure is visually full. Rich context is wired through immediate
        // value-changing envelopes and throttled streaming updates.
        assert_eq!(
            agent_labels(&matrix, "pi", MatrixCellState::Partial),
            ["idle"]
        );
        assert_eq!(
            agent_labels(&matrix, "pi", MatrixCellState::Absent),
            ["plan", "ask", "answer", "sub", "bg", "remote"]
        );

        let cursor = agent_cells(&matrix, "cursor");
        assert_eq!(count(&cursor, MatrixCellState::Ok), 4);
        assert_eq!(count(&cursor, MatrixCellState::Partial), 2);
        assert_eq!(count(&cursor, MatrixCellState::Absent), 10);
        assert_eq!(
            agent_labels(&matrix, "cursor", MatrixCellState::Partial),
            ["compact", "idle"]
        );
        assert_eq!(
            agent_labels(&matrix, "cursor", MatrixCellState::Absent),
            [
                "perm", "plan", "ask", "answer", "sub", "bg", "live$", "rich", "spend", "remote"
            ]
        );

        let droid = agent_cells(&matrix, "droid");
        assert_eq!(count(&droid, MatrixCellState::Ok), 5);
        assert_eq!(count(&droid, MatrixCellState::Partial), 0);
        assert_eq!(count(&droid, MatrixCellState::Absent), 11);

        let kiro = agent_cells(&matrix, "kiro");
        assert_eq!(count(&kiro, MatrixCellState::Ok), 0);
        assert_eq!(count(&kiro, MatrixCellState::Partial), 5);
        assert_eq!(count(&kiro, MatrixCellState::Absent), 11);
        assert_eq!(
            agent_labels(&matrix, "kiro", MatrixCellState::Partial),
            ["turn", "perm", "end", "idle", "usage"]
        );

        let qwen = agent_cells(&matrix, "qwen");
        assert_eq!(count(&qwen, MatrixCellState::Ok), 12);
        assert_eq!(count(&qwen, MatrixCellState::Partial), 2);
        assert_eq!(count(&qwen, MatrixCellState::Absent), 2);
        assert_eq!(
            agent_labels(&matrix, "qwen", MatrixCellState::Partial),
            ["live$", "spend"]
        );
        assert_eq!(
            agent_labels(&matrix, "qwen", MatrixCellState::Absent),
            ["answer", "remote"]
        );

        let antigravity = agent_cells(&matrix, "antigravity");
        assert_eq!(count(&antigravity, MatrixCellState::Ok), 0);
        assert_eq!(count(&antigravity, MatrixCellState::Partial), 3);
        assert_eq!(count(&antigravity, MatrixCellState::Absent), 13);
        assert_eq!(
            agent_labels(&matrix, "antigravity", MatrixCellState::Partial),
            ["turn", "end", "idle"]
        );
    }

    #[test]
    fn full_coverage_reports_no_gaps() {
        let matrix = collect_coverage();
        let claude = agent_cells(&matrix, "claude");
        assert_eq!(
            count(&claude, MatrixCellState::Ok),
            IntegrationConcern::ALL.len()
        );
        assert_eq!(count(&claude, MatrixCellState::Partial), 0);
        assert_eq!(count(&claude, MatrixCellState::Absent), 0);
    }

    #[test]
    fn hook_matrix_pins_lifecycle_signals() {
        let matrix = collect_hook_matrix();
        assert_eq!(
            matrix.agents,
            [
                "claude",
                "codex",
                "amp",
                "copilot",
                "kimi",
                "pi",
                "opencode",
                "antigravity",
                "cursor",
                "droid",
                "kiro",
                "qwen"
            ]
        );
        assert_eq!(matrix.rows.len(), LifecycleSignalKind::ALL.len());

        let ended = row(&matrix, "ended");
        assert_eq!(
            states(ended),
            [
                MatrixCellState::Ok,      // claude
                MatrixCellState::Partial, // codex
                MatrixCellState::Partial, // amp
                MatrixCellState::Ok,      // copilot
                MatrixCellState::Ok,      // kimi
                MatrixCellState::Ok,      // pi
                MatrixCellState::Ok,      // opencode
                MatrixCellState::Partial, // antigravity
                MatrixCellState::Ok,      // cursor
                MatrixCellState::Ok,      // droid
                MatrixCellState::Partial, // kiro
                MatrixCellState::Ok,      // qwen
            ]
        );
        assert!(cell_detail(&matrix, ended, "codex").contains("SessionEnd hook"));

        let subagent_started = row(&matrix, "subagent_started");
        assert_eq!(
            states(subagent_started),
            [
                MatrixCellState::Ok,      // claude
                MatrixCellState::Ok,      // codex
                MatrixCellState::Absent,  // amp
                MatrixCellState::Absent,  // copilot
                MatrixCellState::Partial, // kimi
                MatrixCellState::Absent,  // pi
                MatrixCellState::Ok,      // opencode
                MatrixCellState::Absent,  // antigravity
                MatrixCellState::Absent,  // cursor
                MatrixCellState::Absent,  // droid
                MatrixCellState::Absent,  // kiro
                MatrixCellState::Ok,      // qwen
            ]
        );
    }

    #[test]
    fn human_report_renders_grid_legend_and_detail() {
        let report = CoverageReport {
            coverage: CoverageMatrix {
                agents: vec!["codex".to_owned(), "pi".to_owned()],
                rows: vec![
                    MatrixRow {
                        label: "turn".to_owned(),
                        cells: vec![
                            MatrixCell::ok("SessionStart/UserPromptSubmit/Stop"),
                            MatrixCell::ok("agent_started/prompt"),
                        ],
                    },
                    MatrixRow {
                        label: "end".to_owned(),
                        cells: vec![
                            MatrixCell::partial("pane liveness + reaper — no SessionEnd hook"),
                            MatrixCell::ok("agent_end"),
                        ],
                    },
                    MatrixRow {
                        label: "plan".to_owned(),
                        cells: vec![
                            MatrixCell::absent("no plan-approval gate"),
                            MatrixCell::absent("no plan-approval gate"),
                        ],
                    },
                ],
            },
            hooks_matrix: CoverageMatrix {
                agents: Vec::new(),
                rows: Vec::new(),
            },
        };

        let out = strip(|w| render_human(&report, w));
        assert!(out.contains("Rimz coverage"), "{out}");
        assert!(
            out.contains("AGENT") && out.contains("codex") && out.contains("pi"),
            "grid header and agent rows:\n{out}"
        );
        assert!(
            out.contains("legend ✓ wired   ◐ partial   ✗ unsupported"),
            "legend renders:\n{out}"
        );
        assert!(out.contains("DETAIL"), "detail block title:\n{out}");
        assert!(out.contains("CONCERN"), "detail label header:\n{out}");
        assert!(
            out.contains("✓ SessionStart/UserPromptSubmit/Stop"),
            "ok detail annotated:\n{out}"
        );
        assert!(
            out.contains("◐ pane liveness + reaper — no SessionEnd hook"),
            "partial detail kept:\n{out}"
        );
        assert!(
            out.contains("✗ no plan-approval gate"),
            "absent detail kept:\n{out}"
        );

        let detail = out.split_once("DETAIL").expect("detail block").1;
        let codex = detail.find("codex").expect("codex section");
        let pi = detail.find("pi").expect("pi section");
        assert!(codex < pi, "detail preserves registry agent order:\n{out}");
    }

    #[test]
    fn human_report_renders_hook_legend_words() {
        let report = CoverageReport {
            coverage: CoverageMatrix {
                agents: Vec::new(),
                rows: Vec::new(),
            },
            hooks_matrix: CoverageMatrix {
                agents: vec!["claude".to_owned(), "codex".to_owned()],
                rows: vec![MatrixRow {
                    label: "ended".to_owned(),
                    cells: vec![
                        MatrixCell::ok("SessionEnd"),
                        MatrixCell::partial("pane liveness + reaper — no SessionEnd hook"),
                    ],
                }],
            },
        };

        let out = strip(|w| render_human(&report, w));
        assert!(out.contains("HOOKS MATRIX"), "{out}");
        assert!(out.contains("SIGNAL"), "{out}");
        assert!(
            out.contains("legend ✓ native   ◐ derived   ✗ absent"),
            "hook legend renders:\n{out}"
        );
    }
}
