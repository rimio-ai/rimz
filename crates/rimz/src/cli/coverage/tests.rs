use super::*;

fn strip(render_one: impl FnOnce(&mut anstream::StripStream<Vec<u8>>) -> io::Result<()>) -> String {
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
            "qwen",
            "grok",
            "hermes",
            "qoder",
            "omp",
            "muse",
            "minimax"
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
    assert_eq!(count(&codex, MatrixCellState::Ok), 14);
    assert_eq!(count(&codex, MatrixCellState::Partial), 3);
    assert_eq!(count(&codex, MatrixCellState::Absent), 2);
    // `end` and `idle` are derived, while live tool hooks miss two paths.
    assert_eq!(
        agent_labels(&matrix, "codex", MatrixCellState::Partial),
        ["end", "idle", "tools"]
    );
    assert_eq!(
        agent_labels(&matrix, "codex", MatrixCellState::Absent),
        ["bg", "bgsh"]
    );
    assert!(cell_detail(&matrix, row(&matrix, "end"), "codex").contains("SessionEnd"));

    let amp = agent_cells(&matrix, "amp");
    assert_eq!(count(&amp, MatrixCellState::Ok), 3);
    assert_eq!(count(&amp, MatrixCellState::Partial), 2);
    assert_eq!(count(&amp, MatrixCellState::Absent), 14);
    assert_eq!(
        agent_labels(&matrix, "amp", MatrixCellState::Partial),
        ["end", "idle"]
    );
    assert_eq!(
        agent_labels(&matrix, "amp", MatrixCellState::Absent),
        [
            "plan", "ask", "answer", "compact", "sub", "remind", "bg", "bgsh", "usage", "live$",
            "rich", "spend", "tools", "remote"
        ]
    );

    let copilot = agent_cells(&matrix, "copilot");
    assert_eq!(count(&copilot, MatrixCellState::Ok), 7);
    assert_eq!(count(&copilot, MatrixCellState::Partial), 5);
    assert_eq!(count(&copilot, MatrixCellState::Absent), 7);
    assert_eq!(
        agent_labels(&matrix, "copilot", MatrixCellState::Partial),
        ["compact", "sub", "idle", "live$", "spend"]
    );
    assert_eq!(
        agent_labels(&matrix, "copilot", MatrixCellState::Absent),
        ["plan", "answer", "remind", "bg", "bgsh", "tools", "remote"]
    );

    let kimi = agent_cells(&matrix, "kimi");
    assert_eq!(count(&kimi, MatrixCellState::Ok), 8);
    assert_eq!(count(&kimi, MatrixCellState::Partial), 4);
    assert_eq!(count(&kimi, MatrixCellState::Absent), 7);
    assert_eq!(
        agent_labels(&matrix, "kimi", MatrixCellState::Partial),
        ["sub", "idle", "usage", "spend"]
    );
    assert_eq!(
        agent_labels(&matrix, "kimi", MatrixCellState::Absent),
        ["answer", "remind", "bg", "bgsh", "rich", "tools", "remote"]
    );

    let pi = agent_cells(&matrix, "pi");
    assert_eq!(count(&pi, MatrixCellState::Ok), 13);
    assert_eq!(count(&pi, MatrixCellState::Partial), 1);
    assert_eq!(count(&pi, MatrixCellState::Absent), 5);
    // Pi's `agent_settled` marks final idle, while the stall window
    // reconstructs the missing idle-timeout nudge — partial, like Codex,
    // not absent. Subagents are wired: the rimz extension bridges pi's
    // native `subagent_started`/`subagent_stopped` bus events in full;
    // nicobailon foreground runs emit no bus lifecycle at all, an
    // extension-side omission rather than an adapter gap. `live$` is wired:
    // the extension pushes a running dollar reconciled to the authoritative
    // session spend sum every turn. Rich context is wired through immediate
    // value-changing envelopes and throttled streaming updates.
    assert_eq!(
        agent_labels(&matrix, "pi", MatrixCellState::Partial),
        ["idle"]
    );
    assert_eq!(
        agent_labels(&matrix, "pi", MatrixCellState::Absent),
        ["perm", "plan", "bg", "bgsh", "remote"]
    );

    let cursor = agent_cells(&matrix, "cursor");
    assert_eq!(count(&cursor, MatrixCellState::Ok), 5);
    assert_eq!(count(&cursor, MatrixCellState::Partial), 6);
    assert_eq!(count(&cursor, MatrixCellState::Absent), 8);
    assert_eq!(
        agent_labels(&matrix, "cursor", MatrixCellState::Partial),
        ["plan", "ask", "compact", "sub", "idle", "live$"]
    );
    assert_eq!(
        agent_labels(&matrix, "cursor", MatrixCellState::Absent),
        [
            "perm", "answer", "remind", "bg", "bgsh", "spend", "tools", "remote"
        ]
    );

    let droid = agent_cells(&matrix, "droid");
    assert_eq!(count(&droid, MatrixCellState::Ok), 7);
    assert_eq!(count(&droid, MatrixCellState::Partial), 5);
    assert_eq!(count(&droid, MatrixCellState::Absent), 7);
    assert_eq!(
        agent_labels(&matrix, "droid", MatrixCellState::Partial),
        ["plan", "compact", "idle", "live$", "rich"]
    );
    assert_eq!(
        agent_labels(&matrix, "droid", MatrixCellState::Absent),
        ["answer", "sub", "bg", "bgsh", "spend", "tools", "remote"]
    );

    let kiro = agent_cells(&matrix, "kiro");
    assert_eq!(count(&kiro, MatrixCellState::Ok), 3);
    assert_eq!(count(&kiro, MatrixCellState::Partial), 5);
    assert_eq!(count(&kiro, MatrixCellState::Absent), 11);
    assert_eq!(
        agent_labels(&matrix, "kiro", MatrixCellState::Partial),
        ["perm", "ask", "idle", "usage", "rich"]
    );

    let qwen = agent_cells(&matrix, "qwen");
    assert_eq!(count(&qwen, MatrixCellState::Ok), 13);
    assert_eq!(count(&qwen, MatrixCellState::Partial), 2);
    assert_eq!(count(&qwen, MatrixCellState::Absent), 4);
    assert_eq!(
        agent_labels(&matrix, "qwen", MatrixCellState::Partial),
        ["live$", "spend"]
    );
    assert_eq!(
        agent_labels(&matrix, "qwen", MatrixCellState::Absent),
        ["answer", "bgsh", "tools", "remote"]
    );

    let grok = agent_cells(&matrix, "grok");
    let qoder = agent_cells(&matrix, "qoder");
    assert_eq!(count(&qoder, MatrixCellState::Ok), 10);
    assert_eq!(count(&qoder, MatrixCellState::Partial), 0);
    assert_eq!(count(&qoder, MatrixCellState::Absent), 9);
    assert_eq!(
        agent_labels(&matrix, "qoder", MatrixCellState::Absent),
        [
            "answer", "bg", "bgsh", "usage", "live$", "rich", "spend", "tools", "remote"
        ]
    );
    assert_eq!(count(&grok, MatrixCellState::Ok), 11);
    assert_eq!(count(&grok, MatrixCellState::Partial), 3);
    assert_eq!(count(&grok, MatrixCellState::Absent), 5);
    assert_eq!(
        agent_labels(&matrix, "grok", MatrixCellState::Partial),
        ["sub", "live$", "rich"]
    );
    assert_eq!(
        agent_labels(&matrix, "grok", MatrixCellState::Absent),
        ["answer", "bg", "bgsh", "tools", "remote"]
    );

    let omp = agent_cells(&matrix, "omp");
    assert_eq!(count(&omp, MatrixCellState::Ok), 10);
    assert_eq!(count(&omp, MatrixCellState::Partial), 3);
    assert_eq!(count(&omp, MatrixCellState::Absent), 6);
    assert_eq!(
        agent_labels(&matrix, "omp", MatrixCellState::Partial),
        ["idle", "live$", "spend"]
    );
    assert_eq!(
        agent_labels(&matrix, "omp", MatrixCellState::Absent),
        ["plan", "answer", "sub", "bg", "bgsh", "remote"]
    );

    let muse = agent_cells(&matrix, "muse");
    assert_eq!(count(&muse, MatrixCellState::Ok), 4);
    assert_eq!(count(&muse, MatrixCellState::Partial), 0);
    assert_eq!(count(&muse, MatrixCellState::Absent), 15);
    assert_eq!(
        agent_labels(&matrix, "muse", MatrixCellState::Ok),
        ["turn", "perm", "end", "install"]
    );

    let antigravity = agent_cells(&matrix, "antigravity");
    assert_eq!(count(&antigravity, MatrixCellState::Ok), 5);
    assert_eq!(count(&antigravity, MatrixCellState::Partial), 7);
    assert_eq!(count(&antigravity, MatrixCellState::Absent), 7);
    assert_eq!(
        agent_labels(&matrix, "antigravity", MatrixCellState::Partial),
        ["perm", "ask", "sub", "end", "idle", "live$", "tools"]
    );
    assert_eq!(
        agent_labels(&matrix, "antigravity", MatrixCellState::Absent),
        [
            "plan", "answer", "compact", "remind", "bgsh", "spend", "remote"
        ]
    );

    let minimax = agent_cells(&matrix, "minimax");
    assert_eq!(count(&minimax, MatrixCellState::Ok), 7);
    assert_eq!(count(&minimax, MatrixCellState::Partial), 2);
    assert_eq!(count(&minimax, MatrixCellState::Absent), 10);
    assert_eq!(
        agent_labels(&matrix, "minimax", MatrixCellState::Partial),
        ["idle", "rich"]
    );
    assert_eq!(
        agent_labels(&matrix, "minimax", MatrixCellState::Absent),
        [
            "plan", "ask", "answer", "sub", "bg", "bgsh", "usage", "live$", "spend", "remote"
        ]
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
            "qwen",
            "grok",
            "hermes",
            "qoder",
            "omp",
            "muse",
            "minimax"
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
            MatrixCellState::Ok,      // kiro
            MatrixCellState::Ok,      // qwen
            MatrixCellState::Ok,      // grok
            MatrixCellState::Partial, // hermes
            MatrixCellState::Ok,      // qoder
            MatrixCellState::Ok,      // omp
            MatrixCellState::Ok,      // muse
            MatrixCellState::Ok,      // minimax
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
            MatrixCellState::Partial, // copilot
            MatrixCellState::Partial, // kimi
            MatrixCellState::Ok,      // pi
            MatrixCellState::Ok,      // opencode
            MatrixCellState::Partial, // antigravity
            MatrixCellState::Partial, // cursor
            MatrixCellState::Absent,  // droid
            MatrixCellState::Absent,  // kiro
            MatrixCellState::Ok,      // qwen
            MatrixCellState::Ok,      // grok
            MatrixCellState::Ok,      // hermes
            MatrixCellState::Ok,      // qoder
            MatrixCellState::Absent,  // omp
            MatrixCellState::Absent,  // muse
            MatrixCellState::Absent,  // minimax
        ]
    );

    let subagent_stopped = row(&matrix, "subagent_stopped");
    assert_eq!(
        states(subagent_stopped),
        [
            MatrixCellState::Ok,      // claude
            MatrixCellState::Ok,      // codex
            MatrixCellState::Absent,  // amp
            MatrixCellState::Partial, // copilot
            MatrixCellState::Partial, // kimi
            MatrixCellState::Ok,      // pi
            MatrixCellState::Ok,      // opencode
            MatrixCellState::Partial, // antigravity
            MatrixCellState::Partial, // cursor
            MatrixCellState::Absent,  // droid
            MatrixCellState::Absent,  // kiro
            MatrixCellState::Ok,      // qwen
            MatrixCellState::Partial, // grok
            MatrixCellState::Ok,      // hermes
            MatrixCellState::Ok,      // qoder
            MatrixCellState::Absent,  // omp
            MatrixCellState::Absent,  // muse
            MatrixCellState::Absent,  // minimax
        ]
    );
}

