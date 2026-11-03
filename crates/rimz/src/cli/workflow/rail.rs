use std::collections::BTreeMap;
use std::io::Write;
use unicode_width::UnicodeWidthStr;

use rimz::config::GlyphRole;
use rimz::harness::schedule::when::WhenExpr;
use rimz::harness::workflow::{
    CheckKind, Edge, EdgeSource, Fireability, Hop, HopKind, StepKind, WalkNode, WorkflowSpec,
};
use rimz::store::workflow::{JournalEntry, JournalEvent, RunRecord, RunState, StepLifecycle};

use super::render;

pub(super) fn reached<'a>(hops: &'a [Hop], journal: &'a [JournalEntry]) -> BTreeMap<&'a str, u64> {
    let mut counts = BTreeMap::new();
    let mut position = None;
    for hop in hops {
        if hop.kind == HopKind::VisitsReset {
            continue;
        }
        let node = hop.node.as_str();
        if position != Some(node) || hop.kind == HopKind::Resume {
            *counts.entry(node).or_default() += 1;
        }
        position = Some(node);
        if hop.kind == HopKind::Route
            && let Some(target) = hop
                .target
                .as_deref()
                .filter(|target| !target.starts_with('@'))
        {
            *counts.entry(target).or_default() += hop.count;
            position = Some(target);
        }
    }
    for entry in journal {
        if matches!(
            entry.event,
            JournalEvent::Attempt { .. } | JournalEvent::Evaluation { .. }
        ) {
            counts.entry(entry.node.as_str()).or_insert(1);
        }
    }
    counts
}

struct TrackNode {
    name: String,
    parent: Option<String>,
    glyph: String,
    glyph_style: anstyle::Style,
    name_style: anstyle::Style,
    reached: bool,
    current: bool,
    ending: bool,
    visits: u64,
}

impl TrackNode {
    fn width(&self) -> usize {
        self.glyph.width() + 1 + self.name.width()
    }

    fn repeat(&self) -> String {
        if self.visits > 1 {
            format!(" ×{}", self.visits)
        } else {
            String::new()
        }
    }

    fn write(&self, out: &mut impl Write) -> std::io::Result<()> {
        write!(
            out,
            "{} {}",
            render::paint(self.glyph_style, &self.glyph),
            render::paint(self.name_style, &self.name)
        )
    }
}

fn track_positions<'a>(
    main: &[&'a TrackNode],
    left: usize,
    right: usize,
) -> Vec<(usize, &'a TrackNode)> {
    main[left..right]
        .iter()
        .scan(if left > 0 { 2 } else { 0 }, |column, node| {
            let start = *column;
            *column += node.width() + 3;
            Some((start, *node))
        })
        .collect()
}

fn track_width(positions: &[(usize, &TrackNode)], hangs: &[&TrackNode]) -> usize {
    positions
        .iter()
        .map(|(column, node)| {
            let hang = hangs
                .iter()
                .filter(|hang| hang.parent.as_deref() == Some(node.name.as_str()))
                .map(|hang| 3 + hang.width() + hang.repeat().width() + 2)
                .max()
                .unwrap_or(0);
            column + node.width().max(hang)
        })
        .max()
        .unwrap_or(0)
}