#[test]
fn human_report_renders_grid_legend_and_detail() {
    let report = CoverageReport {
        capabilities: CoverageMatrix {
            agents: Vec::new(),
            rows: Vec::new(),
        },
        coverage: Some(CoverageMatrix {
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
        }),
        hooks_matrix: None,
    };

    let out = strip(|w| render_human(&report, w));
    assert!(out.contains("RimZ coverage"), "{out}");
    assert!(
        out.contains("AGENT") && out.contains("codex") && out.contains("pi"),
        "grid header and agent rows:\n{out}"
    );
    assert!(
        out.contains("legend ✓ wired   ! partial   ✗ unsupported"),
        "legend renders:\n{out}"
    );
    assert!(out.contains("DETAIL"), "detail block title:\n{out}");
    assert!(out.contains("CONCERN"), "detail label header:\n{out}");
    assert!(
        out.contains("✓ SessionStart/UserPromptSubmit/Stop"),
        "ok detail annotated:\n{out}"
    );
    assert!(
        out.contains("! pane liveness + reaper — no SessionEnd hook"),
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
        capabilities: CoverageMatrix {
            agents: Vec::new(),
            rows: Vec::new(),
        },
        coverage: None,
        hooks_matrix: Some(CoverageMatrix {
            agents: vec!["claude".to_owned(), "codex".to_owned()],
            rows: vec![MatrixRow {
                label: "ended".to_owned(),
                cells: vec![
                    MatrixCell::ok("SessionEnd"),
                    MatrixCell::partial("pane liveness + reaper — no SessionEnd hook"),
                ],
            }],
        }),
    };

    let out = strip(|w| render_human(&report, w));
    assert!(out.contains("WIRING — LIFECYCLE HOOKS"), "{out}");
    assert!(out.contains("SIGNAL"), "{out}");
    assert!(
        out.contains("legend ✓ native   ! derived   ✗ absent"),
        "hook legend renders:\n{out}"
    );
}