pub(super) fn horizontal(
    out: &mut impl Write,
    spec: &WorkflowSpec,
    record: &RunRecord,
    hops: &[Hop],
    journal: &[JournalEntry],
    width: Option<usize>,
    indent: usize,
) -> std::io::Result<()> {
    let counts = reached(hops, journal);
    let ending = hops
        .iter()
        .rposition(|hop| matches!(hop.kind, HopKind::Terminal { .. }))
        .and_then(|index| {
            hops[..index]
                .iter()
                .rev()
                .find(|hop| hop.kind == HopKind::Route)
        })
        .map(|hop| hop.node.as_str());
    let role = super::workflow_view::glyph_role(record);
    let machine = super::super::machine_config();
    let glyph = rimz::theme::theme_glyphs(&machine.theme);
    let walk = spec.walk();
    let reachable = walk.nodes.len();
    let nodes = walk
        .nodes
        .into_iter()
        .chain(walk.unreachable.into_iter().map(|node| WalkNode {
            node,
            hangs_under: None,
        }))
        .map(|node| {
            let current = !record.state.is_terminal()
                && (node.node == record.node
                    || record
                        .held
                        .as_ref()
                        .is_some_and(|held| held.attempt.node == node.node));
            let reached = counts.contains_key(node.node.as_str());
            let check = spec.checks.contains_key(&node.node);
            let terminal = record.state.is_terminal() && ending == Some(node.node.as_str());
            let state_style =
                render::status::workflow(record.state, role == GlyphRole::StatusWaiting);
            let (glyph_style, name_style) = if current {
                (state_style.bold(), state_style.bold())
            } else if reached {
                let style = if terminal && record.state == RunState::Failed {
                    render::palette::alarm()
                } else {
                    render::palette::good()
                };
                (
                    style,
                    if terminal {
                        style
                    } else {
                        render::palette::body()
                    },
                )
            } else {
                (render::palette::muted(), render::palette::muted())
            };
            TrackNode {
                glyph: glyph(if current {
                    role
                } else {
                    match (check, reached) {
                        (true, true) => GlyphRole::PipelineCheckPassed,
                        (true, false) => GlyphRole::PipelineCheckFuture,
                        (false, true) => GlyphRole::PipelinePassed,
                        (false, false) => GlyphRole::PipelineFuture,
                    }
                }),
                glyph_style,
                name_style,
                reached,
                current,
                ending: terminal,
                visits: record
                    .visits
                    .get(&node.node)
                    .map_or(0, |visits| visits.total),
                name: node.node,
                parent: node.hangs_under,
            }
        })
        .collect::<Vec<_>>();
    let (nodes, unreachable) = nodes.split_at(reachable);
    let main = nodes
        .iter()
        .filter(|node| node.parent.is_none())
        .collect::<Vec<_>>();
    let hangs = nodes
        .iter()
        .filter(|node| node.parent.is_some() && (node.visits > 0 || node.current || node.ending))
        .collect::<Vec<_>>();
    let anchors = main
        .iter()
        .enumerate()
        .filter(|(_, node)| {
            node.current
                || node.ending
                || hangs.iter().any(|hang| {
                    (hang.current || hang.ending)
                        && hang.parent.as_deref() == Some(node.name.as_str())
                })
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let first = anchors
        .first()
        .copied()
        .unwrap_or(main.len().saturating_sub(1))
        .saturating_sub(1);
    let last = anchors
        .last()
        .copied()
        .unwrap_or(main.len().saturating_sub(1));
    let (mut left, mut right) = (0, main.len());
    let positions = loop {
        let positions = track_positions(&main, left, right);
        if width.is_none_or(|width| track_width(&positions, &hangs) <= width.saturating_sub(indent))
        {
            break positions;
        }
        if left < first {
            left += 1;
            continue;
        }
        if right > last + 1 {
            right -= 1;
            continue;
        }
        break positions;
    };
    if positions.is_empty() {
        return Ok(());
    }
    write!(out, "{:indent$}", "")?;
    if left > 0 {
        write!(out, "{}", render::paint(render::palette::faint(), "… "))?;
    }
    for (index, (_, node)) in positions.iter().enumerate() {
        if index > 0 {
            write!(out, "{}", render::paint(render::palette::faint(), " ─ "))?;
        }
        node.write(out)?;
    }
    writeln!(out)?;
    for (column, parent) in &positions {
        for hang in hangs
            .iter()
            .filter(|hang| hang.parent.as_deref() == Some(parent.name.as_str()))
        {
            write!(
                out,
                "{:padding$}{}",
                "",
                render::paint(render::palette::faint(), "╰─ "),
                padding = indent + column
            )?;
            hang.write(out)?;
            writeln!(
                out,
                "{}{}",
                hang.repeat(),
                render::paint(render::palette::faint(), " ↑")
            )?;
        }
    }
    for node in unreachable
        .iter()
        .filter(|node| node.current || node.reached)
    {
        write!(out, "{:indent$}", "")?;
        node.write(out)?;
        writeln!(out)?;
    }
    Ok(())
}

pub(super) struct Rail {
    pub(super) rows: Vec<Row>,
    pub(super) unreachable: Vec<Row>,
    start: String,
}

pub(super) struct Row {
    pub(super) name: String,
    pub(super) hangs_under: Option<String>,
    pub(super) glyph: GlyphRole,
    pub(super) style: anstyle::Style,
    pub(super) heading: String,
    pub(super) summary: Option<String>,
    pub(super) details: Vec<String>,
    pub(super) cases: Vec<CaseRow>,
}

pub(super) struct CaseRow {
    pub(super) mark: String,
    pub(super) style: anstyle::Style,
    pub(super) number: Option<usize>,
    pub(super) when: String,
    pub(super) outcome: String,
    pub(super) target: String,
    pub(super) readings: Vec<String>,
}

impl Rail {
    pub(super) fn definition(spec: &WorkflowSpec) -> Self {
        let walk = spec.walk();
        let order = walk
            .nodes
            .iter()
            .map(|row| row.node.as_str())
            .chain(walk.unreachable.iter().map(String::as_str))
            .collect::<Vec<_>>();
        let rows = walk
            .nodes
            .iter()
            .map(|row| Row::definition(spec, &row.node, row.hangs_under.clone(), &order))
            .collect();
        let unreachable = walk
            .unreachable
            .iter()
            .map(|name| Row::definition(spec, name, None, &order))
            .collect();
        Self {
            rows,
            unreachable,
            start: spec.workflow.start.clone(),
        }
    }

    pub(super) fn write(&self, out: &mut impl Write, width: usize) -> std::io::Result<()> {
        for row in &self.rows {
            row.write(out, width)?;
        }
        if !self.unreachable.is_empty() {
            writeln!(out, "\nunreachable from {}", self.start)?;
            for row in &self.unreachable {
                row.write(out, width)?;
            }
        }
        Ok(())
    }
}

impl Row {
    fn definition(
        spec: &WorkflowSpec,
        name: &str,
        hangs_under: Option<String>,
        order: &[&str],
    ) -> Self {
        let edges = spec.edges(name);
        let live = edges
            .iter()
            .filter(|edge| edge.fires != Fireability::Never)
            .collect::<Vec<_>>();
        let mut row = Self {
            name: name.into(),
            hangs_under,
            glyph: GlyphRole::PipelineFuture,
            style: render::palette::body(),
            heading: String::new(),
            summary: None,
            details: Vec::new(),
            cases: Vec::new(),
        };
        if let Some(check) = spec.checks.get(name) {
            let runner = match check.kind() {
                Some(CheckKind::Run(run)) => render::one_line(run),
                Some(CheckKind::Shell(shell)) => format!("sh: {}", render::one_line(shell)),
                Some(CheckKind::Agent(agent)) => format!("{agent}, headless"),
                Some(CheckKind::Condition) => "condition".into(),
                None => "invalid runner; see workflow validate".into(),
            };
            row.heading = format!("check  {runner}");
            row.summary.clone_from(&check.summary);
            if let Some(timeout) = &check.timeout
                && let Some(edge) = live.iter().find(|edge| edge.outcome == "timeout")
            {
                row.heading.push_str(&format!(
                    " · waits ≤{timeout}, then {}",
                    target(name, &edge.target, order)
                ));
            }
            if let Some(window) = &check.window {
                row.heading.push_str(&format!(" · window {window}"));
            }
            if !check.cases.is_empty() {
                if let Some(note) = case_note(name, &edges, order) {
                    row.details.push(note);
                }
                for (index, case) in check.cases.iter().enumerate() {
                    if let Some(edge) = live.iter().find(|edge| edge.outcome == case.outcome) {
                        let hold = case
                            .hold
                            .as_ref()
                            .map(|hold| format!(" for {hold}"))
                            .unwrap_or_default();
                        row.cases.push(CaseRow {
                            mark: String::new(),
                            style: match edge.target.as_str() {
                                "@done" => render::palette::good(),
                                "@failed" => render::palette::alarm(),
                                _ => render::palette::body(),
                            },
                            number: Some(index + 1),
                            when: format!(
                                "{}{hold}",
                                WhenExpr::parse(&case.when)
                                    .map(|expression| expression.to_string())
                                    .unwrap_or_else(|_| format!(
                                        "invalid condition: {:?}",
                                        case.when
                                    ))
                            ),
                            outcome: case.outcome.clone(),
                            target: target(name, &edge.target, order),
                            readings: Vec::new(),
                        });
                    }
                }
                if let Some(outcome) = &check.else_outcome
                    && let Some(edge) = live.iter().find(|edge| edge.outcome == *outcome)
                {
                    row.cases.push(CaseRow {
                        mark: String::new(),
                        style: match edge.target.as_str() {
                            "@done" => render::palette::good(),
                            "@failed" => render::palette::alarm(),
                            _ => render::palette::body(),
                        },
                        number: None,
                        when: "none true".into(),
                        outcome: outcome.clone(),
                        target: target(name, &edge.target, order),
                        readings: Vec::new(),
                    });
                }
            }
            let mut routes = Vec::new();
            for edge in live {
                if edge.outcome == "timeout"
                    || edge.outcome == "deadline"
                    || (edge.outcome == "invalid" && edge.source == EdgeSource::Default)
                    || (!check.cases.is_empty()
                        && (edge.outcome == "unknown"
                            || check.cases.iter().any(|case| case.outcome == edge.outcome)
                            || check.else_outcome.as_deref() == Some(&edge.outcome)))
                {
                    continue;
                }
                routes.push(edge_text(name, edge, order));
            }
            if !routes.is_empty() {
                row.details.push(routes.join(" · "));
            }
        } else if let Some(step) = spec.steps.get(name) {
            let runner = match step.kind() {
                Some(StepKind::Run(run)) => render::one_line(run),
                Some(StepKind::Shell(shell)) => format!("sh: {}", render::one_line(shell)),
                Some(StepKind::Agent(agent)) => format!(
                    "{agent} · {}",
                    match step.lifecycle.unwrap_or_default() {
                        StepLifecycle::Turn => "1 turn",
                        StepLifecycle::Stay => "lifecycle stay",
                        StepLifecycle::Until => "lifecycle until",
                    }
                ),
                None => "invalid runner; see workflow validate".into(),
            };
            row.heading = format!("step  {runner}");
            if let Some(timeout) = &step.timeout {
                row.heading.push_str(&format!(" ≤{timeout}"));
            }
            row.summary.clone_from(&step.summary);
            if step.takeover == Some(true) {
                row.details.push("takes over".into());
            }
            if let Some(window) = &step.window {
                row.details.push(format!("window {window}"));
            }
            if let Some(wait) = step.capacity_wait
                && wait != rimz::config::CapacityWait::Forever
            {
                row.details
                    .push(format!("capacity-wait {}", String::from(wait)));
            }
            for edge in live {
                match edge.outcome.as_str() {
                    "deadline" => continue,
                    "cap" => {
                        if let Some(max) = step.max_visits {
                            row.details.push(format!(
                                "{max} visits, then {}",
                                target(name, &edge.target, order)
                            ));
                        }
                        if let Some(max) = &step.max_visits_per_outcome {
                            let reset_on = match max.reset_on() {
                                [] => String::new(),
                                outcomes => format!(" (reset-on {})", outcomes.join(", ")),
                            };
                            row.details.push(format!(
                                "max-visits-per-outcome {}{reset_on}, then {}",
                                max.limit(),
                                target(name, &edge.target, order)
                            ));
                        }
                    }
                    "then" => row.details.push(target(name, &edge.target, order)),
                    _ => row.details.push(edge_text(name, edge, order)),
                }
            }
            row.heading
                .push_str(&format!(" · {}", row.details.join(" · ")));
            row.details.clear();
        }
        row
    }

    fn write(&self, out: &mut impl Write, width: usize) -> std::io::Result<()> {
        let machine = super::super::machine_config();
        let glyph = rimz::theme::theme_glyphs(&machine.theme)(self.glyph);
        let prefix = if self.hangs_under.is_some() {
            "├─"
        } else {
            ""
        };
        let heading = format!("{prefix}{glyph} {}  {}", self.name, self.heading);
        write_wrapped(out, "", "│   ", &heading, width, self.style)?;
        if let Some(summary) = &self.summary {
            write_wrapped(
                out,
                "│   ",
                "│   ",
                summary,
                width,
                render::palette::muted(),
            )?;
        }
        for detail in &self.details {
            write_wrapped(out, "│   ", "│   ", detail, width, render::palette::body())?;
        }
        for case in &self.cases {
            let number = case.number.map(|n| n.to_string()).unwrap_or_default();
            let text = format!(
                "{} {number}  {}  {} {}",
                case.mark, case.when, case.outcome, case.target
            );
            write_wrapped(out, "│   ", "│      ", text.trim_start(), width, case.style)?;
            for reading in &case.readings {
                write_wrapped(
                    out,
                    "│      ",
                    "│      ",
                    reading,
                    width,
                    render::palette::muted(),
                )?;
            }
        }
        writeln!(out, "│")
    }
}

pub(super) fn case_note(node: &str, edges: &[Edge], order: &[&str]) -> Option<String> {
    let edge = edges.iter().find(|edge| edge.outcome == "unknown")?;
    let action = if edge.target == "@wait" {
        "waits".into()
    } else {
        format!("goes {}", target(node, &edge.target, order))
    };
    Some(format!(
        "the first true case wins; an unread term above it {action}"
    ))
}

fn edge_text(node: &str, edge: &Edge, order: &[&str]) -> String {
    let mark = edge
        .mark()
        .map(|mark| format!(" ({mark})"))
        .unwrap_or_default();
    format!(
        "{} {}{mark}",
        edge.outcome,
        target(node, &edge.target, order)
    )
}

fn target(node: &str, target: &str, order: &[&str]) -> String {
    if target.starts_with('@') {
        return target.into();
    }
    let above = order.iter().position(|name| *name == target)
        <= order.iter().position(|name| *name == node);
    format!("{} {target}", if above { "↑" } else { "→" })
}

fn write_wrapped(
    out: &mut impl Write,
    first: &str,
    rest: &str,
    text: &str,
    width: usize,
    style: anstyle::Style,
) -> std::io::Result<()> {
    let budget = width
        .saturating_sub(unicode_width::UnicodeWidthStr::width(rest))
        .max(1);
    let mut lines = Vec::new();
    let mut current = String::new();
    for part in text.split(" · ") {
        let joined = if current.is_empty() {
            part.to_owned()
        } else {
            format!("{current} · {part}")
        };
        if unicode_width::UnicodeWidthStr::width(joined.as_str()) <= budget {
            current = joined;
            continue;
        }
        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
        }
        let mut wrapped = render::wrap_words(part, budget);
        current = wrapped.pop().unwrap_or_default();
        lines.extend(wrapped);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    for (index, line) in lines.iter().enumerate() {
        let prefix = if index == 0 { first } else { rest };
        writeln!(out, "{prefix}{}", render::paint(style, line))?;
    }
    Ok(())
}

pub(super) fn defaults(spec: &WorkflowSpec) -> Vec<&'static str> {
    let mut sentences = Vec::new();
    let edges = spec
        .checks
        .keys()
        .chain(spec.steps.keys())
        .flat_map(|name| spec.edges(name))
        .collect::<Vec<_>>();
    if spec.steps.values().any(|step| {
        matches!(step.kind(), Some(StepKind::Agent(_)))
            && step.lifecycle.unwrap_or_default().is_turn()
    }) {
        sentences.push("A step goes on to its next check however its turn ends.");
    }
    if edges.iter().any(|edge| {
        edge.outcome == "invalid"
            && edge.source == EdgeSource::Default
            && edge.fires != Fireability::Never
    }) {
        sentences.push("A check that cannot run parks at @user.");
    }
    if spec.workflow.deadline.is_none()
        && edges
            .iter()
            .any(|edge| edge.outcome == "deadline" && edge.source == EdgeSource::Default)
    {
        sentences.push("No deadline is set; a deadline granted to a run parks it at @user.");
    }
    sentences
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAND: &str = r#"
    [workflow]
    scope = 'worktree'
    start = 'begin'
    [checks.begin]
    shell = 'true'
    on.pass = 'sweep'
    on.fail = '@user'
    [steps.sweep]
    shell = 'echo sweep'
    timeout = '1h'
    then = 'watch'
    [checks.watch]
    cases = [{ when = ['ci=passed'], outcome = 'green' }]
    else = 'red'
    on.green = 'notes'
    on.red = 'fix'
    timeout = '3d'
    [steps.fix]
    shell = 'echo fix'
    timeout = '1h'
    then = 'watch'
    max-visits = 4
    [checks.notes]
    shell = 'true'
    on.pass = 'triage'
    on.fail = '@done'
    [steps.triage]
    shell = 'echo triage'
    timeout = '1h'
    then = 'ruled'
    max-visits = 2
    [checks.ruled]
    shell = 'true'
    on.pass = 'triage'
    on.fail = '@done'
    "#;

    #[test]
    fn turn_end_footer_only_describes_turn_agent_steps() {
        for (runner, turn_end) in [
            ("shell='true'", false),
            ("agent='claude'\nlifecycle='stay'", false),
            ("agent='claude'\nlifecycle='until'", false),
            ("agent='claude'", true),
        ] {
            let spec: WorkflowSpec = toml::from_str(&format!(
                "[workflow]\nscope='worktree'\nstart='gate'\n\
                 [checks.gate]\nshell='true'\non.pass='work'\n\
                 [steps.work]\nthen='gate'\n{runner}\n"
            ))
            .unwrap();
            assert_eq!(
                defaults(&spec)
                    .contains(&"A step goes on to its next check however its turn ends."),
                turn_end,
                "{runner}"
            );
        }
    }

    fn record(node: &str) -> RunRecord {
        serde_json::from_value(serde_json::json!({
            "version":1, "id":rimz::ids::WorkflowRunId::new(), "workflow":"land",
            "spec_dir":"/spec", "spec_digest":"digest", "project_root":"/repo",
            "scope":{"kind":"worktree", "path":"/repo/lane"},
            "created_at":jiff::Timestamp::UNIX_EPOCH, "entered_at":jiff::Timestamp::UNIX_EPOCH,
            "state":"active", "node":node, "visits":{}, "visit_grant":0,
            "deadline_fired":false, "next_seq":1,
        }))
        .unwrap()
    }

    fn route(node: &str, outcome: &str, target: &str) -> Hop {
        Hop {
            at: jiff::Timestamp::UNIX_EPOCH,
            node: node.into(),
            outcome: Some(outcome.into()),
            target: Some(target.into()),
            count: 1,
            kind: HopKind::Route,
        }
    }

    fn land_hops() -> Vec<Hop> {
        vec![
            route("begin", "pass", "sweep"),
            route("sweep", "ended", "watch"),
            route("watch", "green", "notes"),
            route("notes", "pass", "triage"),
        ]
    }

    fn horizontal_text(record: &RunRecord, hops: &[Hop], width: Option<usize>) -> String {
        let spec: WorkflowSpec = toml::from_str(LAND).unwrap();
        let mut out = anstream::StripStream::new(Vec::new());
        horizontal(&mut out, &spec, record, hops, &[], width, 6).unwrap();
        String::from_utf8(out.into_inner()).unwrap()
    }

    #[test]
    fn reached_counts_entries_not_repeated_wait_answers() {
        let mut wait = route("watch", "pending", "@wait");
        wait.count = 3;
        let hops = [
            route("begin", "pass", "sweep"),
            route("sweep", "ended", "watch"),
            wait,
            route("watch", "green", "clean"),
        ];
        assert_eq!(
            reached(&hops, &[]),
            BTreeMap::from([("begin", 1), ("sweep", 1), ("watch", 1), ("clean", 1),])
        );
    }

    #[test]
    fn horizontal_walk_distinguishes_kinds_and_states_without_color() {
        let hops = land_hops();
        let text = horizontal_text(&record("triage"), &hops, None);
        assert_eq!(
            text,
            "      ◆ begin ─ ● sweep ─ ◆ watch ─ ◆ notes ─ ⢿ triage ─ ◇ ruled\n"
        );
        assert_eq!(text.matches("triage").count(), 1, "{text}");
        let current_check = horizontal_text(&record("watch"), &hops[..2], None);
        assert!(
            current_check.contains("◆ begin ─ ● sweep ─ ⢿ watch ─ ◇ notes ─ ○ triage ─ ◇ ruled"),
            "{current_check}"
        );
        assert!(
            !text.contains("fix") && !current_check.contains("fix"),
            "{text}\n{current_check}"
        );
        let mut held = record("sweep");
        held.wait = Some(
            serde_json::from_value(serde_json::json!({
                "node":"sweep", "outcome":"capacity", "hold":"queue",
                "first":jiff::Timestamp::UNIX_EPOCH, "last":jiff::Timestamp::UNIX_EPOCH,
                "count":1,
            }))
            .unwrap(),
        );
        let text = horizontal_text(&held, &hops[..1], None);
        assert!(text.contains("◆ begin ─ ◉ sweep ─ ◇ watch"), "{text}");
        for set in ["unicode", "nerd_font"] {
            let mut theme = rimz::config::ThemeConfig::default();
            theme.glyphs.set = Some(set.into());
            let glyph = rimz::theme::theme_glyphs(&theme);
            let current = glyph(super::super::workflow_view::glyph_role(&held));
            for future in [GlyphRole::PipelineFuture, GlyphRole::PipelineCheckFuture] {
                assert_ne!(current, glyph(future), "{set}");
            }
        }
    }

    #[test]
    fn horizontal_detours_hang_under_their_parent_and_follow_width_elision() {
        let mut record = record("triage");
        record.visits.insert(
            "fix".into(),
            rimz::store::workflow::StepVisits {
                total: 2,
                ..Default::default()
            },
        );
        let mut hops = land_hops();
        hops.splice(
            2..2,
            [
                route("watch", "red", "fix"),
                route("fix", "ended", "watch"),
                route("watch", "red", "fix"),
                route("fix", "ended", "watch"),
            ],
        );
        let text = horizontal_text(&record, &hops, None);
        let lines = text.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(
            lines[1].find('╰'),
            lines[0]
                .find("◆ watch")
                .map(|at| unicode_width::UnicodeWidthStr::width(&lines[0][..at])),
            "{text}"
        );
        assert!(lines[1].contains("╰─ ● fix ×2 ↑"), "{text}");
        record.node = "fix".into();
        let current = horizontal_text(&record, &hops[..5], None);
        assert!(current.contains("╰─ ⢿ fix ×2 ↑"), "{current}");
        assert_eq!(current.matches("fix").count(), 1, "{current}");
        record.node = "triage".into();
        for width in [40, 60] {
            let text = horizontal_text(&record, &hops, Some(width));
            assert!(
                text.starts_with("      … ") && text.contains("◆ notes ─ ⢿ triage"),
                "{width}: {text}"
            );
            assert!(
                text.lines()
                    .all(|line| unicode_width::UnicodeWidthStr::width(line) <= width),
                "{width}: {text}"
            );
            assert_eq!(text.contains("╰─"), text.contains("◆ watch"), "{text}");
        }
    }

    #[test]
    fn horizontal_elides_unreached_left_and_reached_right_branches() {
        let spec: WorkflowSpec = toml::from_str(
            "[workflow]\nscope='worktree'\nstart='choose'\n\
             [checks.choose]\nshell='true'\non.a='unit_tests'\n\
             on.b='integration_tests'\non.c='used'\n\
             [steps.unit_tests]\nshell='true'\nthen='used'\n\
             [steps.integration_tests]\nshell='true'\nthen='used'\n\
             [checks.used]\nshell='true'\non.pass='final'\n\
             [checks.final]\nshell='true'\non.pass='@done'\non.retry='choose'\n",
        )
        .unwrap();
        let mut hops = vec![route("choose", "c", "used"), route("used", "pass", "final")];
        let mut rendered = Vec::new();
        for (node, width, anchors) in [
            ("final", 60, "◆ used ─ ⢿ final"),
            ("choose", 40, "⢿ choose"),
        ] {
            if node == "choose" {
                hops.push(route("final", "retry", "choose"));
            }
            let mut out = anstream::StripStream::new(Vec::new());
            horizontal(&mut out, &spec, &record(node), &hops, &[], Some(width), 6).unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            assert!(text.contains(anchors), "{text}");
            rendered.push((width, text));
        }
        assert!(
            rendered
                .iter()
                .all(|(width, text)| text.lines().all(|line| line.width() <= *width)),
            "{rendered:#?}"
        );
    }

    #[test]
    fn horizontal_keeps_a_terminal_detour_and_its_parent_even_without_visits() {
        let spec: WorkflowSpec = toml::from_str(LAND).unwrap();
        let mut record = record("fix");
        record.state = RunState::Failed;
        let mut hops = land_hops()[..2].to_vec();
        hops.extend([
            route("watch", "red", "fix"),
            route("fix", "capacity", "@failed"),
            Hop {
                kind: HopKind::Terminal {
                    state: record.state,
                },
                ..route("fix", "failed", "@failed")
            },
        ]);
        let mut rendered = Vec::new();
        for visits in [0, 1] {
            record.visits.insert(
                "fix".into(),
                rimz::store::workflow::StepVisits {
                    total: visits,
                    ..Default::default()
                },
            );
            for width in [None, Some(40)] {
                let mut out = Vec::new();
                horizontal(&mut out, &spec, &record, &hops, &[], width, 6).unwrap();
                rendered.push((width, String::from_utf8(out).unwrap()));
            }
        }
        assert!(
            rendered.iter().all(|(_, text)| text.contains("watch")
                && text.contains(&render::paint(render::palette::alarm(), "fix"))
                && text.contains(&render::paint(render::palette::alarm(), "●"))),
            "{rendered:#?}"
        );
        for (width, text) in rendered {
            let text = anstream::adapter::strip_str(&text).to_string();
            assert_eq!(text.matches("fix").count(), 1, "{text}");
            assert!(
                width.is_none_or(|width| text.lines().all(|line| line.width() <= width)),
                "{text}"
            );
        }
    }

    #[test]
    fn horizontal_appends_resumed_and_reached_unreachable_nodes() {
        let spec: WorkflowSpec = toml::from_str(&format!(
            "{LAND}\n[steps.resumed]\nshell='true'\nthen='watch'\n\
             [steps.never_used]\nshell='true'\nthen='watch'\n"
        ))
        .unwrap();
        let machine = super::super::super::machine_config();
        let glyph = rimz::theme::theme_glyphs(&machine.theme);
        let mut rendered = Vec::new();
        for (state, node) in [
            (RunState::Active, "resumed"),
            (RunState::Parked, "resumed"),
            (RunState::Active, "watch"),
        ] {
            let mut record = record(node);
            record.state = state;
            let mut hops = vec![Hop {
                kind: HopKind::Resume,
                ..route("resumed", "resume", "resumed")
            }];
            if node == "watch" {
                hops.push(route("resumed", "ended", "watch"));
            }
            let role = if node == "resumed" {
                super::super::workflow_view::glyph_role(&record)
            } else {
                GlyphRole::PipelinePassed
            };
            let mut out = anstream::StripStream::new(Vec::new());
            horizontal(&mut out, &spec, &record, &hops, &[], Some(40), 6).unwrap();
            rendered.push((
                format!("      {} resumed", glyph(role)),
                String::from_utf8(out.into_inner()).unwrap(),
            ));
        }
        assert!(
            rendered
                .iter()
                .all(|(last, text)| text.lines().last() == Some(last.as_str())
                    && text.matches("resumed").count() == 1
                    && !text.contains("never_used")
                    && text.lines().all(|line| line.width() <= 40)),
            "{rendered:#?}"
        );
    }

    #[test]
    fn horizontal_terminal_marks_only_the_failed_ending_node_alarm() {
        let spec: WorkflowSpec = toml::from_str(LAND).unwrap();
        let actual = [RunState::Done, RunState::Failed].map(|state| {
            let mut record = record("watch");
            record.state = state;
            let mut hops = land_hops()[..2].to_vec();
            hops.push(route(
                "watch",
                state.as_str(),
                if state == RunState::Done {
                    "@done"
                } else {
                    "@failed"
                },
            ));
            hops.push(Hop {
                kind: HopKind::Terminal { state },
                target: None,
                ..hops.last().unwrap().clone()
            });
            let mut out = Vec::new();
            horizontal(&mut out, &spec, &record, &hops, &[], None, 0).unwrap();
            let text = String::from_utf8(out).unwrap();
            (
                text.matches(&render::paint(render::palette::good(), "◆"))
                    .count(),
                text.matches(&render::paint(render::palette::alarm(), "◆"))
                    .count(),
            )
        });
        assert_eq!(actual, [(2, 0), (1, 1)]);
    }

    #[test]
    fn limit_routes_use_the_same_direction_marks_as_other_edges() {
        let spec: WorkflowSpec = toml::from_str(
            &LAND
                .replace("timeout = '3d'", "timeout = '3d'\non.timeout = 'fix'")
                .replace("max-visits = 4", "max-visits = 4\non.cap = 'notes'"),
        )
        .unwrap();
        let rail = Rail::definition(&spec);
        let watch = rail.rows.iter().find(|row| row.name == "watch").unwrap();
        let fix = rail.rows.iter().find(|row| row.name == "fix").unwrap();
        assert!(
            watch.heading.contains("waits ≤3d, then → fix"),
            "{}",
            watch.heading
        );
        assert!(
            fix.heading.contains("4 visits, then → notes"),
            "{}",
            fix.heading
        );
    }

    #[test]
    fn multiline_commands_stay_on_one_rail_line() {
        let headings = ["run", "shell"].map(|runner| {
            let spec: WorkflowSpec = toml::from_str(&format!(
                "[workflow]\nscope='worktree'\nstart='gate'\n\
                 [checks.gate]\n{runner}=\"\"\"echo first\necho second\"\"\"\non.pass='work'\n\
                 [steps.work]\n{runner}=\"\"\"echo first\necho second\"\"\"\nthen='gate'\n"
            ))
            .unwrap();
            Rail::definition(&spec)
                .rows
                .into_iter()
                .map(|row| row.heading)
                .collect::<Vec<_>>()
        });
        assert!(
            headings
                .iter()
                .flatten()
                .all(|heading| heading.contains("echo first; echo second")
                    && !heading.contains('\n')),
            "{headings:?}"
        );
    }

    #[test]
    fn land_rail_agrees_with_validate_and_arrows_follow_the_walk() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("land");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("workflow.toml"), LAND).unwrap();
        let spec = rimz::harness::workflow::load(&dir).unwrap();
        let report = rimz::harness::workflow::validate(&dir, &Default::default(), None);
        assert!(report.errors.is_empty(), "{:?}", report.errors);
        let mut out = anstream::StripStream::new(Vec::new());
        let rail = Rail::definition(&spec);
        rail.write(&mut out, 140).unwrap();
        let text = String::from_utf8(out.into_inner()).unwrap();
        assert!(text.contains("├─○ fix"), "{text}");
        assert!(!text.contains("├─○ sweep"), "{text}");
        assert!(!text.contains("├─○ triage"), "{text}");
        let footer = defaults(&spec).join("\n");
        let walk = spec.walk();
        for (index, row) in report.rows.iter().enumerate() {
            let mut out = anstream::StripStream::new(Vec::new());
            rail.rows
                .iter()
                .find(|rail| rail.name == row.node)
                .unwrap()
                .write(&mut out, 140)
                .unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            assert!(text.contains(&format!("○ {}", row.node)), "{text}");
            for edge in &row.edges {
                match edge.outcome.as_str() {
                    "deadline" => assert!(footer.contains("deadline")),
                    "invalid" => assert!(footer.contains("cannot run")),
                    "unknown" => assert!(text.contains("an unread term above it waits")),
                    "timeout" if edge.fires != Fireability::Never => {
                        assert!(text.contains(&format!("waits ≤3d, then {}", edge.target)));
                    }
                    "cap" if edge.fires != Fireability::Never => {
                        assert!(text.contains(&format!("visits, then {}", edge.target)));
                    }
                    _ if edge.fires == Fireability::Never => {
                        assert!(!text.contains(&format!("{} → {}", edge.outcome, edge.target)));
                    }
                    _ => {
                        let position = walk.nodes.iter().position(|node| node.node == edge.target);
                        let target = match position {
                            Some(position) if position <= index => format!("↑ {}", edge.target),
                            Some(_) => format!("→ {}", edge.target),
                            None => edge.target.clone(),
                        };
                        assert!(text.contains(&target), "missing {target}:\n{text}");
                    }
                }
            }
        }
        for width in [80, 140] {
            let mut out = anstream::StripStream::new(Vec::new());
            Rail::definition(&spec).write(&mut out, width).unwrap();
            let text = String::from_utf8(out.into_inner()).unwrap();
            assert!(
                text.lines()
                    .all(|line| unicode_width::UnicodeWidthStr::width(line) <= width)
            );
        }
    }
}
